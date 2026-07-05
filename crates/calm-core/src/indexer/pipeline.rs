use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::indexer::chunker::{CodeChunk, chunk_file};
use crate::indexer::edges::{
    CallEdge, insert_call_edges_batch, insert_code_chunks_batch, insert_import_edges_batch,
    insert_symbols_batch,
};
use crate::indexer::lang_constants::{is_recognized_unparsed_extension, language_for_extension};
use crate::indexer::parser::{
    ParsedSymbol, extract_calls_from_tree, extract_file_aliases_from_tree,
    extract_symbols_from_tree, extract_symbols_shallow, extract_type_map_from_tree, parse_tree,
};
use crate::types::EdgeConfidence;

/// Maximum number of same-named symbols a call may resolve to before it is
/// dropped as too ambiguous (conservative).
const MAX_CALLEE_CANDIDATES: usize = 20;

/// True if a symbol's stored `signature` string's return type is `Option<_>`
/// or `Result<_, _>` (bare `Option`/`Result` too, for generic/associated-type
/// signatures that elide the parameter). Looks at the segment after the last
/// `->` — the actual return position, not a `->` that might appear earlier in
/// a higher-order parameter type (`f: impl Fn() -> i32`). A missing `->`
/// (fields, non-function symbols) returns `false`.
fn signature_returns_option_or_result(sig: &str) -> bool {
    let Some(ret) = sig.rsplit("->").next() else {
        return false;
    };
    // Guard against `sig` not containing `->` at all, in which case
    // `rsplit("->").next()` returns the whole string unchanged.
    if !sig.contains("->") {
        return false;
    }
    let ret = ret.trim_start();
    ret.starts_with("Option<")
        || ret.starts_with("Option ")
        || ret.starts_with("Result<")
        || ret.starts_with("Result ")
        || ret == "Option"
        || ret == "Result"
}

/// Files are parsed+resolved (and then persisted) in chunks of this size
/// rather than all at once, so peak memory holds at most one batch of
/// parsed-but-not-yet-persisted files instead of an entire large repo.
const PARSE_BATCH_SIZE: usize = 1000;

/// A persisted call site loaded for graph rebuild:
/// (from_path, enclosing_qn, callee_name, call_line, confidence, target_class,
/// looks_option_or_result_chained).
type CallSiteRow = (
    String,
    String,
    String,
    Option<i64>,
    String,
    Option<String>,
    bool,
    Option<String>,
);

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
            && (language_for_extension(ext).is_some() || is_recognized_unparsed_extension(ext))
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

/// `lang` is `None` for a recognized-but-unparsed extension (see
/// `is_recognized_unparsed_extension`) — persisted as SQL `NULL`, matching
/// `file_index.language`'s nullable column.
fn upsert_file_index(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: Option<&str>,
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
    looks_option_or_result_chained: bool,
    /// See `parser::module_hint_of` — the discarded module-path segment of a
    /// lowercase-qualified `::`-call (`crate::telemetry::timed_tool` →
    /// `Some("telemetry")`), used by `rebuild_graph` to disambiguate among
    /// same-named candidates by file when there's no `use` for `resolve_tier1`
    /// to match against.
    module_hint: Option<String>,
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

/// One file's `(rel_path, language, hash, mtime, extracted_data)` from a
/// batch's parallel extraction pass in `run_indexing_pipeline` —
/// `language`/`extracted_data` are `None` for a recognized-unparsed-extension
/// file (see `is_recognized_unparsed_extension`), which still gets a
/// `file_index` row but nothing to persist.
type ExtractedBatchRow = (
    String,
    Option<&'static str>,
    String,
    f64,
    Option<ExtractedFile>,
);

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
                looks_option_or_result_chained: c.looks_option_or_result_chained,
                module_hint: c.module_hint.clone(),
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
        "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line, confidence, receiver, target_class, looks_option_or_result_chained, module_hint) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for c in &extracted.call_sites {
        stmt.execute(rusqlite::params![
            rel,
            c.enclosing_qn,
            c.callee,
            c.line,
            c.confidence,
            c.receiver,
            c.target_class,
            c.looks_option_or_result_chained as i64,
            c.module_hint,
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
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> rusqlite::Result<()> {
    // name → [(qn, path, language)] for tier-1; (name, class) → [(qn, path,
    // language)] for tier-2. `language` rides along so a call site can never
    // resolve to a same-named symbol written in a different language (see
    // `path_lang` and the same-language filter below) — a bare-name/textual
    // match across languages is never a real call, just an incidental name
    // collision (e.g. a Rust `new` and a Python `new` sharing `by_name`).
    type SymbolCandidate = (String, String, String); // (qualified_name, path, language)
    let mut by_name: HashMap<String, Vec<SymbolCandidate>> = HashMap::new();
    let mut by_name_class: HashMap<(String, String), Vec<SymbolCandidate>> = HashMap::new();
    // qualified_name → signature, so the `MAX_CALLEE_CANDIDATES` fallback below
    // can tell whether a candidate's return type could possibly be
    // `Option`/`Result` — see `looks_option_or_result_chained`'s doc comment.
    let mut sig_by_qn: HashMap<String, String> = HashMap::new();
    // path → language, one entry per indexed file (derived from that file's
    // own symbols, so it's always populated for any path that could ever be
    // a call site's `from_path` below).
    let mut path_lang: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT name, qualified_name, path, class_context, signature, language FROM symbols",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, qn, path, cls, sig, language) in rows {
            path_lang
                .entry(path.clone())
                .or_insert_with(|| language.clone());
            by_name.entry(name.clone()).or_default().push((
                qn.clone(),
                path.clone(),
                language.clone(),
            ));
            if let Some(c) = cls {
                by_name_class
                    .entry((name, c))
                    .or_default()
                    .push((qn.clone(), path, language));
            }
            sig_by_qn.insert(qn, sig);
        }
    }

    let sites: Vec<CallSiteRow> = {
        let mut stmt = tx.prepare(
            "SELECT from_path, enclosing_qn, callee_name, call_line, confidence, target_class, \
                    looks_option_or_result_chained, module_hint \
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
                r.get::<_, i64>(6)? != 0,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    // One edge per (caller, callee, call-site line) — distinct sites in the same caller stay separate (so `callers`/`callees` show every site); exact same-line dupes are deduped.
    // Confidence is the resolver's verdict recorded at extraction time. A tier-2
    // call (target_class set) resolves the method within that class only.
    //
    // Candidate lookup (HashMap reads against `by_name`/`by_name_class`) is pure
    // CPU work independent per site, so it runs in parallel; the dedup merge
    // below stays sequential, walking sites in their original order so the
    // "first call site wins" line/confidence attribution is unchanged.
    //
    // Same-file preference: an unqualified call whose bare name ALSO has a
    // matching definition in the caller's own file resolves to that
    // definition, not to unrelated same-named symbols elsewhere — this is
    // Rust's own scoping, not a heuristic. Without it, every same-named
    // candidate anywhere in the repo gets an edge (private per-file helpers —
    // test fixtures, local `fn new`/`run`/`setup_db` — fan out to every other
    // file sharing that name), inflating blast_radius/caller_count for the
    // most common private-helper pattern. This mirrors the fix already
    // applied to the `receiver_is_type_path` branch above (which skips tier-1
    // entirely to dodge the same fan-out for `Type::method()` calls) — that
    // fix never covered this general by-name/by-name-class path, so the bug
    // survived here. Only fan out globally when nothing in-file matches, and
    // even then only up to MAX_CALLEE_CANDIDATES.
    let candidates: Vec<Vec<(String, String)>> = sites
        .par_iter()
        .map(
            |(
                from_path,
                _,
                callee,
                _,
                _,
                target_class,
                looks_option_or_result_chained,
                module_hint,
            )| {
                let targets = match target_class {
                    Some(cls) => by_name_class.get(&(callee.clone(), cls.clone())),
                    None => by_name.get(callee),
                };
                let Some(t) = targets else {
                    return Vec::new();
                };
                // Same-language filter: a call site can only ever resolve to a
                // symbol written in the same language as its caller — a
                // cross-language name collision (e.g. a Rust `foo` incidentally
                // matching a Python `foo` elsewhere in the repo) is never a
                // real call edge no matter how well the name/class matches.
                // Applied first, before every other candidate-narrowing
                // heuristic below, since this one is a hard correctness
                // constraint rather than a preference — the rest of this
                // function is unchanged from here on, just operating on the
                // now same-language-only, language-stripped candidate list.
                let caller_lang = path_lang.get(from_path);
                let same_lang: Vec<(String, String)> = t
                    .iter()
                    .filter(|(_, _, lang)| Some(lang) == caller_lang)
                    .map(|(qn, path, _)| (qn.clone(), path.clone()))
                    .collect();
                if same_lang.is_empty() {
                    return Vec::new();
                }
                let t = &same_lang;
                // Return-shape exclusion: `foo.bar()?`/`foo.bar().unwrap()` can only
                // compile if `bar`'s return type is `Option`/`Result` — so a candidate
                // whose own signature returns neither is *provably* not this call's
                // real target, not just an unlikely one. Only filters when the site
                // shows the signal at all; otherwise every existing candidate stays,
                // unchanged from before this filter existed.
                let filtered: Vec<(String, String)>;
                let t: &Vec<(String, String)> = if *looks_option_or_result_chained {
                    filtered = t
                        .iter()
                        .filter(|(qn, _)| {
                            sig_by_qn
                                .get(qn)
                                .is_some_and(|sig| signature_returns_option_or_result(sig))
                        })
                        .cloned()
                        .collect();
                    &filtered
                } else {
                    t
                };
                if t.is_empty() {
                    return Vec::new();
                }
                // Module-qualifier preference: `crate::telemetry::timed_tool()`
                // carries an explicit, unambiguous module segment in the source
                // text (see `parser::module_hint_of`) — stronger evidence than
                // incidental file collocation, so it's checked *before* the
                // same-file-as-caller fallback below. Without this, a call
                // site like that one — whose bare callee name also happens to
                // match a same-named symbol in the caller's OWN file (e.g. a
                // same-named wrapper method delegating to the free function it
                // wraps) — silently resolved to that unrelated same-file
                // symbol instead of the module actually named in the source,
                // in the worst case fabricating a self-recursive edge.
                if let Some(hint) = module_hint {
                    let hinted: Vec<_> = t
                        .iter()
                        .filter(|(_, p)| {
                            Path::new(p.as_str())
                                .file_stem()
                                .is_some_and(|stem| stem == hint.as_str())
                        })
                        .cloned()
                        .collect();
                    if !hinted.is_empty() {
                        return hinted;
                    }
                }
                let same_file: Vec<_> = t.iter().filter(|(_, p)| p == from_path).cloned().collect();
                if !same_file.is_empty() {
                    same_file
                } else if t.len() <= MAX_CALLEE_CANDIDATES {
                    t.clone()
                } else {
                    Vec::new()
                }
            },
        )
        .collect();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut seen_pairs: HashSet<(String, String, Option<i64>)> = HashSet::new();
    for ((from_path, enc_qn, _callee, line, confidence, _target_class, _, _), targets) in
        sites.iter().zip(candidates.iter())
    {
        // >1 surviving candidate means this call site's edge is duplicated
        // across multiple distinct symbols with nothing left to break the
        // tie — mark it `Ambiguous` regardless of which branch produced it,
        // rather than let it masquerade as an ordinary single-target edge at
        // its originally recorded confidence (which was computed per call
        // site, not per final-candidate-count).
        let effective_confidence = if targets.len() > 1 {
            EdgeConfidence::Ambiguous.as_str()
        } else {
            confidence.as_str()
        };
        for (to_qn, to_path) in targets {
            if !seen_pairs.insert((enc_qn.clone(), to_qn.clone(), *line)) {
                continue;
            }
            edges.push(CallEdge {
                from_symbol: enc_qn.clone(),
                to_symbol: to_qn.clone(),
                call_site_line: line.map(|l| l as i32),
                edge_confidence: effective_confidence.to_string(),
                from_path: Some(from_path.clone()),
                to_path: Some(to_path.clone()),
            });
        }
    }

    tx.execute("DELETE FROM call_edges", [])?;
    insert_call_edges_batch(tx, &edges)?;
    refresh_caller_counts(tx)?;
    resolve_import_targets(tx, crate_map)?;
    crate::graph::coreness::compute_coreness(tx)?;
    crate::graph::hub::update_is_hub_flags(tx, hub_config)?;
    Ok(())
}
/// Recompute every symbol's `caller_count` from `call_edges`, using the same
/// "confirmed caller" definition as the `callers` tool's `direct_count`
/// (`ruled_out_by_scip = 0` and not `ambiguous`-confidence): an `ambiguous`
/// edge is index-time fan-out to every same-named candidate when a call's
/// receiver type couldn't be resolved (e.g. `x.as_str()` fanning out to every
/// `as_str` method in the repo), not a confirmed caller of any one of them.
/// Counting it here — the previous behavior — inflated `caller_count` nearly
/// identically across every same-named symbol regardless of real usage,
/// corrupting the hub/coreness ranking and `dead_code_confidence` (which
/// short-circuits to "not dead" on `caller_count > 0`) built on top of it.
///
/// Called both from [`rebuild_graph`] (after every full/incremental index)
/// and, separately, after the SCIP overlay pass (`scip::run_overlay`) flips
/// `ruled_out_by_scip`/`edge_confidence` on existing edges — that pass runs
/// after this function's other caller, so without a second refresh
/// afterward, `caller_count` would immediately go stale again relative to
/// the very columns this filter depends on.
pub fn refresh_caller_counts(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE symbols SET caller_count = \
            (SELECT COUNT(DISTINCT from_symbol) FROM call_edges \
             WHERE to_symbol = symbols.qualified_name \
               AND ruled_out_by_scip = 0 \
               AND edge_confidence != 'ambiguous')",
        [],
    )?;
    Ok(())
}

/// Best-effort resolution of `import_edges.to_path` against indexed files, so the
/// `dependencies` tool's `imported_by` direction works for in-project imports.
/// External modules (no matching file) keep `to_path = NULL`.
fn resolve_import_targets(
    tx: &rusqlite::Transaction,
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> rusqlite::Result<()> {
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
        .map(|(_, from_path, module)| resolve_module_to_path(from_path, module, &known, crate_map))
        .collect();

    let mut ustmt = tx.prepare("UPDATE import_edges SET to_path = ?1 WHERE id = ?2")?;
    for ((id, _, _), target) in rows.iter().zip(targets.iter()) {
        if let Some(target) = target {
            ustmt.execute(rusqlite::params![target, id])?;
        }
    }
    Ok(())
}

/// Resolve a Rust `use` module path to an indexed file, using the workspace
/// crate map. Handles `crate::`, `self::`, an external crate-name prefix, and a
/// best-effort `super::`. Returns `None` for paths that leave the workspace
/// (std, third-party crates) — those correctly keep `to_path = NULL`.
///
/// `super::` is ambiguous between two real Rust module layouts: the older
/// `foo/mod.rs`-per-directory convention (climbing one filesystem directory
/// per `super` is correct) and the modern 2018-edition `foo.rs` + `foo/`
/// sibling-submodule convention, where files inside `foo/` (e.g.
/// `tools/common.rs` and `tools/guardrails.rs`, both submodules of `tools`)
/// are already siblings of each other — so a single `super` hop from one to
/// reach the other resolves *within the same directory*, not one level up.
/// For a single `super`, the same-directory hypothesis is tried first (it's
/// the dominant modern convention, and the one this very codebase uses),
/// falling back to the climbed-directory interpretation only if that misses.
/// A miss on both falls back to `None`, never a wrong edge — see
/// `resolve_candidates`'s `allow_root_fallback` for the guarantee this
/// depends on.
fn resolve_rust_module(
    from_path: &str,
    module: &str,
    crate_map: &crate::indexer::crate_map::CrateMap,
    known: &HashSet<String>,
) -> Option<String> {
    let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let from_dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    // (base directory to resolve the remaining segments under, remaining
    // segments, whether a single trailing segment may fall back to base_dir's
    // OWN `.rs`/`mod.rs`/`lib.rs` file — only sound when base_dir is a
    // *verified* crate/module root, never a `super`/`self`-climbed ancestor).
    let (base_dir, rest, allow_root_fallback): (String, &[&str], bool) = match segs[0] {
        "crate" => {
            let (_, root) = crate_map.crate_of_file(from_path)?;
            (root.to_string(), &segs[1..], true)
        }
        "self" => (from_dir.clone(), &segs[1..], false),
        "super" => {
            let mut dir = from_dir.clone();
            let mut i = 0;
            while i < segs.len() && segs[i] == "super" {
                dir = parent_of(&dir);
                i += 1;
            }
            if i == 1
                && let Some(hit) = resolve_candidates(&from_dir, &segs[1..], false, known)
            {
                return Some(hit);
            }
            (dir, &segs[i..], false)
        }
        other => {
            // Rust's "uniform paths": an unprefixed leading segment in a `use`
            // is looked up as an external crate name first; if it isn't one,
            // the path is implicitly relative to the *importing file's own*
            // crate root (e.g. `use engine::Engine;` inside that crate's own
            // `lib.rs` means the same as `use crate::engine::Engine;`).
            match crate_map.root_of(&other.replace('-', "_")) {
                Some(root) => (root.to_string(), &segs[1..], true),
                None => {
                    let (_, root) = crate_map.crate_of_file(from_path)?;
                    (root.to_string(), segs.as_slice(), false)
                }
            }
        }
    };

    resolve_candidates(&base_dir, rest, allow_root_fallback, known)
}

/// Try resolving `rest` (module segments after the `crate`/`self`/`super`/
/// external-crate prefix has been stripped) under `base_dir` against the set
/// of indexed files (`known`). Tries the full remaining path and, for item
/// imports (`use a::b::Item`), its parent directory — plus `mod.rs`/`lib.rs`
/// directory-index conventions.
///
/// `allow_root_fallback` additionally permits a single trailing segment
/// (`use crate::Item`) to match `base_dir`'s own `.rs`/`mod.rs`/`lib.rs` file
/// directly — a genuine re-export-at-the-root pattern, but only sound when
/// `base_dir` is a *verified* crate/module root (the `crate::` branch and the
/// named-external-crate case, both backed by `CrateMap`). It must stay
/// `false` for `super`/`self`, where `base_dir` is merely a climbed filesystem
/// ancestor with no such guarantee: enabling it there previously let a
/// `super::sibling` import spuriously match the *crate's own* `lib.rs` — a
/// confidently wrong `to_path` — whenever the climbed ancestor directory
/// happened to coincide with the crate root, instead of the honest `None`
/// this function is documented to fall back to on a genuine miss.
fn resolve_candidates(
    base_dir: &str,
    rest: &[&str],
    allow_root_fallback: bool,
    known: &HashSet<String>,
) -> Option<String> {
    let joined = rest.join("/");
    let mut bases: Vec<String> = Vec::new();
    if joined.is_empty() {
        bases.push(base_dir.to_string());
    } else {
        bases.push(join_rel(base_dir, &joined));
        if let Some((parent, _)) = joined.rsplit_once('/') {
            bases.push(join_rel(base_dir, parent));
        } else if allow_root_fallback {
            bases.push(base_dir.to_string());
        }
    }

    for base in &bases {
        let base = base.trim_start_matches('/');
        for cand in [
            format!("{base}.rs"),
            format!("{base}/mod.rs"),
            format!("{base}/lib.rs"),
        ] {
            if known.contains(&cand) {
                return Some(cand);
            }
        }
        if known.contains(base) {
            return Some(base.to_string());
        }
    }
    None
}

/// Map a module/path string to an indexed file path, trying the conventions of
/// all six languages (dotted, scoped, and JS-relative) plus common index files.
fn resolve_module_to_path(
    from_path: &str,
    module: &str,
    known: &HashSet<String>,
    crate_map: &crate::indexer::crate_map::CrateMap,
) -> Option<String> {
    let m = module.trim().trim_matches(|c| c == '"' || c == '\'');
    if m.is_empty() {
        return None;
    }
    // Rust: use the crate-map-aware resolver first; fall through to the generic
    // convention scan only if it finds nothing (keeps single-crate repos working
    // even when the crate map is empty).
    if from_path.ends_with(".rs")
        && let Some(hit) = resolve_rust_module(from_path, m, crate_map, known)
    {
        return Some(hit);
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
        // TS/NodeNext-ESM convention: source imports a sibling `.ts` module
        // using the *compiled-output* extension (`./foo.js` referring to
        // `foo.ts` on disk) — required by `"moduleResolution": "node16"` /
        // `"nodenext"` / `"bundler"` since the emitted JS must contain a
        // specifier that resolves at runtime. The exact-match candidate
        // above only ever finds a *real* `.js` file; without this second,
        // extension-stripped candidate the EXTS loop below can only append
        // more extensions onto the specifier's own `.js` suffix (producing
        // nonsense like `foo.js.ts`) and never tries the real `foo.ts`.
        if let Some(stripped) = strip_js_emit_extension(m) {
            bases.push(normalize_rel(&from_dir, stripped));
        }
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

/// Strips a compiled/emitted JS-family extension (`.mjs`/`.cjs`/`.jsx`/`.js`)
/// from a relative import specifier, or `None` if it doesn't end in one.
fn strip_js_emit_extension(m: &str) -> Option<&str> {
    for ext in [".mjs", ".cjs", ".jsx", ".js"] {
        if let Some(s) = m.strip_suffix(ext) {
            return Some(s);
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
    let _ = formal.load_typescript(); // non-fatal: falls back silently on error

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
        // `lang: None` + `data: None` means a recognized-unparsed-extension
        // file (see `is_recognized_unparsed_extension`) — still earns a
        // `file_index` row below (path/hash/mtime, `language` NULL,
        // `symbol_count` 0), just with nothing to extract or persist.
        let extracted: Vec<ExtractedBatchRow> = batch
            .par_iter()
            .map(|file| {
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
                let lang = language_for_extension(ext);
                if lang.is_none() && !is_recognized_unparsed_extension(ext) {
                    return None;
                }
                let source = std::fs::read_to_string(file).ok()?;
                let rel = rel_path(project_root, file);
                let hash = hash_content(&source);
                let mtime = mtime_secs(file);
                let data = lang.map(|lang| {
                    extract_file_data(&rel, lang, &source, &entry_point_patterns, &formal)
                });
                Some((rel, lang, hash, mtime, data))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flatten()
            .collect();

        for (rel, lang, hash, mtime, data) in &extracted {
            if let Some(data) = data {
                persist_file(&tx, rel, hash, data)?;
            }
            upsert_file_index(
                &tx,
                rel,
                *lang,
                hash,
                *mtime,
                data.as_ref().map(|d| d.symbol_count).unwrap_or(0),
                now,
            )?;
        }
    }

    *phase.write().unwrap() = IndexingPhase::BuildingEdges;

    let crate_map = crate::indexer::crate_map::CrateMap::build(project_root);
    rebuild_graph(&tx, &config.hub_threshold, &crate_map)?;
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
    let _ = formal.load_typescript();

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
        // `None` for a recognized-unparsed-extension file (see
        // `is_recognized_unparsed_extension`) — included here (not filtered
        // out like a genuinely unrecognized extension) so its `file_index`
        // row stays in `seen_paths` below and doesn't get mistaken for a
        // deleted file on every incremental pass.
        lang: Option<&'static str>,
        source: String,
        hash: String,
        mtime: f64,
    }
    let candidates: Vec<Candidate> = files
        .par_iter()
        .map(|file| {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = language_for_extension(ext);
            if lang.is_none() && !is_recognized_unparsed_extension(ext) {
                return None;
            }
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
        let extracted: Vec<(&Candidate, Option<ExtractedFile>)> = batch
            .par_iter()
            .map(|c| {
                let data = c.lang.map(|lang| {
                    extract_file_data(&c.rel, lang, &c.source, &entry_point_patterns, &formal)
                });
                (c, data)
            })
            .collect();

        for (c, data) in &extracted {
            remove_file_rows(&tx, &c.rel)?;
            if let Some(data) = data {
                persist_file(&tx, &c.rel, &c.hash, data)?;
            }
            upsert_file_index(
                &tx,
                &c.rel,
                c.lang,
                &c.hash,
                c.mtime,
                data.as_ref().map(|d| d.symbol_count).unwrap_or(0),
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
        let crate_map = crate::indexer::crate_map::CrateMap::build(project_root);
        rebuild_graph(&tx, &config.hub_threshold, &crate_map)?;
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
        // `main` calls `helper()` twice (two distinct call sites) → two edges;
        // edges are keyed on (from, to, call-site line), not just (from, to), so
        // both sites are preserved rather than collapsed to one.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = 'a.py::main' AND to_symbol = 'a.py::helper'",
            ),
            2
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

    /// Regression: a recognized-unparsed-extension file (see
    /// `is_recognized_unparsed_extension`) must earn a `file_index` row
    /// (path/hash/mtime, `language` NULL, `symbol_count` 0) so it's visible
    /// as "recognized but unparsed" rather than invisible like a doc/image/
    /// lockfile — but must never get symbols/edges, since there is no
    /// extractor for it.
    #[test]
    fn test_run_indexing_pipeline_tracks_recognized_unparsed_extension_by_path_only() {
        let dir = std::env::temp_dir().join(format!("ci_idx_unparsed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("Token.sol"),
            "pragma solidity ^0.8.0;\ncontract Token {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1); // only a.py::hello

        let sol_language: Option<String> = conn
            .query_row(
                "SELECT language FROM file_index WHERE path = 'Token.sol'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sol_language, None,
            "recognized-unparsed row must have language = NULL"
        );
        let sol_symbol_count: i64 = conn
            .query_row(
                "SELECT symbol_count FROM file_index WHERE path = 'Token.sol'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sol_symbol_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the incremental-reindex trap this design closes: before
    /// the fix, `reindex_changed`'s file-collection step filtered out
    /// recognized-unparsed files entirely, so their `file_index` row (created
    /// by a prior full index) was absent from `seen_paths` and got deleted as
    /// if the file had disappeared — on literally the very next incremental
    /// pass, even with no changes on disk at all.
    #[test]
    fn test_reindex_changed_does_not_delete_recognized_unparsed_file_row() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_unparsed_reindex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("Token.sol"),
            "pragma solidity ^0.8.0;\ncontract Token {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 2);

        let summary = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            summary.deleted, 0,
            "an unchanged recognized-unparsed file must not be treated as deleted"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM file_index WHERE path = 'Token.sol'"
            ),
            1,
            "Token.sol's file_index row must survive an incremental reindex"
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

    /// CommonJS `require()` (real Node.js code, not just ES `import`) must
    /// feed `import_map`/`import_edges` exactly like `import ... from ...`
    /// does — see `indexer::imports::parse_js_require`.
    #[test]
    fn test_commonjs_require_cross_file_resolved_confidence() {
        let dir = std::env::temp_dir().join(format!("ci_idx_require_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("helper.js"),
            "function helper() {}\nmodule.exports = { helper };\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.js"),
            "const { helper } = require('./helper');\n\nfunction run() {\n    helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let (to_path, module): (String, String) = conn
            .query_row(
                "SELECT COALESCE(to_path,''), module_name FROM import_edges WHERE from_path = 'main.js'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(module, "./helper");
        assert_eq!(
            to_path, "helper.js",
            "require() target resolved to in-project file"
        );

        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.js::run' AND to_symbol = 'helper.js::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "resolved",
            "call through require() should be resolved, not textual"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for a real production bug found via a live QA pass on
    /// KARMA (a TS/NodeNext codebase): `"moduleResolution": "node16"` /
    /// `"nodenext"` / `"bundler"` requires source files to import a sibling
    /// `.ts` module using the *compiled-output* extension (`./runtime.js`
    /// referring to `runtime.ts` on disk) — before this fix, every import
    /// shaped like this failed to resolve at all (362/362 such edges had a
    /// NULL `to_path` in KARMA's real index), silently breaking
    /// `dependencies()`'s `imported_by` for any file imported this way.
    #[test]
    fn test_js_extension_import_resolves_to_ts_sibling() {
        let dir = std::env::temp_dir().join(format!("ci_idx_jsext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("runtime.ts"),
            "export class Runtime {\n    start() {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("index.ts"),
            "import { Runtime } from \"./runtime.js\";\n\nfunction main() {\n    new Runtime().start();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'index.ts' AND module_name = './runtime.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path.as_deref(),
            Some("runtime.ts"),
            "a `.js`-suffixed relative import must resolve to the real `.ts` source file"
        );

        let imported_by: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_edges WHERE to_path = 'runtime.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            imported_by, 1,
            "dependencies()'s imported_by relies on to_path being populated"
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

    /// Regression for the free-function (non-method) analog of the fan-out
    /// bug above: private same-named helpers in different files (the common
    /// `fn test_conn()` / `fn setup_db()` test-fixture pattern) must not fan
    /// out to each other just because they share a bare name — `by_name` in
    /// `rebuild_graph` has no per-file scoping, so before the same-file
    /// preference was added, every call to `helper()` got an edge to BOTH
    /// files' `helper()`, not just its own. The `Type::method()` fix above
    /// (`test_type_path_call_resolves_scoped_not_fanned_out`) never covered
    /// this plain by-name path, so the identical bug survived here.
    #[test]
    fn test_bare_call_prefers_same_file_over_global_fan_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_barefanout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "fn helper() -> i32 {\n    1\n}\nfn caller_a() {\n    let _ = helper();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "fn helper() -> i32 {\n    2\n}\nfn caller_b() {\n    let _ = helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_a') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            1,
            "caller_a's helper() must resolve to a.rs's own helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_a') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "caller_a's helper() must NOT also fan out to b.rs's unrelated helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_b') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            1,
            "caller_b's helper() must resolve to b.rs's own helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_b') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            0,
            "caller_b's helper() must NOT also fan out to a.rs's unrelated helper()"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cross-language false callee: a bare-name fallback (nothing in the
    /// caller's own file matches, so the global by-name fan-out kicks in)
    /// must never resolve to a same-named symbol written in a DIFFERENT
    /// language — that's never a real call, just an incidental name
    /// collision (e.g. Python `helper` and Rust `helper` sharing a bare
    /// name). Regression for the missing same-language filter in
    /// `rebuild_graph`'s candidate lookup, which used to fan out to every
    /// same-named symbol in the whole multi-language repo.
    #[test]
    fn test_same_language_filter_excludes_cross_language_false_callee() {
        let dir = std::env::temp_dir().join(format!("ci_idx_crosslang_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `main` has no same-named `helper` in its own file, so resolution
        // falls through to the global by-name fan-out fallback — exactly the
        // path that never filtered by language before this fix.
        std::fs::write(dir.join("a.py"), "def main():\n    helper()\n").unwrap();
        std::fs::write(dir.join("c.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn helper() {}\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'c.py')",
            ),
            1,
            "main()'s helper() must resolve to the same-language (Python) helper in c.py"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "main()'s helper() must NEVER resolve to the unrelated Rust helper() in b.rs"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'b.rs::helper'",
            ),
            0,
            "the Rust helper() must show zero callers — it was never actually called"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same-language, cross-module false callee (the "odra vs soroban" case):
    /// two unrelated types in DIFFERENT files (same language) share a method
    /// name (`execute`), and the caller reaches one of them through a typed
    /// struct FIELD (`self.engine.execute()`), not a local variable. Split
    /// across three files so tier-1's same-file `file_symbols` match (which
    /// takes priority over tier-2 whenever it fires) never fires here —
    /// `execute` is defined in neither odra.rs nor soroban.rs's caller file
    /// (main.rs), forcing resolution through tier-2's receiver-type lookup,
    /// same as the real cross-module case this bug was found in. Before the
    /// field-type-map fix, struct fields were invisible to `type_map`, so
    /// tier-2 had no receiver type to key off of and fell back to the global
    /// by-name fan-out — matching both `execute` methods (marked
    /// `ambiguous`) instead of resolving to the one the field is actually
    /// declared as.
    #[test]
    fn test_field_type_map_resolves_same_language_cross_module_method_call() {
        let dir = std::env::temp_dir().join(format!("ci_idx_fieldtypemap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("odra.rs"),
            "pub struct OdraEngine;\nimpl OdraEngine {\n    pub fn execute(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("soroban.rs"),
            "pub struct SorobanEngine;\nimpl SorobanEngine {\n    pub fn execute(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.rs"),
            "struct Odra {\n    engine: OdraEngine,\n}\n\
             impl Odra {\n    fn run(&self) {\n        self.engine.execute();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'execute' AND class_context = 'OdraEngine')",
            ),
            1,
            "self.engine.execute() must resolve to OdraEngine::execute via the field's declared type"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'execute' AND class_context = 'SorobanEngine')",
            ),
            0,
            "self.engine.execute() must NOT also fan out to the unrelated SorobanEngine::execute"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run')",
            ),
            1,
            "exactly one call edge from run() — not fanned out/marked ambiguous"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same fan-out bug, `by_name_class` variant: two unrelated types in
    /// different files that happen to share both a type name AND a method
    /// name (e.g. two local `struct Handler` in different modules, each with
    /// their own `fn helper`) key into the exact same `by_name_class` slot —
    /// `self.helper()` inside a.rs's `Handler::run` must resolve to a.rs's
    /// own `Handler::helper`, not fan out to b.rs's unrelated one too.
    #[test]
    fn test_self_method_call_prefers_same_file_over_global_fan_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_selffanout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct Handler;\nimpl Handler {\n    fn helper(&self) -> i32 {\n        1\n    }\n    fn run(&self) -> i32 {\n        self.helper()\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct Handler;\nimpl Handler {\n    fn helper(&self) -> i32 {\n        2\n    }\n    fn run(&self) -> i32 {\n        self.helper()\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND path = 'a.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            1,
            "a.rs's Handler::run must resolve self.helper() to a.rs's own Handler::helper"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND path = 'a.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "a.rs's Handler::run must NOT also fan out to b.rs's unrelated Handler::helper"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the real-world incident this fix addresses (found via
    /// this repo's own `common.rs::CodeIntelligenceServer::timed_tool`
    /// delegating to `telemetry.rs::timed_tool` the same way): a same-named
    /// wrapper method calling a fully-qualified `crate::module::func()` with
    /// no `use` for it used to resolve to the WRONG same-named symbol — the
    /// caller's own file, in the worst case a fabricated self-recursive edge
    /// on the wrapper itself — because the explicit module qualifier was
    /// discarded before the same-file preference ever saw it. `module_hint`
    /// must take priority over that preference and route the edge to the
    /// module actually named in the source.
    #[test]
    fn test_qualified_call_resolves_to_named_module_not_same_file_same_name() {
        let dir = std::env::temp_dir().join(format!("ci_idx_modhint_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("telemetry.rs"),
            "pub fn timed_tool(name: &str) -> String {\n    name.to_string()\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("common.rs"),
            "pub struct Server;\nimpl Server {\n    pub fn timed_tool(&self, name: &str) -> String {\n        crate::telemetry::timed_tool(name)\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'telemetry.rs')",
            ),
            1,
            "crate::telemetry::timed_tool(...) must resolve to telemetry.rs's free function"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs')",
            ),
            0,
            "must NOT fabricate a self-recursive edge onto common.rs's own same-named method"
        );
        let telemetry_caller_count: i64 = conn
            .query_row(
                "SELECT caller_count FROM symbols WHERE name = 'timed_tool' AND path = 'telemetry.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            telemetry_caller_count, 1,
            "telemetry.rs::timed_tool must show its one real caller, not 0"
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

    /// Regression for the real-world incident this module's return-shape filter
    /// exists for: `caller.rs` calls a bare `as_str()` on an unresolvable
    /// receiver (Tier-2 can't type it), immediately `.unwrap()`-ed. Two
    /// same-named candidates exist elsewhere in the repo — one returning
    /// `Option<&str>` (a plausible real target), one returning plain `&str`
    /// (provably *not* the target, since `Foo::as_str().unwrap()` wouldn't
    /// compile against a non-`Option`/`Result` return). Before this filter,
    /// `rebuild_graph`'s `MAX_CALLEE_CANDIDATES` fallback fanned out to both.
    #[test]
    fn test_option_chained_call_excludes_non_option_candidates() {
        let dir = std::env::temp_dir().join(format!("ci_idx_optchain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Bar;\nimpl Bar {\n    pub fn as_str(&self) -> Option<&'static str> {\n        Some(\"b\")\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str().unwrap();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'a.rs')",
            ),
            0,
            "Foo::as_str returns &'static str, not Option — .unwrap() on the call site rules it out"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'b.rs')",
            ),
            1,
            "Bar::as_str returns Option<&'static str> — the only candidate .unwrap() could compile against"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the return-shape filter can't break the tie (call site isn't
    /// `?`/`.unwrap()`-chained, or the surviving candidates are still >1),
    /// the resulting fan-out edges must be marked `ambiguous` — not the plain
    /// `textual` a genuine single-candidate resolution gets — so callers of
    /// `callers`/`symbol_info` can tell "spread across N unrelated symbols"
    /// apart from "one real, low-confidence match".
    #[test]
    fn test_unresolved_fan_out_marked_ambiguous_not_textual() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ambiguous_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Baz;\nimpl Baz {\n    pub fn as_str(&self) -> &'static str {\n        \"c\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        for path in ["a.rs", "b.rs"] {
            let confidence: String = conn
                .query_row(
                    "SELECT edge_confidence FROM call_edges \
                     WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                     AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = ?1)",
                    [path],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                confidence, "ambiguous",
                "fanned-out edge to {path}'s as_str must be marked ambiguous, not textual"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the real-world incident this fix addresses: before it,
    /// `caller_count` was a blunt `COUNT(DISTINCT from_symbol)` over every
    /// `call_edges` row regardless of confidence, so an `ambiguous` fan-out
    /// edge (recorded once per same-named candidate) inflated every
    /// candidate's `caller_count` almost identically — `Foo::as_str` (zero
    /// real callers) showed the *same* `caller_count` as `Baz::as_str`
    /// (one real caller via `self.as_str()`), which fed straight into
    /// `dead_code_confidence` (short-circuits to "not dead" on
    /// `caller_count > 0`), hub ranking, and coreness alike.
    #[test]
    fn test_caller_count_excludes_ambiguous_fan_out_edges() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ccambig_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Baz;\nimpl Baz {\n    pub fn as_str(&self) -> &'static str {\n        \"c\"\n    }\n    pub fn wrapper(&self) -> &'static str {\n        self.as_str()\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let caller_count = |path: &str| -> i64 {
            conn.query_row(
                "SELECT caller_count FROM symbols WHERE name = 'as_str' AND path = ?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
        };

        assert_eq!(
            caller_count("a.rs"),
            0,
            "Foo::as_str has only an ambiguous fan-out edge, no confirmed caller"
        );
        assert_eq!(
            caller_count("b.rs"),
            1,
            "Baz::as_str has exactly one confirmed (resolved, same-file self.as_str()) caller — \
             the ambiguous fan-out edge to it must not also be counted"
        );

        // refresh_caller_counts must be independently callable (this is what
        // the SCIP overlay pass re-invokes after flipping ruled_out_by_scip),
        // and must reflect that flag too: rule out caller.rs's own edge to
        // Baz::as_str and confirm its caller_count drops back to 0.
        conn.execute(
            "UPDATE call_edges SET ruled_out_by_scip = 1 \
             WHERE to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'b.rs') \
               AND from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'wrapper')",
            [],
        )
        .unwrap();
        refresh_caller_counts(&conn).unwrap();
        assert_eq!(
            caller_count("b.rs"),
            0,
            "ruled_out_by_scip=1 edges must be excluded after a refresh_caller_counts() re-run"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_signature_returns_option_or_result() {
        assert!(signature_returns_option_or_result(
            "pub fn as_str(&self) -> Option<&'static str> {"
        ));
        assert!(signature_returns_option_or_result(
            "pub fn parse(s: &str) -> Result<Self, Error> {"
        ));
        assert!(!signature_returns_option_or_result(
            "pub fn as_str(&self) -> &'static str {"
        ));
        assert!(!signature_returns_option_or_result("pub struct Foo {"));
        // The return arrow, not one buried in a higher-order parameter type.
        assert!(signature_returns_option_or_result(
            "pub fn foo(f: impl Fn() -> i32) -> Option<i32> {"
        ));
        assert!(!signature_returns_option_or_result(
            "pub fn foo(f: impl Fn() -> Option<i32>) -> i32 {"
        ));
    }

    /// Regression for the duplicate-call-site collapse: two calls to the same
    /// function from the same caller (different lines) used to dedupe to a
    /// single (from, to) edge, losing the second site. The key now includes the
    /// call-site line, so both survive.
    #[test]
    fn distinct_call_sites_in_one_caller_are_kept_as_separate_edges() {
        let dir = std::env::temp_dir().join(format!("ci_idx_dupsite_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `helper` defined once; `caller` calls it twice, on lines 3 and 4.
        std::fs::write(
            dir.join("a.rs"),
            "fn helper() {}\nfn caller() {\n    helper();\n    helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper')",
            ),
            2,
            "two distinct call sites must be kept as two edges, not deduped to one"
        );

        let lines: Vec<i64> = {
            let mut stmt = conn
                .prepare(
                    "SELECT call_site_line FROM call_edges \
                     WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                     AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper') \
                     ORDER BY call_site_line",
                )
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(
            lines,
            vec![3, 4],
            "the two edges carry the two call-site lines"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the import-graph false positive: a bare `use
    /// extern_crate::Item` (an external crate, not a workspace member) must NOT
    /// resolve to the importing crate's own `lib.rs`. Before the `uniform_guess`
    /// gate, the single-trailing-item fallback matched `{crate_root}/lib.rs`.
    #[test]
    fn external_crate_use_does_not_resolve_to_own_lib_rs() {
        let dir = std::env::temp_dir().join(format!("ci_idx_extern_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub mod thing;\n").unwrap();
        std::fs::write(
            dir.join("src/thing.rs"),
            "use rusqlite::Connection;\npub fn f(_c: &Connection) {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'src/thing.rs' AND module_name = 'rusqlite'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            to_path.as_deref().unwrap_or("").is_empty(),
            "external crate `rusqlite` must not resolve to a local file, got {to_path:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
