use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};

use crate::embedding::Embedder;
use crate::types::SearchKind;

const BM25_EXACT_WEIGHT: f64 = 1.5;
const BM25_TOKENS_WEIGHT: f64 = 1.0;
/// Multiplier applied to a result's final score when its path looks like a
/// test/generated/example file — a tie-breaker, not a filter: a noisy-path
/// result can still rank first if nothing cleaner matches at all, it's just
/// pushed behind an equally-relevant non-noisy one. Applied uniformly across
/// every search kind at the point scores are finalized (`search_symbol`'s own
/// sort and `rrf_merge_n`'s fused sort) so ranking behavior doesn't depend on
/// which source found the match.
const NOISE_PENALTY: f64 = 0.6;
/// Default RRF k constant; overridden at runtime by `config.search.rrf_k`.
/// Used as the fallback when config load fails — see `SearchConfig::default`.
pub const DEFAULT_RRF_K: f64 = 20.0;
/// Weight applied to the FTS source in hybrid RRF (design spec: 1.5×).
const RRF_FTS_WEIGHT: f64 = 1.5;
/// Weight applied to the Layer-1 symbol-identity semantic source (name +
/// signature + docstring) in RRF fusion.
const RRF_SEMANTIC_WEIGHT: f64 = 1.0;
/// Weight applied to the Layer-2 code-body chunk semantic source in RRF
/// fusion — same trust tier as Layer 1; RRF's rank-based scoring already lets
/// whichever layer actually matched a given query dominate on its own.
const RRF_CHUNK_WEIGHT: f64 = 1.0;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub qualified_name: String,
    pub path: String,
    pub kind: Option<String>,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub score: f64,
    pub match_type: String,
    pub snippet: Option<String>,
    /// From `symbols.is_test` (see `indexer::parser::detect_is_test`) — used
    /// alongside `is_noisy_path` for the noise penalty. Catches the common
    /// case a path check alone misses: inline `#[test]`/`#[cfg(test)] mod
    /// tests` functions living in the *same file* as the implementation
    /// they test, so there's no separate test-directory path to flag.
    /// `false` for non-symbol results (file/gap-chunk hits).
    pub is_test: bool,
}

#[derive(Debug, Clone)]
pub struct SearchOutput {
    pub results: Vec<SearchResult>,
    pub truncated: bool,
    pub degraded: bool,
    pub note: Option<String>,
}

pub fn search(
    conn: &Connection,
    query: &str,
    kind: SearchKind,
    limit: usize,
    embedder: Option<&Embedder>,
    rrf_k: f64,
) -> rusqlite::Result<SearchOutput> {
    match kind {
        SearchKind::Symbol => search_symbol(conn, query, limit),
        SearchKind::Text => search_text(conn, query, limit),
        SearchKind::File => search_file(conn, query, limit),
        SearchKind::Semantic => search_semantic(conn, query, limit, embedder, rrf_k),
        SearchKind::Hybrid => search_hybrid(conn, query, limit, embedder, rrf_k),
    }
}

/// `true` when `path` looks like a test, generated, or example/fixture file
/// — cheap substring checks on a lowercased path, deliberately conservative
/// (false negatives are fine; a false positive would wrongly demote a real
/// implementation file with e.g. "test" in a legitimate directory name like
/// `latest/`, so checks anchor on path-segment boundaries via `/`, `_`, `.`
/// rather than bare substring wherever that risk is realistic).
fn is_noisy_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    // Paths are stored project-root-relative with no leading slash (e.g.
    // "src/main.py"), so a top-level noisy directory has no "/" before it —
    // check both "at the start" and "after a directory separator".
    let has_dir = |name: &str| {
        let with_slash = format!("{name}/");
        p.starts_with(&with_slash) || p.contains(&format!("/{with_slash}"))
    };
    let is_test = has_dir("test")
        || has_dir("tests")
        || p.starts_with("test_")
        || p.contains("/test_")
        || p.contains("_test.")
        || p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_spec.");
    let is_generated = has_dir("generated")
        || p.contains(".generated.")
        || has_dir("vendor")
        || has_dir("dist")
        || has_dir("node_modules")
        || has_dir("build")
        || p.contains(".min.");
    let is_example = has_dir("examples")
        || has_dir("example")
        || has_dir("fixtures")
        || has_dir("fixture")
        || has_dir("mocks")
        || has_dir("mock");
    is_test || is_generated || is_example
}

/// Score multiplier for a result — see `NOISE_PENALTY`, `is_noisy_path`, and
/// `SearchResult::is_test`.
fn noise_multiplier(path: &str, is_test: bool) -> f64 {
    if is_test || is_noisy_path(path) {
        NOISE_PENALTY
    } else {
        1.0
    }
}

fn escape_fts5_query(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len() + 2);
    escaped.push('"');
    for ch in query.chars() {
        if ch == '"' {
            escaped.push('"');
            escaped.push('"');
        } else {
            escaped.push(ch);
        }
    }
    escaped.push('"');
    escaped
}

fn search_symbol(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<SearchOutput> {
    let fts_query = escape_fts5_query(query);
    let fetch_limit = (limit * 2) as i64;

    // BTreeMap, not HashMap: same determinism reasoning as `rrf_merge_n` —
    // ties in score must break the same way (alphabetically by
    // qualified_name) on every run, not depend on HashMap's
    // per-process-random iteration order.
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    let mut data: BTreeMap<String, SearchResult> = BTreeMap::new();

    let mut stmt_exact = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_exact) AS score, s.is_test
         FROM fts_exact
         JOIN symbols s ON s.id = fts_exact.rowid
         WHERE fts_exact MATCH ?1
         LIMIT ?2",
    )?;

    let rows_exact = stmt_exact.query_map(rusqlite::params![fts_query, fetch_limit], |row| {
        Ok(RawRow {
            qualified_name: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            line_start: row.get(3)?,
            line_end: row.get(4)?,
            kind: row.get(5)?,
            score: row.get(6)?,
            is_test: row.get(7)?,
        })
    })?;

    for row in rows_exact {
        let row = row?;
        *scores.entry(row.qualified_name.clone()).or_default() += row.score * BM25_EXACT_WEIGHT;
        data.entry(row.qualified_name.clone())
            .or_insert_with(|| row.into_result("exact"));
    }

    let mut stmt_tokens = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_tokens) AS score, s.is_test
         FROM fts_tokens
         JOIN symbols s ON s.id = fts_tokens.rowid
         WHERE fts_tokens MATCH ?1
         LIMIT ?2",
    )?;

    let rows_tokens = stmt_tokens.query_map(rusqlite::params![fts_query, fetch_limit], |row| {
        Ok(RawRow {
            qualified_name: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            line_start: row.get(3)?,
            line_end: row.get(4)?,
            kind: row.get(5)?,
            score: row.get(6)?,
            is_test: row.get(7)?,
        })
    })?;

    for row in rows_tokens {
        let row = row?;
        *scores.entry(row.qualified_name.clone()).or_default() += row.score * BM25_TOKENS_WEIGHT;
        data.entry(row.qualified_name.clone())
            .or_insert_with(|| row.into_result("tokens"));
    }

    for (qname, r) in data.iter() {
        if let Some(s) = scores.get_mut(qname) {
            *s *= noise_multiplier(&r.path, r.is_test);
        }
    }

    let mut ranked: Vec<_> = data.into_iter().collect();
    ranked.sort_by(|a, b| {
        let sa = scores.get(&a.0).unwrap_or(&0.0);
        let sb = scores.get(&b.0).unwrap_or(&0.0);
        sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let truncated = ranked.len() > limit;
    let results: Vec<SearchResult> = ranked
        .into_iter()
        .take(limit)
        .map(|(qname, mut r)| {
            r.score = *scores.get(&qname).unwrap_or(&0.0);
            r
        })
        .collect();

    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

fn search_text(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<SearchOutput> {
    let raw_query = escape_fts5_query(query);
    // FTS5 global column filter: {docstring signature} restricts ALL tokens to
    // just those two columns — deliberately excludes `name` (that's what
    // kind="symbol" is for) but, unlike the old docstring-only filter, now
    // also matches a symbol's signature (parameter/return types), not just
    // its docstring. Still doesn't cover function bodies, imports, or
    // non-code files — that's what kind="grep" is for (search_grep, above).
    let fts_query = format!("{{docstring signature}} : {raw_query}");
    // Fetch one extra row beyond `limit` so `truncated` can tell "exactly
    // `limit` results, no more" apart from "more results exist beyond the
    // page" — LIMIT ?2 alone would always return at most `limit` rows,
    // making a `>= limit` truncation check spuriously true whenever the
    // result count happens to equal `limit`.
    let fetch_limit = (limit + 1) as i64;

    let mut stmt = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_exact) AS score, s.is_test
         FROM fts_exact
         JOIN symbols s ON s.id = fts_exact.rowid
         WHERE fts_exact MATCH ?1
         ORDER BY score DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![fts_query, fetch_limit], |row| {
        Ok(RawRow {
            qualified_name: row.get(0)?,
            name: row.get(1)?,
            path: row.get(2)?,
            line_start: row.get(3)?,
            line_end: row.get(4)?,
            kind: row.get(5)?,
            score: row.get(6)?,
            is_test: row.get(7)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?.into_result("text"));
    }

    let truncated = results.len() > limit;
    results.truncate(limit);
    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

/// A query is treated as a glob (matched via `globset` against the whole
/// relative path) instead of a plain substring when it contains any glob
/// metacharacter — otherwise `search_file` keeps its original SQL `LIKE`
/// substring behavior unchanged, so existing non-glob callers see no
/// difference.
fn looks_like_glob(query: &str) -> bool {
    query.contains(['*', '?', '['])
}

fn search_file(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<SearchOutput> {
    // Fetch one extra match beyond `limit` in both branches so `truncated`
    // can distinguish "exactly `limit` matches, no more" from "more exist
    // beyond the page" — matching search_symbol's over-fetch-then-truncate
    // approach instead of the previous `>= limit` check, which was always
    // true-or-false-wrong since neither branch below could ever return
    // more than `limit` rows on its own.
    let fetch_limit = limit + 1;

    let mut results = if looks_like_glob(query) {
        let Ok(matcher) = globset::Glob::new(query).map(|g| g.compile_matcher()) else {
            // Invalid glob syntax — degrade to zero results rather than erroring
            // the whole tool call; the caller sees an empty result set and can
            // retry with a corrected pattern.
            return Ok(SearchOutput {
                results: vec![],
                truncated: false,
                degraded: true,
                note: Some(format!("invalid glob pattern: {query}")),
            });
        };

        let mut stmt = conn.prepare("SELECT fi.path FROM file_index fi ORDER BY fi.path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut matched = Vec::new();
        for row in rows {
            let path = row?;
            if matcher.is_match(&path) {
                matched.push(path);
            }
            if matched.len() >= fetch_limit {
                break;
            }
        }
        matched
    } else {
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT fi.path
             FROM file_index fi
             WHERE fi.path LIKE ?1
             ORDER BY fi.path
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, fetch_limit as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut matched = Vec::new();
        for row in rows {
            matched.push(row?);
        }
        matched
    };

    let truncated = results.len() > limit;
    results.truncate(limit);
    let results = results
        .into_iter()
        .map(|path| SearchResult {
            name: path.rsplit('/').next().unwrap_or(&path).to_string(),
            qualified_name: path.clone(),
            path,
            kind: None,
            line_start: None,
            line_end: None,
            score: 1.0,
            match_type: "file".to_string(),
            snippet: None,
            is_test: false,
        })
        .collect();

    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

/// Find the tightest enclosing symbol for `(path, line)`, if any — enriches
/// `search_grep` matches with a `name`/`qualified_name`/`kind` a raw text
/// search can't offer on its own. Narrowest span wins when ranges nest
/// (e.g. a method inside a class).
fn enclosing_symbol(
    conn: &Connection,
    path: &str,
    line: i64,
) -> rusqlite::Result<Option<(String, String, Option<String>)>> {
    conn.query_row(
        "SELECT name, qualified_name, kind FROM symbols
         WHERE path = ?1 AND line_start <= ?2 AND line_end >= ?2
         ORDER BY (line_end - line_start) ASC
         LIMIT 1",
        rusqlite::params![path, line],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

/// Hard cap on a single rendered snippet line's length. Without this, a
/// pathological single-line file (minified JSON, a generated lockfile, a
/// vendored data blob) that happens to contain a match turns one grep
/// result into a multi-megabyte response — this bit a live run against
/// `crates/calm-core/assets/potion-code-16m/tokenizer.json` (a ~1-line JSON
/// vocab file) during Tier A verification.
const MAX_SNIPPET_LINE_CHARS: usize = 300;

/// Files larger than this are skipped by `search_grep` entirely, before
/// reading — grepping multi-megabyte generated/vendored blobs is rarely
/// useful and wastes time; real source files essentially never hit this.
const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn truncate_snippet_line(line: &str) -> std::borrow::Cow<'_, str> {
    if line.len() <= MAX_SNIPPET_LINE_CHARS {
        return std::borrow::Cow::Borrowed(line);
    }
    // Truncate on a char boundary, not a byte offset, to stay UTF8-safe.
    let cut = line
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= MAX_SNIPPET_LINE_CHARS)
        .last()
        .unwrap_or(0);
    std::borrow::Cow::Owned(format!(
        "{}… (truncated, {} more chars)",
        &line[..cut],
        line.len() - cut
    ))
}

/// Render `context` lines of surrounding text around the matched line
/// (0-indexed `match_idx` into `lines`), 1-indexed and marked, e.g.:
/// `"    12: fn foo() {\n>   13:     bar();\n    14: }"`. Each rendered line
/// is capped at `MAX_SNIPPET_LINE_CHARS` — see its doc comment.
fn build_snippet(lines: &[&str], match_idx: usize, context: usize) -> String {
    let start = match_idx.saturating_sub(context);
    let end = (match_idx + context).min(lines.len().saturating_sub(1));
    (start..=end)
        .map(|i| {
            let marker = if i == match_idx { ">" } else { " " };
            format!("{marker} {:>5}: {}", i + 1, truncate_snippet_line(lines[i]))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One regex match found while scanning a file — pure text-layer data, no DB
/// involvement yet (`enclosing_symbol` enrichment happens afterward, in a
/// single sequential pass — see `search_grep`).
struct RawGrepMatch {
    rel_path: String,
    line_no: i64,
    snippet: String,
}

/// `search(kind="grep")`: regex + optional glob search over raw file
/// content read straight off disk via the shared `crate::walk` walker — not
/// the FTS/DB index. This is what lets it cover files the indexer never
/// parses at all (`Cargo.toml`, `docs/*.md`, `Containerfile`, ...), unlike
/// `search_text` which only ever matches the `docstring` FTS column of
/// already-indexed symbols. Each match is opportunistically enriched with
/// its enclosing symbol (if any) via `enclosing_symbol` — a plain grep tool
/// can't do that because it has no code graph to consult.
///
/// Two-phase to parallelize the expensive part safely: (1) walk + glob/size
/// filter is sequential and cheap (metadata only), (2) reading and
/// regex-scanning each candidate file's content is the real cost on a large
/// repo and runs via `rayon` — the same "parallel CPU work, no DB" pattern
/// `indexer::pipeline::reindex_changed` already uses for its hash pass.
/// Symbol enrichment stays a single sequential pass afterward because
/// `rusqlite::Connection` is `Send` but not `Sync` — it can't be shared by
/// reference across rayon's worker threads.
#[allow(clippy::too_many_arguments)]
pub fn search_grep(
    conn: &Connection,
    project_root: &Path,
    pattern: &str,
    glob: Option<&str>,
    case_insensitive: bool,
    context: usize,
    ignore_patterns: &[String],
    limit: usize,
) -> anyhow::Result<SearchOutput> {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()?;
    let glob_matcher = glob
        .map(|g| globset::Glob::new(g).map(|gl| gl.compile_matcher()))
        .transpose()?;

    // Phase 1 (sequential, cheap): walk + glob/size filter to candidates.
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    for entry in crate::walk::build_walker(project_root, ignore_patterns) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let rel_path = path
            .strip_prefix(project_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if let Some(matcher) = &glob_matcher
            && !matcher.is_match(&rel_path)
        {
            continue;
        }
        // Skip oversized files before reading — see MAX_GREP_FILE_BYTES.
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_GREP_FILE_BYTES) {
            continue;
        }
        candidates.push((path, rel_path));
    }

    // Phase 2 (parallel, expensive): read + regex-scan each candidate.
    // `.par_iter().map().collect::<Vec<_>>()` on a `Vec` preserves the
    // original (walk) order in the output, so results stay deterministic.
    let per_file: Vec<Vec<RawGrepMatch>> = candidates
        .par_iter()
        .map(|(path, rel_path)| {
            // Non-UTF8/binary files fail to decode here and are silently
            // skipped, same as ripgrep's default binary-detection behavior.
            let Ok(content) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let lines: Vec<&str> = content.lines().collect();
            let mut matches = Vec::new();
            for (idx, line) in lines.iter().enumerate() {
                if re.is_match(line) {
                    matches.push(RawGrepMatch {
                        rel_path: rel_path.clone(),
                        line_no: (idx + 1) as i64,
                        // Raw disk content, never indexed/DB-stored — must be
                        // redacted here directly, same as `source()`'s body
                        // text, since this is the only point it ever passes
                        // through before reaching the caller.
                        snippet: crate::sanitize::sanitize_source_output(&build_snippet(
                            &lines, idx, context,
                        )),
                    });
                }
            }
            matches
        })
        .collect();

    // Phase 3 (sequential, single connection): enrich with the enclosing
    // symbol (if any) and apply the result limit.
    let mut results = Vec::new();
    let mut truncated = false;
    'enrich: for file_matches in per_file {
        for m in file_matches {
            if results.len() >= limit {
                truncated = true;
                break 'enrich;
            }
            let symbol = enclosing_symbol(conn, &m.rel_path, m.line_no)?;
            let (name, qualified_name, kind) = match symbol {
                Some((name, qn, kind)) => (name, qn, kind),
                None => (
                    m.rel_path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&m.rel_path)
                        .to_string(),
                    format!("{}:{}", m.rel_path, m.line_no),
                    None,
                ),
            };

            results.push(SearchResult {
                name,
                qualified_name,
                path: m.rel_path,
                kind,
                line_start: Some(m.line_no),
                line_end: Some(m.line_no),
                score: 1.0,
                match_type: "grep".to_string(),
                snippet: Some(m.snippet),
                is_test: false,
            });
        }
    }

    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

/// Layer 1: KNN over symbol-identity vectors (name + signature + docstring),
/// resolved back to their symbol row.
fn symbol_semantic_results(
    conn: &Connection,
    qvec: &[f32],
    limit: usize,
) -> rusqlite::Result<Vec<SearchResult>> {
    // Fetch one extra hit beyond `limit` — `knn` returns at most `k` rows,
    // so asking for exactly `limit` makes it impossible for a caller to
    // tell "exactly limit matches" apart from "more exist" (the same
    // off-by-one search_text/search_file had — see their fix comments).
    let hits = crate::embedding::knn(conn, qvec, limit + 1)?;
    let mut stmt = conn.prepare(
        "SELECT qualified_name, name, path, line_start, line_end, kind, is_test FROM symbols WHERE id = ?1",
    )?;
    let mut results = Vec::with_capacity(hits.len());
    for (id, dist) in &hits {
        if let Ok(mut r) = stmt.query_row(rusqlite::params![id], |row| {
            Ok(SearchResult {
                qualified_name: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                line_start: row.get(3)?,
                line_end: row.get(4)?,
                kind: row.get(5)?,
                score: 0.0,
                match_type: "semantic".to_string(),
                snippet: None,
                is_test: row.get(6)?,
            })
        }) {
            // cosine distance → similarity in [0, 1] for a friendlier score.
            r.score = 1.0 - dist;
            results.push(r);
        }
    }
    Ok(results)
}

/// Layer 2: KNN over code-body chunk vectors, resolved back to a
/// `SearchResult` anchored at the chunk's own line range (the specific
/// window that matched, not the whole enclosing symbol) — see
/// [`chunk_hit_to_result`].
fn chunk_semantic_results(
    conn: &Connection,
    qvec: &[f32],
    limit: usize,
) -> rusqlite::Result<Vec<SearchResult>> {
    // See symbol_semantic_results' comment: over-fetch by 1 so truncation
    // can be detected accurately by callers instead of always appearing
    // "truncated" whenever the count happens to equal `limit` exactly.
    let hits = crate::embedding::knn_chunks(conn, qvec, limit + 1)?;
    let mut results = Vec::with_capacity(hits.len());
    for (chunk_id, dist) in &hits {
        if let Some(mut r) = chunk_hit_to_result(conn, *chunk_id)? {
            // cosine distance → similarity in [0, 1] for a friendlier score.
            r.score = 1.0 - dist;
            results.push(r);
        }
    }
    Ok(results)
}

/// Resolve one `code_chunks` row into a `SearchResult`. When the chunk has an
/// enclosing symbol (`symbol_qn` set), the result carries that symbol's real
/// `qualified_name`/`name`/`kind` — which lets RRF merging in
/// [`rrf_merge_n`] naturally fuse a Layer-2 chunk hit with a Layer-1 hit for
/// the *same* symbol, since both share the same dedup key. A gap chunk (no
/// enclosing symbol) gets a synthesized key unique to its line range and
/// falls back to the bare filename for `name`, mirroring `search_file`.
fn chunk_hit_to_result(conn: &Connection, chunk_id: i64) -> rusqlite::Result<Option<SearchResult>> {
    let mut chunk_stmt = conn
        .prepare("SELECT path, line_start, line_end, symbol_qn FROM code_chunks WHERE id = ?1")?;
    let row = chunk_stmt.query_row(rusqlite::params![chunk_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    });
    let Ok((path, line_start, line_end, symbol_qn)) = row else {
        return Ok(None);
    };

    let (qualified_name, name, kind, is_test) = match &symbol_qn {
        Some(qn) => {
            let mut sym_stmt =
                conn.prepare("SELECT name, kind, is_test FROM symbols WHERE qualified_name = ?1")?;
            let sym = sym_stmt.query_row(rusqlite::params![qn], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            });
            match sym {
                Ok((name, kind, is_test)) => (qn.clone(), name, kind, is_test),
                Err(_) => (qn.clone(), qn.clone(), None, false),
            }
        }
        None => {
            let fname = path.rsplit('/').next().unwrap_or(&path).to_string();
            (
                format!("{path}#chunk:{line_start}-{line_end}"),
                fname,
                None,
                false,
            )
        }
    };

    Ok(Some(SearchResult {
        name,
        qualified_name,
        path,
        kind,
        line_start: Some(line_start),
        line_end: Some(line_end),
        score: 0.0,
        match_type: "semantic_chunk".to_string(),
        snippet: None,
        is_test,
    }))
}

/// The `note` explaining why semantic results are unavailable for `search_hybrid`/
/// `search_semantic`, tailored to the actual reason instead of one generic
/// hardcoded guess — a build without the `embeddings` feature, config with
/// `semantic_search.enabled: false`, or a process whose embedder hasn't
/// loaded yet/failed/was blocked need different next steps, and guessing
/// wrong sends the caller chasing the wrong fix (e.g. "recompile" when the
/// binary already has the feature and the real issue is a `Failed`/
/// `OfflineUnavailable` status visible in `indexing_status`).
fn embedder_unavailable_note() -> String {
    if !crate::embedding::ENABLED {
        "Semantic search inactive — this binary wasn't compiled with the `embeddings` Cargo \
         feature (rebuild with `--features embeddings`, or use the default feature set, which \
         already includes it)"
            .to_string()
    } else {
        "Semantic search inactive for this process — either `semantic_search.enabled` is \
         `false` in config.json, the embedder is still loading, or a prior load attempt \
         failed/was blocked. Call indexing_status for `embeddings_status`/`embeddings_error`, \
         and `retry_embeddings: true` to retry a failed load."
            .to_string()
    }
}

fn search_semantic(
    conn: &Connection,
    query: &str,
    limit: usize,
    embedder: Option<&Embedder>,
    rrf_k: f64,
) -> rusqlite::Result<SearchOutput> {
    let Some(embedder) = embedder else {
        return Ok(SearchOutput {
            results: Vec::new(),
            truncated: false,
            degraded: true,
            note: Some(embedder_unavailable_note()),
        });
    };

    let qvec = embedder.embed_one(query);
    if qvec.is_empty() {
        return Ok(SearchOutput {
            results: Vec::new(),
            truncated: false,
            degraded: true,
            note: Some("Embedding model unavailable".to_string()),
        });
    }

    // Layer 1 (symbol identity) and Layer 2 (code body) are independent
    // vector spaces — query both and fuse. See `indexer::chunker`'s module
    // doc for why a symbol's name+signature+docstring alone can miss a query
    // that only matches vocabulary used *inside* its body.
    let sym_results = symbol_semantic_results(conn, &qvec, limit)?;
    let chunk_results = chunk_semantic_results(conn, &qvec, limit)?;
    // Both sources over-fetch by 1 (see their own comments) — `> limit`,
    // not `>= limit`, is the correct "more exist beyond this page" check.
    let truncated = sym_results.len() > limit || chunk_results.len() > limit;

    let mut results = match (sym_results.is_empty(), chunk_results.is_empty()) {
        (true, true) => Vec::new(),
        (false, true) => sym_results,
        (true, false) => chunk_results,
        (false, false) => rrf_merge_n(
            &[
                (&sym_results, RRF_SEMANTIC_WEIGHT),
                (&chunk_results, RRF_CHUNK_WEIGHT),
            ],
            limit,
            rrf_k,
            "semantic",
        ),
    };
    // rrf_merge_n already caps its own output to `limit`; this only bites
    // the two direct-passthrough branches above, which can carry the extra
    // over-fetched row.
    results.truncate(limit);

    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

/// Vector-similarity search anchored at a specific code location instead of
/// a text query — "find code that looks like *this*", not "find code
/// matching these words". Reuses the same chunk embeddings
/// `chunk_semantic_results` scans, and is cheaper than a text query since
/// the anchor's own stored vector is the query vector (no embedder
/// inference needed).
///
/// Excludes the anchor chunk itself and any other chunk belonging to the
/// same symbol as the anchor (sliding-window neighbours of the anchor's own
/// function aren't "elsewhere"), then keeps only the highest-scoring chunk
/// per symbol/gap-region so the result set can't collapse into N windows of
/// one other function.
pub fn search_similar(
    conn: &Connection,
    path: &str,
    line: i64,
    limit: usize,
) -> rusqlite::Result<SearchOutput> {
    let Some((anchor_id, anchor_vec, anchor_symbol_qn)) =
        crate::embedding::chunk_at(conn, path, line)?
    else {
        return Ok(SearchOutput {
            results: Vec::new(),
            truncated: false,
            degraded: true,
            note: Some(
                "No embedded chunk at this location — the embeddings feature may be off, the index hasn't embedded this file/line yet, or the line is out of range"
                    .to_string(),
            ),
        });
    };

    // Overfetch: the anchor itself, same-symbol windows, and repeat symbols
    // all get filtered below, so asking knn_chunks for exactly `limit` would
    // under-return once any filtering kicks in.
    let hits = crate::embedding::knn_chunks(conn, &anchor_vec, (limit * 4).max(20))?;

    let mut seen = std::collections::HashSet::new();
    let mut results = Vec::with_capacity(limit);
    for (chunk_id, dist) in hits {
        if chunk_id == anchor_id {
            continue;
        }
        let Some(mut r) = chunk_hit_to_result(conn, chunk_id)? else {
            continue;
        };
        if anchor_symbol_qn.as_deref() == Some(r.qualified_name.as_str()) {
            continue; // another window of the anchor's own symbol, not "elsewhere"
        }
        if !seen.insert(r.qualified_name.clone()) {
            continue; // keep only the best-scoring chunk per symbol/gap-region
        }
        r.score = 1.0 - dist;
        r.match_type = "similar_chunk".to_string();
        results.push(r);
        // Collect one past `limit` (already-fetched, in-memory candidates —
        // no extra query cost) so truncation can be detected accurately
        // instead of firing whenever the count happens to equal `limit`.
        if results.len() > limit {
            break;
        }
    }

    let truncated = results.len() > limit;
    results.truncate(limit);
    Ok(SearchOutput {
        truncated,
        degraded: false,
        note: None,
        results,
    })
}
fn search_hybrid(
    conn: &Connection,
    query: &str,
    limit: usize,
    embedder: Option<&Embedder>,
    rrf_k: f64,
) -> rusqlite::Result<SearchOutput> {
    let fts_output = search_symbol(conn, query, limit)?;

    let Some(embedder) = embedder else {
        return Ok(SearchOutput {
            degraded: true,
            note: Some(format!(
                "Hybrid search degraded to FTS-only — {}",
                embedder_unavailable_note()
            )),
            ..fts_output
        });
    };

    let qvec = embedder.embed_one(query);
    let (sym_results, chunk_results) = if qvec.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        (
            symbol_semantic_results(conn, &qvec, limit)?,
            chunk_semantic_results(conn, &qvec, limit)?,
        )
    };

    if sym_results.is_empty() && chunk_results.is_empty() {
        return Ok(SearchOutput {
            degraded: true,
            note: Some("Semantic results empty — hybrid degraded to FTS-only".to_string()),
            ..fts_output
        });
    }

    // sym_results/chunk_results over-fetch by 1 (see symbol_semantic_results'
    // comment) so `> limit`, not `>= limit`, correctly signals "more exist"
    // instead of firing whenever the count happens to equal `limit` exactly.
    // fts_output.truncated is already correct (search_symbol over-fetches too).
    let truncated =
        fts_output.truncated || sym_results.len() > limit || chunk_results.len() > limit;

    // True 3-way RRF: FTS, Layer-1 symbol-semantic, and Layer-2 chunk-semantic
    // each contribute their own rank-based score for a given result, summed —
    // not a nested 2-way merge, which would double-count rank for anything
    // appearing in more than one source.
    let merged = rrf_merge_n(
        &[
            (&fts_output.results, RRF_FTS_WEIGHT),
            (&sym_results, RRF_SEMANTIC_WEIGHT),
            (&chunk_results, RRF_CHUNK_WEIGHT),
        ],
        limit,
        rrf_k,
        "hybrid",
    );

    Ok(SearchOutput {
        results: merged,
        truncated,
        degraded: false,
        note: None,
    })
}

/// Reciprocal Rank Fusion across any number of ranked sources: each
/// `(results, weight)` source contributes `weight / (rrf_k + rank + 1)` per
/// item, summed by `qualified_name` — the standard RRF formula, generalized
/// from 2 sources to N so hybrid search can fuse FTS + both semantic layers
/// in one flat pass instead of nesting 2-way merges (which would compound
/// rank-based scoring for anything present in more than one source).
fn rrf_merge_n(
    sources: &[(&[SearchResult], f64)],
    limit: usize,
    rrf_k: f64,
    match_type: &str,
) -> Vec<SearchResult> {
    // BTreeMap, not HashMap: ties in RRF score must break the same way on
    // every run. HashMap's per-process-random iteration order fed
    // `data.into_iter().collect()` below a different starting order each
    // run, and `sort_by`'s stable sort preserves whatever order it's given
    // — so two results tied on score could swap rank from one process
    // launch to the next. BTreeMap iterates in `qualified_name` order
    // instead, so ties now break alphabetically, deterministically.
    let mut scores: BTreeMap<String, f64> = BTreeMap::new();
    let mut data: BTreeMap<String, SearchResult> = BTreeMap::new();

    for (results, weight) in sources {
        for (rank, r) in results.iter().enumerate() {
            let rrf_score = weight / (rrf_k + rank as f64 + 1.0);
            *scores.entry(r.qualified_name.clone()).or_default() += rrf_score;
            data.entry(r.qualified_name.clone())
                .or_insert_with(|| r.clone());
        }
    }

    for (qname, r) in data.iter() {
        if let Some(s) = scores.get_mut(qname) {
            *s *= noise_multiplier(&r.path, r.is_test);
        }
    }

    let mut ranked: Vec<_> = data.into_iter().collect();
    ranked.sort_by(|a, b| {
        let sa = scores.get(&a.0).unwrap_or(&0.0);
        let sb = scores.get(&b.0).unwrap_or(&0.0);
        sb.partial_cmp(sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    ranked
        .into_iter()
        .take(limit)
        .map(|(qname, mut r)| {
            r.score = *scores.get(&qname).unwrap_or(&0.0);
            r.match_type = match_type.to_string();
            r
        })
        .collect()
}

struct RawRow {
    qualified_name: String,
    name: String,
    path: String,
    line_start: Option<i64>,
    line_end: Option<i64>,
    kind: Option<String>,
    score: f64,
    is_test: bool,
}

impl RawRow {
    fn into_result(self, match_type: &str) -> SearchResult {
        SearchResult {
            name: self.name,
            qualified_name: self.qualified_name,
            path: self.path,
            kind: self.kind,
            line_start: self.line_start,
            line_end: self.line_end,
            score: self.score,
            match_type: match_type.to_string(),
            snippet: None,
            is_test: self.is_test,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn setup_db_with_symbols() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["src/main.py", "abc123", "python", 0.0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["src/utils.py", "def456", "python", 0.0],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["src/helper.ts", "ghi789", "typescript", 0.0],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "get_user",
                "src/main.py::get_user",
                "function",
                "src/main.py",
                "python",
                10,
                20,
                "Fetch a user by ID from the database",
                "get user"
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "update_user",
                "src/main.py::update_user",
                "function",
                "src/main.py",
                "python",
                25,
                40,
                "Update user fields in the database",
                "update user"
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "parse_config",
                "src/utils.py::parse_config",
                "function",
                "src/utils.py",
                "python",
                1,
                15,
                "Parse configuration from TOML file",
                "parse config"
            ],
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_escape_fts5_query() {
        assert_eq!(escape_fts5_query("hello"), "\"hello\"");
        assert_eq!(
            escape_fts5_query("it's a \"test\""),
            "\"it's a \"\"test\"\"\""
        );
    }

    #[test]
    fn test_search_symbol_finds_results() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Symbol, 10, None, DEFAULT_RRF_K).unwrap();
        assert!(
            output.results.len() >= 2,
            "Should find get_user and update_user, got: {:?}",
            output.results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_search_symbol_respects_limit() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Symbol, 1, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 1);
        assert!(output.truncated);
    }

    #[test]
    fn test_search_symbol_no_results() {
        let conn = setup_db_with_symbols();
        let output = search(
            &conn,
            "nonexistent_xyz",
            SearchKind::Symbol,
            10,
            None,
            DEFAULT_RRF_K,
        )
        .unwrap();
        assert!(output.results.is_empty());
        assert!(!output.truncated);
    }

    #[test]
    fn test_search_symbol_ties_break_deterministically_by_qualified_name() {
        // Same reasoning as rrf_merge_n's determinism test: two symbols with
        // identical name/docstring/tokens genuinely tie on combined BM25
        // score. Before switching search_symbol's internal maps from
        // HashMap to BTreeMap, the tie's winner depended on HashMap's
        // per-process-random iteration order.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for qn in ["src/b.py::widget", "src/a.py::widget"] {
            let path = qn.split("::").next().unwrap();
            conn.execute(
                "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    "widget", qn, "function",
                    path, "python", 1, 5, "", "widget"
                ],
            )
            .unwrap();
        }

        let output = search(&conn, "widget", SearchKind::Symbol, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 2);
        assert_eq!(
            output.results[0].score, output.results[1].score,
            "must be a genuine tie"
        );
        assert_eq!(
            output.results[0].qualified_name,
            "src/a.py::widget",
            "tied results must order deterministically (alphabetically), got: {:?}",
            output
                .results
                .iter()
                .map(|r| &r.qualified_name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_search_text() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "database", SearchKind::Text, 10, None, DEFAULT_RRF_K).unwrap();
        assert!(
            output.results.len() >= 2,
            "Should find symbols with 'database' in docstring, got: {:?}",
            output.results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_search_text_truncated_flag_is_accurate() {
        // Regression coverage: search_text used to SELECT ... LIMIT <limit>
        // directly, so results.len() could never exceed limit — checking
        // `results.len() >= limit` then made `truncated` spuriously true
        // whenever the result count happened to equal limit exactly, even
        // with no further results beyond the page.
        let conn = setup_db_with_symbols();
        // "database" appears in both get_user's and update_user's docstring.
        let exact = search(&conn, "database", SearchKind::Text, 2, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(exact.results.len(), 2);
        assert!(
            !exact.truncated,
            "exactly `limit` results with no more beyond it must not be reported as truncated"
        );

        let capped = search(&conn, "database", SearchKind::Text, 1, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(capped.results.len(), 1);
        assert!(
            capped.truncated,
            "fewer results returned than actually match must be reported as truncated"
        );
    }

    #[test]
    fn test_search_text_does_not_match_name_only() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Symbol: name contains "authorize", docstring is EMPTY
        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "authorize_user", "auth::authorize_user", "function",
                "auth.py", "python", 1, 10, "", "authorize user"
            ],
        ).unwrap();

        // Symbol: name does NOT contain "authorize", docstring DOES
        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "check_perms", "auth::check_perms", "function",
                "auth.py", "python", 12, 20, "Checks if user can authorize the given action.", "check perms"
            ],
        ).unwrap();

        let output = search(
            &conn,
            "authorize",
            SearchKind::Text,
            10,
            None,
            DEFAULT_RRF_K,
        )
        .unwrap();
        let names: Vec<&str> = output.results.iter().map(|r| r.name.as_str()).collect();

        assert!(
            names.contains(&"check_perms"),
            "check_perms (docstring match) must appear, got: {names:?}"
        );
        assert!(
            !names.contains(&"authorize_user"),
            "authorize_user must NOT appear — its docstring is empty, got: {names:?}"
        );
    }

    #[test]
    fn test_search_file() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "main", SearchKind::File, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "src/main.py");
    }

    #[test]
    fn test_search_file_multiple() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "src/", SearchKind::File, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 3);
    }

    #[test]
    fn test_search_file_glob_matches_extension() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "*.ts", SearchKind::File, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "src/helper.ts");
    }

    #[test]
    fn test_search_file_glob_no_match_falls_back_empty_not_substring() {
        let conn = setup_db_with_symbols();
        // A glob query must NOT silently degrade to substring matching —
        // "*.py" as a literal substring would match nothing anyway here,
        // but this pins the glob-vs-substring branch selection itself via
        // looks_like_glob rather than accidentally exercising LIKE.
        let output = search(&conn, "*.rs", SearchKind::File, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 0);
    }

    #[test]
    fn test_search_file_plain_substring_still_works() {
        // Non-glob queries keep the original LIKE substring behavior.
        let conn = setup_db_with_symbols();
        let output = search(&conn, "helper", SearchKind::File, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "src/helper.ts");
    }

    #[test]
    fn test_search_file_truncated_flag_is_accurate() {
        // Same off-by-one class as test_search_text_truncated_flag_is_accurate,
        // covering both the LIKE branch and the glob branch of search_file.
        let conn = setup_db_with_symbols();

        // LIKE branch: "src/" matches all 3 indexed files.
        let exact = search(&conn, "src/", SearchKind::File, 3, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(exact.results.len(), 3);
        assert!(
            !exact.truncated,
            "LIKE branch: exact count must not be truncated"
        );

        let capped = search(&conn, "src/", SearchKind::File, 2, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(capped.results.len(), 2);
        assert!(
            capped.truncated,
            "LIKE branch: more matches than limit must be truncated"
        );

        // Glob branch: "*.py" matches src/main.py and src/utils.py.
        let exact_glob = search(&conn, "*.py", SearchKind::File, 2, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(exact_glob.results.len(), 2);
        assert!(
            !exact_glob.truncated,
            "glob branch: exact count must not be truncated"
        );

        let capped_glob = search(&conn, "*.py", SearchKind::File, 1, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(capped_glob.results.len(), 1);
        assert!(
            capped_glob.truncated,
            "glob branch: more matches than limit must be truncated"
        );
    }

    #[test]
    fn test_search_text_matches_signature_not_just_docstring() {
        let conn = setup_db_with_symbols();
        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, signature, docstring, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "reticulate",
                "src/main.py::reticulate",
                "function",
                "src/main.py",
                "python",
                50,
                55,
                "fn reticulate(spline: Widgetronic) -> bool",
                "",
                "reticulate"
            ],
        )
        .unwrap();

        let output = search(
            &conn,
            "widgetronic",
            SearchKind::Text,
            10,
            None,
            DEFAULT_RRF_K,
        )
        .unwrap();
        assert_eq!(
            output.results.len(),
            1,
            "kind=text should now also match a symbol's signature, not just its docstring"
        );
        assert_eq!(output.results[0].name, "reticulate");
    }

    fn make_temp_project(suffix: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ci_search_grep_{suffix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (rel, content) in files {
            let path = dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    #[test]
    fn test_search_grep_finds_raw_text_in_non_indexed_file() {
        let dir = make_temp_project(
            "nonindexed",
            &[("Cargo.toml", "[dependencies]\nrayon = \"1\"\n")],
        );
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "rayon", None, false, 0, &[], 10).unwrap();
        assert_eq!(
            output.results.len(),
            1,
            "grep must reach files the indexer never parses"
        );
        assert_eq!(output.results[0].path, "Cargo.toml");
        assert_eq!(output.results[0].line_start, Some(2));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_regex_and_case_insensitive() {
        let dir = make_temp_project("regex", &[("a.py", "FOO_BAR = 1\nbaz = 2\n")]);
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "^foo_bar", None, true, 0, &[], 10).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].line_start, Some(1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_glob_filter_restricts_files() {
        let dir = make_temp_project("globfilter", &[("a.py", "needle\n"), ("b.md", "needle\n")]);
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "needle", Some("*.py"), false, 0, &[], 10).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "a.py");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_enriches_match_with_enclosing_symbol() {
        let dir = make_temp_project(
            "enrich",
            &[(
                "a.py",
                "def helper():\n    needle_marker = 1\n    return needle_marker\n",
            )],
        );
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, name_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params!["helper", "a.py::helper", "function", "a.py", "python", 1, 3, "helper"],
        )
        .unwrap();

        let output =
            search_grep(&conn, &dir, "needle_marker = 1", None, false, 0, &[], 10).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].qualified_name, "a.py::helper");
        assert_eq!(output.results[0].kind.as_deref(), Some("function"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_respects_limit_and_truncates() {
        let dir = make_temp_project("limit", &[("a.py", "needle\nneedle\nneedle\n")]);
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "needle", None, false, 0, &[], 2).unwrap();
        assert_eq!(output.results.len(), 2);
        assert!(output.truncated);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for a live-verification bug: a pathological single-line
    /// file (minified JSON, a generated data blob — this exact shape hit
    /// `crates/calm-core/assets/potion-code-16m/tokenizer.json` in practice)
    /// must not balloon one match's snippet into megabytes of output.
    #[test]
    fn test_search_grep_truncates_pathological_long_line() {
        let huge_line = format!("{}needle{}", "x".repeat(10_000), "y".repeat(10_000));
        let dir = make_temp_project("longline", &[("blob.json", &huge_line)]);
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "needle", None, false, 0, &[], 10).unwrap();
        assert_eq!(output.results.len(), 1);
        let snippet = output.results[0].snippet.as_ref().unwrap();
        assert!(
            snippet.len() < 1_000,
            "snippet must be capped, got {} chars",
            snippet.len()
        );
        assert!(snippet.contains("truncated"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_skips_oversized_files() {
        let huge_content = "x".repeat((MAX_GREP_FILE_BYTES + 1) as usize);
        let dir = make_temp_project(
            "oversized",
            &[("huge.txt", &huge_content), ("small.txt", "needle\n")],
        );
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let output = search_grep(&conn, &dir, "x{5}", None, false, 0, &[], 10).unwrap();
        assert!(
            output.results.iter().all(|r| r.path != "huge.txt"),
            "oversized file must be skipped entirely, not just truncated in output"
        );

        let output = search_grep(&conn, &dir, "needle", None, false, 0, &[], 10).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "small.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_grep_respects_ignore_patterns_and_gitignore() {
        // `ignore::WalkBuilder` only honors `.gitignore` when the walked
        // root looks like an actual git repo (`require_git` defaults to
        // true, matching git's own behavior) — a `.git` marker is required
        // for this fixture to exercise gitignore handling at all, same as
        // `crate::gitignore::ensure_gitignore`'s own `.git` existence check.
        let dir = make_temp_project(
            "ignore",
            &[
                (".git/HEAD", "ref: refs/heads/main\n"),
                ("keep.py", "needle\n"),
                ("vendor/skip.py", "needle\n"),
                (".gitignore", "skipped_by_git/\n"),
                ("skipped_by_git/also.py", "needle\n"),
            ],
        );
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let ignore = vec!["vendor".to_string()];
        let output = search_grep(&conn, &dir, "needle", None, false, 0, &ignore, 10).unwrap();
        let paths: Vec<&str> = output.results.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["keep.py"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_search_semantic_not_ready() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Semantic, 10, None, DEFAULT_RRF_K).unwrap();
        assert!(output.degraded);
        assert!(output.results.is_empty());
    }

    #[test]
    fn test_search_hybrid_degraded_to_fts() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Hybrid, 10, None, DEFAULT_RRF_K).unwrap();
        assert!(output.degraded);
        assert!(!output.results.is_empty());
    }

    #[test]
    fn test_rrf_merge_combines_results() {
        let fts = vec![
            SearchResult {
                name: "a".into(),
                qualified_name: "mod::a".into(),
                path: "a.py".into(),
                kind: None,
                line_start: None,
                line_end: None,
                score: 10.0,
                match_type: "exact".into(),
                snippet: None,
                is_test: false,
            },
            SearchResult {
                name: "b".into(),
                qualified_name: "mod::b".into(),
                path: "b.py".into(),
                kind: None,
                line_start: None,
                line_end: None,
                score: 5.0,
                match_type: "exact".into(),
                snippet: None,
                is_test: false,
            },
        ];
        let semantic = vec![SearchResult {
            name: "b".into(),
            qualified_name: "mod::b".into(),
            path: "b.py".into(),
            kind: None,
            line_start: None,
            line_end: None,
            score: 0.9,
            match_type: "semantic".into(),
            snippet: None,
            is_test: false,
        }];

        let merged = rrf_merge_n(
            &[(&fts, 1.5), (&semantic, 1.0)],
            10,
            DEFAULT_RRF_K,
            "hybrid",
        );
        assert_eq!(merged.len(), 2);
        // "b" appears in both lists so should have higher RRF score
        assert_eq!(merged[0].qualified_name, "mod::b");
    }

    fn stub_result(qn: &str, match_type: &str) -> SearchResult {
        SearchResult {
            name: qn.into(),
            qualified_name: qn.into(),
            path: "x.py".into(),
            kind: None,
            line_start: None,
            line_end: None,
            score: 0.0,
            match_type: match_type.into(),
            snippet: None,
            is_test: false,
        }
    }

    #[test]
    fn test_rrf_merge_n_three_way_fusion_outranks_single_source() {
        // "shared" appears in all three sources; "fts_only"/"sym_only"/"chunk_only"
        // each appear in exactly one. Flat 3-way RRF must rank "shared" first and
        // relabel every result with the merge's own match_type.
        let fts = vec![
            stub_result("shared", "exact"),
            stub_result("fts_only", "exact"),
        ];
        let sym = vec![
            stub_result("shared", "semantic"),
            stub_result("sym_only", "semantic"),
        ];
        let chunk = vec![
            stub_result("shared", "semantic_chunk"),
            stub_result("chunk_only", "semantic_chunk"),
        ];

        let merged = rrf_merge_n(
            &[(&fts, 1.5), (&sym, 1.0), (&chunk, 1.0)],
            10,
            DEFAULT_RRF_K,
            "hybrid",
        );

        assert_eq!(merged.len(), 4);
        assert_eq!(merged[0].qualified_name, "shared");
        assert!(merged.iter().all(|r| r.match_type == "hybrid"));
    }

    #[test]
    fn test_rrf_merge_n_respects_limit() {
        let fts = vec![
            stub_result("a", "exact"),
            stub_result("b", "exact"),
            stub_result("c", "exact"),
        ];
        let merged = rrf_merge_n(&[(&fts, 1.0)], 2, DEFAULT_RRF_K, "semantic");
        assert_eq!(merged.len(), 2);
    }

    /// `chunk_hit_to_result` and `rrf_merge_n` are pure DB/logic — no
    /// `embeddings` feature or real embedder needed to test them directly
    /// (only `knn_chunks` itself, which sits behind the vec0 extension, does).
    #[test]
    fn test_chunk_hit_to_result_with_symbol_qn_resolves_real_symbol() {
        let conn = setup_db_with_symbols();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash) \
             VALUES ('src/main.py', 11, 15, 'a = 1\nreturn a', 'src/main.py::get_user', 'h')",
            [],
        )
        .unwrap();
        let chunk_id: i64 = conn
            .query_row("SELECT id FROM code_chunks", [], |r| r.get(0))
            .unwrap();

        let r = chunk_hit_to_result(&conn, chunk_id).unwrap().unwrap();
        assert_eq!(r.qualified_name, "src/main.py::get_user");
        assert_eq!(r.name, "get_user");
        assert_eq!(r.kind.as_deref(), Some("function"));
        assert_eq!(r.path, "src/main.py");
        // The chunk's own window, not the whole symbol's line range.
        assert_eq!(r.line_start, Some(11));
        assert_eq!(r.line_end, Some(15));
        assert_eq!(r.match_type, "semantic_chunk");
    }

    #[test]
    fn test_chunk_hit_to_result_gap_chunk_falls_back_to_filename() {
        let conn = setup_db_with_symbols();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash) \
             VALUES ('src/main.py', 1, 2, 'import os', NULL, 'h')",
            [],
        )
        .unwrap();
        let chunk_id: i64 = conn
            .query_row("SELECT id FROM code_chunks", [], |r| r.get(0))
            .unwrap();

        let r = chunk_hit_to_result(&conn, chunk_id).unwrap().unwrap();
        assert_eq!(r.name, "main.py");
        assert!(r.kind.is_none());
        assert_eq!(r.line_start, Some(1));
        assert_eq!(r.line_end, Some(2));
        // Synthesized key must be unique per line range, not collide with a
        // real qualified_name.
        assert!(r.qualified_name.contains("#chunk:1-2"));
    }

    #[test]
    fn test_chunk_hit_to_result_missing_chunk_returns_none() {
        let conn = setup_db_with_symbols();
        assert!(chunk_hit_to_result(&conn, 999).unwrap().is_none());
    }

    /// A chunk's `symbol_qn` can go stale (the symbol was renamed/removed by a
    /// reindex that hasn't re-chunked yet) — must degrade to the synthesized
    /// key instead of erroring.
    #[test]
    fn test_chunk_hit_to_result_dangling_symbol_qn_falls_back() {
        let conn = setup_db_with_symbols();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash) \
             VALUES ('src/main.py', 1, 2, 'x', 'src/main.py::gone', 'h')",
            [],
        )
        .unwrap();
        let chunk_id: i64 = conn
            .query_row("SELECT id FROM code_chunks", [], |r| r.get(0))
            .unwrap();

        let r = chunk_hit_to_result(&conn, chunk_id).unwrap().unwrap();
        assert_eq!(r.qualified_name, "src/main.py::gone");
        assert_eq!(r.name, "src/main.py::gone");
        assert!(r.kind.is_none());
    }

    #[test]
    fn test_search_similar_no_chunk_returns_degraded() {
        let conn = setup_db_with_symbols();
        // No code_chunks at all for this path/line — same result whether the
        // embeddings feature is on (no chunk indexed there) or off (stub).
        let output = search_similar(&conn, "src/main.py", 12, 10).unwrap();
        assert!(output.degraded);
        assert!(output.results.is_empty());
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn search_similar_excludes_anchor_and_same_symbol_windows() {
        let conn = setup_db_with_symbols();
        crate::embedding::create_chunk_embedding_table(&conn, 3).unwrap();

        // get_user has two overlapping windows (its own sliding-window
        // chunks) — both near-identical to the anchor vector, but neither
        // should come back since they're the anchor's own symbol.
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/main.py', 10, 14, 'a', 'src/main.py::get_user', 'h')",
            [],
        )
        .unwrap();
        let anchor_id: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE line_start = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/main.py', 15, 20, 'b', 'src/main.py::get_user', 'h')",
            [],
        )
        .unwrap();
        let same_symbol_id: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE line_start = 15",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/main.py', 25, 40, 'c', 'src/main.py::update_user', 'h')",
            [],
        )
        .unwrap();
        let other_symbol_id: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE line_start = 25",
                [],
                |r| r.get(0),
            )
            .unwrap();

        crate::embedding::store_chunk_embedding(&conn, anchor_id, &[1.0, 0.0, 0.0]).unwrap();
        crate::embedding::store_chunk_embedding(&conn, same_symbol_id, &[0.99, 0.01, 0.0]).unwrap();
        crate::embedding::store_chunk_embedding(&conn, other_symbol_id, &[0.9, 0.1, 0.0]).unwrap();

        let output = search_similar(&conn, "src/main.py", 12, 10).unwrap();
        assert!(!output.degraded);
        assert_eq!(
            output.results.len(),
            1,
            "only the other symbol's chunk should survive"
        );
        assert_eq!(output.results[0].qualified_name, "src/main.py::update_user");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn search_similar_dedupes_by_symbol_keeping_best_scoring_chunk() {
        let conn = setup_db_with_symbols();
        crate::embedding::create_chunk_embedding_table(&conn, 3).unwrap();

        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/main.py', 10, 14, 'a', 'src/main.py::get_user', 'h')",
            [],
        )
        .unwrap();
        let anchor_id: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE line_start = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Two chunks for the SAME other symbol, at different similarity.
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/utils.py', 1, 7, 'a', 'src/utils.py::parse_config', 'h')",
            [],
        )
        .unwrap();
        let closer_id: i64 = conn
            .query_row("SELECT id FROM code_chunks WHERE line_start = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/utils.py', 8, 15, 'b', 'src/utils.py::parse_config', 'h')",
            [],
        )
        .unwrap();
        let farther_id: i64 = conn
            .query_row("SELECT id FROM code_chunks WHERE line_start = 8", [], |r| {
                r.get(0)
            })
            .unwrap();

        crate::embedding::store_chunk_embedding(&conn, anchor_id, &[1.0, 0.0, 0.0]).unwrap();
        crate::embedding::store_chunk_embedding(&conn, closer_id, &[0.95, 0.05, 0.0]).unwrap();
        crate::embedding::store_chunk_embedding(&conn, farther_id, &[0.8, 0.2, 0.0]).unwrap();

        let output = search_similar(&conn, "src/main.py", 12, 10).unwrap();
        assert_eq!(
            output.results.len(),
            1,
            "only the best-scoring chunk per symbol should survive"
        );
        assert_eq!(
            output.results[0].line_start,
            Some(1),
            "the closer window, not the farther one"
        );
    }

    #[test]
    fn search_similar_truncated_flag_is_accurate() {
        // Same off-by-one class as the search_text/search_file regressions:
        // the old `results.len() >= limit` broke the accumulation loop the
        // instant it reached `limit`, so it could never tell "exactly limit
        // real matches" apart from "more exist beyond the page".
        let conn = setup_db_with_symbols();
        crate::embedding::create_chunk_embedding_table(&conn, 3).unwrap();

        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
             VALUES ('src/main.py', 10, 14, 'a', 'src/main.py::get_user', 'h')",
            [],
        )
        .unwrap();
        let anchor_id: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE line_start = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        crate::embedding::store_chunk_embedding(&conn, anchor_id, &[1.0, 0.0, 0.0]).unwrap();

        // 3 distinct, unrelated gap chunks (no symbol_qn) — none excluded by
        // the anchor/same-symbol filters, each gets a distinct path:line
        // fallback qualified_name so dedup doesn't collapse them.
        let vectors: [[f32; 3]; 3] = [[0.9, 0.1, 0.0], [0.85, 0.15, 0.0], [0.8, 0.2, 0.0]];
        for (i, vec) in vectors.iter().enumerate() {
            let line = 20 + (i as i64) * 10;
            conn.execute(
                "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
                 VALUES ('src/utils.py', ?1, ?1, 'x', NULL, 'h')",
                rusqlite::params![line],
            )
            .unwrap();
            let id: i64 = conn
                .query_row(
                    "SELECT id FROM code_chunks WHERE line_start = ?1",
                    [line],
                    |r| r.get(0),
                )
                .unwrap();
            crate::embedding::store_chunk_embedding(&conn, id, vec).unwrap();
        }

        // limit == exact number of real (post-filter) candidates -> not truncated.
        let exact = search_similar(&conn, "src/main.py", 12, 3).unwrap();
        assert_eq!(exact.results.len(), 3);
        assert!(
            !exact.truncated,
            "exactly `limit` real matches with no more beyond it must not be reported as truncated"
        );

        // limit < number of real candidates -> truncated.
        let capped = search_similar(&conn, "src/main.py", 12, 2).unwrap();
        assert_eq!(capped.results.len(), 2);
        assert!(
            capped.truncated,
            "more real matches than `limit` must be reported as truncated"
        );
    }
    #[test]
    fn test_search_symbol_scores_positive() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "config", SearchKind::Symbol, 10, None, DEFAULT_RRF_K).unwrap();
        for r in &output.results {
            assert!(r.score > 0.0, "Scores should be positive, got {}", r.score);
        }
    }

    // -----------------------------------------------------------------
    // Noise-penalty ranking
    // -----------------------------------------------------------------

    #[test]
    fn test_is_noisy_path_detects_test_generated_example() {
        assert!(is_noisy_path("tests/test_auth.py"));
        assert!(is_noisy_path("src/auth_test.go"));
        assert!(is_noisy_path("src/auth.test.ts"));
        assert!(is_noisy_path("src/auth.spec.ts"));
        assert!(is_noisy_path("vendor/lib/foo.py"));
        assert!(is_noisy_path("dist/bundle.js"));
        assert!(is_noisy_path("examples/quickstart.py"));
        assert!(is_noisy_path("fixtures/sample.py"));
    }

    #[test]
    fn test_is_noisy_path_does_not_flag_real_implementation() {
        assert!(!is_noisy_path("src/auth/login.py"));
        assert!(!is_noisy_path("crates/calm-core/src/search.rs"));
        // "test" as a substring of an unrelated word must not false-positive.
        assert!(!is_noisy_path("src/latest/handler.py"));
        assert!(!is_noisy_path("src/protest/handler.py"));
    }

    #[test]
    fn test_noise_multiplier_values() {
        assert_eq!(noise_multiplier("tests/test_foo.py", false), NOISE_PENALTY);
        assert_eq!(noise_multiplier("src/foo.py", false), 1.0);
        assert_eq!(
            noise_multiplier("src/foo.py", true),
            NOISE_PENALTY,
            "is_test=true must penalize even a clean-looking path"
        );
    }

    fn stub_result_at(qn: &str, path: &str) -> SearchResult {
        SearchResult {
            name: qn.into(),
            qualified_name: qn.into(),
            path: path.into(),
            kind: None,
            line_start: None,
            line_end: None,
            score: 0.0,
            match_type: "exact".into(),
            snippet: None,
            is_test: false,
        }
    }

    /// Two results with otherwise-identical rank contributions: the one in a
    /// test file must rank behind the one in real implementation code.
    #[test]
    fn test_rrf_merge_n_demotes_noisy_path_on_tie() {
        let fts = [
            stub_result_at("real_impl", "src/auth/login.py"),
            stub_result_at("test_impl", "tests/test_login.py"),
        ];
        // Give both the SAME rank by putting each as the sole entry of its
        // own source list — without the penalty their RRF scores would tie
        // and HashMap iteration order would make the outcome nondeterministic.
        let merged = rrf_merge_n(
            &[(&fts[..1], 1.0), (&fts[1..], 1.0)],
            10,
            DEFAULT_RRF_K,
            "hybrid",
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].qualified_name, "real_impl",
            "non-test path must outrank an equally-ranked test path, got: {merged:?}"
        );
    }

    #[test]
    fn test_rrf_merge_n_ties_break_deterministically_by_qualified_name() {
        // Neither noise-penalized nor path-noisy, so these two genuinely
        // tie on RRF score (each is the sole rank-0 entry of its own source
        // list, both weight 1.0). Before switching rrf_merge_n's internal
        // maps from HashMap to BTreeMap, this tie's winner depended on
        // HashMap's per-process-random iteration order — i.e. it could flip
        // between separate `calm serve` launches with byte-identical input.
        // BTreeMap iterates by key, so the tie must now always resolve the
        // same way: alphabetically by qualified_name.
        let a = stub_result("aaa_symbol", "semantic");
        let z = stub_result("zzz_symbol", "semantic");
        let merged = rrf_merge_n(
            &[(&[a][..], 1.0), (&[z][..], 1.0)],
            10,
            DEFAULT_RRF_K,
            "semantic",
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].score, merged[1].score, "must be a genuine tie");
        assert_eq!(
            merged[0].qualified_name, "aaa_symbol",
            "tied results must order deterministically (alphabetically), got: {merged:?}"
        );
    }

    #[test]
    fn test_search_symbol_demotes_test_file_on_equal_relevance() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for (qn, path) in [
            ("real::widget", "src/widget.py"),
            ("test::widget", "tests/test_widget.py"),
        ] {
            conn.execute(
                "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens)
                 VALUES ('widget', ?1, 'function', ?2, 'python', 1, 5, '', 'widget')",
                rusqlite::params![qn, path],
            )
            .unwrap();
        }

        let output = search(&conn, "widget", SearchKind::Symbol, 10, None, DEFAULT_RRF_K).unwrap();
        assert_eq!(output.results.len(), 2);
        assert_eq!(
            output.results[0].qualified_name,
            "real::widget",
            "identical-relevance match in src/ must rank above the tests/ one, got: {:?}",
            output
                .results
                .iter()
                .map(|r| &r.qualified_name)
                .collect::<Vec<_>>()
        );
    }

    /// Real-world case a path check alone misses: Rust's `#[cfg(test)] mod
    /// tests` convention puts the test function in the *same file* as the
    /// implementation, so there's no separate tests/ path to flag — only
    /// `symbols.is_test` distinguishes them.
    #[test]
    fn test_search_symbol_demotes_inline_test_function_same_file() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for (qn, is_test) in [("rrf_merge_n", 0), ("test_rrf_merge_n_works", 1)] {
            conn.execute(
                "INSERT INTO symbols (name, qualified_name, kind, path, language, line_start, line_end, docstring, name_tokens, is_test)
                 VALUES ('rrf_merge_n', ?1, 'function', 'src/search.rs', 'rust', 1, 5, '', 'rrf merge n', ?2)",
                rusqlite::params![qn, is_test],
            )
            .unwrap();
        }

        let output = search(
            &conn,
            "rrf merge n",
            SearchKind::Symbol,
            10,
            None,
            DEFAULT_RRF_K,
        )
        .unwrap();
        assert_eq!(output.results.len(), 2);
        assert_eq!(
            output.results[0].qualified_name,
            "rrf_merge_n",
            "the real implementation must outrank its same-file test function, got: {:?}",
            output
                .results
                .iter()
                .map(|r| &r.qualified_name)
                .collect::<Vec<_>>()
        );
    }
}
