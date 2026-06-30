use crate::types::IndexingPhase;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::indexer::edges::{CallEdge, insert_call_edges_batch, insert_symbols_batch};
use crate::indexer::lang_constants::language_for_extension;
use crate::indexer::parser::{
    extract_calls, extract_file_aliases, extract_symbols, extract_type_map,
};

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

/// A persisted call site loaded for graph rebuild:
/// (from_path, enclosing_qn, callee_name, call_line, confidence, target_class).
type CallSiteRow = (String, String, String, Option<i64>, String, Option<String>);

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
    tx.execute("DELETE FROM import_edges WHERE from_path = ?1", [rel])?;
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
    entry_point_patterns: &[String],
) -> rusqlite::Result<usize> {
    let mut syms = extract_symbols(source, lang, rel).unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    for s in &mut syms {
        s.path = rel.to_string();
        // Methods are qualified by their class so two classes' `run` don't collide.
        s.qualified_name = match &s.class_context {
            Some(cls) => format!("{}::{}::{}", rel, cls, s.name),
            None => format!("{}::{}", rel, s.name),
        };
        if !seen.insert(s.qualified_name.clone()) {
            s.qualified_name = format!("{}#{}", s.qualified_name, s.line_start);
            seen.insert(s.qualified_name.clone());
        }
        if !s.is_entry_point
            && entry_point_patterns
                .iter()
                .any(|p| s.qualified_name.contains(p.as_str()))
        {
            s.is_entry_point = true;
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

    // Imports → import_edges (to_path resolved later, globally) + import_map.
    let imports = crate::indexer::imports::extract_imports(source, lang);
    let mut import_map: HashMap<String, String> = HashMap::new();
    {
        let mut istmt = tx.prepare(
            "INSERT INTO import_edges (from_path, to_path, module_name, symbols_used) \
             VALUES (?1, NULL, ?2, ?3)",
        )?;
        for imp in &imports {
            let symbols_used =
                serde_json::to_string(&imp.imported_names).unwrap_or_else(|_| "[]".to_string());
            istmt.execute(rusqlite::params![rel, imp.module_name, symbols_used])?;
            for n in &imp.imported_names {
                import_map
                    .entry(n.clone())
                    .or_insert_with(|| imp.module_name.clone());
            }
        }
    }

    // Full resolver context: file symbols + imports + type annotations.
    let ctx = crate::resolver::FileContext {
        file_symbols,
        import_map,
        type_map: extract_type_map(source, lang),
    };
    let resolver = crate::resolver::conservative::ConservativeResolver::new();
    let aliases = extract_file_aliases(source, lang, &ctx);

    // Calls → call_sites. Tier-1 (conservative resolver): file symbol / import /
    // alias → "resolved", else "textual". Tier-2: a still-textual *method* call
    // whose receiver type is inferable (self/this → enclosing class, or a typed
    // variable) becomes "inferred" with a target_class for the rebuild to match.
    let calls = extract_calls(source, lang, rel).unwrap_or_default();
    let mut stmt = tx.prepare(
        "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line, confidence, receiver, target_class) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for c in &calls {
        if let Some(enc_qn) = qn_by_loc.get(&(c.enclosing_name.clone(), c.enclosing_line)) {
            let mut confidence = resolver
                .resolve_tier1(&c.callee, &ctx, &aliases)
                .confidence
                .to_string();
            let mut target_class: Option<String> = None;
            if confidence == "textual"
                && let Some(receiver) = &c.receiver
                && let Some(cls) =
                    resolver.resolve_tier2(receiver, &ctx, c.enclosing_class.as_deref())
            {
                confidence = "inferred".to_string();
                target_class = Some(cls);
            }
            let callee = aliases.get(&c.callee).unwrap_or(&c.callee);
            stmt.execute(rusqlite::params![
                rel,
                enc_qn,
                callee,
                c.line as i64,
                confidence,
                c.receiver,
                target_class
            ])?;
        }
    }
    Ok(count)
}

/// Rebuild the call graph from the persisted `call_sites` against the current
/// symbol table, then refresh caller_count, coreness, and is_hub.
///
/// This is pure DB work (no file parsing), so incremental passes only re-parse
/// the files that actually changed while the graph stays globally consistent.
fn rebuild_graph(
    tx: &rusqlite::Transaction,
    hub_config: &crate::config::HubThresholdConfig,
) -> rusqlite::Result<()> {
    // name → [(qn, path)] for tier-1; (name, class) → [(qn, path)] for tier-2.
    let mut by_name: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut by_name_class: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT name, qualified_name, path, class_context FROM symbols")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, qn, path, cls) in rows {
            by_name
                .entry(name.clone())
                .or_default()
                .push((qn.clone(), path.clone()));
            if let Some(c) = cls {
                by_name_class.entry((name, c)).or_default().push((qn, path));
            }
        }
    }

    let sites: Vec<CallSiteRow> = {
        let mut stmt = tx.prepare(
            "SELECT from_path, enclosing_qn, callee_name, call_line, confidence, target_class \
             FROM call_sites",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    // One edge per (caller, callee) pair; the first call site supplies the line.
    // Confidence is the resolver's verdict recorded at extraction time. A tier-2
    // call (target_class set) resolves the method within that class only.
    let mut edges: Vec<CallEdge> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for (from_path, enc_qn, callee, line, confidence, target_class) in &sites {
        let targets = match target_class {
            Some(cls) => by_name_class.get(&(callee.clone(), cls.clone())),
            None => by_name.get(callee),
        };
        let Some(targets) = targets else {
            continue;
        };
        if targets.len() > MAX_CALLEE_CANDIDATES {
            continue;
        }
        for (to_qn, to_path) in targets {
            if !seen_pairs.insert((enc_qn.clone(), to_qn.clone())) {
                continue;
            }
            edges.push(CallEdge {
                from_symbol: enc_qn.clone(),
                to_symbol: to_qn.clone(),
                call_site_line: line.map(|l| l as i32),
                edge_confidence: confidence.clone(),
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
    resolve_import_targets(tx)?;
    crate::graph::coreness::compute_coreness(tx)?;
    crate::graph::hub::update_is_hub_flags(tx, hub_config)?;
    Ok(())
}

/// Best-effort resolution of `import_edges.to_path` against indexed files, so the
/// `dependencies` tool's `imported_by` direction works for in-project imports.
/// External modules (no matching file) keep `to_path = NULL`.
fn resolve_import_targets(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    let known: HashSet<String> = {
        let mut stmt = tx.prepare("SELECT path FROM file_index")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    };
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare("SELECT id, from_path, module_name FROM import_edges")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut ustmt = tx.prepare("UPDATE import_edges SET to_path = ?1 WHERE id = ?2")?;
    for (id, from_path, module) in &rows {
        if let Some(target) = resolve_module_to_path(from_path, module, &known) {
            ustmt.execute(rusqlite::params![target, id])?;
        }
    }
    Ok(())
}

/// Map a module/path string to an indexed file path, trying the conventions of
/// all six languages (dotted, scoped, and JS-relative) plus common index files.
fn resolve_module_to_path(
    from_path: &str,
    module: &str,
    known: &HashSet<String>,
) -> Option<String> {
    let m = module.trim().trim_matches(|c| c == '"' || c == '\'');
    if m.is_empty() {
        return None;
    }

    // Build candidate base paths (without extension), forward-slash normalised.
    let mut bases: Vec<String> = Vec::new();
    let from_dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    if m.starts_with("./") || m.starts_with("../") {
        // JS/TS relative import, resolved against the importing file's directory.
        bases.push(normalize_rel(&from_dir, m));
    } else if let Some(stripped) = m.strip_prefix('.') {
        // Python relative import: leading dots climb packages.
        let ups = m.len() - m.trim_start_matches('.').len();
        let tail = stripped.trim_start_matches('.').replace('.', "/");
        let mut dir = from_dir.clone();
        for _ in 1..ups {
            dir = parent_of(&dir);
        }
        bases.push(if tail.is_empty() {
            dir
        } else {
            join_rel(&dir, &tail)
        });
    } else {
        // Absolute/dotted/scoped module, relative to project root.
        let norm = m.replace("::", "/").replace('.', "/");
        let norm = norm
            .trim_start_matches("crate/")
            .trim_start_matches("self/")
            .trim_start_matches("super/")
            .to_string();
        // The full path, and — for item imports like `use a::b::Item` — its parent.
        // Also try a conventional `src/` source root.
        bases.push(norm.clone());
        if let Some((parent, _)) = norm.rsplit_once('/') {
            bases.push(parent.to_string());
            bases.push(format!("src/{parent}"));
        }
        bases.push(format!("src/{norm}"));
    }

    const EXTS: &[&str] = &[".py", ".rs", ".go", ".ts", ".tsx", ".js", ".jsx", ".java"];
    const INDEX_SUFFIXES: &[&str] = &[
        "/__init__.py",
        "/mod.rs",
        "/index.ts",
        "/index.tsx",
        "/index.js",
    ];
    for base in &bases {
        let base = base.trim_start_matches("./");
        if known.contains(base) {
            return Some(base.to_string());
        }
        for e in EXTS {
            let c = format!("{base}{e}");
            if known.contains(&c) {
                return Some(c);
            }
        }
        for s in INDEX_SUFFIXES {
            let c = format!("{base}{s}");
            if known.contains(&c) {
                return Some(c);
            }
        }
    }
    None
}

fn parent_of(dir: &str) -> String {
    std::path::Path::new(dir)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn join_rel(dir: &str, tail: &str) -> String {
    if dir.is_empty() {
        tail.to_string()
    } else {
        format!("{dir}/{tail}")
    }
}

/// Resolve `./`, `../` and `.` components of a JS-style relative path against a base dir.
fn normalize_rel(base_dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Full (re)index of a project tree into `conn`.
///
/// Scan → extract symbols + call sites (tree-sitter) → rebuild graph
/// (caller_count, coreness, is_hub). Everything is one transaction so the graph
/// is never observed half-built.
pub fn run_indexing_pipeline(conn: &mut Connection, project_root: &Path) -> rusqlite::Result<()> {
    let config = crate::config::load_config(project_root).unwrap_or_default();
    let entry_point_patterns = config.entry_points;

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
        let count = index_one_file(&tx, &rel, lang, &source, &entry_point_patterns)?;
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

    rebuild_graph(&tx, &config.hub_threshold)?;
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
    let config = crate::config::load_config(project_root).unwrap_or_default();
    let entry_point_patterns = config.entry_points;

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
        let count = index_one_file(&tx, &rel, lang, &source, &entry_point_patterns)?;
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
        rebuild_graph(&tx, &config.hub_threshold)?;
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
    fn test_entry_points_config_escape_hatch() {
        let dir = std::env::temp_dir().join(format!("ci_idx_entrycfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def custom_entry():\n    pass\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"entry_points": ["a.py::custom_entry"]}"#,
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT is_entry_point FROM symbols WHERE qualified_name = 'a.py::custom_entry'",
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT is_entry_point FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test: `rebuild_graph` used to hardcode `HubThresholdConfig::default()`
    /// instead of loading the project's `config.json`, so a custom `hub_threshold`
    /// (like `entry_points`'s config escape hatch above) was silently ignored.
    #[test]
    fn test_hub_threshold_config_escape_hatch() {
        let dir = std::env::temp_dir().join(format!("ci_idx_hubcfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\n\
             def caller_a():\n    helper()\n\n\
             def caller_b():\n    helper()\n\n\
             def caller_c():\n    helper()\n",
        )
        .unwrap();

        let mut conn_default = Connection::open_in_memory().unwrap();
        init_db(&conn_default).unwrap();
        run_indexing_pipeline(&mut conn_default, &dir).unwrap();
        assert_eq!(
            count(
                &conn_default,
                "SELECT is_hub FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0,
            "default min_callers=5 should not flag a 3-caller symbol as hub"
        );

        std::fs::write(
            dir.join("config.json"),
            r#"{"hub_threshold": {"min_callers": 1, "top_pct": 100, "min_callers_bridge": 1, "coreness_pct": 100}}"#,
        )
        .unwrap();
        let mut conn_custom = Connection::open_in_memory().unwrap();
        init_db(&conn_custom).unwrap();
        run_indexing_pipeline(&mut conn_custom, &dir).unwrap();
        assert_eq!(
            count(
                &conn_custom,
                "SELECT is_hub FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1,
            "custom min_callers=1/top_pct=100 should flag the same symbol as hub"
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
    fn test_imports_and_cross_file_resolved_confidence() {
        let dir = std::env::temp_dir().join(format!("ci_idx_imp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("main.py"),
            "from helper import helper\n\ndef run():\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        // import_edges populated and to_path resolved to the in-project file.
        let (to_path, module): (String, String) = conn
            .query_row(
                "SELECT COALESCE(to_path,''), module_name FROM import_edges WHERE from_path = 'main.py'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(module, "helper");
        assert_eq!(
            to_path, "helper.py",
            "import target resolved to in-project file"
        );

        // The cross-file call through the import is labelled "resolved".
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.py::run' AND to_symbol = 'helper.py::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "resolved",
            "imported call should be resolved, not textual"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tier2_method_resolution() {
        let dir = std::env::temp_dir().join(format!("ci_idx_tier2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a.py: a class with a method. b.py: a typed-parameter method call on it.
        std::fs::write(
            dir.join("a.py"),
            "class Service:\n    def process(self):\n        pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.py"),
            "def run(svc: Service):\n    svc.process()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        // Method is class-qualified.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::Service::process'",
            ),
            1,
            "method qualified_name should include its class"
        );

        // Tier-2: svc:Service ⇒ svc.process() resolves into Service, confidence inferred.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'b.py::run' AND to_symbol = 'a.py::Service::process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "inferred",
            "typed-receiver method call is tier-2 inferred"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tier2_go_pointer_receiver() {
        let dir = std::env::temp_dir().join(format!("ci_idx_go2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.go"),
            "package p\ntype Service struct{}\nfunc (s *Service) Process() int { return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.go"),
            "package p\nfunc run(s *Service) int { return s.Process() }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir).unwrap();

        // Go method is tagged with its receiver type as class_context.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.go::Service::Process'",
            ),
            1
        );
        // `*Service` receiver ⇒ s.Process() resolves into Service, inferred.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'b.go::run' AND to_symbol = 'a.go::Service::Process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(confidence, "inferred");

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
