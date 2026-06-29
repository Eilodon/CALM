use rusqlite::Connection;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS symbols (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    qualified_name  TEXT NOT NULL,
    name            TEXT NOT NULL,
    kind            TEXT NOT NULL,
    language        TEXT NOT NULL,
    path            TEXT NOT NULL,
    line_start      INTEGER NOT NULL,
    line_end        INTEGER NOT NULL,
    signature       TEXT NOT NULL DEFAULT '',
    docstring       TEXT NOT NULL DEFAULT '',
    name_tokens     TEXT NOT NULL DEFAULT '',
    caller_count    INTEGER NOT NULL DEFAULT 0,
    is_hub          INTEGER NOT NULL DEFAULT 0,
    coreness        INTEGER,
    is_entry_point  INTEGER NOT NULL DEFAULT 0,
    file_hash       TEXT NOT NULL DEFAULT '',
    indexed_at      REAL NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_symbols_qualified ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS idx_symbols_path     ON symbols(path);
CREATE INDEX IF NOT EXISTS idx_symbols_name     ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_hub      ON symbols(is_hub) WHERE is_hub = 1;

CREATE TABLE IF NOT EXISTS call_edges (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    from_symbol     TEXT NOT NULL,
    to_symbol       TEXT NOT NULL,
    call_site_line  INTEGER,
    edge_confidence TEXT NOT NULL DEFAULT 'textual',
    from_path       TEXT,
    to_path         TEXT
);

CREATE INDEX IF NOT EXISTS idx_call_edges_from  ON call_edges(from_symbol);
CREATE INDEX IF NOT EXISTS idx_call_edges_to    ON call_edges(to_symbol);
CREATE INDEX IF NOT EXISTS idx_call_edges_fpath ON call_edges(from_path);

CREATE TABLE IF NOT EXISTS import_edges (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_path     TEXT NOT NULL,
    to_path       TEXT,
    module_name   TEXT NOT NULL,
    symbols_used  TEXT DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_import_from ON import_edges(from_path);
CREATE INDEX IF NOT EXISTS idx_import_to   ON import_edges(to_path);

CREATE TABLE IF NOT EXISTS file_index (
    path          TEXT PRIMARY KEY,
    hash          TEXT NOT NULL,
    language      TEXT,
    symbol_count  INTEGER NOT NULL DEFAULT 0,
    last_indexed  REAL NOT NULL,
    mtime         REAL
);
";

const FTS5_SQL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS fts_exact USING fts5(
    name,
    docstring,
    content='symbols',
    content_rowid='id',
    tokenize='unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS fts_tokens USING fts5(
    name_tokens,
    content='symbols',
    content_rowid='id',
    tokenize='unicode61'
);
";

const TRIGGERS_SQL: &str = "
CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO fts_exact(rowid, name, docstring)
        VALUES (new.id, new.name, new.docstring);
    INSERT INTO fts_tokens(rowid, name_tokens)
        VALUES (new.id, new.name_tokens);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO fts_exact(fts_exact, rowid, name, docstring)
        VALUES ('delete', old.id, old.name, old.docstring);
    INSERT INTO fts_tokens(fts_tokens, rowid, name_tokens)
        VALUES ('delete', old.id, old.name_tokens);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au
    AFTER UPDATE OF name, docstring, name_tokens ON symbols BEGIN
    INSERT INTO fts_exact(fts_exact, rowid, name, docstring)
        VALUES ('delete', old.id, old.name, old.docstring);
    INSERT INTO fts_exact(rowid, name, docstring)
        VALUES (new.id, new.name, new.docstring);
    INSERT INTO fts_tokens(fts_tokens, rowid, name_tokens)
        VALUES ('delete', old.id, old.name_tokens);
    INSERT INTO fts_tokens(rowid, name_tokens)
        VALUES (new.id, new.name_tokens);
END;
";

pub fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(FTS5_SQL)?;
    conn.execute_batch(TRIGGERS_SQL)?;
    run_migrations(conn)?;
    tracing::info!("Database schema initialized");
    Ok(())
}

fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    migrate_add_column(conn, "symbols", "name_tokens", "TEXT NOT NULL DEFAULT ''")?;
    migrate_add_column(
        conn,
        "symbols",
        "is_entry_point",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_add_column(conn, "symbols", "coreness", "INTEGER")?;
    migrate_add_column(conn, "file_index", "mtime", "REAL")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_call_edges_to ON call_edges(to_symbol);")?;
    Ok(())
}

fn migrate_add_column(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();

    if !existing.iter().any(|c| c == column) {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {col_type};"
        ))?;
        tracing::info!("Migration: added {table}.{column}");
    }
    Ok(())
}

pub fn create_embedding_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS embedding_vecs USING vec0(
            symbol_id INTEGER,
            embedding FLOAT[768]
        );",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_fts5_trigger_sync() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, name_tokens, indexed_at) \
             VALUES ('mod.hello', 'hello', 'function', 'python', 'mod.py', 1, 5, \
             'hello', 0.0)",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_exact WHERE fts_exact MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        conn.execute("DELETE FROM symbols WHERE qualified_name = 'mod.hello'", [])
            .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_exact WHERE fts_exact MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
