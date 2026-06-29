use std::collections::HashMap;

use rusqlite::Connection;

use crate::types::SearchKind;

const BM25_EXACT_WEIGHT: f64 = 1.5;
const BM25_TOKENS_WEIGHT: f64 = 1.0;
const RRF_K: f64 = 20.0;

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
    embeddings_ready: bool,
) -> rusqlite::Result<SearchOutput> {
    match kind {
        SearchKind::Symbol => search_symbol(conn, query, limit),
        SearchKind::Text => search_text(conn, query, limit),
        SearchKind::File => search_file(conn, query, limit),
        SearchKind::Semantic => search_semantic(conn, query, limit, embeddings_ready),
        SearchKind::Hybrid => search_hybrid(conn, query, limit, embeddings_ready),
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
                -bm25(fts_exact) AS score
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
                -bm25(fts_tokens) AS score
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
        })
    })?;

    for row in rows_tokens {
        let row = row?;
        *scores.entry(row.qualified_name.clone()).or_default() += row.score * BM25_TOKENS_WEIGHT;
        data.entry(row.qualified_name.clone())
            .or_insert_with(|| row.into_result("tokens"));
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
    let fts_query = escape_fts5_query(query);

    let mut stmt = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_exact) AS score
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

fn search_semantic(
    conn: &Connection,
    query: &str,
    limit: usize,
    embeddings_ready: bool,
) -> rusqlite::Result<SearchOutput> {
    if !embeddings_ready {
        return Ok(SearchOutput {
            results: Vec::new(),
            truncated: false,
            degraded: true,
            note: Some("Embeddings not ready — semantic search unavailable".to_string()),
        });
    }

    // Semantic search requires embedding the query and doing vec distance.
    // The embedding model integration is deferred — this returns the degraded
    // result until the embedder pipeline is wired in Phase 4+.
    let _ = (conn, query, limit);
    Ok(SearchOutput {
        results: Vec::new(),
        truncated: false,
        degraded: true,
        note: Some("Semantic search not yet wired — use symbol or hybrid".to_string()),
    })
}

fn search_hybrid(
    conn: &Connection,
    query: &str,
    limit: usize,
    embeddings_ready: bool,
) -> rusqlite::Result<SearchOutput> {
    let fts_output = search_symbol(conn, query, limit)?;

    if !embeddings_ready {
        return Ok(SearchOutput {
            degraded: true,
            note: Some("Embeddings not ready — hybrid degraded to FTS-only".to_string()),
            ..fts_output
        });
    }

    let semantic_output = search_semantic(conn, query, limit, embeddings_ready)?;

    if semantic_output.results.is_empty() {
        return Ok(SearchOutput {
            degraded: true,
            note: Some("Semantic results empty — hybrid degraded to FTS-only".to_string()),
            ..fts_output
        });
    }

    let merged = rrf_merge(&fts_output.results, &semantic_output.results, limit);
    let truncated = fts_output.truncated || semantic_output.truncated;

    Ok(SearchOutput {
        results: merged,
        truncated,
        degraded: false,
        note: None,
    })
}

fn rrf_merge(
    fts_results: &[SearchResult],
    semantic_results: &[SearchResult],
    limit: usize,
) -> Vec<SearchResult> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut data: HashMap<String, SearchResult> = HashMap::new();

    for (rank, r) in fts_results.iter().enumerate() {
        let rrf_score = 1.0 / (RRF_K + rank as f64 + 1.0);
        *scores.entry(r.qualified_name.clone()).or_default() += rrf_score;
        data.entry(r.qualified_name.clone())
            .or_insert_with(|| r.clone());
    }

    for (rank, r) in semantic_results.iter().enumerate() {
        let rrf_score = 1.0 / (RRF_K + rank as f64 + 1.0);
        *scores.entry(r.qualified_name.clone()).or_default() += rrf_score;
        data.entry(r.qualified_name.clone())
            .or_insert_with(|| r.clone());
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
            r.match_type = "hybrid".to_string();
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
        let output = search(&conn, "user", SearchKind::Symbol, 10, false).unwrap();
        assert!(
            output.results.len() >= 2,
            "Should find get_user and update_user, got: {:?}",
            output.results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_search_symbol_respects_limit() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Symbol, 1, false).unwrap();
        assert_eq!(output.results.len(), 1);
        assert!(output.truncated);
    }

    #[test]
    fn test_search_symbol_no_results() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "nonexistent_xyz", SearchKind::Symbol, 10, false).unwrap();
        assert!(output.results.is_empty());
        assert!(!output.truncated);
    }

    #[test]
    fn test_search_text() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "database", SearchKind::Text, 10, false).unwrap();
        assert!(
            output.results.len() >= 2,
            "Should find symbols with 'database' in docstring, got: {:?}",
            output.results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_search_file() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "main", SearchKind::File, 10, false).unwrap();
        assert_eq!(output.results.len(), 1);
        assert_eq!(output.results[0].path, "src/main.py");
    }

    #[test]
    fn test_search_file_multiple() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "src/", SearchKind::File, 10, false).unwrap();
        assert_eq!(output.results.len(), 3);
    }

    #[test]
    fn test_search_semantic_not_ready() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Semantic, 10, false).unwrap();
        assert!(output.degraded);
        assert!(output.results.is_empty());
    }

    #[test]
    fn test_search_hybrid_degraded_to_fts() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "user", SearchKind::Hybrid, 10, false).unwrap();
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
        }];

        let merged = rrf_merge(&fts, &semantic, 10);
        assert_eq!(merged.len(), 2);
        // "b" appears in both lists so should have higher RRF score
        assert_eq!(merged[0].qualified_name, "mod::b");
    }

    #[test]
    fn test_search_symbol_scores_positive() {
        let conn = setup_db_with_symbols();
        let output = search(&conn, "config", SearchKind::Symbol, 10, false).unwrap();
        for r in &output.results {
            assert!(r.score > 0.0, "Scores should be positive, got {}", r.score);
        }
    }
}
