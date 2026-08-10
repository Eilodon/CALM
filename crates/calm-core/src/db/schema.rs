// -- Downgrade guard (added post-2026-08-05 state.db rewiring incident) --
// See docs/plans/2026-08-05-state-db-rewiring-execution-plan.md for the
// incident this is a direct response to: a schema split shipped with no
// version marker anywhere, so an older binary opening a newer-schema file
// had no way to detect the mismatch and just failed on "no such table"
// instead of refusing up front with a clear message.
//
// `INDEX_DB_SCHEMA_VERSION`/`STATE_DB_SCHEMA_VERSION` are stamped into
// `PRAGMA user_version` -- SQLite's own reserved 32-bit header integer, so
// this costs no extra table or I/O. Bump the relevant constant whenever a
// schema change means an OLDER binary can no longer safely continue
// writing to the file.
//
// `init_db`/`init_state_db` themselves are deliberately left untouched --
// both are hub symbols with 100+ callers (nearly all test helpers
// building a fresh, disposable temp DB, where a version check is a
// no-op), and this repo's own edit-safety gate correctly refuses a
// citation-only edit to a >10-caller "high" risk hub without a live
// human-approval round-trip. `init_db_versioned`/`init_state_db_versioned`
// below are thin wrappers real (non-test) entry points should call
// instead: calm-cli `index`/`fitness-check`, calm-server `new_with_preset`
// (the `calm serve`/daemon bootstrap) and `doctor`.
pub const INDEX_DB_SCHEMA_VERSION: i64 = 1;
// v2 (CCK-07): adds evidence_snapshots/change_intents/change_intent_targets.
// v3 (CCK-09, docs/plans/2026-08-08-master-change-control-execution-blueprint.md):
// adds review_authorities(+targets,+evidence) and edit_transactions.authority_id.
// v4 (CCK-25, audit follow-up on the same blueprint): adds
// review_authorities.consumed_by_tx_id, provenance-binding a consumed
// authority to the exact edit_transactions row it authorized (previously
// the FK existed the other way -- edit_transactions.authority_id -- but
// nothing ever set it, and authority consume/txn begin were two separate,
// non-atomic steps).
// v5 (CCK-26, audit follow-up): adds review_authorities.policy_decision_digest/
// risk_vector_digest/required_approver_class -- a real PolicyEngine::evaluate()
// decision, not just a policy-config digest, now backs each authority.
// v6 (CCK-26, same audit follow-up): adds evidence_snapshots.provider_state_digest
// -- EvidenceSnapshot::snapshot_id now also binds SCIP/LSP provider run state
// (authority/snapshot.rs::provider_state_digest), so a proof-coverage change
// with no source/config/graph_generation change still mints a fresh snapshot.
// v7 (CCK-27, audit follow-up): adds change_intents.status/superseded_by_intent_id
// -- plan_change's idempotency dedup can now mark a stale intent superseded
// (and free its idempotency_key) instead of silently reusing it after
// evidence has drifted; review_change refuses to mint against one.
// v8 (WS3, audit follow-up): adds approval_receipts -- a durable record
// that a ReviewAuthority's required_approver_class was actually satisfied
// (self-attestation at mint for SelfReviewed, a real MRTR/legacy
// elicitation round-trip at spend for Human), not just signed as a claim.
// v9 (WS3 follow-up): adds approval_receipts.signature -- an HMAC over the
// receipt row itself, so a row can be verified against tampering after the
// fact. See db/state_migrations.rs's registered v1->v2 through v8->v9 steps.
// v10 (CCK-30R2, audit 2026-08-10): adds approval_receipts.signature_provenance
// ("native" vs "legacy_unverified") and folds it into the signed payload --
// v9's signature alone could not distinguish a row insert_approval_receipt
// genuinely signed at approval time from one a schema migration backfilled
// wholesale (indistinguishable by the time either migration runs), so the
// v9-era doc comments overclaiming that distinction have been corrected.
// v11 (audit 2026-08-10 follow-up): adds pending_reviews -- a durable,
// MCP-protocol-independent channel for HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW
// ("calm review"), for the case (verified empirically the same day) where a
// connected MCP client completes the elicitation round-trip without ever
// showing anything to an actual human. See db/state_migrations.rs's
// registered v10->v11 step for the full rationale.
pub const STATE_DB_SCHEMA_VERSION: i64 = 11;

/// Refuses to proceed if `conn`'s stamped `PRAGMA user_version` is HIGHER
/// than `expected` -- meaning a newer CALM binary already created or
/// migrated this exact file. An older binary continuing past this point
/// would write under schema assumptions the file has already outgrown,
/// silently corrupting durable state a newer process later trusts. A
/// version <= `expected` (including SQLite's default of 0, meaning never
/// stamped) is always fine here -- bringing an older-but-stamped file
/// forward is `run_migrations`'s job for index.db, or simply re-running
/// the idempotent `IF NOT EXISTS` DDL for state.db.
fn refuse_if_schema_newer(
    conn: &Connection,
    expected: i64,
    db_label: &str,
) -> rusqlite::Result<()> {
    let on_disk: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if on_disk > expected {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "{db_label} was created or migrated by a newer CALM version (schema {on_disk}) \
                 than this binary supports (schema {expected}). Refusing to open it -- \
                 continuing would risk silently corrupting durable state. Upgrade this CALM \
                 binary, then try again."
            )),
        ));
    }
    Ok(())
}

/// Production-path wrapper around `init_db` -- enforces the downgrade
/// guard, then delegates to the unchanged `init_db`, then stamps the
/// current version. Real (non-test) entry points should call this instead
/// of `init_db` directly; test helpers keep calling `init_db` unversioned
/// (see the module-level doc comment above for why that's fine).
pub fn init_db_versioned(conn: &Connection) -> rusqlite::Result<()> {
    refuse_if_schema_newer(conn, INDEX_DB_SCHEMA_VERSION, "index.db")?;
    init_db(conn)?;
    conn.pragma_update(None, "user_version", INDEX_DB_SCHEMA_VERSION)?;
    Ok(())
}

/// `init_state_db`'s counterpart to `init_db_versioned` -- see that
/// function's doc comment.
///
/// CCK-28 (audit follow-up): a genuinely empty file gets the CURRENT full
/// schema (`init_state_db`) directly, DDL and version stamp wrapped in one
/// transaction so a crash between them can never leave a file with the
/// current physical schema but an old (or unstamped) `user_version`. A
/// pre-existing file instead goes straight to `migrate_state_db_to_current`
/// WITHOUT running `init_state_db` first -- every prior version of this
/// function ran the full current-schema DDL unconditionally before
/// migrating, which only stayed safe by accident: every statement in it
/// happens to be `IF NOT EXISTS`/idempotent today, but a future migration
/// step needing something schema-version-dependent to NOT already exist
/// (e.g. `CREATE INDEX ... ON t(new_column)` before the ALTER that adds
/// `new_column`) would silently break under "current schema always runs
/// first." Each registered step is independently self-sufficient (creates
/// whatever it needs, not just ALTERs an assumed-present table) precisely
/// so it never depends on that ordering -- see
/// `state_migrations.rs`'s own `registered_migrations_are_self_sufficient_
/// against_a_genuine_pre_v2_database` test.
pub fn init_state_db_versioned(conn: &Connection) -> rusqlite::Result<()> {
    refuse_if_schema_newer(conn, STATE_DB_SCHEMA_VERSION, "state.db")?;
    let is_empty: bool = conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table')",
        [],
        |r| r.get(0),
    )?;
    if is_empty {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> rusqlite::Result<()> {
            init_state_db(conn)?;
            conn.pragma_update(None, "user_version", STATE_DB_SCHEMA_VERSION)?;
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
    } else {
        // CCK-01: run any registered forward migrations from the on-disk
        // version up to STATE_DB_SCHEMA_VERSION, then stamp it (see
        // state_migrations.rs) -- already atomic per-step on its own.
        super::state_migrations::migrate_state_db_to_current(conn)?;
    }
    Ok(())
}

use rusqlite::Connection;
use std::path::Path;

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
    possible_coreness INTEGER,
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
CREATE INDEX IF NOT EXISTS idx_pattern_debt_topic ON pattern_debt(topic);

-- Tier 1 semantic facts (2026-08-07 roadmap, docs/plans/2026-08-07-
-- pecorino-adoption-roadmap.md T1): extends/implements extracted directly
-- from tree-sitter syntax (indexer::semantic_facts). Same rebuild lifecycle
-- as call_sites/import_edges -- DELETE-by-path then re-INSERT on every
-- reindex of the owning file (see pipeline::remove_file_rows/persist_file),
-- so unlike external_proofs this needs no source_hash of its own: it is
-- always exactly as fresh as the file's last index pass.
--
-- from_symbol is the class/struct/trait's OWN qualified_name (a real row in
-- `symbols`) -- resolved either by exact (bare_name, def_line) lookup, or
-- for Rust's `impl Trait for Type` (which never gets its own `symbols` row
-- -- see semantic_facts.rs's module doc comment) by same-file bare-name
-- fallback. target_text is always the raw syntactic text of the base/
-- interface/trait; to_symbol is populated only when that text resolves to
-- a symbol in the SAME file (same-file-only resolution in v1, no cross-file
-- global pass yet) -- NULL otherwise, with confidence dropping to
-- 'textual'. Never guessed: a relation whose target can't be resolved is
-- still recorded (to_symbol NULL), never silently dropped or fabricated.
--
-- PR A (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md,
-- P4.1 Type Resolver Soundness): resolution_source records WHICH pass owns
-- a resolved row -- 'same_file_ast' (extraction, pipeline::extract_file_
-- data, this table's original resolver) or 'cross_file_unique' (graph::
-- type_resolve's graph-wide follow-up pass). NULL when to_symbol is NULL
-- (nothing resolved it yet). Ownership must never be inferred from
-- confidence alone -- both resolvers can produce 'resolved', but only the
-- pass that OWNS a row is allowed to reset/downgrade it on the next
-- rebuild (see type_resolve.rs's module doc comment for why cross-file
-- rows specifically need this to support 'resolved -> textual' when their
-- evidence disappears, unlike same_file_ast rows which are re-derived from
-- scratch by extraction on every reindex of their own file anyway).
CREATE TABLE IF NOT EXISTS type_relations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    from_symbol     TEXT NOT NULL,
    relation_kind   TEXT NOT NULL CHECK (relation_kind IN ('extends', 'implements')),
    target_text     TEXT NOT NULL,
    to_symbol       TEXT,
    confidence      TEXT NOT NULL CHECK (confidence IN ('resolved', 'textual')),
    evidence_source TEXT NOT NULL DEFAULT 'ast',
    resolution_source TEXT,
    source_path     TEXT NOT NULL,
    line            INTEGER NOT NULL,
    UNIQUE(from_symbol, relation_kind, target_text, line)
);
CREATE INDEX IF NOT EXISTS idx_type_relations_from ON type_relations(from_symbol);
CREATE INDEX IF NOT EXISTS idx_type_relations_path ON type_relations(source_path);

-- Tier 1 semantic facts, effect half: explicit throws and direct
-- self/this-field writes, extracted the same conservative way (see
-- semantic_facts.rs's module doc comment for exactly what is and isn't
-- captured per language and why). symbol_qn is the enclosing function/
-- method's OWN qualified_name, resolved the identical two-phase way
-- call_sites.enclosing_qn already is. Same rebuild lifecycle as
-- type_relations above -- no source_hash needed.
CREATE TABLE IF NOT EXISTS symbol_effects (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_qn          TEXT NOT NULL,
    effect_kind        TEXT NOT NULL CHECK (effect_kind IN ('explicit_throw', 'write_field')),
    target_text        TEXT NOT NULL,
    -- P3 (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md):
    -- split from the old single `confidence` column -- `event_confidence`
    -- is certainty THAT the effect happened (currently always 'exact': every
    -- extraction site fires only on a real syntactic raise/throw/write
    -- node); `target_confidence` is certainty about WHAT the target is
    -- ('exact' | 'none'). A Python `raise e`/`raise factory()`/bare `raise`
    -- is a real, certain throw EVENT whose exact exception TYPE isn't
    -- syntactically knowable without full resolution -- previously dropped
    -- entirely (see semantic_facts.rs's module doc comment history);
    -- target_confidence='none' now records the event honestly instead of
    -- losing it. write_field's target (the field name) is always exact
    -- once detected, so it's always 'exact' on both dimensions.
    event_confidence   TEXT NOT NULL DEFAULT 'exact',
    target_confidence  TEXT NOT NULL DEFAULT 'exact',
    source_path        TEXT NOT NULL,
    line               INTEGER NOT NULL,
    UNIQUE(symbol_qn, effect_kind, target_text, line)
);
CREATE INDEX IF NOT EXISTS idx_symbol_effects_symbol ON symbol_effects(symbol_qn);
CREATE INDEX IF NOT EXISTS idx_symbol_effects_path ON symbol_effects(source_path);

-- Tier 2 semantic fact (2026-08-07 roadmap T2): Architecture Digest --
-- deterministic, factual (never LLM-generated) per-symbol summary. Full
-- DELETE-then-reinsert on EVERY graph rebuild (graph::digest::compute_digests,
-- called from the same place coreness/hub/churn already are) -- no
-- selective invalidation, so a PRESENT row is always current as of the
-- last successful rebuild; a missing row just means this symbol's kind
-- isn't digestable or no rebuild has run yet. `graph_generation` is a pure
-- observability breadcrumb (never compared for correctness -- see
-- graph/digest.rs's module doc comment for why generation-fencing was
-- deliberately dropped from the original roadmap sketch).
CREATE TABLE IF NOT EXISTS symbol_digests (
    symbol_qn            TEXT PRIMARY KEY,
    facts_json           TEXT NOT NULL,
    rendered_text        TEXT NOT NULL,
    recursive_component  INTEGER NOT NULL DEFAULT 0,
    graph_generation     INTEGER NOT NULL DEFAULT 0,
    truncated            INTEGER NOT NULL DEFAULT 0
);

-- T4a Package Dependency Graph (2026-08-07 roadmap, TIER 4 first stage):
-- DECLARED external dependencies parsed straight from manifest files
-- (Cargo.toml/package.json/go.mod/requirements.txt/pyproject.toml -- see
-- indexer::package_deps's module doc comment for exactly what is and
-- isn't covered). Full DELETE-then-reinsert on every graph rebuild, same
-- posture as symbol_digests above -- manifests are small and cheap to
-- re-scan; no incremental invalidation needed. version_spec is the raw
-- declared string verbatim (never range-parsed), NULL when the manifest
-- genuinely declares no version (e.g. Cargo `{ workspace = true }`).
CREATE TABLE IF NOT EXISTS package_dependencies (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    manifest_path    TEXT NOT NULL,
    ecosystem        TEXT NOT NULL CHECK (ecosystem IN ('cargo', 'npm', 'go', 'pypi')),
    dependency_name  TEXT NOT NULL,
    version_spec     TEXT,
    dependency_kind  TEXT NOT NULL CHECK (dependency_kind IN ('runtime', 'dev', 'build', 'peer', 'optional')),
    UNIQUE(manifest_path, ecosystem, dependency_name, dependency_kind)
);
CREATE INDEX IF NOT EXISTS idx_package_dependencies_manifest ON package_dependencies(manifest_path);
CREATE INDEX IF NOT EXISTS idx_package_dependencies_name ON package_dependencies(dependency_name);
";

/// Durable state (project memory, edit-transaction journal, audit ledger,
/// maintenance outbox) lives in a SEPARATE file (`state.db`, see
/// `init_state_db` below) at `PRAGMA synchronous=FULL` -- none of it is
/// rebuildable from source the way everything in `SCHEMA_SQL` above is.
/// `migrate_legacy_durable_tables` copies any pre-existing rows out of an
/// older `index.db` that still has these tables (from before this split).
const STATE_SCHEMA_SQL: &str = "
-- Durable, agent-written interpretive notes (architecture decisions, gotchas,
-- rationale) — distinct from anything derived from the AST/call-graph, and
-- distinct from `session_context`'s per-session navigational state (which
-- resets every server restart). One row per `topic`; `remember` upserts.
CREATE TABLE IF NOT EXISTS project_memory (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic       TEXT NOT NULL UNIQUE,
    content     TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    -- Plan 3 §3.5(d): HMAC-SHA256(topic, content), written by `remember`.
    -- Nullable, not backfilled -- a pre-existing note has no MAC to check,
    -- and memory::verify_integrity reports that case as unverified.
    content_mac TEXT,
    -- audit F7 follow-up: set when `remember` flags the note's content as
    -- prompt-injection-shaped (still saved either way, detection-only);
    -- `recall` excludes a quarantined note from its default results unless
    -- `include_quarantined: true` is passed.
    quarantined INTEGER NOT NULL DEFAULT 0
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

-- WS-1 (docs/plans/2026-08-02-phase1-p0-execution-plan.md §4.2): durable edit-transaction
-- journal. 'state' is a CACHE of replay(tx_events WHERE tx_id=?) -> last state, not an
-- independent source of truth -- txn::advance() is the only code path allowed to write it,
-- and always writes a matching tx_events row in the same call. Mirrors VHEATM's
-- lifecycle.py::AuditLifecycle (ALLOWED_TRANSITIONS + replayable event log).
CREATE TABLE IF NOT EXISTS edit_transactions (
    tx_id                   TEXT PRIMARY KEY,
    project_id               TEXT NOT NULL,
    path                      TEXT NOT NULL,
    base_digest               TEXT NOT NULL,
    proposed_digest           TEXT NOT NULL,
    review_token_id           TEXT,
    -- v3 / CCK-09: which review_authorities row (if any) authorized this
    -- write via the new structured path -- NULL for every transaction
    -- still going through the legacy edit_context+confirm+reason path.
    -- CCK-R6 (audit follow-up): a real FK, not just a comment -- ON
    -- DELETE SET NULL rather than CASCADE, since this journal row must
    -- outlive a future retention/GC pass over review_authorities; only
    -- the link goes away, never the durable transaction record itself.
    authority_id              TEXT REFERENCES review_authorities(authority_id) ON DELETE SET NULL,
    state                     TEXT NOT NULL DEFAULT 'PREPARED',
    temp_path                 TEXT,
    graph_generation_before   INTEGER,
    graph_generation_after    INTEGER,
    created_at                REAL NOT NULL,
    updated_at                REAL NOT NULL,
    error_code                TEXT,
    error_detail              TEXT
);
CREATE INDEX IF NOT EXISTS idx_edit_transactions_state ON edit_transactions(state);
CREATE INDEX IF NOT EXISTS idx_edit_transactions_path  ON edit_transactions(path);

-- Replay log for edit_transactions.state above -- one row per txn::advance() call, content-
-- addressed id (mirrors provenance.py::expected_journal_event_id) so a corrupted/edited row
-- is detectable, not just a plain audit trail.
CREATE TABLE IF NOT EXISTS tx_events (
    event_id       TEXT PRIMARY KEY,
    tx_id           TEXT NOT NULL REFERENCES edit_transactions(tx_id) ON DELETE CASCADE,
    sequence        INTEGER NOT NULL,
    from_state      TEXT NOT NULL,
    to_state        TEXT NOT NULL,
    actor           TEXT NOT NULL,
    reason          TEXT NOT NULL,
    occurred_at     REAL NOT NULL,
    UNIQUE(tx_id, sequence)
);

-- Durable trigger for the two fire-and-forget background refreshes already spawned from
-- edit_lines_impl_gated/format_files_impl (crates/calm-server/src/tools/edit.rs) --
-- scip_overlay::run_all_coalesced and embedding::embed_pending(_chunks). Both of those
-- functions already re-scan/coalesce globally and are idempotent; the gap this table closes
-- is durability of the trigger itself (see plan doc §4.1b) -- a process killed between
-- 'reindex committed' and 'background thread finished' previously left nothing recording
-- that a refresh was still owed. Singleton per job_kind (dedupe_key = job_kind), NOT per-path
-- or per-tx: these are whole-repo passes, not per-file jobs.
CREATE TABLE IF NOT EXISTS maintenance_jobs (
    job_id               TEXT PRIMARY KEY,
    job_kind              TEXT NOT NULL,
    dedupe_key            TEXT NOT NULL UNIQUE,
    state                 TEXT NOT NULL DEFAULT 'queued',
    triggered_by_tx_id     TEXT,
    attempts               INTEGER NOT NULL DEFAULT 0,
    available_at           REAL NOT NULL,
    lease_owner            TEXT,
    lease_expires_at       REAL,
    last_error             TEXT,
    last_completed_at      REAL
);
CREATE INDEX IF NOT EXISTS idx_maintenance_jobs_available ON maintenance_jobs(state, available_at);

-- P0-4 (docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md#p0-4): append-only,
-- hash-chained audit ledger. Runs alongside AUDIT_TARGET tracing (a SIEM log line)
-- as a separate durable, tamper-evident channel -- event_hash =
-- SHA-256(canonical(payload) || prev_hash) chains every row to the one before it,
-- so an out-of-band UPDATE/DELETE on this table is detectable by
-- ledger::verify_chain(), not just a plain audit trail. Mirrors VHEATM's
-- provenance.py hash chain.
CREATE TABLE IF NOT EXISTS audit_ledger (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    prev_hash    TEXT NOT NULL,
    event_hash   TEXT NOT NULL UNIQUE,
    ts           REAL NOT NULL,
    actor        TEXT NOT NULL,
    payload      TEXT NOT NULL
);

-- v2 / CCK-07 (docs/plans/2026-08-08-master-change-control-execution-blueprint.md):
-- persisted form of authority::snapshot::EvidenceSnapshot (CCK-06), which until
-- now was compute-only. snapshot_id is that struct's own content-addressed
-- SNP-sha256 id, so re-persisting an identical snapshot is a harmless
-- INSERT OR IGNORE, not a duplicate/conflict.
CREATE TABLE IF NOT EXISTS evidence_snapshots (
    snapshot_id            TEXT PRIMARY KEY,
    source_catalog_digest  TEXT NOT NULL,
    graph_generation       INTEGER NOT NULL,
    provider_state_digest  TEXT NOT NULL DEFAULT '',
    freshness_class        TEXT NOT NULL,
    created_at             REAL NOT NULL
);

-- v2 / CCK-07: a declared ChangeIntent (change::intent::ChangeIntent) --
-- what a caller says it is about to do, bound to the EvidenceSnapshot in
-- effect when it was declared. `kind` is a change::classify::ChangeKind variant
-- name (the DECLARED half; change::classify::classify_observed_change
-- produces the OBSERVED half from the real diff, never persisted here --
-- cheap to recompute, and persisting it would invite the two to drift).
CREATE TABLE IF NOT EXISTS change_intents (
    intent_id        TEXT PRIMARY KEY,
    kind             TEXT NOT NULL,
    reason           TEXT NOT NULL,
    snapshot_id      TEXT NOT NULL REFERENCES evidence_snapshots(snapshot_id),
    created_at       REAL NOT NULL,
    idempotency_key  TEXT,
    status                   TEXT NOT NULL DEFAULT 'active',
    superseded_by_intent_id  TEXT REFERENCES change_intents(intent_id)
);
CREATE INDEX IF NOT EXISTS idx_change_intents_snapshot ON change_intents(snapshot_id);
-- CCK-11: plan_change's idempotency contract -- a partial unique index
-- (not a plain UNIQUE column) because the pre-existing CCK-07 caller
-- (mint_review_authority_for_edit_context's single-symbol compat wrapper)
-- never sets this and must keep inserting NULLs freely; SQLite already
-- treats distinct NULLs as never conflicting under a plain UNIQUE
-- constraint, but the partial WHERE clause makes that non-enforcement
-- explicit rather than relying on that default behavior implicitly.
CREATE UNIQUE INDEX IF NOT EXISTS idx_change_intents_idempotency ON change_intents(idempotency_key) WHERE idempotency_key IS NOT NULL;

-- v2 / CCK-07: the file(s) (optionally symbol-scoped) one change_intents row
-- declares as its target. One-to-many so a single intent can already span
-- multiple files ahead of Phase 2 (multi-file ChangeSet) actually landing --
-- the shape is useful the moment CCK-11's plan_change tool exists, not just
-- once ChangeSet does.
CREATE TABLE IF NOT EXISTS change_intent_targets (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    intent_id      TEXT NOT NULL REFERENCES change_intents(intent_id) ON DELETE CASCADE,
    path           TEXT NOT NULL,
    qualified_name TEXT
);
CREATE INDEX IF NOT EXISTS idx_change_intent_targets_intent ON change_intent_targets(intent_id);

-- v3 / CCK-09 (#65): a signed, single-use, snapshot-bound ReviewAuthority
-- (authority::review::ReviewAuthority). Durable, structured replacement for
-- EditContextReview's session-local HashMap entry -- signature is an
-- HMAC-SHA256 over every other bound column below (see authority/key.rs's
-- control.key), so tampering with any one of them out of band invalidates
-- the row instead of silently being trusted. consumed_at NULL means unused;
-- set exactly once, by the UPDATE ... WHERE consumed_at IS NULL that makes
-- single-use atomic (see ReviewAuthority::verify_and_consume).
CREATE TABLE IF NOT EXISTS review_authorities (
    authority_id       TEXT PRIMARY KEY,
    intent_id          TEXT NOT NULL REFERENCES change_intents(intent_id),
    snapshot_id        TEXT NOT NULL REFERENCES evidence_snapshots(snapshot_id),
    graph_generation   INTEGER NOT NULL,
    caller_set_digest  TEXT NOT NULL,
    analysis_version   TEXT NOT NULL,
    policy_digest      TEXT NOT NULL,
    principal          TEXT NOT NULL,
    target_scope_digest TEXT NOT NULL DEFAULT '',
    nonce              TEXT NOT NULL,
    expires_at         REAL NOT NULL,
    signature          TEXT NOT NULL,
    created_at         REAL NOT NULL,
    consumed_at        REAL,
    -- v4 / CCK-25: which edit_transactions row this authority's single use
    -- was actually spent on -- set atomically together with consumed_at by
    -- authority::review::authorize_and_begin_edit, never independently.
    consumed_by_tx_id  TEXT REFERENCES edit_transactions(tx_id) ON DELETE SET NULL,
    -- v5 / CCK-26 (audit follow-up): what a REAL PolicyEngine::evaluate()
    -- run actually decided for this authority, not just a policy-config
    -- digest -- policy_decision_digest/risk_vector_digest are audit/
    -- provenance (bound+signed, not yet re-verified fresh at spend time --
    -- that staleness check is a follow-up, same shape as target_scope_digest's).
    -- required_approver_class is what review_change actually gates minting
    -- on: 'human' cannot be satisfied by self-attested approved:true alone.
    policy_decision_digest  TEXT NOT NULL DEFAULT '',
    risk_vector_digest      TEXT NOT NULL DEFAULT '',
    required_approver_class TEXT NOT NULL DEFAULT 'self_reviewed'
);
CREATE INDEX IF NOT EXISTS idx_review_authorities_intent ON review_authorities(intent_id);

-- v3 / CCK-09: mirrors change_intent_targets shape, scoped to the
-- authority rather than the intent -- a caller may request authority for
-- only a subset of its intents declared targets.
CREATE TABLE IF NOT EXISTS review_authority_targets (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    authority_id   TEXT NOT NULL REFERENCES review_authorities(authority_id) ON DELETE CASCADE,
    path           TEXT NOT NULL,
    qualified_name TEXT,
    UNIQUE(authority_id, path, qualified_name)
);
CREATE INDEX IF NOT EXISTS idx_review_authority_targets_authority ON review_authority_targets(authority_id);

-- v8 / WS3 (audit follow-up): a durable, off-band-tamper-evident-adjacent
-- record that a ReviewAuthority's required_approver_class was actually
-- satisfied -- required_approver_class alone (v5/CCK-26) is signed but
-- says nothing about whether the approval it names really happened. One
-- row per approval event: review_change's approved:true self-attestation
-- (mechanism='self_attested') at mint for SelfReviewed, or a real
-- MRTR/legacy elicitation round-trip (mechanism='elicitation') at spend
-- for Human. change_id/authority_id/tx_id are nullable because the two
-- call sites don't all have all three at receipt-write time (mint has no
-- tx_id yet; a pure legacy elicitation approval with no ReviewAuthority
-- at all has no authority_id).
-- v10 (CCK-30R2, audit 2026-08-10): signature_provenance distinguishes a
-- signature insert_approval_receipt computed at the true moment of
-- approval (native) from one a schema migration re-derived after the fact
-- over pre-existing rows (legacy_unverified) -- folded into the signed
-- payload itself so it can't be silently upgraded by anyone with raw
-- state.db write access. See authority::receipt's module doc comment.
CREATE TABLE IF NOT EXISTS approval_receipts (
    receipt_id     TEXT PRIMARY KEY,
    change_id      TEXT REFERENCES change_intents(intent_id),
    authority_id   TEXT REFERENCES review_authorities(authority_id) ON DELETE SET NULL,
    subject_digest TEXT NOT NULL,
    approved_by    TEXT NOT NULL,
    mechanism      TEXT NOT NULL,
    decision       TEXT NOT NULL,
    approved_at    REAL NOT NULL,
    tx_id          TEXT REFERENCES edit_transactions(tx_id) ON DELETE SET NULL,
    signature      TEXT,
    signature_provenance TEXT
);
CREATE INDEX IF NOT EXISTS idx_approval_receipts_change ON approval_receipts(change_id);
CREATE INDEX IF NOT EXISTS idx_approval_receipts_authority ON approval_receipts(authority_id);

-- v11 (audit 2026-08-10 follow-up): durable, MCP-protocol-independent
-- pending-review requests for HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW --
-- see db/state_migrations.rs's registered v10->v11 step for the full
-- rationale. A row here is never itself authority to write anything; the
-- edit-time gate still requires status='approved' AND a matching fresh
-- content fingerprint before treating a retry as reviewed.
CREATE TABLE IF NOT EXISTS pending_reviews (
    review_id     TEXT PRIMARY KEY,
    tool          TEXT NOT NULL,
    path          TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    diff_preview  TEXT NOT NULL,
    risk          TEXT,
    hub_kind      TEXT,
    reason        TEXT,
    status        TEXT NOT NULL DEFAULT 'pending',
    created_at    REAL NOT NULL,
    expires_at    REAL NOT NULL,
    decided_at    REAL,
    decided_by    TEXT
);
CREATE INDEX IF NOT EXISTS idx_pending_reviews_lookup ON pending_reviews(path, fingerprint, status);
CREATE INDEX IF NOT EXISTS idx_pending_reviews_status ON pending_reviews(status);
";

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

/// Initializes `state.db` -- the durable-state sibling of `index.db`
/// (project memory, edit-transaction journal, audit ledger, maintenance
/// outbox; see `STATE_SCHEMA_SQL`'s own doc comment for why these live in a
/// separate file at a separate durability posture). Unlike `init_db`, there
/// is no incremental migration story here beyond the one-time data copy
/// (`migrate_legacy_durable_tables`): `state.db` is always created fresh
/// with the CURRENT full schema (including columns/tables that used to be
/// added onto `index.db` incrementally, e.g. `project_memory.content_mac`,
/// `project_memory_fts`), so there's nothing to migrate forward on repeat
/// calls -- every statement here is `IF NOT EXISTS`, safe to run on every
/// startup.
pub fn init_state_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch(STATE_SCHEMA_SQL)?;
    conn.execute_batch(PROJECT_MEMORY_FTS_SQL)?;
    conn.execute_batch(PROJECT_MEMORY_TRIGGERS_SQL)?;
    tracing::info!("state.db schema initialized");
    Ok(())
}

/// One-time, idempotent copy of durable-table rows out of a pre-split
/// `index.db` -- before the state.db split, `project_memory`,
/// `project_memory_refs`, `edit_transactions`, `tx_events`,
/// `maintenance_jobs`, and `audit_ledger` all lived in `index.db` itself.
/// Copy-only: never DROPs/DELETEs anything from `legacy_index_db_path`, so a
/// crash mid-copy just means the remaining rows get picked up on retry.
/// Every insert is `INSERT OR IGNORE` keyed on each table's real primary
/// key, so calling this on every startup (not just once) stays cheap and
/// safe -- already-copied rows are silently skipped. `audit_ledger.seq` is
/// copied verbatim (not re-autoincremented) so `ledger::verify_chain`'s
/// hash chain, which is keyed on row order, stays intact across the copy.
/// `project_memory`'s AFTER INSERT trigger fires per genuinely-inserted row
/// (SQLite skips it for rows an `OR IGNORE` conflict suppresses), so
/// `project_memory_fts` stays in sync without a separate rebuild step.
/// A no-op if `legacy_index_db_path` doesn't exist yet (fresh project) or
/// predates these tables entirely (already-split `index.db`).
pub fn migrate_legacy_durable_tables(
    state_conn: &Connection,
    legacy_index_db_path: &Path,
) -> rusqlite::Result<()> {
    if !legacy_index_db_path.exists() {
        return Ok(());
    }
    state_conn.execute(
        "ATTACH DATABASE ?1 AS legacy",
        [legacy_index_db_path.to_string_lossy().as_ref()],
    )?;
    let result = migrate_legacy_durable_tables_inner(state_conn);
    // Always detach, even on failure, so a caller can retry without leaking
    // the attachment on a `Connection` it may keep reusing.
    state_conn.execute_batch("DETACH DATABASE legacy;")?;
    result
}

fn migrate_legacy_durable_tables_inner(conn: &Connection) -> rusqlite::Result<()> {
    let present: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM legacy.sqlite_master WHERE type = 'table' AND name IN \
             ('project_memory', 'project_memory_refs', 'edit_transactions', \
              'tx_events', 'maintenance_jobs', 'audit_ledger')",
        )?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect()
    };
    let has = |t: &str| present.iter().any(|n| n == t);

    // Parent-before-child for the one real FK (tx_events -> edit_transactions
    // ON DELETE CASCADE); the rest have no cross-table FK so order among them
    // doesn't matter.
    if has("project_memory") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO project_memory \
                (id, topic, content, created_at, updated_at, content_mac, quarantined) \
             SELECT id, topic, content, created_at, updated_at, content_mac, quarantined \
             FROM legacy.project_memory;",
        )?;
    }
    if has("project_memory_refs") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO project_memory_refs (id, topic, ref_path, ref_hash) \
             SELECT id, topic, ref_path, ref_hash FROM legacy.project_memory_refs;",
        )?;
    }
    if has("edit_transactions") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO edit_transactions \
                (tx_id, project_id, path, base_digest, proposed_digest, review_token_id, \
                 state, temp_path, graph_generation_before, graph_generation_after, \
                 created_at, updated_at, error_code, error_detail) \
             SELECT tx_id, project_id, path, base_digest, proposed_digest, review_token_id, \
                    state, temp_path, graph_generation_before, graph_generation_after, \
                    created_at, updated_at, error_code, error_detail \
             FROM legacy.edit_transactions;",
        )?;
    }
    if has("tx_events") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO tx_events \
                (event_id, tx_id, sequence, from_state, to_state, actor, reason, occurred_at) \
             SELECT event_id, tx_id, sequence, from_state, to_state, actor, reason, occurred_at \
             FROM legacy.tx_events;",
        )?;
    }
    if has("maintenance_jobs") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO maintenance_jobs \
                (job_id, job_kind, dedupe_key, state, triggered_by_tx_id, attempts, \
                 available_at, lease_owner, lease_expires_at, last_error, last_completed_at) \
             SELECT job_id, job_kind, dedupe_key, state, triggered_by_tx_id, attempts, \
                    available_at, lease_owner, lease_expires_at, last_error, last_completed_at \
             FROM legacy.maintenance_jobs;",
        )?;
    }
    if has("audit_ledger") {
        conn.execute_batch(
            "INSERT OR IGNORE INTO audit_ledger (seq, prev_hash, event_hash, ts, actor, payload) \
             SELECT seq, prev_hash, event_hash, ts, actor, payload FROM legacy.audit_ledger;",
        )?;
    }
    if !present.is_empty() {
        tracing::info!(tables = ?present, "Migrated legacy durable-state rows into state.db");
    }
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
    migrate_add_column(conn, "symbols", "possible_coreness", "INTEGER")?;
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
    // project_memory's own content_mac/quarantined columns are handled by
    // STATE_SCHEMA_SQL/init_state_db now (project_memory lives in state.db,
    // not here -- see the "Durable state... lives in a SEPARATE file"
    // comment above STATE_SCHEMA_SQL).
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
    // P3 (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md):
    // splits the old `symbol_effects.confidence` into `event_confidence`/
    // `target_confidence` (see the fresh-install CREATE TABLE comment for
    // the semantics). The old `confidence` column is left in place on an
    // upgraded DB rather than dropped -- every migration in this function
    // is purely additive, and `symbol_effects` is fully DELETE-then-
    // reinsert on every reindex of the owning file anyway, so it becomes
    // dead weight the next time each row's file is touched, not a
    // permanent liability.
    migrate_add_column(
        conn,
        "symbol_effects",
        "event_confidence",
        "TEXT NOT NULL DEFAULT 'exact'",
    )?;
    migrate_add_column(
        conn,
        "symbol_effects",
        "target_confidence",
        "TEXT NOT NULL DEFAULT 'exact'",
    )?;
    // PR A (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md,
    // P4.1 Type Resolver Soundness): resolution_source records which pass
    // (same_file_ast / cross_file_unique) owns a resolved type_relations
    // row -- see the fresh-install CREATE TABLE comment for the full
    // rationale. Nullable, no DEFAULT: an upgraded DB's pre-existing
    // resolved rows genuinely don't know their own owner yet (NULL is
    // honest here, not a placeholder) -- they self-heal to a labeled value
    // on the next graph rebuild (cross_file_unique rows) or file reindex
    // (same_file_ast rows), same as every other column in this function.
    migrate_add_column(conn, "type_relations", "resolution_source", "TEXT")?;
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
    fn init_db_versioned_stamps_version_on_fresh_db_and_stays_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db_versioned(&conn).unwrap();
        let stamped: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stamped, INDEX_DB_SCHEMA_VERSION);

        // Calling again (every real startup after the first) must not fail
        // -- on_disk == expected is explicitly the "nothing to do" case.
        init_db_versioned(&conn).unwrap();
        let stamped_again: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stamped_again, INDEX_DB_SCHEMA_VERSION);
    }

    #[test]
    fn init_db_versioned_refuses_when_disk_schema_is_newer_than_this_binary() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulates a file a NEWER CALM binary already created/migrated --
        // this binary must refuse rather than silently write past it.
        conn.pragma_update(None, "user_version", INDEX_DB_SCHEMA_VERSION + 1)
            .unwrap();
        let err = init_db_versioned(&conn).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("newer CALM version") && msg.contains("index.db"),
            "error should name the db and explain the mismatch, got: {msg}"
        );

        // Refusal must happen BEFORE any schema DDL runs -- confirms this
        // by checking the table that DDL would have created doesn't exist.
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='symbols'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        assert!(!table_exists, "must refuse before creating any schema DDL");
    }

    #[test]
    fn init_state_db_versioned_stamps_version_on_fresh_db_and_stays_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db_versioned(&conn).unwrap();
        let stamped: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stamped, STATE_DB_SCHEMA_VERSION);

        init_state_db_versioned(&conn).unwrap();
        let stamped_again: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stamped_again, STATE_DB_SCHEMA_VERSION);
    }

    #[test]
    fn init_state_db_versioned_migrates_a_non_empty_pre_v2_db_instead_of_bootstrapping_over_it() {
        // CCK-28 (audit follow-up): the real production entry point, not
        // `migrate_state_db_to_current` directly -- proves
        // `init_state_db_versioned`'s own is_empty branch selection routes
        // a pre-existing (but unstamped) file straight to the migration
        // chain rather than running the current full-schema DDL first. A
        // pre-v2 install only ever had `project_memory` from this table
        // set -- everything else this migrates in is new.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE project_memory (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                topic       TEXT NOT NULL UNIQUE,
                content     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE TABLE edit_transactions (
                tx_id            TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL,
                path             TEXT NOT NULL,
                base_digest      TEXT NOT NULL,
                proposed_digest  TEXT NOT NULL,
                state            TEXT NOT NULL DEFAULT 'PREPARED',
                created_at       REAL NOT NULL,
                updated_at       REAL NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_memory (topic, content, created_at, updated_at) \
             VALUES ('gotcha', 'remember this', 't0', 't0')",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0,
            "must start unstamped, same as every real pre-versioning install"
        );

        init_state_db_versioned(&conn).unwrap();

        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            STATE_DB_SCHEMA_VERSION
        );
        for table in ["evidence_snapshots", "change_intents", "review_authorities"] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(exists, "{table} should exist after migrating this fixture");
        }
        let memory_content: String = conn
            .query_row(
                "SELECT content FROM project_memory WHERE topic = 'gotcha'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            memory_content, "remember this",
            "pre-existing rows must survive"
        );
    }

    #[test]
    fn edit_transactions_authority_id_carries_a_real_foreign_key() {
        // CCK-R6 (audit follow-up): the comment on this column used to be
        // the only thing tying it to review_authorities -- now it's a real
        // FK, checked the same way call_site_identity_schema_uses_byte_
        // spans_and_edge_foreign_key checks call_edges.call_site_id above.
        let conn = Connection::open_in_memory().unwrap();
        init_state_db_versioned(&conn).unwrap();

        let foreign_keys: Vec<(String, String, String, String)> = conn
            .prepare("PRAGMA foreign_key_list(edit_transactions)")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(2)?, row.get(3)?, row.get(4)?, row.get(6)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            foreign_keys.iter().any(|(table, from, to, on_delete)| {
                table == "review_authorities"
                    && from == "authority_id"
                    && to == "authority_id"
                    && on_delete == "SET NULL"
            }),
            "edit_transactions.authority_id must carry a database foreign-key \
             relationship to review_authorities with ON DELETE SET NULL, got {foreign_keys:?}"
        );
    }

    #[test]
    fn deleting_a_review_authority_clears_but_does_not_cascade_the_edit_transaction() {
        // The durable edit-transaction journal must outlive a future
        // retention/GC pass over review_authorities -- only the "which
        // authority approved this" link should go away.
        let conn = Connection::open_in_memory().unwrap();
        init_state_db_versioned(&conn).unwrap();
        conn.execute(
            "INSERT INTO evidence_snapshots \
             (snapshot_id, source_catalog_digest, graph_generation, freshness_class, created_at) \
             VALUES ('SNP-1', 'digest-1', 1, 'current', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO change_intents (intent_id, kind, reason, snapshot_id, created_at) \
             VALUES ('INT-1', 'body', 'test fixture', 'SNP-1', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO review_authorities \
             (authority_id, intent_id, snapshot_id, graph_generation, caller_set_digest, \
              analysis_version, policy_digest, principal, target_scope_digest, nonce, \
              expires_at, signature, created_at, consumed_at) \
             VALUES ('AUTH-1', 'INT-1', 'SNP-1', 1, 'callers', 'av', 'pd', 'session:x', '', \
              'nonce', 1.0, 'sig', 0.0, NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edit_transactions \
             (tx_id, project_id, path, base_digest, proposed_digest, authority_id, state, \
              created_at, updated_at) \
             VALUES ('TXN-1', 'proj', 'a.rs', 'base', 'proposed', 'AUTH-1', 'PREPARED', 0.0, 0.0)",
            [],
        )
        .unwrap();

        conn.execute(
            "DELETE FROM review_authorities WHERE authority_id = 'AUTH-1'",
            [],
        )
        .unwrap();

        let (path, authority_id): (String, Option<String>) = conn
            .query_row(
                "SELECT path, authority_id FROM edit_transactions WHERE tx_id = 'TXN-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, "a.rs", "the transaction journal row must survive");
        assert_eq!(
            authority_id, None,
            "only the dangling authority_id link should be cleared"
        );
    }

    #[test]
    fn init_state_db_versioned_refuses_when_disk_schema_is_newer_than_this_binary() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", STATE_DB_SCHEMA_VERSION + 1)
            .unwrap();
        let err = init_state_db_versioned(&conn).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("newer CALM version") && msg.contains("state.db"),
            "error should name the db and explain the mismatch, got: {msg}"
        );
    }

    #[test]
    fn refuse_if_schema_newer_allows_older_and_equal_versions() {
        let conn = Connection::open_in_memory().unwrap();
        // SQLite default (never stamped) is 0 -- always <= any expected.
        assert!(refuse_if_schema_newer(&conn, 5, "test.db").is_ok());

        conn.pragma_update(None, "user_version", 5i64).unwrap();
        assert!(refuse_if_schema_newer(&conn, 5, "test.db").is_ok());
        assert!(refuse_if_schema_newer(&conn, 10, "test.db").is_ok());
        assert!(refuse_if_schema_newer(&conn, 4, "test.db").is_err());
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
        init_state_db(&conn).unwrap();

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

    /// Simulates a pre-split `index.db`: same durable-table DDL as
    /// `STATE_SCHEMA_SQL`, just living in a file that plays the role of the
    /// old, unsplit `index.db`.
    fn write_legacy_index_db(path: &std::path::Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(STATE_SCHEMA_SQL).unwrap();
    }

    #[test]
    fn migrate_legacy_durable_tables_copies_rows_from_a_pre_split_index_db() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("index.db");
        write_legacy_index_db(&legacy_path);
        {
            let legacy_conn = Connection::open(&legacy_path).unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO project_memory (topic, content, created_at, updated_at) \
                     VALUES ('t', 'legacy note content', '2024-01-01', '2024-01-01')",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO project_memory_refs (topic, ref_path, ref_hash) \
                     VALUES ('t', 'src/lib.rs', 'abc123')",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO edit_transactions \
                        (tx_id, project_id, path, base_digest, proposed_digest, created_at, updated_at) \
                     VALUES ('tx1', 'proj', 'f.rs', 'base', 'proposed', 1.0, 1.0)",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO tx_events \
                        (event_id, tx_id, sequence, from_state, to_state, actor, reason, occurred_at) \
                     VALUES ('ev1', 'tx1', 1, 'NONE', 'PREPARED', 'agent', 'begin', 1.0)",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO maintenance_jobs (job_id, job_kind, dedupe_key, available_at) \
                     VALUES ('j1', 'reindex', 'reindex', 1.0)",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO audit_ledger (seq, prev_hash, event_hash, ts, actor, payload) \
                     VALUES (1, 'genesis', 'h1', 1.0, 'agent', '{}')",
                    [],
                )
                .unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO audit_ledger (seq, prev_hash, event_hash, ts, actor, payload) \
                     VALUES (2, 'h1', 'h2', 2.0, 'agent', '{}')",
                    [],
                )
                .unwrap();
        }

        let state_conn = Connection::open_in_memory().unwrap();
        init_state_db(&state_conn).unwrap();
        migrate_legacy_durable_tables(&state_conn, &legacy_path).unwrap();

        let count = |table: &str| -> i64 {
            state_conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count("project_memory"), 1);
        assert_eq!(count("project_memory_refs"), 1);
        assert_eq!(count("edit_transactions"), 1);
        assert_eq!(count("tx_events"), 1);
        assert_eq!(count("maintenance_jobs"), 1);
        assert_eq!(count("audit_ledger"), 2);

        // FTS trigger fired for the copied row, not just a table-level copy.
        let fts_hits: i64 = state_conn
            .query_row(
                "SELECT COUNT(*) FROM project_memory_fts WHERE project_memory_fts MATCH 'legacy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_hits, 1);

        // audit_ledger.seq is copied verbatim, not renumbered -- the hash
        // chain in `payload`/`prev_hash` is keyed on exact row order.
        let seqs: Vec<i64> = {
            let mut stmt = state_conn
                .prepare("SELECT seq FROM audit_ledger ORDER BY seq")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn migrate_legacy_durable_tables_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("index.db");
        write_legacy_index_db(&legacy_path);
        {
            let legacy_conn = Connection::open(&legacy_path).unwrap();
            legacy_conn
                .execute(
                    "INSERT INTO project_memory (topic, content, created_at, updated_at) \
                     VALUES ('t', 'c', '2024-01-01', '2024-01-01')",
                    [],
                )
                .unwrap();
        }

        let state_conn = Connection::open_in_memory().unwrap();
        init_state_db(&state_conn).unwrap();
        migrate_legacy_durable_tables(&state_conn, &legacy_path).unwrap();
        migrate_legacy_durable_tables(&state_conn, &legacy_path).unwrap();
        migrate_legacy_durable_tables(&state_conn, &legacy_path).unwrap();

        let count: i64 = state_conn
            .query_row("SELECT COUNT(*) FROM project_memory", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "repeat migrations must not duplicate rows");
    }

    #[test]
    fn migrate_legacy_durable_tables_is_a_no_op_when_legacy_db_is_missing() {
        let state_conn = Connection::open_in_memory().unwrap();
        init_state_db(&state_conn).unwrap();

        let missing = std::path::Path::new("/nonexistent/does-not-exist/index.db");
        migrate_legacy_durable_tables(&state_conn, missing).unwrap();

        let count: i64 = state_conn
            .query_row("SELECT COUNT(*) FROM project_memory", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_legacy_durable_tables_is_a_no_op_when_legacy_db_predates_durable_tables() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("index.db");
        {
            // An already-split (or pre-durable-state) `index.db`: has the
            // rebuildable schema but none of the durable tables.
            let legacy_conn = Connection::open(&legacy_path).unwrap();
            legacy_conn.execute_batch(SCHEMA_SQL).unwrap();
        }

        let state_conn = Connection::open_in_memory().unwrap();
        init_state_db(&state_conn).unwrap();
        migrate_legacy_durable_tables(&state_conn, &legacy_path).unwrap();

        let count: i64 = state_conn
            .query_row("SELECT COUNT(*) FROM project_memory", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
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
