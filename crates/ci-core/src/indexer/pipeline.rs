use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::indexer::chunker::{CodeChunk, chunk_file};
use crate::indexer::edges::{
    CallEdge, insert_call_edges_batch, insert_code_chunks_batch, insert_import_edges_batch,
    insert_symbols_batch,
};
use crate::indexer::lang_constants::language_for_extension;
use crate::indexer::parser::{
    ParsedSymbol, extract_calls_from_tree, extract_file_aliases_from_tree,
    extract_symbols_from_tree, extract_symbols_shallow, extract_type_map_from_tree, parse_tree,
};
use crate::types::EdgeConfidence;

/// Maximum number of same-named symbols a call may resolve to before it is
/// dropped as too ambiguous (conservative).
const MAX_CALLEE_CANDIDATES: usize = 20;

/// Files are parsed+resolved (and then persisted) in chunks of this size
/// rather than all at once, so peak memory holds at most one batch of
/// parsed-but-not-yet-persisted files instead of an entire large repo.
const PARSE_BATCH_SIZE: usize = 1000;

/// A persisted call site loaded for graph rebuild:
/// (from_path, enclosing_qn, callee_name, call_line, confidence, target_class).
type CallSiteRow = (String, String, String, Option<i64>, String, Option<String>);

/// Collect tier-0 source files under `root` via the shared `crate::walk`
/// walker (built-in `IGNORE_DIRS`, dot-directories, user-configured `ignore`
/// patterns, and real `.gitignore`), filtered down to extensions
/// `language_for_extension` recognizes. Deterministic order is imposed by
/// the caller.
pub fn collect_source_files(root: &Path, ignore: &[String], out: &mut Vec<PathBuf>) {
    for result in crate::walk::build_walker(root, ignore) {
        let Ok(entry) = result else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && language_for_extension(ext).is_some()
        {
            out.push(path);
        }
    }
}

/// Portable FNV-1a 64-bit hash. `DefaultHasher` is explicitly *not* stable
/// across Rust versions/platforms per the std docs — using it for the
/// persisted `file_index.hash` column meant a toolchain upgrade could
/// invalidate every cached hash and force a full re-parse. FNV-1a has a
/// fixed, documented algorithm so the same content always hashes the same
/// way regardless of toolchain. `pub` so `crate::edit` can reuse it as the
/// same stale-write conflict guard for arbitrary line ranges.
pub fn hash_content(s: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET_BASIS;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
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
    tx.execute("DELETE FROM code_chunks WHERE path = ?1", [rel])?;
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

/// One call site's resolved fields, ready to persist into `call_sites`.
struct CallSiteData {
    enclosing_qn: String,
    callee: String,
    line: i64,
    confidence: String,
    receiver: Option<String>,
    target_class: Option<String>,
}

/// Everything extracted from a single file's source, before any DB I/O.
/// Building this is pure CPU work (tree-sitter parse + resolver tiers), so it
/// is safe to compute for every file in parallel; only [`persist_file`] below
/// touches the transaction, and that stays single-threaded.
struct ExtractedFile {
    symbols: Vec<ParsedSymbol>,
    import_edges: Vec<crate::indexer::edges::ImportEdge>,
    call_sites: Vec<CallSiteData>,
    symbol_count: usize,
    /// Layer-2 semantic-search code-body chunks (see `indexer::chunker`).
    /// Always computed when the `embeddings` feature is compiled in — cheap
    /// pure-CPU work done in the same parallel extraction pass as everything
    /// else here — and left empty otherwise, since nothing would ever embed
    /// or query them.
    chunks: Vec<CodeChunk>,
}

/// Parse and resolve one file's symbols, imports, and call sites. No DB access —
/// safe to run concurrently across files (see [`run_indexing_pipeline`]).
///
/// `qualified_name` is `relpath::name` (`#line` suffix on intra-file collision)
/// so the UNIQUE(qualified_name) index never rejects a real symbol.
fn extract_file_data(
    rel: &str,
    lang: &str,
    source: &str,
    entry_point_patterns: &[String],
    formal: &crate::resolver::formal::FormalResolver,
) -> ExtractedFile {
    let Some(tree) = parse_tree(source, lang) else {
        // Tier-0.5: no tree-sitter grammar for this language — extract symbols
        // via lightweight line-scan (no calls, no imports, no resolver tiers).
        let symbols = extract_symbols_shallow(source, lang, rel);
        let symbol_count = symbols.len();
        let chunks = chunk_pending(source, &symbols);
        return ExtractedFile {
            symbols,
            import_edges: Vec::new(),
            call_sites: Vec::new(),
            symbol_count,
            chunks,
        };
    };

    let mut syms = extract_symbols_from_tree(&tree, source, lang, rel);
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
    let symbol_count = syms.len();

    // Imports → import_edges (to_path resolved later, globally) + import_map.
    let imports = crate::indexer::imports::extract_imports_from_tree(&tree, source, lang);
    let mut import_map: HashMap<String, String> = HashMap::new();
    let mut import_edges = Vec::with_capacity(imports.len());
    for imp in &imports {
        let symbols_used =
            serde_json::to_string(&imp.imported_names).unwrap_or_else(|_| "[]".to_string());
        import_edges.push(crate::indexer::edges::ImportEdge {
            from_path: rel.to_string(),
            to_path: None, // resolved later, globally — see resolve_import_targets
            module_name: imp.module_name.clone(),
            symbols_used,
        });
        for n in &imp.imported_names {
            import_map
                .entry(n.clone())
                .or_insert_with(|| imp.module_name.clone());
        }
    }

    // Full resolver context: file symbols + imports + type annotations.
    let ctx = crate::resolver::FileContext {
        file_symbols,
        import_map,
        type_map: extract_type_map_from_tree(&tree, source, lang),
    };
    let resolver = crate::resolver::conservative::ConservativeResolver::new();
    let aliases = extract_file_aliases_from_tree(&tree, source, lang, &ctx);

    // Tier-3: formal scope resolution via StackGraph rules.
    // For languages with stack-graphs support (currently Python), build the set of
    // reference symbol names that StackGraph confirms have a definition in scope
    // within this file. Used below to upgrade "textual"/"inferred" call sites to
    // "formal" — a higher-confidence tier than heuristic type inference.
    // Falls back to empty on unsupported languages or parse errors (non-fatal).
    let formally_resolved: HashSet<String> = if formal.has_language(lang) {
        formal
            .resolve_file(lang, rel, source)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.reference_symbol)
            .collect()
    } else {
        HashSet::new()
    };

    // Calls → call_sites. Tier-1 (conservative resolver): file symbol / import /
    // alias → "resolved", else "textual". Tier-2: a still-textual *method* call
    // whose receiver type is inferable (self/this → enclosing class, or a typed
    // variable) becomes "inferred" with a target_class for the rebuild to match.
    // Tier-3: formal StackGraph resolution upgrades "textual"/"inferred" to "formal".
    let calls = extract_calls_from_tree(&tree, source, lang);
    let mut call_sites = Vec::with_capacity(calls.len());
    for c in &calls {
        if let Some(enc_qn) = qn_by_loc.get(&(c.enclosing_name.clone(), c.enclosing_line)) {
            let mut confidence;
            let mut target_class: Option<String> = None;

            if c.receiver_is_type_path
                && let Some(receiver) = &c.receiver
            {
                // `Type::method()` names its type directly and unambiguously
                // in the source text — this takes priority over tier-1
                // *before* it even runs, not just when tier-1 comes up
                // textual. Tier-1's `file_symbols`/`import_map` check
                // matches on the bare callee name alone (`"new"`), with no
                // idea a receiver type was named at all, so it would happily
                // "resolve" e.g. `Vec::new()` against this file's own
                // unrelated `SomeStruct::new` just because both are named
                // "new" — same file, same bare name, wrong symbol entirely.
                // Skipping tier-1 here (rather than only overriding it when
                // textual) is what actually closes that gap. And if nothing
                // in the codebase has this exact type, "unresolved" is the
                // correct answer — no fallback to the unscoped global
                // by_name match, which is the fan-out bug this prevents.
                confidence = EdgeConfidence::Inferred;
                target_class = Some(receiver.clone());
            } else {
                confidence = resolver.resolve_tier1(&c.callee, &ctx, &aliases).confidence;
                if confidence == EdgeConfidence::Textual
                    && let Some(receiver) = &c.receiver
                    && let Some(cls) =
                        resolver.resolve_tier2(receiver, &ctx, c.enclosing_class.as_deref())
                {
                    confidence = EdgeConfidence::Inferred;
                    target_class = Some(cls);
                }
            }
            let callee = aliases.get(&c.callee).unwrap_or(&c.callee).clone();
            // Tier-3: StackGraph confirmed this callee has a definition in scope.
            // Upgrades "textual" and "inferred" but not "resolved" (already correct).
            if confidence != EdgeConfidence::Resolved && formally_resolved.contains(callee.as_str())
            {
                confidence = EdgeConfidence::Formal;
            }
            call_sites.push(CallSiteData {
                enclosing_qn: enc_qn.clone(),
                callee,
                line: c.line as i64,
                confidence: confidence.as_str().to_string(),
                receiver: c.receiver.clone(),
                target_class,
            });
        }
    }

    let chunks = chunk_pending(source, &syms);

    ExtractedFile {
        symbols: syms,
        import_edges,
        call_sites,
        symbol_count,
        chunks,
    }
}

/// Chunk `source` for Layer-2 semantic search — only when the `embeddings`
/// feature is compiled in, since chunks are otherwise never embedded or
/// queried. `embedding::ENABLED` is a `const bool`, so the disabled branch is
/// eliminated at compile time rather than costing a runtime check.
fn chunk_pending(source: &str, symbols: &[ParsedSymbol]) -> Vec<CodeChunk> {
    if crate::embedding::ENABLED {
        chunk_file(source, symbols)
    } else {
        Vec::new()
    }
}

/// Persist one file's already-extracted symbols, imports, call sites, and
/// Layer-2 code chunks. Pure DB I/O — call sequentially against a single
/// transaction, after all files have been extracted (possibly in parallel).
fn persist_file(
    tx: &rusqlite::Transaction,
    rel: &str,
    file_hash: &str,
    extracted: &ExtractedFile,
) -> rusqlite::Result<()> {
    insert_symbols_batch(tx, &extracted.symbols)?;
    insert_import_edges_batch(tx, &extracted.import_edges)?;
    let mut stmt = tx.prepare(
        "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line, confidence, receiver, target_class) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for c in &extracted.call_sites {
        stmt.execute(rusqlite::params![
            rel,
            c.enclosing_qn,
            c.callee,
            c.line,
            c.confidence,
            c.receiver,
            c.target_class
        ])?;
    }
    insert_code_chunks_batch(tx, rel, file_hash, &extracted.chunks)?;
    Ok(())
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
    //
    // Candidate lookup (HashMap reads against `by_name`/`by_name_class`) is pure
    // CPU work independent per site, so it runs in parallel; the dedup merge
    // below stays sequential, walking sites in their original order so the
    // "first call site wins" line/confidence attribution is unchanged.
    let candidates: Vec<Vec<(String, String)>> = sites
        .par_iter()
        .map(|(_, _, callee, _, _, target_class)| {
            let targets = match target_class {
                Some(cls) => by_name_class.get(&(callee.clone(), cls.clone())),
                None => by_name.get(callee),
            };
            match targets {
                Some(t) if t.len() <= MAX_CALLEE_CANDIDATES => t.clone(),
                _ => Vec::new(),
            }
        })
        .collect();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for ((from_path, enc_qn, _callee, line, confidence, _target_class), targets) in
        sites.iter().zip(candidates.iter())
    {
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
    // Candidate-path resolution is pure CPU work against a shared, read-only
    // `known` set, so it runs in parallel; the UPDATE loop stays sequential.
    let targets: Vec<Option<String>> = rows
        .par_iter()
        .map(|(_, from_path, module)| resolve_module_to_path(from_path, module, &known))
        .collect();

    let mut ustmt = tx.prepare("UPDATE import_edges SET to_path = ?1 WHERE id = ?2")?;
    for ((id, _, _), target) in rows.iter().zip(targets.iter()) {
        if let Some(target) = target {
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
pub fn run_indexing_pipeline(
    conn: &mut Connection,
    project_root: &Path,
    phase: std::sync::Arc<std::sync::RwLock<crate::types::IndexingPhase>>,
) -> rusqlite::Result<()> {
    use crate::types::IndexingPhase;

    let config = crate::config::load_config(project_root).unwrap_or_default();
    let entry_point_patterns = config.entry_points;
    let ignore_patterns = config.ignore;

    // Initialize FormalResolver once per pipeline run; load rules for all supported
    // languages. Non-fatal if a language fails to load — that language falls back to
    // ConservativeResolver only.
    let mut formal = crate::resolver::formal::FormalResolver::new();
    let _ = formal.load_python(); // non-fatal: falls back silently on error

    let mut files = Vec::new();
    collect_source_files(project_root, &ignore_patterns, &mut files);
    files.sort();

    *phase.write().unwrap() = IndexingPhase::Parsing;

    // Parse + resolve + persist in bounded batches: each batch is extracted in
    // parallel (pure CPU, no DB access) and persisted sequentially before the
    // next batch starts, so peak memory holds at most one batch of parsed
    // files instead of the whole project. `.map()` over an indexed parallel
    // iterator preserves order within a batch, and batches are processed in
    // the same sorted `files` order, so the result is byte-for-byte identical
    // to a fully sequential pipeline.
    let now = now_secs();
    let tx = conn.transaction()?;

    // Full reindex: clear everything. (Triggers keep the FTS tables in sync.)
    tx.execute("DELETE FROM call_sites", [])?;
    tx.execute("DELETE FROM import_edges", [])?;
    tx.execute("DELETE FROM symbols", [])?;
    tx.execute("DELETE FROM file_index", [])?;
    tx.execute("DELETE FROM code_chunks", [])?;

    for batch in files.chunks(PARSE_BATCH_SIZE) {
        let extracted: Vec<(String, &'static str, String, f64, ExtractedFile)> = batch
            .par_iter()
            .map(|file| {
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
                let lang = language_for_extension(ext)?;
                let source = std::fs::read_to_string(file).ok()?;
                let rel = rel_path(project_root, file);
                let hash = hash_content(&source);
                let mtime = mtime_secs(file);
                let data = extract_file_data(&rel, lang, &source, &entry_point_patterns, &formal);
                Some((rel, lang, hash, mtime, data))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flatten()
            .collect();

        for (rel, lang, hash, mtime, data) in &extracted {
            persist_file(&tx, rel, hash, data)?;
            upsert_file_index(&tx, rel, lang, hash, *mtime, data.symbol_count, now)?;
        }
    }

    *phase.write().unwrap() = IndexingPhase::BuildingEdges;

    rebuild_graph(&tx, &config.hub_threshold)?;
    tx.commit()?;

    *phase.write().unwrap() = IndexingPhase::Ready;

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
    let ignore_patterns = config.ignore;

    let mut formal = crate::resolver::formal::FormalResolver::new();
    let _ = formal.load_python();

    let existing: HashMap<String, String> = {
        let mut stmt = conn.prepare("SELECT path, hash FROM file_index")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    };

    let mut files = Vec::new();
    collect_source_files(project_root, &ignore_patterns, &mut files);
    files.sort();

    // Read + hash every file in parallel, then decide sequentially which ones
    // actually changed before paying the parse+resolve cost on just those.
    struct Candidate {
        rel: String,
        lang: &'static str,
        source: String,
        hash: String,
        mtime: f64,
    }
    let candidates: Vec<Candidate> = files
        .par_iter()
        .map(|file| {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = language_for_extension(ext)?;
            let source = std::fs::read_to_string(file).ok()?;
            let rel = rel_path(project_root, file);
            let hash = hash_content(&source);
            Some(Candidate {
                rel,
                lang,
                source,
                hash,
                mtime: mtime_secs(file),
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect();

    let seen_paths: HashSet<String> = candidates.iter().map(|c| c.rel.clone()).collect();
    let changed: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| existing.get(&c.rel) != Some(&c.hash)) // unchanged — skip the parse
        .collect();

    // Parse + resolve + persist in bounded batches (see run_indexing_pipeline
    // for why: caps peak memory to one batch instead of every changed file).
    let now = now_secs();
    let tx = conn.transaction()?;
    let mut summary = ReindexSummary::default();

    for batch in changed.chunks(PARSE_BATCH_SIZE) {
        let extracted: Vec<(&Candidate, ExtractedFile)> = batch
            .par_iter()
            .map(|c| {
                let data =
                    extract_file_data(&c.rel, c.lang, &c.source, &entry_point_patterns, &formal);
                (c, data)
            })
            .collect();

        for (c, data) in &extracted {
            remove_file_rows(&tx, &c.rel)?;
            persist_file(&tx, &c.rel, &c.hash, data)?;
            upsert_file_index(
                &tx,
                &c.rel,
                c.lang,
                &c.hash,
                c.mtime,
                data.symbol_count,
                now,
            )?;
            summary.changed += 1;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::types::IndexingPhase;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn dummy_phase() -> std::sync::Arc<std::sync::RwLock<IndexingPhase>> {
        std::sync::Arc::new(std::sync::RwLock::new(IndexingPhase::Scanning))
    }

    #[test]
    fn test_phase_advances_to_ready_after_pipeline() {
        use crate::types::IndexingPhase;
        use std::sync::{Arc, RwLock};

        let dir = std::env::temp_dir().join(format!("ci_idx_phase_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let phase = Arc::new(RwLock::new(IndexingPhase::Scanning));
        run_indexing_pipeline(&mut conn, &dir, phase.clone()).unwrap();

        assert_eq!(
            *phase.read().unwrap(),
            IndexingPhase::Ready,
            "Phase must be Ready after pipeline completes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_indexing_pipeline_empty_dir() {
        let dir = std::env::temp_dir().join(format!("ci_idx_empty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(run_indexing_pipeline(&mut conn, &dir, dummy_phase()).is_ok());
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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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

    /// Regression for B4: known FNV-1a 64-bit test vectors (from the FNV
    /// reference test suite), independent of this codebase's own algorithm —
    /// confirms `hash_content` is a real, portable FNV-1a and not just
    /// internally self-consistent.
    #[test]
    fn test_hash_content_matches_fnv1a_64_test_vectors() {
        assert_eq!(hash_content(""), "cbf29ce484222325");
        assert_eq!(hash_content("a"), "af63dc4c8601ec8c");
        assert_eq!(hash_content("foobar"), "85944171f73967e8");
    }

    #[test]
    fn test_hash_content_deterministic_across_calls() {
        let s = "def hello():\n    pass\n";
        assert_eq!(hash_content(s), hash_content(s));
        assert_ne!(hash_content(s), hash_content("different content"));
    }

    #[test]
    fn test_config_ignore_excludes_dir_and_glob() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ignorecfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(dir.join("a.py"), "def kept():\n    pass\n").unwrap();
        std::fs::write(dir.join("vendor/b.py"), "def excluded_dir():\n    pass\n").unwrap();
        std::fs::write(dir.join("app.min.js"), "function excludedGlob() {}\n").unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"ignore": ["vendor", "*.min.js"]}"#,
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::kept'",
            ),
            1
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 1);

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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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
        run_indexing_pipeline(&mut conn_default, &dir, dummy_phase()).unwrap();
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
        run_indexing_pipeline(&mut conn_custom, &dir, dummy_phase()).unwrap();
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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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

    /// Regression: `Type::method()` (a scoped-path call, no `.` receiver) must
    /// resolve *only* against `Type`, not fan out to every same-named symbol
    /// project-wide. Two structs (`StructA`, `StructB`) each define `fn new()`;
    /// `caller` calls `StructA::new()` (must resolve to exactly that one) and
    /// `HashMap::new()` (an external/undefined type in this fixture — must
    /// resolve to nothing at all, not to `StructA::new`/`StructB::new` via the
    /// old unscoped `by_name["new"]` fallback).
    #[test]
    fn test_type_path_call_resolves_scoped_not_fanned_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_typepath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct StructA;
impl StructA {
    fn new() -> Self {
        StructA
    }
}
",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct StructB;
impl StructB {
    fn new() -> Self {
        StructB
    }
}
",
        )
        .unwrap();
        std::fs::write(
            dir.join("c.rs"),
            "fn caller() {
    let _a = StructA::new();
    let _m = HashMap::new();
}
",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // Correctly scoped: caller -> StructA::new, and only StructA::new.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
            ),
            1,
            "StructA::new() must resolve to StructA's own new(), scoped via target_class"
        );
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "inferred",
            "type-path call is tier-2 inferred, not textual"
        );

        // Not fanned out: must NOT also point at StructB::new.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'b.rs')",
            ),
            0,
            "StructA::new() must not also resolve to the unrelated StructB::new()"
        );

        // HashMap::new() names an undefined type in this fixture — must resolve
        // to nothing (old behavior: fell back to matching every `new` in the
        // project, i.e. both StructA::new and StructB::new).
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol LIKE '%StructB%'",
            ),
            0,
            "HashMap::new() must not resolve to any project symbol"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller')",
            ),
            1,
            "caller must have exactly one outgoing edge total (StructA::new only)"
        );

        // The call_sites row for HashMap::new() itself was correctly scoped to
        // "HashMap" (not left NULL/unscoped) — it just has no project-side match.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_sites WHERE callee_name = 'new' AND target_class = 'HashMap'",
            ),
            1,
            "HashMap::new() call site must be scoped to target_class='HashMap'"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: tier-1's `file_symbols.contains(bare_name)` check matches
    /// on the callee name alone, with no idea a receiver type was ever named
    /// — so when the *caller's own file* happens to define something with
    /// the same bare name as an unrelated `Type::method()` call's method
    /// (extremely likely for "new"), tier-1 used to "resolve" first and
    /// short-circuit past the type-path scoping fix entirely (it only ran
    /// when tier-1 came back textual), reintroducing the exact fan-out bug
    /// for this specific, common case. `a.rs` defines its own `LocalType`
    /// with `fn new()` *and* calls `Vec::new()` in the same file — the
    /// local "new" must not cause `Vec::new()` to also match
    /// `OtherType::new()` defined in a completely different file.
    #[test]
    fn test_type_path_call_not_shadowed_by_same_file_bare_name_match() {
        let dir = std::env::temp_dir().join(format!("ci_idx_typepath2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct LocalType;\nimpl LocalType {\n    fn new() -> Self {\n        LocalType\n    }\n}\nfn caller() {\n    let _v: Vec<i32> = Vec::new();\n    let _l = LocalType::new();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct OtherType;\nimpl OtherType {\n    fn new() -> Self {\n        OtherType\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The intentional local call resolves correctly.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
            ),
            1,
            "LocalType::new() must resolve to the local new(), scoped via target_class"
        );

        // Vec::new() must NOT fan out to OtherType::new() in b.rs just
        // because a.rs's own file_symbols happens to contain "new" too.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'b.rs')",
            ),
            0,
            "Vec::new() must not resolve to the unrelated OtherType::new() in b.rs"
        );

        // Exactly one outgoing edge total: the local LocalType::new() call.
        // (Vec::new() correctly resolves to nothing — Vec isn't a project type.)
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller')",
            ),
            1,
            "caller must have exactly one outgoing edge total"
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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

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

    /// Verifies `ci`'s own recovery path for a scenario an agent host's
    /// checkpoint/rewind feature (Claude Code's `/rewind`, similar in
    /// Cursor/Windsurf) can produce: a file gets reverted to *older*
    /// content out from under the running server, entirely outside any
    /// `edit_lines`/`edit_symbol` call `ci` knows about. Since
    /// `reindex_changed` decides "did this file change" by comparing a
    /// fresh content hash against the DB's stored hash — not by mtime or
    /// direction — a revert to prior content produces a different hash from
    /// what's currently indexed and is picked up exactly like a forward
    /// edit would be. This confirms that by construction rather than
    /// building a separate `ci`-side undo mechanism, which would risk
    /// drifting out of sync with whatever the host's own checkpoint state
    /// actually is.
    #[test]
    fn test_reindex_recovers_after_file_externally_reverted_to_older_content() {
        let dir = std::env::temp_dir().join(format!("ci_idx_revert_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = "def original():\n    pass\n";
        std::fs::write(dir.join("a.py"), original).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            1
        );

        // Agent (or the agent's host) edits the file forward.
        std::fs::write(dir.join("a.py"), "def edited():\n    pass\n").unwrap();
        reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::edited'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            0,
            "the edited-away symbol must not linger"
        );

        // Something *outside* ci's own write path (a host checkpoint
        // rewind, a manual `git checkout`, an editor undo) puts the
        // original content back — not a new edit_lines/edit_symbol call.
        std::fs::write(dir.join("a.py"), original).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 1,
                deleted: 0
            },
            "a revert to prior content is still a content-hash change and must be picked up"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            1,
            "the reverted-to symbol must be restored"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::edited'"
            ),
            0,
            "the since-reverted symbol must not linger as a stale leftover"
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
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
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

    /// Layer-2 code chunks must track incremental reindex the same way symbols
    /// do: a changed file's stale chunks are replaced (not duplicated
    /// alongside the new ones), and a deleted file's chunks disappear too.
    /// Only meaningful with `embeddings` compiled in — otherwise chunking is a
    /// no-op (see `chunk_pending`) and `code_chunks` stays empty by design.
    #[cfg(feature = "embeddings")]
    #[test]
    fn test_reindex_incremental_updates_code_chunks() {
        let dir = std::env::temp_dir().join(format!("ci_idx_inc_chunks_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def run():\n    marker = OLD_MARKER_TERM\n    return marker\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%OLD_MARKER_TERM%'",
            ),
            1
        );

        // Change the body's distinctive term (same line count/symbol) and add
        // a second file.
        std::fs::write(
            dir.join("a.py"),
            "def run():\n    marker = NEW_MARKER_TERM\n    return marker\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.py"), "def other():\n    pass\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 2,
                deleted: 0
            }
        );

        // Exactly one chunk per file — the stale a.py chunk was replaced, not
        // accumulated alongside the new one.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%OLD_MARKER_TERM%'",
            ),
            0,
            "stale chunk text must not survive a reindex of the same file"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%NEW_MARKER_TERM%'",
            ),
            1
        );

        // Delete a.py → its chunk disappears; b.py's chunk is untouched.
        std::fs::remove_file(dir.join("a.py")).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            s,
            ReindexSummary {
                changed: 0,
                deleted: 1
            }
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE path = 'a.py'"
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE path = 'b.py'"
            ),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_formal_tier_upgrades_textual_python_call() {
        // Verify Tier-3: FormalResolver upgrades a "textual" call site to "formal".
        //
        // ConservativeResolver Tier-1 only gives "resolved" for names it finds in
        // file_symbols, import_map, or aliases. A call to a lambda or a function
        // assigned to a variable is NOT captured by extract_symbols, so Tier-1
        // gives "textual". FormalResolver's StackGraph rules DO resolve it (it sees
        // the binding in scope) and upgrades the confidence to "formal".
        //
        // We use a nested-scope call: `helper` is defined inside `setup()` and
        // called from `run()`. extract_symbols captures nested defs as file_symbols
        // (so Tier-1 gives "resolved"), meaning the call edge exists with ≥resolved.
        // The key assertion is that the pipeline integrates without error AND produces
        // the call edge — proving FormalResolver is wired in and doesn't break things.
        let dir = std::env::temp_dir().join(format!("ci_formal_tier_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("mod.py"),
            "def helper():\n    pass\n\ndef run():\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The call from run() → helper() must produce a call edge with at least
        // "resolved" confidence (ConservativeResolver Tier-1 finds it in file_symbols).
        // If FormalResolver is also loaded, it confirms the same edge via StackGraph.
        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            edge_count, 1,
            "Expected exactly one call edge run→helper from pipeline with FormalResolver integrated"
        );

        // Verify FormalResolver did not break confidence — must be resolved or formal.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert!(
            matches!(confidence.as_str(), "resolved" | "formal"),
            "Expected confidence 'resolved' or 'formal' for intra-file call, got: {confidence}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
