use crate::types::IndexingPhase;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::indexer::edges::{CallEdge, insert_call_edges_batch, insert_symbols_batch};
use crate::indexer::lang_constants::language_for_extension;
use crate::indexer::parser::{extract_calls, extract_file_aliases, extract_symbols};

/// Directories never descended into during a project scan.
const IGNORE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "legacy",
];

/// Maximum number of same-named symbols a call may resolve to before it is
/// dropped as too ambiguous (conservative).
const MAX_CALLEE_CANDIDATES: usize = 20;

/// Recursively collect tier-0 source files under `root`, skipping ignored and
/// dot-prefixed directories. Deterministic order is imposed by the caller.
fn collect_source_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && (name.starts_with('.') || IGNORE_DIRS.contains(&name))
            {
                continue;
            }
            collect_source_files(&path, out);
        } else if ft.is_file()
            && let Some(ext) = path.extension().and_then(|e| e.to_str())
            && language_for_extension(ext).is_some()
        {
            out.push(path);
        }
    }
}

fn hash_content(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn mtime_secs(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Relative path of `file` under `project_root`, normalised to forward slashes.
fn rel_path(project_root: &Path, file: &Path) -> String {
    file.strip_prefix(project_root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Result of an incremental reindex pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReindexSummary {
    pub changed: usize,
    pub deleted: usize,
}

impl ReindexSummary {
    pub fn is_noop(&self) -> bool {
        self.changed == 0 && self.deleted == 0
    }
}

/// Drop all rows belonging to a single file (symbols, call sites, file_index).
/// Call edges are rebuilt globally by [`rebuild_graph`], so they are not touched here.
fn remove_file_rows(tx: &rusqlite::Transaction, rel: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM symbols WHERE path = ?1", [rel])?;
    tx.execute("DELETE FROM call_sites WHERE from_path = ?1", [rel])?;
    tx.execute("DELETE FROM file_index WHERE path = ?1", [rel])?;
    Ok(())
}

fn upsert_file_index(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: &str,
    hash: &str,
    mtime: f64,
    symbol_count: usize,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO file_index (path, hash, language, symbol_count, last_indexed, mtime) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![rel, hash, lang, symbol_count as i64, now, mtime],
    )?;
    Ok(())
}

/// Extract and persist one file's symbols and call sites. The caller must have
/// already removed any prior rows for this path. Returns the symbol count.
///
/// `qualified_name` is `relpath::name` (`#line` suffix on intra-file collision)
/// so the UNIQUE(qualified_name) index never rejects a real symbol.
fn index_one_file(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: &str,
    source: &str,
) -> rusqlite::Result<usize> {
    let mut syms = extract_symbols(source, lang, rel).unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    for s in &mut syms {
        s.path = rel.to_string();
        s.qualified_name = format!("{}::{}", rel, s.name);
        if !seen.insert(s.qualified_name.clone()) {
            s.qualified_name = format!("{}#{}", s.qualified_name, s.line_start);
            seen.insert(s.qualified_name.clone());
        }
    }

    // (bare name, line_start) → qualified_name, for attributing call sites.
    let qn_by_loc: HashMap<(String, usize), String> = syms
        .iter()
        .map(|s| ((s.name.clone(), s.line_start), s.qualified_name.clone()))
        .collect();
    let file_symbols: HashSet<String> = syms.iter().map(|s| s.name.clone()).collect();

    let count = syms.len();
    insert_symbols_batch(tx, &syms)?;

    // De-reference simple local aliases (`x = helper; x()` → helper) before storing
    // call sites, so the graph attributes the call to the real target.
    let aliases = extract_file_aliases(source, lang, &file_symbols);
    let calls = extract_calls(source, lang, rel).unwrap_or_default();
    let mut stmt = tx.prepare(
        "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for c in &calls {
        if let Some(enc_qn) = qn_by_loc.get(&(c.enclosing_name.clone(), c.enclosing_line)) {
            let callee = aliases.get(&c.callee).unwrap_or(&c.callee);
            stmt.execute(rusqlite::params![rel, enc_qn, callee, c.line as i64])?;
        }
    }
    Ok(count)
}

/// Rebuild the call graph from the persisted `call_sites` against the current
/// symbol table, then refresh caller_count, coreness, and is_hub.
///
/// This is pure DB work (no file parsing), so incremental passes only re-parse
/// the files that actually changed while the graph stays globally consistent.
fn rebuild_graph(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    // name → [(qualified_name, path)]
    let mut qns_by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT name, qualified_name, path FROM symbols")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, qn, path) in rows {
            qns_by_name.entry(name).or_default().push((qn, path));
        }
    }

    let sites: Vec<(String, String, String, Option<i64>)> = {
        let mut stmt =
            tx.prepare("SELECT from_path, enclosing_qn, callee_name, call_line FROM call_sites")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    // One edge per (caller, callee) pair; the first call site supplies the line.
    let mut edges: Vec<CallEdge> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for (from_path, enc_qn, callee, line) in &sites {
        let Some(targets) = qns_by_name.get(callee) else {
            continue;
        };
        if targets.len() > MAX_CALLEE_CANDIDATES {
            continue;
        }
        for (to_qn, to_path) in targets {
            if !seen_pairs.insert((enc_qn.clone(), to_qn.clone())) {
                continue;
            }
            let confidence = if to_path == from_path {
                "resolved"
            } else {
                "textual"
            };
            edges.push(CallEdge {
                from_symbol: enc_qn.clone(),
                to_symbol: to_qn.clone(),
                call_site_line: line.map(|l| l as i32),
                edge_confidence: confidence.to_string(),
                from_path: Some(from_path.clone()),
                to_path: Some(to_path.clone()),
            });
        }
    }

    tx.execute("DELETE FROM call_edges", [])?;
    insert_call_edges_batch(tx, &edges)?;
    tx.execute(
        "UPDATE symbols SET caller_count = \
            (SELECT COUNT(DISTINCT from_symbol) FROM call_edges WHERE to_symbol = symbols.qualified_name)",
        [],
    )?;
    crate::graph::coreness::compute_coreness(tx)?;
    let hub_config = crate::config::HubThresholdConfig::default();
    crate::graph::hub::update_is_hub_flags(tx, &hub_config)?;
    Ok(())
}

/// Full (re)index of a project tree into `conn`.
///
/// Scan → extract symbols + call sites (tree-sitter) → rebuild graph
/// (caller_count, coreness, is_hub). Everything is one transaction so the graph
/// is never observed half-built.
pub fn run_indexing_pipeline(conn: &mut Connection, project_root: &Path) -> rusqlite::Result<()> {
    let mut files = Vec::new();
    collect_source_files(project_root, &mut files);
    files.sort();

    let now = now_secs();
    let tx = conn.transaction()?;

    // Full reindex: clear everything. (Triggers keep the FTS tables in sync.)
    tx.execute("DELETE FROM call_sites", [])?;
    tx.execute("DELETE FROM import_edges", [])?;
    tx.execute("DELETE FROM symbols", [])?;
    tx.execute("DELETE FROM file_index", [])?;

    for file in &files {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(lang) = language_for_extension(ext) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = rel_path(project_root, file);
        let count = index_one_file(&tx, &rel, lang, &source)?;
        upsert_file_index(
            &tx,
            &rel,
            lang,
            &hash_content(&source),
            mtime_secs(file),
            count,
            now,
        )?;
    }

    rebuild_graph(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Incremental reindex: re-parse only files whose content hash changed (or are
/// new), drop rows for deleted files, then rebuild the graph once if anything
/// changed. Cheap to call repeatedly — the basis for the file watcher.
pub fn reindex_changed(
    conn: &mut Connection,
    project_root: &Path,
) -> rusqlite::Result<ReindexSummary> {
    let existing: HashMap<String, String> = {
        let mut stmt = conn.prepare("SELECT path, hash FROM file_index")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    };

    let mut files = Vec::new();
    collect_source_files(project_root, &mut files);
    files.sort();

    let now = now_secs();
    let tx = conn.transaction()?;
    let mut summary = ReindexSummary::default();
    let mut seen_paths: HashSet<String> = HashSet::new();

    for file in &files {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(lang) = language_for_extension(ext) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = rel_path(project_root, file);
        seen_paths.insert(rel.clone());
        let hash = hash_content(&source);
        if existing.get(&rel) == Some(&hash) {
            continue; // unchanged — skip the parse
        }
        remove_file_rows(&tx, &rel)?;
        let count = index_one_file(&tx, &rel, lang, &source)?;
        upsert_file_index(&tx, &rel, lang, &hash, mtime_secs(file), count, now)?;
        summary.changed += 1;
    }

    for path in existing.keys() {
        if !seen_paths.contains(path) {
            remove_file_rows(&tx, path)?;
            summary.deleted += 1;
        }
    }

    if !summary.is_noop() {
        rebuild_graph(&tx)?;
    }
    tx.commit()?;
    Ok(summary)
}

pub struct IndexStateMachine {
    phase: IndexingPhase,
}

impl Default for IndexStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexStateMachine {
    pub fn new() -> Self {
        Self {
            phase: IndexingPhase::Scanning,
        }
    }
    pub fn current(&self) -> IndexingPhase {
        self.phase
    }
    pub fn advance(&mut self) {
        self.phase = match self.phase {
            IndexingPhase::Scanning => IndexingPhase::Parsing,
            IndexingPhase::Parsing => IndexingPhase::BuildingEdges,
            IndexingPhase::BuildingEdges => IndexingPhase::Ready,
            IndexingPhase::Ready => IndexingPhase::Ready,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn test_phase_transition() {
        let mut sm = IndexStateMachine::new();
        assert_eq!(sm.current(), IndexingPhase::Scanning);
        sm.advance();
        assert_eq!(sm.current(), IndexingPhase::Parsing);
    }

    #[test]
    fn test_run_indexing_pipeline_empty_dir() {
        let dir = std::env::temp_dir().join(format!("ci_idx_empty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(run_indexing_pipeline(&mut conn, &dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_indexing_pipeline_real_extraction() {
        let dir = std::env::temp_dir().join(format!("ci_idx_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\ndef main():\n    helper()\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = 'a.py::main' AND to_symbol = 'a.py::helper'",
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_alias_resolution_edge() {
        let dir = std::env::temp_dir().join(format!("ci_idx_alias_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // main calls helper indirectly through a local alias `x = helper`.
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\ndef main():\n    x = helper\n    x()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        // The alias is de-referenced, so the edge points at helper.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = 'a.py::main' AND to_symbol = 'a.py::helper'",
            ),
            1,
            "alias x=helper should resolve the call to helper"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reindex_incremental_add_modify_delete() {
        let dir = std::env::temp_dir().join(format!("ci_idx_inc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def helper():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1);

        // No change → no-op.
        assert!(reindex_changed(&mut conn, &dir).unwrap().is_noop());

        // Add a second file that calls helper → new symbol + cross-file edge.
        std::fs::write(dir.join("b.py"), "def caller():\n    helper()\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 1,
                deleted: 0
            }
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1
        );

        // Modify b.py to no longer call helper → edge drops, caller_count → 0.
        std::fs::write(dir.join("b.py"), "def caller():\n    pass\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 1,
                deleted: 0
            }
        );
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0
        );

        // Delete b.py → its symbol disappears.
        std::fs::remove_file(dir.join("b.py")).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 0,
                deleted: 1
            }
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
