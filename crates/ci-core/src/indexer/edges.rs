use rusqlite::Transaction;
use super::parser::ParsedSymbol;

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
        "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, is_entry_point)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
    )?;
    
    for sym in symbols {
        stmt.execute(rusqlite::params![
            sym.qualified_name, sym.name, sym.kind.as_str(), sym.language, sym.path,
            sym.line_start, sym.line_end, sym.signature, sym.docstring, sym.name_tokens, sym.is_entry_point as i32
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
            e.from_symbol, e.to_symbol, e.call_site_line, e.edge_confidence, e.from_path, e.to_path
        ])?;
    }
    Ok(())
}

pub fn insert_import_edges_batch(tx: &Transaction, edges: &[ImportEdge]) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO import_edges (from_path, to_path, module_name, symbols_used)
         VALUES (?1, ?2, ?3, ?4)"
    )?;
    for e in edges {
        stmt.execute(rusqlite::params![
            e.from_path, e.to_path, e.module_name, e.symbols_used
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::db::schema::init_db;
    use crate::types::SymbolKind;

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
        }];
        insert_symbols_batch(&tx, &symbols).unwrap();
        tx.commit().unwrap();
        
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0)).unwrap();
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
        
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }
}
