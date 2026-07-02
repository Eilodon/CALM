use super::chunker::CodeChunk;
use super::parser::ParsedSymbol;
use rusqlite::Transaction;

pub struct CallEdge {
    pub from_symbol: String,
    pub to_symbol: String,
    pub call_site_line: Option<i32>,
    pub edge_confidence: String,
    pub from_path: Option<String>,
    pub to_path: Option<String>,
}

pub struct ImportEdge {
    pub from_path: String,
    pub to_path: Option<String>,
    pub module_name: String,
    pub symbols_used: String,
}

pub fn insert_symbols_batch(tx: &Transaction, symbols: &[ParsedSymbol]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, is_entry_point, class_context, is_test, cyclomatic_complexity)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
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
            sym.complexity
        ])?;
    }
    Ok(())
}

pub fn insert_call_edges_batch(tx: &Transaction, edges: &[CallEdge]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
    )?;
    for e in edges {
        stmt.execute(rusqlite::params![
            e.from_symbol,
            e.to_symbol,
            e.call_site_line,
            e.edge_confidence,
            e.from_path,
            e.to_path
        ])?;
    }
    Ok(())
}

pub fn insert_import_edges_batch(tx: &Transaction, edges: &[ImportEdge]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO import_edges (from_path, to_path, module_name, symbols_used)
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
            signature: "".to_string(),
            docstring: "".to_string(),
            name_tokens: "hello".to_string(),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
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
            edge_confidence: "resolved".to_string(),
            from_path: Some("a.rs".to_string()),
            to_path: Some("b.rs".to_string()),
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
