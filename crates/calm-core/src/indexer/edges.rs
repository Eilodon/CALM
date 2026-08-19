use super::chunker::CodeChunk;
use super::parser::ParsedSymbol;
use rusqlite::Transaction;

pub struct CallEdge {
    pub from_symbol: String,
    pub to_symbol: String,
    pub call_site_line: Option<i32>,
    /// `call_sites.id` for current byte-span identities. `None` is reserved
    /// for legacy rows that predate D4's exact call-site key.
    pub call_site_id: Option<i64>,
    pub edge_confidence: String,
    pub from_path: Option<String>,
    pub to_path: Option<String>,
    /// `"call"` (every non-SQL producer) or `"reference"` (SQL's
    /// `indexer::sql` module, for a view/proc's FROM/JOIN read of a table —
    /// see `call_edges.edge_kind`'s migration comment in `db::schema`).
    pub edge_kind: String,
    /// WS5 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D5):
    /// `0` = preferred (e.g. a same-directory match when other candidates
    /// exist outside the caller's directory), `1+` = alternate -- ordinal,
    /// not a score. `0` for every edge with no ranking signal (the
    /// overwhelming majority) -- see `call_edges.candidate_rank`'s own
    /// schema comment.
    pub candidate_rank: i64,
}

pub struct ImportEdge {
    pub from_path: String,
    pub to_path: Option<String>,
    pub module_name: String,
    pub symbols_used: String,
}

pub fn insert_symbols_batch(tx: &Transaction, symbols: &[ParsedSymbol]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, is_entry_point, class_context, is_test, cyclomatic_complexity, arity, arity_variadic)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
    )?;

    for sym in symbols {
        stmt.execute(rusqlite::params![
            sym.qualified_name,
            sym.name,
            sym.kind.as_str(),
            sym.language,
            sym.path,
            sym.line_start,
            sym.line_end,
            sym.signature,
            sym.docstring,
            sym.name_tokens,
            sym.is_entry_point as i32,
            sym.class_context,
            sym.is_test as i32,
            sym.complexity,
            sym.arity,
            sym.arity_variadic as i32
        ])?;
    }
    Ok(())
}

pub fn insert_call_edges_batch(tx: &Transaction, edges: &[CallEdge]) -> rusqlite::Result<()> {
    // OR IGNORE: the UNIQUE index on call_edges (see db::schema
    // dedup_edges_and_add_unique_indexes) makes every insert path idempotent,
    // so a redundant re-insert (e.g. the same edge extracted twice in one
    // pass) collapses to a no-op instead of a duplicate row.
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO call_edges (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind, candidate_rank)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
    )?;
    for e in edges {
        stmt.execute(rusqlite::params![
            e.from_symbol,
            e.to_symbol,
            e.call_site_line,
            e.call_site_id,
            e.edge_confidence,
            e.from_path,
            e.to_path,
            e.edge_kind,
            e.candidate_rank
        ])?;
    }
    Ok(())
}

pub fn insert_import_edges_batch(tx: &Transaction, edges: &[ImportEdge]) -> rusqlite::Result<()> {
    // OR IGNORE: byte-identical import rows collapse via the UNIQUE index
    // (see db::schema dedup_edges_and_add_unique_indexes).
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO import_edges (from_path, to_path, module_name, symbols_used)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for e in edges {
        stmt.execute(rusqlite::params![
            e.from_path,
            e.to_path,
            e.module_name,
            e.symbols_used
        ])?;
    }
    Ok(())
}

/// One resolved `extends`/`implements` fact, ready to persist into
/// `type_relations` -- see `db::schema`'s table comment and
/// `indexer::semantic_facts::RawTypeRelation` (the pre-resolution shape
/// this is built from in `pipeline::extract_file_data`).
pub struct TypeRelationData {
    pub from_symbol: String,
    pub relation_kind: &'static str,
    pub target_text: String,
    pub to_symbol: Option<String>,
    pub confidence: &'static str,
    // PR A (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md,
    // P4.1): Some("same_file_ast") when to_symbol was resolved here at
    // extraction time, None (persisted as SQL NULL) otherwise -- never
    // cross_file_unique, which only graph::type_resolve ever writes, via
    // its own UPDATE, not through this insert path.
    pub resolution_source: Option<&'static str>,
    pub source_path: String,
    pub line: i64,
}

/// One resolved `explicit_throw`/`write_field` fact, ready to persist into
/// `symbol_effects` -- see `db::schema`'s table comment and
/// `indexer::semantic_facts::RawEffect`.
pub struct SymbolEffectData {
    pub symbol_qn: String,
    pub effect_kind: &'static str,
    pub target_text: String,
    /// P3 (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md):
    /// "exact" | "none" -- see `RawEffect::target_confidence`, which this
    /// mirrors verbatim.
    pub target_confidence: &'static str,
    pub source_path: String,
    pub line: i64,
}

/// OR IGNORE: same idiom as `insert_import_edges_batch` above -- a
/// structurally-identical duplicate (same `UNIQUE(from_symbol,
/// relation_kind, target_text, line)`) collapses instead of aborting the
/// whole file's transaction.
pub fn insert_type_relations_batch(
    tx: &Transaction,
    relations: &[TypeRelationData],
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO type_relations (from_symbol, relation_kind, target_text, to_symbol, confidence, resolution_source, source_path, line)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for r in relations {
        stmt.execute(rusqlite::params![
            r.from_symbol,
            r.relation_kind,
            r.target_text,
            r.to_symbol,
            r.confidence,
            r.resolution_source,
            r.source_path,
            r.line,
        ])?;
    }
    Ok(())
}

/// Same OR IGNORE idiom as `insert_type_relations_batch` above.
pub fn insert_symbol_effects_batch(
    tx: &Transaction,
    effects: &[SymbolEffectData],
) -> rusqlite::Result<()> {
    // event_confidence is a literal 'exact' here, not threaded from
    // SymbolEffectData -- see schema.rs's symbol_effects comment: every
    // current extraction site fires only on a real syntactic raise/throw/
    // write node, so the EVENT is always certain in v1. Only
    // target_confidence varies per-row.
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO symbol_effects (symbol_qn, effect_kind, target_text, event_confidence, target_confidence, source_path, line)
         VALUES (?1, ?2, ?3, 'exact', ?4, ?5, ?6)",
    )?;
    for e in effects {
        stmt.execute(rusqlite::params![
            e.symbol_qn,
            e.effect_kind,
            e.target_text,
            e.target_confidence,
            e.source_path,
            e.line,
        ])?;
    }
    Ok(())
}

/// Persist one file's Layer-2 semantic-search code chunks (see
/// `indexer::chunker`). `path`/`file_hash` are shared by every row since a
/// file is always chunked and persisted as a unit.
pub fn insert_code_chunks_batch(
    tx: &Transaction,
    path: &str,
    file_hash: &str,
    chunks: &[CodeChunk],
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for c in chunks {
        stmt.execute(rusqlite::params![
            path,
            c.line_start as i64,
            c.line_end as i64,
            c.chunk_text,
            c.symbol_qn,
            file_hash
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::types::SymbolKind;
    use rusqlite::Connection;

    #[test]
    fn test_insert_symbols_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let tx = conn.transaction().unwrap();
        let symbols = vec![ParsedSymbol {
            qualified_name: "test.hello".to_string(),
            name: "hello".to_string(),
            kind: SymbolKind::Function,
            language: "python".to_string(),
            path: "test.py".to_string(),
            line_start: 1,
            line_end: 2,
            start_byte: 0,
            signature: "".to_string(),
            docstring: "".to_string(),
            name_tokens: "hello".to_string(),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
            arity: None,
            arity_variadic: false,
        }];
        insert_symbols_batch(&tx, &symbols).unwrap();
        tx.commit().unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_call_edges_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let tx = conn.transaction().unwrap();
        let edges = vec![CallEdge {
            from_symbol: "a".to_string(),
            to_symbol: "b".to_string(),
            call_site_line: Some(10),
            call_site_id: None,
            edge_confidence: "resolved".to_string(),
            from_path: Some("a.rs".to_string()),
            to_path: Some("b.rs".to_string()),
            edge_kind: "call".to_string(),
            candidate_rank: 0,
        }];
        insert_call_edges_batch(&tx, &edges).unwrap();
        tx.commit().unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Regression for W3: this batch helper existed but `index_one_file()`
    /// inserted import edges through its own separate inline statement,
    /// leaving `insert_import_edges_batch` entirely unused — a half-finished
    /// refactor. Now wired in; this is its first direct test coverage.
    #[test]
    fn test_insert_import_edges_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let tx = conn.transaction().unwrap();
        let edges = vec![ImportEdge {
            from_path: "a.py".to_string(),
            to_path: None,
            module_name: "os".to_string(),
            symbols_used: "[\"path\"]".to_string(),
        }];
        insert_import_edges_batch(&tx, &edges).unwrap();
        tx.commit().unwrap();

        let (from_path, module_name, symbols_used): (String, String, String) = conn
            .query_row(
                "SELECT from_path, module_name, symbols_used FROM import_edges",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(from_path, "a.py");
        assert_eq!(module_name, "os");
        assert_eq!(symbols_used, "[\"path\"]");
    }

    #[test]
    fn test_insert_code_chunks_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let tx = conn.transaction().unwrap();
        let chunks = vec![
            CodeChunk {
                line_start: 1,
                line_end: 2,
                chunk_text: "def f():\n    pass".to_string(),
                symbol_qn: Some("a.py::f".to_string()),
            },
            CodeChunk {
                line_start: 4,
                line_end: 4,
                chunk_text: "CONST = 1".to_string(),
                symbol_qn: None,
            },
        ];
        insert_code_chunks_batch(&tx, "a.py", "deadbeef", &chunks).unwrap();
        tx.commit().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_chunks WHERE path = 'a.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        let symbol_qn: Option<String> = conn
            .query_row(
                "SELECT symbol_qn FROM code_chunks WHERE line_start = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(symbol_qn.as_deref(), Some("a.py::f"));

        let gap_qn: Option<String> = conn
            .query_row(
                "SELECT symbol_qn FROM code_chunks WHERE line_start = 4",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gap_qn, None);
    }
}
