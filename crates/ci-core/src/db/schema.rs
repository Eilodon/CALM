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
    indexed_at      REAL NOT NULL DEFAULT 0,
    class_context   TEXT,
    is_test         INTEGER NOT NULL DEFAULT 0,
    cyclomatic_complexity INTEGER NOT NULL DEFAULT 1
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

CREATE TABLE IF NOT EXISTS symbol_metrics_history (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    qualified_name  TEXT NOT NULL,
    snapshot_at     TEXT NOT NULL,
    caller_count    INTEGER NOT NULL DEFAULT 0,
    callee_count    INTEGER NOT NULL DEFAULT 0,
    coreness        INTEGER NOT NULL DEFAULT 0,
    is_hub          INTEGER NOT NULL DEFAULT 0,
    churn_count     INTEGER NOT NULL DEFAULT 0,
    complexity      REAL,
    UNIQUE(qualified_name, snapshot_at)
);
CREATE INDEX IF NOT EXISTS idx_smh_symbol ON symbol_metrics_history(qualified_name);
CREATE INDEX IF NOT EXISTS idx_smh_time   ON symbol_metrics_history(snapshot_at);

CREATE TABLE IF NOT EXISTS call_sites (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    from_path    TEXT NOT NULL,
    enclosing_qn TEXT NOT NULL,
    callee_name  TEXT NOT NULL,
    call_line    INTEGER,
    confidence   TEXT NOT NULL DEFAULT 'textual',
    receiver     TEXT,
    target_class TEXT
);
CREATE INDEX IF NOT EXISTS idx_call_sites_from   ON call_sites(from_path);
CREATE INDEX IF NOT EXISTS idx_call_sites_callee ON call_sites(callee_name);

-- Semantic search Layer 2: raw code-body slices (whole short bodies, or a
-- sliding window over longer ones — see indexer::chunker), embedded alongside
-- Layer 1's symbol-identity (name+signature+docstring) vectors so a query
-- matching only implementation vocabulary (e.g. a library name used inside a
-- function body) still has something to match against. Always created —
-- populated only when the `embeddings` feature is enabled at build time; the
-- companion `code_chunk_vecs` vec0 table lives in embedding.rs (needs the
-- sqlite-vec extension registered and a runtime-configured dimension, so it
-- can't be part of this static schema).
CREATE TABLE IF NOT EXISTS code_chunks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    path       TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end   INTEGER NOT NULL,
    chunk_text TEXT NOT NULL,
    symbol_qn  TEXT,
    file_hash  TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_code_chunks_path ON code_chunks(path);

-- Durable, agent-written interpretive notes (architecture decisions, gotchas,
-- rationale) — distinct from anything derived from the AST/call-graph, and
-- distinct from `session_context`'s per-session navigational state (which
-- resets every server restart). One row per `topic`; `remember` upserts.
CREATE TABLE IF NOT EXISTS project_memory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic       TEXT NOT NULL UNIQUE,
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_project_memory_topic ON project_memory(topic);
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
    migrate_add_column(conn, "symbols", "class_context", "TEXT")?;
    migrate_add_column(conn, "symbols", "is_test", "INTEGER NOT NULL DEFAULT 0")?;
    migrate_add_column(
        conn,
        "symbols",
        "cyclomatic_complexity",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    migrate_add_column(conn, "file_index", "mtime", "REAL")?;
    // call_sites columns added after the table first shipped.
    migrate_add_column(
        conn,
        "call_sites",
        "confidence",
        "TEXT NOT NULL DEFAULT 'textual'",
    )?;
    migrate_add_column(conn, "call_sites", "receiver", "TEXT")?;
    migrate_add_column(conn, "call_sites", "target_class", "TEXT")?;
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
    fn test_code_chunks_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='code_chunks'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);

        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, symbol_qn, file_hash) \
             VALUES ('a.py', 1, 3, 'def f():\n    pass', 'a.py::f', 'deadbeef')",
            [],
        )
        .unwrap();

        let (path, symbol_qn): (String, Option<String>) = conn
            .query_row(
                "SELECT path, symbol_qn FROM code_chunks WHERE line_start = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, "a.py");
        assert_eq!(symbol_qn.as_deref(), Some("a.py::f"));

        // symbol_qn is nullable — gap chunks have no enclosing symbol.
        conn.execute(
            "INSERT INTO code_chunks (path, line_start, line_end, chunk_text, file_hash) \
             VALUES ('a.py', 4, 4, '', 'deadbeef')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn test_symbol_metrics_history_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='symbol_metrics_history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);

        conn.execute(
            "INSERT INTO symbol_metrics_history (qualified_name, snapshot_at, caller_count) \
             VALUES ('mod.foo', '2026-01-01T00:00:00Z', 3)",
            [],
        )
        .unwrap();

        let caller_count: i64 = conn
            .query_row(
                "SELECT caller_count FROM symbol_metrics_history WHERE qualified_name = 'mod.foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(caller_count, 3);

        // UNIQUE constraint: same (qualified_name, snapshot_at) must fail
        let dup = conn.execute(
            "INSERT INTO symbol_metrics_history (qualified_name, snapshot_at, caller_count) \
             VALUES ('mod.foo', '2026-01-01T00:00:00Z', 5)",
            [],
        );
        assert!(dup.is_err());

        // Different snapshot_at must succeed
        conn.execute(
            "INSERT INTO symbol_metrics_history (qualified_name, snapshot_at, caller_count) \
             VALUES ('mod.foo', '2026-01-02T00:00:00Z', 5)",
            [],
        )
        .unwrap();
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
