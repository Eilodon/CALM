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
    call_site_id    INTEGER REFERENCES call_sites(id) ON DELETE CASCADE,
    edge_confidence TEXT NOT NULL DEFAULT 'textual',
    evidence_state  TEXT NOT NULL DEFAULT 'unverified',
    from_path       TEXT,
    to_path         TEXT,
    edge_kind       TEXT NOT NULL DEFAULT 'call'
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
    callee_start_byte INTEGER,
    callee_end_byte   INTEGER,
    identity_version  INTEGER NOT NULL DEFAULT 1,
    confidence   TEXT NOT NULL DEFAULT 'textual',
    receiver     TEXT,
    target_class TEXT,
    looks_option_or_result_chained INTEGER NOT NULL DEFAULT 0,
    module_hint  TEXT,
    edge_kind    TEXT NOT NULL DEFAULT 'call',
    arg_count    INTEGER
);
CREATE INDEX IF NOT EXISTS idx_call_sites_from   ON call_sites(from_path);
CREATE INDEX IF NOT EXISTS idx_call_sites_callee ON call_sites(callee_name);

-- D4 durable external evidence. Proof is anchored to a stable CallSite plus
-- target, never a mutable call_edges row. Reindexing deletes/recreates a
-- CallSite, so CASCADE makes stale proof structurally impossible to reattach.
CREATE TABLE IF NOT EXISTS external_proofs (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    call_site_id         INTEGER NOT NULL REFERENCES call_sites(id) ON DELETE CASCADE,
    to_symbol            TEXT NOT NULL,
    provider             TEXT NOT NULL,
    source_file_hash     TEXT NOT NULL,
    callee_start_byte    INTEGER NOT NULL,
    callee_end_byte      INTEGER NOT NULL,
    provider_fingerprint TEXT NOT NULL,
    context_fingerprint  TEXT NOT NULL,
    graph_generation     INTEGER NOT NULL DEFAULT 0,
    call_site_identity_version INTEGER NOT NULL DEFAULT 1,
    definition_snapshot  TEXT,
    status               TEXT NOT NULL CHECK (status IN ('fresh', 'stale', 'legacy', 'unverified', 'rejected')),
    observed_at          REAL NOT NULL,
    failure_reason       TEXT,
    UNIQUE(call_site_id, to_symbol, provider)
);
CREATE INDEX IF NOT EXISTS idx_external_proofs_status ON external_proofs(status, provider);

-- D4 migration observability. This is diagnostic-only and deliberately lives
-- outside the graph baseline transaction: readers still see an all-old or
-- all-new graph, while operators can tell whether an identity rebuild is
-- pending, running, ready, or failed.
CREATE TABLE IF NOT EXISTS identity_migration_state (
    id             INTEGER PRIMARY KEY CHECK (id = 1),
    target_version INTEGER NOT NULL,
    status         TEXT NOT NULL CHECK (status IN ('pending', 'running', 'baseline_ready', 'failed')),
    started_at     REAL,
    completed_at   REAL,
    failed_at      REAL,
    failure_reason  TEXT,
    duration_ms     INTEGER,
    rows_rebuilt    INTEGER,
    busy_retries    INTEGER NOT NULL DEFAULT 0,
    graph_generation INTEGER
);

-- Monotonic identity of the committed call graph. External semantic proofs
-- record this generation and must never be applied to a newer graph baseline.
CREATE TABLE IF NOT EXISTS graph_generation_state (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    generation INTEGER NOT NULL
);
INSERT OR IGNORE INTO graph_generation_state(id, generation) VALUES (1, 0);

-- Durable contract for non-source inputs that can change how identical source
-- bytes are extracted or resolved. A missing or incompatible row is never
-- trusted: startup/reconciliation falls back to a full baseline once.
CREATE TABLE IF NOT EXISTS index_input_state (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    policy_version      INTEGER NOT NULL,
    config_fingerprint  TEXT NOT NULL,
    context_fingerprint TEXT NOT NULL,
    recorded_at         REAL NOT NULL
);

-- Semantic search Layer 2: raw code-body slices (whole short bodies, or a
-- sliding window over longer ones — see indexer::chunker), embedded alongside
-- Layer 1's symbol-identity (name+signature+docstring) vectors so a query
-- matching only implementation vocabulary (e.g. a library name used inside a
-- function body) still has something to match against. Always created —
-- populated only when the `embeddings` feature is enabled at build time; the
-- companion `code_chunk_vecs` table lives in embedding.rs (plain BLOB
-- storage, created once the runtime-configured dimension is known, so it
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

-- File-path references extracted from a `project_memory.content` note at
-- `remember` time, each paired with that file's content hash *then* — lets
-- `recall` detect a note that's gone stale (the file it discusses has since
-- changed, or disappeared) without any NLP, just a hash re-check against the
-- live file. One row per (topic, ref_path); `remember` replaces the full set
-- for a topic on every call, mirroring how it replaces `content` itself.
CREATE TABLE IF NOT EXISTS project_memory_refs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic       TEXT NOT NULL,
    ref_path    TEXT NOT NULL,
    ref_hash    TEXT NOT NULL,
    UNIQUE(topic, ref_path)
);
CREATE INDEX IF NOT EXISTS idx_project_memory_refs_topic ON project_memory_refs(topic);
-- Powers `notes_for_path`'s ambient-injection lookup (docs/superskills/specs/
-- 2026-07-11-superskills-inspired-features.md #3 v2, edit_context/locate's
-- `related_notes`) -- a reverse lookup by file path, the mirror image of
-- the topic index above.
CREATE INDEX IF NOT EXISTS idx_project_memory_refs_path ON project_memory_refs(ref_path);
-- Pattern-debt tracker (docs/superskills/specs/2026-07-11-superskills-inspired-features.md
-- #1, revised post-audit): a registered duplicate-code-pattern anchor, keyed
-- by a stable `anchor_qualified_name` (NOT path+line -- a symbol's lines
-- shift on every unrelated edit elsewhere in the file, but its qualified
-- name survives until the symbol itself is renamed/removed/split, at which
-- point `pattern_debt_status` reports `anchor_lost` explicitly instead of a
-- false `resolved`). Deliberately a dedicated table, not folded into
-- `project_memory`: its structured fields (baseline_count, status) would
-- otherwise pollute `project_memory_fts`'s full-text index used by the
-- unrelated `recall` tool.
CREATE TABLE IF NOT EXISTS pattern_debt (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    topic                  TEXT NOT NULL UNIQUE,
    anchor_qualified_name  TEXT NOT NULL,
    note                   TEXT NOT NULL,
    baseline_count         INTEGER NOT NULL,
    status                 TEXT NOT NULL DEFAULT 'open',
    created_at             TEXT NOT NULL,
    last_checked_at        TEXT,
    last_checked_count     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_pattern_debt_topic ON pattern_debt(topic);";

const FTS5_SQL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS fts_exact USING fts5(
    name,
    docstring,
    signature,
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
    INSERT INTO fts_exact(rowid, name, docstring, signature)
        VALUES (new.id, new.name, new.docstring, new.signature);
    INSERT INTO fts_tokens(rowid, name_tokens)
        VALUES (new.id, new.name_tokens);
END;

CREATE TRIGGER IF NOT EXISTS symbols_ad AFTER DELETE ON symbols BEGIN
    INSERT INTO fts_exact(fts_exact, rowid, name, docstring, signature)
        VALUES ('delete', old.id, old.name, old.docstring, old.signature);
    INSERT INTO fts_tokens(fts_tokens, rowid, name_tokens)
        VALUES ('delete', old.id, old.name_tokens);
END;

CREATE TRIGGER IF NOT EXISTS symbols_au
    AFTER UPDATE OF name, docstring, signature, name_tokens ON symbols BEGIN
    INSERT INTO fts_exact(fts_exact, rowid, name, docstring, signature)
        VALUES ('delete', old.id, old.name, old.docstring, old.signature);
    INSERT INTO fts_exact(rowid, name, docstring, signature)
        VALUES (new.id, new.name, new.docstring, new.signature);
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
    migrate_add_column(
        conn,
        "call_sites",
        "looks_option_or_result_chained",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // See parser::module_hint_of — the module-path segment of a
    // lowercase-qualified `::`-call (`crate::telemetry::timed_tool`), used to
    // disambiguate same-named candidates by file when there's no `use`.
    migrate_add_column(conn, "call_sites", "module_hint", "TEXT")?;
    // B3-Elixir arity gate (Tier B audit): argument count at the call site,
    // when the grammar exposes an "arguments"-kind child directly on the
    // call node (see `parser::count_arguments_node`) -- NULL when it
    // doesn't (every language this isn't wired for yet), never a guessed 0.
    migrate_add_column(conn, "call_sites", "arg_count", "INTEGER")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_call_edges_to ON call_edges(to_symbol);")?;
    // Set by the SCIP overlay (`calm_core::scip::ingest`) when a reference at a
    // given call site is proven — via real type-checked evidence — to NOT be
    // this edge's `to_symbol`: either another candidate in the same
    // ambiguous fan-out group got upgraded to `formal`, or SCIP resolved the
    // site to something outside the fan-out set entirely (e.g. a stdlib
    // method). `edge_confidence` itself is left untouched (still 'ambiguous')
    // — this is an orthogonal, additive annotation, not a downgrade of an
    // existing rank, so it doesn't conflict with ADR-0004 §3's
    // never-downgrade invariant. Query-side (`callers`/`callees`/
    // `edit_context`) filters `ruled_out_by_scip = 0` to keep proven-wrong
    // fan-out siblings out of the `ambiguous` bucket shown to the agent.
    migrate_add_column(
        conn,
        "call_edges",
        "ruled_out_by_scip",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Provenance of a `formal`-confidence edge, set by whichever pass upgraded
    // it: `'scip'` (exact byte span plus source-hash match — see
    // `scip::ingest`), `'lsp'` (an exact current CallSite verified by an
    // on-demand LSP request), or `'stack_graphs'` (per-file name-set match —
    // see `indexer::pipeline::extract_file_data`). NULL for every other
    // confidence tier, and for pre-migration `formal` rows this column cannot
    // retroactively attribute (harmless: `ingest_occurrences` treats NULL the
    // same as `'stack_graphs'` — weaker evidence SCIP is allowed to confirm
    // or override — never the same as `'scip'`, which it never re-touches).
    // SCIP is deliberately allowed to override a `'stack_graphs'`-sourced
    // `formal` edge (re-target an ambiguous group when the two disagree) —
    // exact type-checked evidence beats a per-file name-set heuristic — but
    // never re-litigates its own prior `'scip'` verdict.
    migrate_add_column(conn, "call_edges", "formal_source", "TEXT")?;
    // D4 keeps evidence freshness distinct from confidence/provenance. Existing
    // SCIP/LSP rows predate a durable exact CallSite proof record, so migration
    // marks them legacy rather than presenting a line-derived verdict as fresh.
    migrate_add_column(
        conn,
        "call_edges",
        "evidence_state",
        "TEXT NOT NULL DEFAULT 'unverified'",
    )?;
    conn.execute(
        "UPDATE call_edges SET evidence_state = 'legacy'
         WHERE evidence_state = 'unverified'
           AND formal_source IN ('scip', 'lsp')",
        [],
    )?;
    // SQL indexer (8-language plan P3.3): distinguishes a genuine call
    // (proc/trigger → proc via CALL/EXEC) from a mere read reference
    // (view/proc → table via FROM/JOIN), threaded from `call_sites` straight
    // through to `call_edges` in `rebuild_graph` — so `callers`/`callees`
    // never present a table read as if it were a function call. Every other
    // language's extractor only ever produces genuine calls, so this
    // defaults to `'call'` everywhere except what `indexer::sql` explicitly
    // marks `'reference'`.
    migrate_add_column(
        conn,
        "call_sites",
        "edge_kind",
        "TEXT NOT NULL DEFAULT 'call'",
    )?;
    migrate_add_column(
        conn,
        "call_edges",
        "edge_kind",
        "TEXT NOT NULL DEFAULT 'call'",
    )?;
    // Plan 3 §3.3 (F10): degree-hub vs bridge-hub classification, written by
    // `graph::hub::update_is_hub_flags` as `'degree' | 'bridge' | 'both'` (or
    // left NULL for a non-hub symbol) — lets the edit gate treat a bridge-
    // only touch less strictly than a degree hub without a second query.
    migrate_add_column(conn, "symbols", "hub_kind", "TEXT")?;
    // Tier-1 agent-experience upgrade: flags a symbol whose line_start or
    // line_end shares a physical source line with an adjacent symbol —
    // written by `graph::boundary::update_boundary_ambiguous_flags`,
    // called from `rebuild_graph` right after `update_is_hub_flags` so it
    // gets the exact same per-reindex invalidation guarantee already
    // trusted for `hub_kind` (see docs/superskills/specs/2026-07-13-calm-
    // agent-experience-upgrade.md Risk Assessment, Failure Mode 1).
    migrate_add_column(
        conn,
        "symbols",
        "boundary_ambiguous",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Plan 3 §3.5(d): HMAC-SHA256(topic, content) over `memory::compute_mac`,
    // written by `remember`. Nullable, not backfilled — a pre-existing note
    // has no MAC to check, and `memory::verify_integrity` reports that case
    // as `"unverified"`, distinct from `"ok"`/`"mismatch"`.
    migrate_add_column(conn, "project_memory", "content_mac", "TEXT")?;
    // #3 (2026-07-27 martin/entropy/churn plan): normalized [0,1] churn
    // score, written by `graph::churn::update_churn_scores` from the
    // indexer pipeline (rebuild_graph/incremental_graph_update), read by
    // `search`'s ranking multiplier. NULL means "git unavailable, unknown"
    // -- distinct from `0.0` ("measured, this file had zero commits in the
    // window"); search must never treat the two the same or an entire repo
    // would silently de-rank the moment git becomes unavailable.
    migrate_add_column(conn, "symbols", "churn_score", "REAL")?;
    // B3-Elixir arity gate (Tier B audit): a def/defp's own declared arity
    // (arg count -- arity is part of a function's identity in Elixir, e.g.
    // greet/1 vs greet/2 are different clauses), NULL for every other
    // language until their own arity extraction is verified per-grammar.
    migrate_add_column(conn, "symbols", "arity", "INTEGER")?;
    // A' pass (2026-07-29 self-audit): Go's arity gate generalization needs a
    // second column alongside `arity` -- for Go, `arity` holds the MINIMUM
    // arg count (not an exact count like Elixir's), and this flag says
    // whether the function's last parameter is variadic (`...T`), meaning it
    // accepts `arity` or MORE arguments, never fewer. `0`/`false` default so
    // every pre-existing Elixir row (where this concept doesn't apply) reads
    // as non-variadic, matching its exact-match gate semantics unchanged.
    migrate_add_column(
        conn,
        "symbols",
        "arity_variadic",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_fts_add_signature(conn)?;
    migrate_add_project_memory_fts(conn)?;
    migrate_add_scip_overlay_state(conn)?;
    dedup_edges_and_add_unique_indexes(conn)?;
    migrate_call_site_identity_v2(conn)?;
    migrate_add_column(
        conn,
        "external_proofs",
        "graph_generation",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_add_column(
        conn,
        "external_proofs",
        "call_site_identity_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    migrate_add_column(conn, "external_proofs", "definition_snapshot", "TEXT")?;
    migrate_add_column(conn, "identity_migration_state", "duration_ms", "INTEGER")?;
    migrate_add_column(conn, "identity_migration_state", "rows_rebuilt", "INTEGER")?;
    migrate_add_column(
        conn,
        "identity_migration_state",
        "busy_retries",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    migrate_add_column(
        conn,
        "identity_migration_state",
        "graph_generation",
        "INTEGER",
    )?;
    // A pre-generation proof cannot establish that its captured graph is the
    // one currently served. Retain it for diagnosis but never present it fresh.
    conn.execute(
        "UPDATE external_proofs
         SET status = 'legacy', failure_reason = COALESCE(failure_reason, 'missing D4 graph proof')
         WHERE graph_generation = 0
            OR call_site_identity_version < 2
            OR definition_snapshot IS NULL",
        [],
    )?;
    Ok(())
}

/// Adds the byte-span call-site identity introduced by D4 while retaining
/// legacy rows that only have a line number.  A current edge is unique per
/// persisted call site, target, and kind; the legacy key remains only for rows
/// without a `call_site_id` so older databases stay readable during upgrade.
fn migrate_call_site_identity_v2(conn: &Connection) -> rusqlite::Result<()> {
    migrate_add_column(conn, "call_sites", "callee_start_byte", "INTEGER")?;
    migrate_add_column(conn, "call_sites", "callee_end_byte", "INTEGER")?;
    migrate_add_column(
        conn,
        "call_sites",
        "identity_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    migrate_add_column(
        conn,
        "call_edges",
        "call_site_id",
        "INTEGER REFERENCES call_sites(id) ON DELETE CASCADE",
    )?;

    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_call_edges_unique;
         CREATE UNIQUE INDEX IF NOT EXISTS idx_call_edges_legacy_unique
             ON call_edges(from_symbol, to_symbol, COALESCE(call_site_line, -1), edge_kind)
             WHERE call_site_id IS NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS idx_call_edges_current_identity
             ON call_edges(call_site_id, to_symbol, edge_kind)
             WHERE call_site_id IS NOT NULL;
         CREATE UNIQUE INDEX IF NOT EXISTS idx_call_sites_current_identity
             ON call_sites(
                 from_path,
                 enclosing_qn,
                 callee_start_byte,
                 callee_end_byte,
                 edge_kind,
                 identity_version
             )
             WHERE identity_version >= 2
               AND callee_start_byte IS NOT NULL
               AND callee_end_byte IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_call_edges_call_site
             ON call_edges(call_site_id);",
    )?;
    Ok(())
}

/// One-off cleanup + hardening for the two derived-edge tables (`call_edges`,
/// `import_edges`), added 2026-07-28 after a self-audit found ~62% of
/// `call_edges` rows on a long-lived index were byte-duplicate copies.
///
/// Root cause: the SCIP overlay (`scip::ingest::insert_missing_edges`) is a
/// separate background pass with no pre-clear, and its within-run dedup set —
/// keyed on the target's `symbols.line_start` — never matched the *next* run's
/// reload, keyed on the SCIP def-occurrence line, so every overlay run
/// re-inserted the same `formal` edge. `import_edges` had an independent
/// instance of the same class (the per-file extractor can emit byte-identical
/// rows). Neither table had a UNIQUE constraint to catch it.
///
/// Fix is defense-in-depth: a UNIQUE index makes *every* insert path
/// idempotent regardless of per-path delete discipline (all three production
/// inserts now use `INSERT OR IGNORE`). This first collapses any pre-existing
/// duplicates — keeping the lowest `id` per logical edge; the copies are
/// byte-identical so which survivor is kept is immaterial — then creates the
/// indexes. Guarded on the (last-created) import index's existence so the
/// whole-table cleanup scan runs exactly once, on the first open after
/// upgrade; every later open short-circuits. Re-running is safe either way
/// (both DELETEs and both `CREATE ... IF NOT EXISTS` are idempotent).
///
/// `call_site_line` / `to_path` / `symbols_used` are wrapped in `COALESCE`
/// because SQLite treats NULLs as *distinct* in a UNIQUE index, which would
/// otherwise let NULL-keyed duplicates slip past the constraint. The key
/// deliberately excludes `edge_confidence`/`formal_source`/`ruled_out_by_scip`:
/// those are attributes the overlay mutates in place on the *same* logical
/// edge (`from_symbol`,`to_symbol`,`call_site_line`,`edge_kind`), never a
/// second row.
fn dedup_edges_and_add_unique_indexes(conn: &Connection) -> rusqlite::Result<()> {
    let already_hardened: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type = 'index' AND name = 'idx_import_edges_unique'",
        [],
        |r| r.get(0),
    )?;
    if already_hardened > 0 {
        return Ok(());
    }

    // Collapse pre-constraint duplicates, keeping one row per logical edge.
    conn.execute(
        "DELETE FROM call_edges WHERE id NOT IN ( \
            SELECT MIN(id) FROM call_edges \
            GROUP BY from_symbol, to_symbol, COALESCE(call_site_line, -1), edge_kind)",
        [],
    )?;
    conn.execute(
        "DELETE FROM import_edges WHERE id NOT IN ( \
            SELECT MIN(id) FROM import_edges \
            GROUP BY from_path, COALESCE(to_path, ''), module_name, COALESCE(symbols_used, '[]'))",
        [],
    )?;

    // call_edges index first, import index last: the guard above keys on the
    // import index, so a crash between the two CREATEs re-runs both cleanly.
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_call_edges_unique \
             ON call_edges(from_symbol, to_symbol, COALESCE(call_site_line, -1), edge_kind); \
         CREATE UNIQUE INDEX IF NOT EXISTS idx_import_edges_unique \
             ON import_edges(from_path, COALESCE(to_path, ''), module_name, COALESCE(symbols_used, '[]'));",
    )?;
    Ok(())
}

/// FTS5 virtual tables reject `ALTER TABLE ADD COLUMN` (see
/// `test_fts5_rejects_alter_table_add_column`), so unlike
/// `migrate_add_column` this drops and recreates `fts_exact` — plus its
/// three sync triggers, which also use `CREATE ... IF NOT EXISTS` and would
/// otherwise silently keep their old (signature-unaware) bodies — before
/// rebuilding `fts_exact`'s content from `symbols` via FTS5's `'rebuild'`
/// command. On a fresh DB this is a no-op: `init_db` already creates
/// `fts_exact` with `signature` from `FTS5_SQL` before migrations run.
fn migrate_fts_add_signature(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(fts_exact)")?;
    let existing: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();

    if existing.iter().any(|c| c == "signature") {
        return Ok(());
    }

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS symbols_ai;
         DROP TRIGGER IF EXISTS symbols_ad;
         DROP TRIGGER IF EXISTS symbols_au;
         DROP TABLE IF EXISTS fts_exact;",
    )?;
    conn.execute_batch(FTS5_SQL)?;
    conn.execute_batch(TRIGGERS_SQL)?;
    conn.execute_batch("INSERT INTO fts_exact(fts_exact) VALUES ('rebuild');")?;
    tracing::info!("Migration: rebuilt fts_exact with signature column");
    Ok(())
}

const PROJECT_MEMORY_FTS_SQL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS project_memory_fts USING fts5(
    topic,
    content,
    content='project_memory',
    content_rowid='id',
    tokenize='unicode61'
);
";

const PROJECT_MEMORY_TRIGGERS_SQL: &str = "
CREATE TRIGGER IF NOT EXISTS project_memory_ai AFTER INSERT ON project_memory BEGIN
    INSERT INTO project_memory_fts(rowid, topic, content)
        VALUES (new.id, new.topic, new.content);
END;

CREATE TRIGGER IF NOT EXISTS project_memory_ad AFTER DELETE ON project_memory BEGIN
    INSERT INTO project_memory_fts(project_memory_fts, rowid, topic, content)
        VALUES ('delete', old.id, old.topic, old.content);
END;

CREATE TRIGGER IF NOT EXISTS project_memory_au
    AFTER UPDATE OF content ON project_memory BEGIN
    INSERT INTO project_memory_fts(project_memory_fts, rowid, topic, content)
        VALUES ('delete', old.id, old.topic, old.content);
    INSERT INTO project_memory_fts(rowid, topic, content)
        VALUES (new.id, new.topic, new.content);
END;
";

/// Unlike `fts_exact` (always unconditionally (re)created by `FTS5_SQL`
/// before migrations run, so `migrate_fts_add_signature` above has to key
/// off a column, not the table's existence), `project_memory_fts` is a
/// brand-new table that no pre-existing DB has — so its absence from
/// `sqlite_master` is itself a reliable "not yet migrated" marker. `remember`
/// upserts via `ON CONFLICT DO UPDATE`, which SQLite fires as an UPDATE
/// trigger (not INSERT) against the pre-existing row, hence the `AFTER
/// UPDATE OF content` trigger — `topic` never changes on an upsert (it's the
/// conflict key) so it isn't watched.
fn migrate_add_project_memory_fts(conn: &Connection) -> rusqlite::Result<()> {
    let already_migrated: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'project_memory_fts'",
        [],
        |r| r.get(0),
    )?;
    if already_migrated > 0 {
        return Ok(());
    }

    conn.execute_batch(PROJECT_MEMORY_FTS_SQL)?;
    conn.execute_batch(PROJECT_MEMORY_TRIGGERS_SQL)?;
    conn.execute_batch("INSERT INTO project_memory_fts(project_memory_fts) VALUES ('rebuild');")?;
    tracing::info!("Migration: created project_memory_fts and backfilled existing notes");
    Ok(())
}

/// B4 (2026-07-28 benchmark root-cause): replaces the old
/// `.calm/<provider>.cache` + `.calm/<provider>-stats.json` sidecar files
/// with a DB-resident table, so wiping/rebuilding `index.db` can never
/// leave a stale skip-signal behind for a SCIP overlay pass to trust — see
/// `scip::state`'s module doc comment for the full incident.
fn migrate_add_scip_overlay_state(conn: &Connection) -> rusqlite::Result<()> {
    let already_migrated: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'scip_overlay_state'",
        [],
        |r| r.get(0),
    )?;
    if already_migrated > 0 {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE scip_overlay_state ( \
            provider TEXT PRIMARY KEY, \
            cache_key TEXT, \
            upgraded INTEGER NOT NULL DEFAULT 0, \
            ruled_out INTEGER NOT NULL DEFAULT 0, \
            inserted INTEGER NOT NULL DEFAULT 0, \
            match_rate REAL NOT NULL DEFAULT 0.0, \
            last_run_unix INTEGER \
        );",
    )?;
    tracing::info!("Migration: created scip_overlay_state");
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
    fn dedup_migration_collapses_duplicate_edge_rows_and_blocks_new_dups() {
        // Simulate a pre-hardening DB: base tables exist (SCHEMA_SQL) but the
        // unique edge indexes / dedup migration have not run yet. This is the
        // 2026-07-28 self-audit repro (`boundaries.rs::PathMatcher` had 113
        // duplicate caller rows) reduced to its data-layer essence.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();

        // Three byte-identical call edges for one logical edge, plus a
        // genuinely-distinct edge (different to_symbol) that must survive.
        for _ in 0..3 {
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind) \
                 VALUES ('a::f', 'b::g', 10, 'formal', 'call')",
                [],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind) \
             VALUES ('a::f', 'c::h', 10, 'formal', 'call')",
            [],
        )
        .unwrap();
        // Two byte-identical imports + one distinct (different symbols_used).
        for _ in 0..2 {
            conn.execute(
                "INSERT INTO import_edges (from_path, to_path, module_name, symbols_used) \
                 VALUES ('x.rs', 'y.rs', 'y', '[\"A\"]')",
                [],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name, symbols_used) \
             VALUES ('x.rs', 'y.rs', 'y', '[\"B\"]')",
            [],
        )
        .unwrap();

        dedup_edges_and_add_unique_indexes(&conn).unwrap();

        let call_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(call_rows, 2, "3 identical + 1 distinct call edge -> 2");
        let import_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM import_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(import_rows, 2, "2 identical + 1 distinct import edge -> 2");

        // The constraint now makes further duplicate inserts idempotent, even
        // at a different confidence (the key excludes edge_confidence).
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind) \
                 VALUES ('a::f', 'b::g', 10, 'textual', 'call')",
                [],
            )
            .unwrap();
        assert_eq!(changed, 0, "duplicate (from,to,line,kind) is ignored");

        // NULL call_site_line duplicates are caught too (COALESCE in the key).
        conn.execute(
            "INSERT OR IGNORE INTO call_edges (from_symbol, to_symbol, edge_confidence, edge_kind) \
             VALUES ('a::f', 'd::k', 'textual', 'call')",
            [],
        )
        .unwrap();
        let null_dup = conn
            .execute(
                "INSERT OR IGNORE INTO call_edges (from_symbol, to_symbol, edge_confidence, edge_kind) \
                 VALUES ('a::f', 'd::k', 'textual', 'call')",
                [],
            )
            .unwrap();
        assert_eq!(null_dup, 0, "NULL-line duplicate ignored via COALESCE key");

        // Idempotent: re-running the migration is a cheap no-op.
        dedup_edges_and_add_unique_indexes(&conn).unwrap();
        let call_rows_final: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            call_rows_final, 3,
            "2 survivors + 1 new distinct null-line edge"
        );
    }

    #[test]
    fn migration_adds_boundary_ambiguous_column_defaulting_to_zero() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, path, language, line_start, line_end, signature) \
             VALUES ('x', 'x', 'function', 'a.rs', 'rust', 1, 2, 'fn x()')",
            [],
        )
        .unwrap();
        let val: i64 = conn
            .query_row(
                "SELECT boundary_ambiguous FROM symbols WHERE qualified_name = 'x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(val, 0, "new rows default to not-ambiguous");
    }

    #[test]
    fn migration_adds_arity_and_arg_count_columns_defaulting_to_null() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, path, language, line_start, line_end, signature) \
             VALUES ('x', 'x', 'function', 'a.ex', 'elixir', 1, 2, 'def x()')",
            [],
        )
        .unwrap();
        let arity: Option<i64> = conn
            .query_row(
                "SELECT arity FROM symbols WHERE qualified_name = 'x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(arity, None, "arity defaults to NULL, not a guessed 0");

        conn.execute(
            "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line) \
             VALUES ('a.ex', 'a.ex::x', 'y', 1)",
            [],
        )
        .unwrap();
        let arg_count: Option<i64> = conn
            .query_row(
                "SELECT arg_count FROM call_sites WHERE callee_name = 'y'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            arg_count, None,
            "arg_count defaults to NULL, not a guessed 0"
        );
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

    /// Locks in the reason `migrate_fts_add_signature` (below) uses a
    /// drop-and-rebuild instead of `migrate_add_column`'s usual
    /// `ALTER TABLE ADD COLUMN`: SQLite's FTS5 virtual tables reject
    /// `ALTER TABLE` outright ("virtual tables may not be altered"), unlike
    /// ordinary tables. If a future SQLite/rusqlite upgrade ever lifts this
    /// restriction, this test will fail and the migration can be simplified.
    #[test]
    fn test_fts5_rejects_alter_table_add_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5(name, docstring, tokenize='unicode61');",
        )
        .unwrap();
        let result = conn.execute_batch("ALTER TABLE t ADD COLUMN signature;");
        assert!(
            result.is_err(),
            "FTS5 unexpectedly accepted ALTER TABLE ADD COLUMN — \
             migrate_fts_add_signature's drop-and-rebuild can be simplified"
        );
    }

    /// Simulates a DB created before `signature` was added to `fts_exact`
    /// (old `FTS5_SQL`/`TRIGGERS_SQL` shape, hand-inlined here since the
    /// live constants have since moved on) — a symbol with data already
    /// exists, then `init_db` runs against it as an upgrade would. Confirms
    /// the migration backfills existing rows (not just future inserts) and
    /// that post-migration trigger sync still works.
    #[test]
    fn test_migrate_fts_add_signature_backfills_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE fts_exact USING fts5(
                 name, docstring, content='symbols', content_rowid='id', tokenize='unicode61');
             CREATE VIRTUAL TABLE fts_tokens USING fts5(
                 name_tokens, content='symbols', content_rowid='id', tokenize='unicode61');
             CREATE TRIGGER symbols_ai AFTER INSERT ON symbols BEGIN
                 INSERT INTO fts_exact(rowid, name, docstring) VALUES (new.id, new.name, new.docstring);
                 INSERT INTO fts_tokens(rowid, name_tokens) VALUES (new.id, new.name_tokens);
             END;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, signature, name_tokens, indexed_at) \
             VALUES ('mod.greet', 'greet', 'function', 'python', 'mod.py', 1, 3, \
             'fn greet(who: Widgetronic) -> str', 'greet', 0.0)",
            [],
        )
        .unwrap();

        // Pre-migration: old fts_exact has no signature column at all.
        let cols_before: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(fts_exact)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(!cols_before.iter().any(|c| c == "signature"));

        // init_db on an already-populated old-shape DB is the upgrade path.
        init_db(&conn).unwrap();

        let cols_after: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(fts_exact)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(cols_after.iter().any(|c| c == "signature"));

        // The pre-existing row's signature was backfilled by 'rebuild', not just
        // rows inserted after the migration.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_exact WHERE fts_exact MATCH 'widgetronic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "existing symbol's signature must be backfilled");

        // Triggers still sync signature for a symbol inserted after migration.
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, signature, name_tokens, indexed_at) \
             VALUES ('mod.farewell', 'farewell', 'function', 'python', 'mod.py', 5, 7, \
             'fn farewell(who: Zorbex) -> str', 'farewell', 0.0)",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM fts_exact WHERE fts_exact MATCH 'zorbex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "post-migration trigger must sync signature too");
    }

    #[test]
    fn test_project_memory_fts_trigger_sync() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('db-choice', 'we use postgres for prod', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'postgres'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // remember's upsert (`ON CONFLICT(topic) DO UPDATE`) must fire as an
        // UPDATE trigger, not leave the FTS index stuck on the old content.
        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('db-choice', 'we migrated to mysql', '2026-01-01', '2026-01-02') \
             ON CONFLICT(topic) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
            [],
        )
        .unwrap();

        let old_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'postgres'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0, "stale content must not still match");
        let new_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'mysql'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(new_count, 1, "updated content must match");

        conn.execute("DELETE FROM project_memory WHERE topic = 'db-choice'", [])
            .unwrap();
        let deleted_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'mysql'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            deleted_count, 0,
            "deleted note must be removed from the FTS index"
        );
    }

    #[test]
    fn test_migrate_add_project_memory_fts_backfills_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        // Old-shape DB: SCHEMA_SQL only, predating project_memory_fts entirely.
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('legacy-note', 'uses widgetronic auth', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();

        let exists_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'project_memory_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists_before, 0);

        // init_db on an already-populated old-shape DB is the upgrade path.
        init_db(&conn).unwrap();

        let exists_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'project_memory_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists_after, 1);

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'widgetronic'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "pre-existing note must be backfilled by 'rebuild'"
        );

        // Triggers sync notes inserted after the migration too.
        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('new-note', 'uses zorbex cache', '2026-01-02', '2026-01-02')",
            [],
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'zorbex'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "post-migration trigger must sync new notes too");
    }

    #[test]
    fn call_site_identity_schema_uses_byte_spans_and_edge_foreign_key() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let call_site_columns: Vec<String> = conn
            .prepare("PRAGMA table_info(call_sites)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for column in ["callee_start_byte", "callee_end_byte", "identity_version"] {
            assert!(
                call_site_columns.iter().any(|present| present == column),
                "call_sites must persist {column} for exact CallSite identity"
            );
        }

        let edge_columns: Vec<String> = conn
            .prepare("PRAGMA table_info(call_edges)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for column in ["call_site_id", "evidence_state"] {
            assert!(
                edge_columns.iter().any(|present| present == column),
                "call_edges must persist {column} for D4 provenance"
            );
        }

        let foreign_keys: Vec<(String, String, String)> = conn
            .prepare("PRAGMA foreign_key_list(call_edges)")
            .unwrap()
            .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            foreign_keys.iter().any(|(table, from, to)| {
                table == "call_sites" && from == "call_site_id" && to == "id"
            }),
            "call_site_id must carry a database foreign-key relationship"
        );
    }

    #[test]
    fn migration_marks_preexisting_external_formal_evidence_as_legacy() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, formal_source) \
             VALUES ('caller', 'target', 'formal', 'scip')",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();
        let state: String = conn
            .query_row("SELECT evidence_state FROM call_edges", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            state, "legacy",
            "a pre-D4 external verdict lacks an exact persisted proof record"
        );
    }

    #[test]
    fn external_proof_is_keyed_by_call_site_and_deleted_with_it() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('main.rs', 'main.rs::main', 'target', 1, 0, 6, 2, 'call')",
            [],
        )
        .unwrap();
        let call_site_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO external_proofs
                (call_site_id, to_symbol, provider, source_file_hash, callee_start_byte,
                 callee_end_byte, provider_fingerprint, context_fingerprint, status, observed_at)
             VALUES (?1, 'lib.rs::target', 'scip:rust', 'source-hash', 0, 6,
                     'binary-fingerprint', 'context-fingerprint', 'fresh', 1.0)",
            [call_site_id],
        )
        .unwrap();

        conn.execute("DELETE FROM call_sites WHERE id = ?1", [call_site_id])
            .unwrap();
        let proofs: i64 = conn
            .query_row("SELECT COUNT(*) FROM external_proofs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            proofs, 0,
            "proof lifetime follows CallSite identity, never edge id"
        );
    }
}
