use std::collections::HashMap;

use rusqlite::Connection;

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

    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut data: HashMap<String, SearchResult> = HashMap::new();

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
    // FTS5 global column filter: {docstring} restricts ALL tokens to docstring column only
    let fts_query = format!("{{docstring}} : {raw_query}");

    let mut stmt = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_exact) AS score, s.is_test
         FROM fts_exact
         JOIN symbols s ON s.id = fts_exact.rowid
         WHERE fts_exact MATCH ?1
         ORDER BY score DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![fts_query, limit as i64], |row| {
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

    let truncated = results.len() >= limit;
    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
    })
}

fn search_file(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<SearchOutput> {
    let pattern = format!("%{query}%");

    let mut stmt = conn.prepare(
        "SELECT fi.path
         FROM file_index fi
         WHERE fi.path LIKE ?1
         ORDER BY fi.path
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;

    let mut results = Vec::new();
    for row in rows {
        let path = row?;
        results.push(SearchResult {
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
        });
    }

    let truncated = results.len() >= limit;
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
    let hits = crate::embedding::knn(conn, qvec, limit)?;
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
    let hits = crate::embedding::knn_chunks(conn, qvec, limit)?;
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
            note: Some("Semantic search inactive — compile with `--features embeddings` and set `semantic_search.enabled: true` in config.json".to_string()),
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
    let truncated = sym_results.len() >= limit || chunk_results.len() >= limit;

    let results = match (sym_results.is_empty(), chunk_results.is_empty()) {
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

    Ok(SearchOutput {
        results,
        truncated,
        degraded: false,
        note: None,
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
            note: Some("Hybrid search degraded to FTS-only — semantic search inactive (compile with `--features embeddings` and set `semantic_search.enabled: true` in config.json)".to_string()),
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

    let truncated =
        fts_output.truncated || sym_results.len() >= limit || chunk_results.len() >= limit;

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
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut data: HashMap<String, SearchResult> = HashMap::new();

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
        assert!(!is_noisy_path("crates/ci-core/src/search.rs"));
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
