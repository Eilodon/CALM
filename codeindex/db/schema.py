from __future__ import annotations

import sqlite3
import logging

logger = logging.getLogger("codeindex")

_SCHEMA_SQL = """
-- symbols: source of truth for all tools
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

-- call_edges: call graph from ConservativeResolver
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

-- import_edges: file-level dependency graph
CREATE TABLE IF NOT EXISTS import_edges (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_path     TEXT NOT NULL,
    to_path       TEXT,
    module_name   TEXT NOT NULL,
    symbols_used  TEXT DEFAULT '[]'
);

CREATE INDEX IF NOT EXISTS idx_import_from ON import_edges(from_path);
CREATE INDEX IF NOT EXISTS idx_import_to   ON import_edges(to_path);

-- file_index: incremental indexing state
CREATE TABLE IF NOT EXISTS file_index (
    path          TEXT PRIMARY KEY,
    hash          TEXT NOT NULL,
    language      TEXT,
    symbol_count  INTEGER NOT NULL DEFAULT 0,
    last_indexed  REAL NOT NULL,
    mtime         REAL
);
"""

_FTS5_SQL = """
-- fts_exact: search on original name + docstring
CREATE VIRTUAL TABLE IF NOT EXISTS fts_exact USING fts5(
    name,
    docstring,
    content='symbols',
    content_rowid='id',
    tokenize='unicode61'
);

-- fts_tokens: search on tokenized name (camelCase/snake_case split)
CREATE VIRTUAL TABLE IF NOT EXISTS fts_tokens USING fts5(
    name_tokens,
    content='symbols',
    content_rowid='id',
    tokenize='unicode61'
);
"""

_TRIGGERS_SQL = """
-- INSERT trigger
CREATE TRIGGER IF NOT EXISTS symbols_ai AFTER INSERT ON symbols BEGIN
    INSERT INTO fts_exact(rowid, name, docstring)
        VALUES (new.id, new.name, new.docstring);
    INSERT INTO fts_tokens(rowid, name_tokens)
        VALUES (new.id, new.name_tokens);
END;

-- DELETE trigger
CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO fts_exact(fts_exact, rowid, name, docstring)
        VALUES ('delete', old.id, old.name, old.docstring);
    INSERT INTO fts_tokens(fts_tokens, rowid, name_tokens)
        VALUES ('delete', old.id, old.name_tokens);
END;

-- UPDATE trigger (name, docstring, or name_tokens changed)
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
"""


def init_db(conn: sqlite3.Connection) -> None:
    """Create all tables, FTS5 virtual tables, triggers, and run migrations.

    Idempotent — safe to call on every startup.
    """
    conn.execute("PRAGMA journal_mode=WAL")
    conn.executescript(_SCHEMA_SQL)
    conn.executescript(_FTS5_SQL)
    conn.executescript(_TRIGGERS_SQL)
    _run_migrations(conn)
    logger.info("Database schema initialized")


def _run_migrations(conn: sqlite3.Connection) -> None:
    _migrate_add_column(conn, "symbols", "name_tokens", "TEXT NOT NULL DEFAULT ''")
    _migrate_add_column(conn, "symbols", "is_entry_point", "INTEGER NOT NULL DEFAULT 0")
    _migrate_add_column(conn, "symbols", "coreness", "INTEGER")
    _migrate_add_column(conn, "file_index", "mtime", "REAL")
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_call_edges_to ON call_edges(to_symbol)"
    )
    conn.commit()


def _migrate_add_column(
    conn: sqlite3.Connection, table: str, column: str, col_type: str
) -> None:
    existing = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
    if column not in existing:
        conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {col_type}")
        logger.info("Migration: added %s.%s", table, column)


def create_embedding_table(conn: sqlite3.Connection) -> None:
    """Create sqlite-vec virtual table. Requires sqlite_vec.load(conn) first."""
    conn.execute("""
        CREATE VIRTUAL TABLE IF NOT EXISTS embedding_vecs USING vec0(
            symbol_id INTEGER,
            embedding FLOAT[768]
        )
    """)
    conn.commit()
