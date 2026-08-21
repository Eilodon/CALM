//! `EvidenceSnapshot` -- CCK-06
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! A canonical, content-addressed summary of "what the index currently
//! believes is true" at the moment authority is about to be minted --
//! source content plus the non-source inputs that can change
//! resolution/graph semantics without any source byte changing.
//! [`EvidenceSnapshot::persist`]/[`EvidenceSnapshot::load`] (CCK-07) are
//! the only durable part; everything else here is pure computation.
//!
//! **Adjustment (blueprint's own note on this PR):** reconciliation
//! plumbing already exists -- `index_input_drift` (`indexer::refresh`)
//! already answers "does the index match disk". `freshness_class` here
//! *reads* that answer; it does not re-derive a second, parallel notion of
//! staleness. Inventing a second staleness signal would itself violate
//! invariant #2 (no stale evidence may grant authority) by giving a future
//! caller two answers that could disagree.
//!
//! Digest material intentionally stays self-contained in this module rather
//! than reaching into `indexer::refresh`'s private fingerprint internals --
//! `GLOBAL_CONFIGURATION_PATHS` below is a deliberate, independently-stable
//! duplicate of the same two paths, not a shared dependency, so this module
//! never needs `indexer::refresh` to widen visibility on its behalf.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

use crate::digest::evidence_digest;
use crate::indexer::refresh::{IndexInputDrift, InputCatalog, index_input_drift};

/// Mirrors `indexer::refresh`'s private `GLOBAL_CONFIGURATION_PATHS` --
/// kept as a small, independently-stable duplicate (both are "the global
/// config files CALM reads", unlikely to change independently) rather than
/// widening that module's visibility for one caller.
const GLOBAL_CONFIGURATION_PATHS: [&str; 2] = ["config.json", ".calm/config.json"];

/// How much a caller should trust an `EvidenceSnapshot` as proof the index
/// reflects disk right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessClass {
    /// A full reconciliation against disk completed immediately before this
    /// snapshot was computed -- the strongest guarantee available. Never
    /// inferred automatically; only [`EvidenceSnapshot::compute_after_reconciliation`]
    /// sets this, so a caller can't claim it without actually reconciling.
    Reconciled,
    /// `index_input_drift` reports `Clean` right now, but no reconciliation
    /// was forced specifically for this snapshot.
    Current,
    /// `index_input_drift` reports `Context`, `Configuration`, or `Unknown`
    /// -- the index may not reflect disk. Fail-closed on `Unknown` (an old
    /// database or a future policy version), same posture `index_input_drift`
    /// itself takes.
    Degraded,
}

impl PartialOrd for FreshnessClass {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FreshnessClass {
    /// Strength order, weakest first -- mirrors the ranking `persist`'s
    /// upsert already encodes in SQL (`CASE ... WHEN 'reconciled' THEN 2
    /// WHEN 'current' THEN 1 ELSE 0 END`). Spelled out explicitly rather
    /// than derived: declaration order alone (`Reconciled` listed first,
    /// as the headline variant for readers) does not match this ranking.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn rank(c: FreshnessClass) -> u8 {
            match c {
                FreshnessClass::Degraded => 0,
                FreshnessClass::Current => 1,
                FreshnessClass::Reconciled => 2,
            }
        }
        rank(*self).cmp(&rank(*other))
    }
}

impl FreshnessClass {
    /// WS2 (audit follow-up): the tiered freshness bar the blueprint
    /// originally called for -- Low/Medium risk (`required_approver_class`
    /// other than `Human`) accepts either a cheap fingerprint-based
    /// `Current` read or a full `Reconciled` re-scan; High risk
    /// (`Human`-required) accepts nothing short of `Reconciled` --
    /// watcher/index health alone (`Current`) is never proof for the tier
    /// where a mistake matters most (blueprint's own wording for this PR).
    /// `Degraded` fails every tier unconditionally.
    pub fn meets_bar_for(self, required_approver_class: crate::policy::ApproverClass) -> bool {
        match self {
            Self::Degraded => false,
            Self::Reconciled => true,
            Self::Current => required_approver_class != crate::policy::ApproverClass::Human,
        }
    }

    /// Stable lowercase name persisted in `evidence_snapshots.freshness_class`
    /// -- round-trips through [`FreshnessClass::parse`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reconciled => "reconciled",
            Self::Current => "current",
            Self::Degraded => "degraded",
        }
    }

    /// Inverse of [`as_str`](Self::as_str); `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reconciled" => Some(Self::Reconciled),
            "current" => Some(Self::Current),
            "degraded" => Some(Self::Degraded),
            _ => None,
        }
    }
}

fn drift_to_freshness(drift: IndexInputDrift) -> FreshnessClass {
    match drift {
        IndexInputDrift::Clean => FreshnessClass::Current,
        IndexInputDrift::Context | IndexInputDrift::Configuration | IndexInputDrift::Unknown => {
            FreshnessClass::Degraded
        }
    }
}

/// A canonical, content-addressed summary of index state at one moment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceSnapshot {
    /// `"SNP-sha256:<hex>"` over every field below plus the graph/package
    /// derivation versions -- changes if, and only if, something this
    /// snapshot is meant to attest to actually changed.
    pub snapshot_id: String,
    /// `evidence_digest` over the sorted `path\0hash` rows of `file_index`.
    pub source_catalog_digest: String,
    /// Current `graph_generation_state.generation` (0 if never indexed).
    pub graph_generation: i64,
    /// `evidence_digest` over the sorted `scip_overlay_state` rows -- see
    /// [`provider_state_digest`] for what it does and does not capture.
    pub provider_state_digest: String,
    pub freshness_class: FreshnessClass,
}

impl EvidenceSnapshot {
    /// Computes a snapshot from `conn`'s current state without forcing
    /// reconciliation first -- `freshness_class` reflects whatever
    /// `index_input_drift` reports right now (`Current` or `Degraded`,
    /// never `Reconciled`).
    /// Computes a snapshot from `conn`'s current state without forcing
    /// reconciliation first -- `freshness_class` reflects whatever
    /// `index_input_drift` reports right now (`Current` or `Degraded`,
    /// never `Reconciled`). A `Current` result additionally gets a cheap
    /// live-disk spot-check (2.1, Wave 2 -- `live_mtime_drift`) that can
    /// still downgrade it to `Degraded`: `index_input_drift` only tracks
    /// config/context fingerprints, not "has a source file changed on disk
    /// since the last successful index" -- that's a separate lag window
    /// (the watcher's debounce/reconciliation interval), closed here rather
    /// than left to `source_catalog_digest` (which only ever reads DB rows,
    /// never live bytes). The check is skipped when drift is already
    /// `Degraded` -- no need to pay for it when the answer can't change.
    pub fn compute(conn: &Connection, project_root: &Path) -> rusqlite::Result<Self> {
        let catalog = InputCatalog::for_project(project_root);
        let drift = index_input_drift(conn, &catalog)?;
        let mut freshness_class = drift_to_freshness(drift);
        if freshness_class == FreshnessClass::Current && live_mtime_drift(conn, project_root)? {
            freshness_class = FreshnessClass::Degraded;
        }
        Self::build(conn, project_root, freshness_class)
    }

    /// Same as [`compute`](Self::compute), but for a caller that just ran a
    /// full reconciliation and wants that reflected as `Reconciled` rather
    /// than re-derived from `index_input_drift` -- which would report
    /// `Current` right after a clean reconcile, indistinguishable from "was
    /// already clean and nobody actually checked".
    pub fn compute_after_reconciliation(
        conn: &Connection,
        project_root: &Path,
    ) -> rusqlite::Result<Self> {
        Self::build(conn, project_root, FreshnessClass::Reconciled)
    }

    /// Same as [`compute`](Self::compute), but also consults the durable
    /// `evidence_snapshots` table (CCK-07, `state_conn`) for this exact
    /// content's `snapshot_id` -- if a stronger freshness class was ever
    /// recorded for identical content (e.g. a past full reconciliation via
    /// [`compute_after_reconciliation`](Self::compute_after_reconciliation)),
    /// that's honored here too, not only at persist-time. `snapshot_id` is
    /// content-addressed over what the DB currently believes, so a disk
    /// change that has already been reindexed changes the id and this
    /// lookup simply misses -- no TTL window to race there. A disk change
    /// NOT yet reflected in any DB row is a different lag window, closed
    /// separately by `compute`'s own `live_mtime_drift` check (2.1, Wave 2):
    /// that degrades `freshness_class` to `Degraded` without needing a new
    /// `snapshot_id`, since the DB-visible content genuinely hasn't changed
    /// yet.
    pub fn compute_with_recorded_freshness(
        conn: &Connection,
        project_root: &Path,
        state_conn: &Connection,
    ) -> rusqlite::Result<Self> {
        // Wave 6 (audit follow-up, P0-A): inlines `compute`'s own drift
        // derivation instead of calling it, so this function can tell
        // WHETHER `live_mtime_drift` is what produced a `Degraded` result --
        // that distinction is the whole fix. `live_mtime_drift` closes a
        // real, currently-observed lag window (disk changed, DB not
        // reindexed yet); a recorded snapshot from BEFORE that change can
        // share the same `snapshot_id` (content-addressed over DB rows
        // only, which haven't moved) and must never be allowed to silently
        // overturn what this call just observed live. Without this
        // distinction, the block below would re-promote a live-drifted
        // `Degraded` straight back to a stale `Reconciled`, and
        // `mint_review_authority_for_edit_context` would mint an authority
        // on a false freshness guarantee that survives all the way to
        // `edit.rs`'s spend-time check.
        let catalog = InputCatalog::for_project(project_root);
        let drift = index_input_drift(conn, &catalog)?;
        let mut freshness_class = drift_to_freshness(drift);
        let live_drifted =
            freshness_class == FreshnessClass::Current && live_mtime_drift(conn, project_root)?;
        if live_drifted {
            freshness_class = FreshnessClass::Degraded;
        }
        let mut snapshot = Self::build(conn, project_root, freshness_class)?;
        if !live_drifted
            && let Some(recorded) = Self::load(state_conn, &snapshot.snapshot_id)?
            && recorded.freshness_class > snapshot.freshness_class
        {
            snapshot.freshness_class = recorded.freshness_class;
        }
        Ok(snapshot)
    }

    fn build(
        conn: &Connection,
        project_root: &Path,
        freshness_class: FreshnessClass,
    ) -> rusqlite::Result<Self> {
        let source_catalog_digest = source_catalog_digest(conn)?;
        let graph_generation = current_graph_generation(conn)?;
        let config_digest = config_digest(project_root)?;
        let provider_state_digest = provider_state_digest(conn)?;

        let material = format!(
            "evidence-snapshot-v1\n\
             source_catalog_digest={source_catalog_digest}\n\
             graph_generation={graph_generation}\n\
             config_digest={config_digest}\n\
             provider_state_digest={provider_state_digest}\n\
             graph_derivation_version={}\n\
             package_graph_version={}\n",
            crate::graph::digest::GRAPH_DERIVATION_VERSION,
            crate::indexer::package_deps::PACKAGE_GRAPH_VERSION,
        );
        let snapshot_id = format!("SNP-{}", evidence_digest(material.as_bytes()));

        Ok(Self {
            snapshot_id,
            source_catalog_digest,
            graph_generation,
            provider_state_digest,
            freshness_class,
        })
    }

    /// Persists this snapshot into `evidence_snapshots` (CCK-07,
    /// `db::state_migrations`'s v1->v2 step) -- `state_conn` is a
    /// **state.db** connection, distinct from the `conn` (index.db)
    /// `compute`/`build` read from. `snapshot_id` is content-addressed, so
    /// re-persisting an identical snapshot is a harmless `INSERT OR
    /// IGNORE`, not a duplicate/conflict; `created_at` reflects the first
    /// time this exact snapshot was ever persisted, not this call.
    pub fn persist(&self, state_conn: &Connection) -> rusqlite::Result<()> {
        // CCK-R2 (audit follow-up, docs/plans/2026-08-08-master-change-
        // control-execution-blueprint.md): `freshness_class` is NOT part of
        // the `snapshot_id` hash (deliberately -- it's an observation ABOUT
        // the snapshot's content, not part of the content itself), so two
        // `EvidenceSnapshot` values with different freshness can share a
        // `snapshot_id`. A plain `INSERT OR IGNORE` let whichever one was
        // persisted FIRST win forever -- a `Current` snapshot persisted
        // before a `Reconciled` one for the same content would silently
        // swallow the stronger, later observation. This upsert instead only
        // ever moves `freshness_class` toward stronger (`reconciled` >
        // `current` > `degraded`), never weaker and never silently dropped.
        state_conn.execute(
            "INSERT INTO evidence_snapshots \
             (snapshot_id, source_catalog_digest, graph_generation, provider_state_digest, \
              freshness_class, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(snapshot_id) DO UPDATE SET freshness_class = excluded.freshness_class \
             WHERE (CASE excluded.freshness_class \
                        WHEN 'reconciled' THEN 2 WHEN 'current' THEN 1 ELSE 0 END) \
                 > (CASE evidence_snapshots.freshness_class \
                        WHEN 'reconciled' THEN 2 WHEN 'current' THEN 1 ELSE 0 END)",
            params![
                self.snapshot_id,
                self.source_catalog_digest,
                self.graph_generation,
                self.provider_state_digest,
                self.freshness_class.as_str(),
                now_epoch_secs(),
            ],
        )?;
        Ok(())
    }

    /// `Ok(None)` when no row matches. `state_conn` is a state.db
    /// connection, same as [`persist`](Self::persist).
    pub fn load(state_conn: &Connection, snapshot_id: &str) -> rusqlite::Result<Option<Self>> {
        let row: Option<(String, String, i64, String, String)> = state_conn
            .query_row(
                "SELECT snapshot_id, source_catalog_digest, graph_generation, \
                        provider_state_digest, freshness_class \
                 FROM evidence_snapshots WHERE snapshot_id = ?1",
                params![snapshot_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((
            snapshot_id,
            source_catalog_digest,
            graph_generation,
            provider_state_digest,
            freshness_str,
        )) = row
        else {
            return Ok(None);
        };
        let freshness_class = FreshnessClass::parse(&freshness_str).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("evidence_snapshots.freshness_class {freshness_str:?} is not a known FreshnessClass")
                    .into(),
            )
        })?;
        Ok(Some(Self {
            snapshot_id,
            source_catalog_digest,
            graph_generation,
            provider_state_digest,
            freshness_class,
        }))
    }
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `evidence_digest` over every `(path, hash)` row in `file_index`, sorted
/// by path so digest determinism never depends on SQLite's row-return
/// order (the same discipline PR B4 applied to callee/type-relation
/// rendering in `graph/digest.rs`).
fn source_catalog_digest(conn: &Connection) -> rusqlite::Result<String> {
    let mut stmt = conn.prepare("SELECT path, hash FROM file_index ORDER BY path")?;
    let rows = stmt.query_map([], |r| {
        let path: String = r.get(0)?;
        let hash: String = r.get(1)?;
        Ok(format!("{path}\0{hash}"))
    })?;
    let mut material = String::from("source-catalog-v1\n");
    for row in rows {
        material.push_str(&row?);
        material.push('\n');
    }
    Ok(evidence_digest(material.as_bytes()))
}

/// Live-disk companion to `source_catalog_digest` (2.1, Wave 2): that digest
/// only ever reads DB rows, so a file edited on disk after its last
/// successful index -- but before the watcher's debounce/reconciliation
/// catches up -- is invisible to it (`snapshot_id` stays keyed on the stale
/// DB hash). This closes that specific lag window cheaply by comparing each
/// `file_index` row's live mtime (via `mtime_secs`, the exact function the
/// indexer itself stamps rows with -- reused, not reimplemented, so both
/// sides of the comparison come from one conversion) to its stored `mtime`
/// column. Deliberately mtime-only, not a content rehash: `compute` runs on
/// every gated edit (`edit_lines_impl_gated`), not just `edit_context`, so
/// re-reading every indexed file's bytes here would double that cost across
/// the whole catalog on every edit for a signal `source_catalog_digest`
/// already gets once real reindexing happens.
///
/// **Documented residual, not silently claimed as caught:** a live mtime
/// that matches the stored one on a file whose content genuinely differs
/// (a same-timestamp overwrite) is not detected by this signal alone --
/// closing that would need a full content rehash, deliberately not done
/// here (see 2.1's design decision,
/// docs/plans/2026-08-20-truth-kernel-hardening-execution-plan.md).
///
/// Fail-closed like `index_input_drift`'s own `Unknown` posture: a file
/// missing from disk and a `NULL` stored `mtime` (a pre-migration row, or
/// one indexed before this column existed) both count as drift rather than
/// being silently skipped. Short-circuits on the first mismatch -- this is
/// a boolean gate, not a digest, so there is no reason to keep scanning
/// once drift is already proven.
fn live_mtime_drift(conn: &Connection, project_root: &Path) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare("SELECT path, mtime FROM file_index")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let stored_mtime: Option<f64> = row.get(1)?;
        let Some(stored_mtime) = stored_mtime else {
            return Ok(true);
        };
        let full_path = project_root.join(&path);
        if !full_path.exists() {
            return Ok(true);
        }
        let live_mtime = crate::indexer::pipeline::mtime_secs(&full_path);
        if live_mtime != stored_mtime {
            return Ok(true);
        }
    }
    // Wave 6 (audit follow-up, P0-A.3): a brand-new source file added to
    // disk since the last index has no `file_index` row at all, so the
    // loop above -- which only ever iterates EXISTING rows -- can't see it.
    // A live-tree-walk fix (compare `collect_source_files`'s live path set
    // against `file_index`) was implemented and then REVERTED after it
    // broke 7 existing tests, all with the same root cause: this
    // codebase's own test fixtures routinely `std::fs::write` a file and
    // insert directly into `symbols`/persist a reconciled InputCatalog
    // WITHOUT a matching `file_index` row (a fast test-setup shortcut, not
    // a bug in the tests). That revealed a real, unresolved semantic
    // question, not just a test-fixture inconvenience: `index_input_drift`
    // (and therefore `Current`/`Reconciled`) was designed to answer "does
    // the index match disk" for config/context fingerprints specifically,
    // not "has every matching file actually been read into `file_index`" --
    // conflating the two would also fire on the normal, transient,
    // non-adversarial case of a freshly-added file the watcher hasn't
    // debounced yet, at every risk tier, not just the ones that actually
    // need Reconciled-strength freshness. Left as a documented, deliberate
    // residual pending a real design decision (e.g. a distinct signal
    // rather than folding into this boolean), not silently dropped.
    Ok(false)
}

/// `evidence_digest` over the sorted `provider\0cache_key\0upgraded\0
/// ruled_out\0inserted\0match_rate` rows of `scip_overlay_state` -- the
/// same DB-resident table `scip::state` uses in place of the old
/// `.calm/<provider>.cache` sidecar files (see that module's doc comment).
/// This is the "did a SCIP/LSP provider's proof coverage change" half of
/// the snapshot: a provider that ran again and produced the same
/// cache_key/counts/match_rate did not change anything this snapshot needs
/// to attest to, so `last_run_unix` (wall-clock, not content) is
/// deliberately excluded -- including it would flip `snapshot_id` on every
/// redundant re-run and defeat the "changes iff something real changed"
/// contract every other digest in this module upholds. A project with no
/// SCIP/LSP provider ever run (table genuinely absent, checked against
/// `sqlite_master` rather than inferred from a failed `prepare`) still
/// gets a stable digest of the header alone -- "no provider state" is
/// itself a value, not an error. WS2 (audit follow-up): a `prepare`/row
/// failure against a table that DOES exist is a real anomaly (corruption,
/// lock, permission) and must propagate, not collapse into the same "no
/// provider state" value a healthy-but-empty table produces.
fn provider_state_digest(conn: &Connection) -> rusqlite::Result<String> {
    let mut material = String::from("provider-state-v1\n");
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'scip_overlay_state'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !table_exists {
        return Ok(evidence_digest(material.as_bytes()));
    }
    let mut stmt = conn.prepare(
        "SELECT provider, cache_key, upgraded, ruled_out, inserted, match_rate \
         FROM scip_overlay_state ORDER BY provider",
    )?;
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let provider: String = r.get(0)?;
            let cache_key: Option<String> = r.get(1)?;
            let upgraded: i64 = r.get(2)?;
            let ruled_out: i64 = r.get(3)?;
            let inserted: i64 = r.get(4)?;
            let match_rate: f64 = r.get(5)?;
            Ok(format!(
                "{provider}\0{}\0{upgraded}\0{ruled_out}\0{inserted}\0{match_rate}",
                cache_key.as_deref().unwrap_or("")
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for row in rows {
        material.push_str(&row);
        material.push('\n');
    }
    Ok(evidence_digest(material.as_bytes()))
}

/// WS2 (audit follow-up): distinguishes "never indexed" (no row yet -- a
/// legitimate, expected state for a fresh project, safe default 0) from a
/// genuine DB read error (corruption, lock, permission) via `.optional()`
/// -- previously `.unwrap_or(0)` collapsed both into the same value, so a
/// read failure silently minted authority against a fabricated
/// `graph_generation=0` instead of refusing. `guardrails.rs::edit_context`
/// has its own, separate read of the same table (a different, lower-
/// stakes context -- not part of a signed `EvidenceSnapshot`) and is
/// unchanged by this.
fn current_graph_generation(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT generation FROM graph_generation_state WHERE id = 1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map(|generation| generation.unwrap_or(0))
}

/// `evidence_digest` over the concatenated bytes of every file in
/// [`GLOBAL_CONFIGURATION_PATHS`] that exists, in fixed order -- a missing
/// file contributes its path with no bytes (present vs. absent still
/// changes the digest), so deleting a config file is not indistinguishable
/// from an unchanged one. WS2 (audit follow-up): only a genuinely absent
/// file (`NotFound`) is treated as "no bytes" -- any other read error
/// (permission denied, I/O error) propagates instead of being silently
/// coalesced into the same "absent" value a real DB-corruption/permission
/// problem should never be indistinguishable from.
fn config_digest(project_root: &Path) -> rusqlite::Result<String> {
    let mut material = Vec::from(*b"config-digest-v1\n");
    for relative in GLOBAL_CONFIGURATION_PATHS {
        material.extend_from_slice(relative.as_bytes());
        material.push(b'\0');
        match std::fs::read(project_root.join(relative)) {
            Ok(bytes) => material.extend_from_slice(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(e))),
        }
        material.push(b'\n');
    }
    Ok(evidence_digest(&material))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn conn_with_file_index(rows: &[(&str, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for (path, hash) in rows {
            conn.execute(
                "INSERT INTO file_index (path, hash, last_indexed) VALUES (?1, ?2, 0)",
                rusqlite::params![path, hash],
            )
            .unwrap();
        }
        conn
    }

    fn tmp_project() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn state_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_state_db(&conn).unwrap();
        crate::db::state_migrations::migrate_state_db_to_current(&conn).unwrap();
        conn
    }

    #[test]
    fn digest_is_deterministic_across_repeated_computation() {
        let conn = conn_with_file_index(&[("a.rs", "h1"), ("b.rs", "h2")]);
        let root = tmp_project();
        let one = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        let two = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(one.snapshot_id, two.snapshot_id);
        assert_eq!(one.source_catalog_digest, two.source_catalog_digest);
    }

    #[test]
    fn source_catalog_digest_is_invariant_to_file_index_row_order() {
        let forward = conn_with_file_index(&[("a.rs", "h1"), ("b.rs", "h2"), ("c.rs", "h3")]);
        let backward = conn_with_file_index(&[("c.rs", "h3"), ("a.rs", "h1"), ("b.rs", "h2")]);
        let root = tmp_project();
        let forward_snap = EvidenceSnapshot::compute(&forward, root.path()).unwrap();
        let backward_snap = EvidenceSnapshot::compute(&backward, root.path()).unwrap();
        assert_eq!(
            forward_snap.source_catalog_digest,
            backward_snap.source_catalog_digest
        );
        assert_eq!(forward_snap.snapshot_id, backward_snap.snapshot_id);
    }

    #[test]
    fn graph_generation_change_flips_the_snapshot_id() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(before.graph_generation, 0);

        conn.execute(
            "UPDATE graph_generation_state SET generation = 7 WHERE id = 1",
            [],
        )
        .unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(after.graph_generation, 7);
        assert_ne!(before.snapshot_id, after.snapshot_id);
        // graph_generation alone must not move source_catalog_digest --
        // that field is keyed only on file_index content.
        assert_eq!(before.source_catalog_digest, after.source_catalog_digest);
    }

    #[test]
    fn provider_run_state_change_flips_the_snapshot_id_without_touching_source_catalog() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        crate::scip::state::write_state(&conn, "rust", "cache-key-1", 3, 1, 5, 0.9);
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        assert_ne!(before.provider_state_digest, after.provider_state_digest);
        assert_ne!(before.snapshot_id, after.snapshot_id);
        // source_catalog_digest is keyed only on file_index content -- a
        // provider run touches neither the files nor their hashes.
        assert_eq!(before.source_catalog_digest, after.source_catalog_digest);
    }

    #[test]
    fn provider_run_state_re_run_with_identical_results_does_not_flip_the_snapshot_id() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        crate::scip::state::write_state(&conn, "rust", "cache-key-1", 3, 1, 5, 0.9);
        let first = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        // A redundant re-run producing byte-identical counts/cache_key
        // (only `last_run_unix` differs) must not be mistaken for new
        // evidence -- see `provider_state_digest`'s doc comment.
        crate::scip::state::write_state(&conn, "rust", "cache-key-1", 3, 1, 5, 0.9);
        let second = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        assert_eq!(first.provider_state_digest, second.provider_state_digest);
        assert_eq!(first.snapshot_id, second.snapshot_id);
    }

    #[test]
    fn provider_relevant_config_change_flips_the_snapshot_id() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        std::fs::write(
            root.path().join("config.json"),
            br#"{"languages":["rust"]}"#,
        )
        .unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_ne!(before.snapshot_id, after.snapshot_id);
    }

    #[test]
    fn source_content_change_flips_the_snapshot_id_but_not_via_graph_generation() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        conn.execute(
            "UPDATE file_index SET hash = 'h1-changed' WHERE path = 'a.rs'",
            [],
        )
        .unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_ne!(before.source_catalog_digest, after.source_catalog_digest);
        assert_ne!(before.snapshot_id, after.snapshot_id);
        assert_eq!(before.graph_generation, after.graph_generation);
    }

    #[test]
    fn clean_drift_is_current_not_reconciled_or_degraded() {
        use crate::policy::ApproverClass;
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        // No index_input_state row persisted -> index_input_drift reports
        // Unknown (fail-closed), which must classify as Degraded, not
        // Current -- confirms the fail-closed default flows through.
        let snap = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(snap.freshness_class, FreshnessClass::Degraded);
        assert!(
            !snap
                .freshness_class
                .meets_bar_for(ApproverClass::SelfReviewed)
        );
        assert!(!snap.freshness_class.meets_bar_for(ApproverClass::Human));
    }

    #[test]
    fn compute_after_reconciliation_is_always_reconciled_regardless_of_drift() {
        use crate::policy::ApproverClass;
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        let snap = EvidenceSnapshot::compute_after_reconciliation(&conn, root.path()).unwrap();
        assert_eq!(snap.freshness_class, FreshnessClass::Reconciled);
        assert!(
            snap.freshness_class
                .meets_bar_for(ApproverClass::SelfReviewed)
        );
        assert!(snap.freshness_class.meets_bar_for(ApproverClass::Human));
    }

    #[test]
    // WS2 (audit follow-up): Degraded fails every tier; Current is enough
    // for Low/Medium (SelfReviewed) but NOT for High (Human) -- only a
    // Reconciled snapshot clears the bar for a Human-required change.
    fn high_risk_requires_reconciled_not_just_current() {
        use crate::policy::ApproverClass;
        assert!(!FreshnessClass::Degraded.meets_bar_for(ApproverClass::SelfReviewed));
        assert!(!FreshnessClass::Degraded.meets_bar_for(ApproverClass::Human));
        assert!(FreshnessClass::Current.meets_bar_for(ApproverClass::SelfReviewed));
        assert!(!FreshnessClass::Current.meets_bar_for(ApproverClass::Human));
        assert!(FreshnessClass::Reconciled.meets_bar_for(ApproverClass::SelfReviewed));
        assert!(FreshnessClass::Reconciled.meets_bar_for(ApproverClass::Human));
    }

    #[test]
    // WS2 (audit follow-up, claim 5): a real DB error reading
    // graph_generation_state must propagate, not collapse into the same
    // `0` a legitimate "never indexed" state produces.
    fn current_graph_generation_error_propagates_instead_of_defaulting_to_zero() {
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        conn.execute("DROP TABLE graph_generation_state", [])
            .unwrap();
        assert!(EvidenceSnapshot::compute(&conn, root.path()).is_err());
    }

    #[test]
    // WS2 (audit follow-up, claim 5): a directory named like a config file
    // is a genuine read error (EISDIR) -- never "file absent" -- and must
    // not be silently treated the same as a legitimately missing file.
    fn config_digest_propagates_a_real_read_error_instead_of_treating_it_as_absent() {
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        std::fs::create_dir(root.path().join("config.json")).unwrap();
        assert!(EvidenceSnapshot::compute(&conn, root.path()).is_err());
    }

    #[test]
    // WS2 (audit follow-up, claim 5): scip_overlay_state existing with an
    // incompatible schema is a real anomaly (corruption/incompatible
    // migration), not "no provider ever ran" -- must propagate, not
    // silently digest as if the table were empty.
    fn provider_state_digest_propagates_a_real_query_error_instead_of_treating_it_as_no_provider_state()
     {
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        conn.execute("DROP TABLE scip_overlay_state", []).unwrap();
        conn.execute("CREATE TABLE scip_overlay_state (provider TEXT)", [])
            .unwrap();
        conn.execute("INSERT INTO scip_overlay_state (provider) VALUES ('x')", [])
            .unwrap();
        assert!(EvidenceSnapshot::compute(&conn, root.path()).is_err());
    }

    #[test]
    fn freshness_class_round_trips_through_as_str_and_parse() {
        for class in [
            FreshnessClass::Reconciled,
            FreshnessClass::Current,
            FreshnessClass::Degraded,
        ] {
            assert_eq!(FreshnessClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(FreshnessClass::parse("not_a_real_class"), None);
    }

    #[test]
    fn persist_then_load_round_trips_a_snapshot() {
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let snapshot = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();

        let state = state_conn();
        snapshot.persist(&state).unwrap();
        let loaded = EvidenceSnapshot::load(&state, &snapshot.snapshot_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn persisting_the_same_snapshot_twice_is_a_harmless_no_op() {
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let snapshot = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();

        let state = state_conn();
        snapshot.persist(&state).unwrap();
        snapshot.persist(&state).unwrap(); // must not error (identical freshness -- no-op upsert)

        let count: i64 = state
            .query_row(
                "SELECT COUNT(*) FROM evidence_snapshots WHERE snapshot_id = ?1",
                params![snapshot.snapshot_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn load_returns_none_for_an_unknown_snapshot_id() {
        let state = state_conn();
        assert_eq!(
            EvidenceSnapshot::load(&state, "SNP-does-not-exist").unwrap(),
            None
        );
    }

    #[test]
    fn persisting_a_stronger_freshness_class_upgrades_a_previously_persisted_weaker_one() {
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let state = state_conn();

        // Same underlying content -- freshness_class alone differs, so both
        // computations share one snapshot_id (it is deliberately not part
        // of the digest -- see the module doc comment). No index_input_state
        // row is seeded, so `compute` fail-closes to Degraded (weakest) --
        // persisted first, it must not permanently mask a later, stronger
        // Reconciled for the same content.
        let degraded = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();
        assert_eq!(degraded.freshness_class, FreshnessClass::Degraded);
        degraded.persist(&state).unwrap();

        let reconciled =
            EvidenceSnapshot::compute_after_reconciliation(&index_conn, root.path()).unwrap();
        assert_eq!(reconciled.snapshot_id, degraded.snapshot_id);
        reconciled.persist(&state).unwrap();

        let loaded = EvidenceSnapshot::load(&state, &degraded.snapshot_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.freshness_class, FreshnessClass::Reconciled);
    }

    #[test]
    fn persisting_a_weaker_freshness_class_never_downgrades_a_previously_persisted_stronger_one() {
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let state = state_conn();

        let reconciled =
            EvidenceSnapshot::compute_after_reconciliation(&index_conn, root.path()).unwrap();
        reconciled.persist(&state).unwrap();

        let current = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();
        assert_eq!(current.snapshot_id, reconciled.snapshot_id);
        current.persist(&state).unwrap();

        let loaded = EvidenceSnapshot::load(&state, &reconciled.snapshot_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.freshness_class,
            FreshnessClass::Reconciled,
            "a later, weaker observation must never overwrite an already-persisted stronger one"
        );
    }

    #[test]
    fn freshness_class_orders_degraded_below_current_below_reconciled() {
        assert!(FreshnessClass::Degraded < FreshnessClass::Current);
        assert!(FreshnessClass::Current < FreshnessClass::Reconciled);
        assert!(FreshnessClass::Degraded < FreshnessClass::Reconciled);
        assert_eq!(
            FreshnessClass::Reconciled,
            FreshnessClass::Current.max(FreshnessClass::Reconciled)
        );
    }

    #[test]
    fn compute_with_recorded_freshness_picks_up_a_past_reconciliation_for_identical_content() {
        // A real reconciliation happened at some point in the past (e.g. via
        // WatchSupervisor::refresh after a FullReconciliation cycle) and was
        // persisted as `Reconciled`. A LATER, separate `compute()` for the
        // exact same content -- no drift since -- must see that recorded
        // strength, not just re-derive `Current` from `index_input_drift`
        // and silently forget the reconciliation ever happened.
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let state = state_conn();

        let reconciled =
            EvidenceSnapshot::compute_after_reconciliation(&index_conn, root.path()).unwrap();
        reconciled.persist(&state).unwrap();

        // No index_input_state row is seeded for `index_conn`, so a plain
        // `compute()` fail-closes to Degraded here -- the strongest possible
        // check that the recorded snapshot, not the live drift-derived
        // guess, is what wins.
        let live = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();
        assert_eq!(live.freshness_class, FreshnessClass::Degraded);

        let upgraded =
            EvidenceSnapshot::compute_with_recorded_freshness(&index_conn, root.path(), &state)
                .unwrap();
        assert_eq!(upgraded.snapshot_id, reconciled.snapshot_id);
        assert_eq!(upgraded.freshness_class, FreshnessClass::Reconciled);
    }

    #[test]
    fn compute_with_recorded_freshness_falls_back_to_live_drift_when_content_changed_since() {
        // The recorded Reconciled snapshot is for OLD content. Current disk
        // content differs (different file_index hash), so the current
        // `snapshot_id` differs too -- the lookup must miss and fall back to
        // the live drift-derived class, never leak the stale Reconciled
        // forward onto different content (the TOCTOU-safety property).
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let state = state_conn();

        let stale_reconciled =
            EvidenceSnapshot::compute_after_reconciliation(&index_conn, root.path()).unwrap();
        stale_reconciled.persist(&state).unwrap();

        // Content changes: a new file_index row flips source_catalog_digest,
        // and therefore snapshot_id.
        index_conn
            .execute(
                "INSERT INTO file_index (path, hash, last_indexed) VALUES ('b.rs', 'h2', 0)",
                [],
            )
            .unwrap();

        let after_change =
            EvidenceSnapshot::compute_with_recorded_freshness(&index_conn, root.path(), &state)
                .unwrap();
        assert_ne!(after_change.snapshot_id, stale_reconciled.snapshot_id);
        assert_ne!(after_change.freshness_class, FreshnessClass::Reconciled);
    }

    #[test]
    fn compute_with_recorded_freshness_does_not_revive_a_stale_reconciled_over_live_drift() {
        // Wave 6 (audit follow-up, P0-A): the actual dangerous scenario --
        // content is genuinely unchanged according to the DB
        // (source_catalog_digest/snapshot_id unchanged, since nothing has
        // been reindexed), but the file changed ON DISK in the meantime.
        // Before this fix, the recorded Reconciled row (same snapshot_id,
        // because that digest never saw the disk-only change) would
        // silently overwrite the live-derived Degraded right back to
        // Reconciled -- letting `mint_review_authority_for_edit_context`
        // mint an authority on a false freshness guarantee. The sibling
        // test above (`..._falls_back_to_live_drift_when_content_changed_
        // since`) does NOT cover this despite its name: it mutates
        // file_index directly, which flips snapshot_id and makes the
        // recorded-lookup miss entirely -- a different, already-safe path.
        use crate::indexer::refresh::{InputCatalog, persist_index_input_snapshot};
        let root = tmp_project();
        let file_path = root.path().join("a.rs");
        std::fs::write(&file_path, "fn a() {}").unwrap();
        let indexed_mtime = crate::indexer::pipeline::mtime_secs(&file_path);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, last_indexed, mtime) \
             VALUES ('a.rs', 'h1', 0, ?1)",
            params![indexed_mtime],
        )
        .unwrap();
        persist_index_input_snapshot(&conn, &InputCatalog::for_project(root.path())).unwrap();

        let state = state_conn();

        // Record a real full reconciliation for this exact content (same
        // snapshot_id the DB currently reflects) -- the legitimate case
        // `compute_with_recorded_freshness` exists to serve.
        let reconciled =
            EvidenceSnapshot::compute_after_reconciliation(&conn, root.path()).unwrap();
        reconciled.persist(&state).unwrap();

        // Sanity: right now (no live drift yet), the recorded Reconciled
        // DOES correctly apply -- this is the upgrade path that must keep
        // working after the fix.
        let still_fresh =
            EvidenceSnapshot::compute_with_recorded_freshness(&conn, root.path(), &state).unwrap();
        assert_eq!(still_fresh.freshness_class, FreshnessClass::Reconciled);

        // Mutate the file ON DISK ONLY -- no reindex, so file_index (and
        // therefore snapshot_id) stays byte-for-byte identical to what was
        // just recorded as Reconciled above.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &file_path,
            "fn a() { /* changed on disk, not reindexed */ }",
        )
        .unwrap();

        let after_live_change =
            EvidenceSnapshot::compute_with_recorded_freshness(&conn, root.path(), &state).unwrap();
        assert_eq!(
            after_live_change.snapshot_id, reconciled.snapshot_id,
            "snapshot_id is content-addressed over file_index DB rows only -- a \
             live-disk-only change must NOT flip it (this assertion proves the \
             test is actually exercising the dangerous path, not the already-safe \
             snapshot_id-changed path the sibling test above covers)"
        );
        assert_eq!(
            after_live_change.freshness_class,
            FreshnessClass::Degraded,
            "a live-disk-only change must not be silently revived back to \
             Reconciled by a stale recorded snapshot sharing the same snapshot_id"
        );
    }

    #[test]
    fn live_disk_mtime_drift_downgrades_current_to_degraded() {
        use crate::indexer::refresh::{InputCatalog, persist_index_input_snapshot};
        let root = tmp_project();
        let file_path = root.path().join("a.rs");
        std::fs::write(&file_path, "fn a() {}").unwrap();
        let indexed_mtime = crate::indexer::pipeline::mtime_secs(&file_path);

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, last_indexed, mtime) \
             VALUES ('a.rs', 'h1', 0, ?1)",
            params![indexed_mtime],
        )
        .unwrap();
        persist_index_input_snapshot(&conn, &InputCatalog::for_project(root.path())).unwrap();

        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(
            before.freshness_class,
            FreshnessClass::Current,
            "no drift yet -- stored mtime matches the file as indexed"
        );

        // Mutate the file on disk WITHOUT reindexing (no file_index update) --
        // the exact "watcher hasn't caught up yet" lag window 2.1 exists to
        // catch. A short sleep guards against filesystems with coarse mtime
        // granularity reporting an identical timestamp for both writes.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &file_path,
            "fn a() { /* changed on disk, not reindexed */ }",
        )
        .unwrap();

        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(
            after.freshness_class,
            FreshnessClass::Degraded,
            "live mtime no longer matches file_index.mtime -- must not still claim Current"
        );
    }

    #[test]
    fn live_disk_mtime_drift_missing_file_fails_closed() {
        use crate::indexer::refresh::{InputCatalog, persist_index_input_snapshot};
        let root = tmp_project();
        // file_index claims a row for a file that was never actually written
        // to this project_root -- e.g. deleted since indexing.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, last_indexed, mtime) \
             VALUES ('missing.rs', 'h1', 0, 123.0)",
            [],
        )
        .unwrap();
        persist_index_input_snapshot(&conn, &InputCatalog::for_project(root.path())).unwrap();

        let snap = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(
            snap.freshness_class,
            FreshnessClass::Degraded,
            "a file_index row with no file on disk must fail closed, not be skipped"
        );
    }

    #[test]
    fn live_disk_mtime_drift_null_mtime_fails_closed() {
        use crate::indexer::refresh::{InputCatalog, persist_index_input_snapshot};
        let root = tmp_project();
        std::fs::write(root.path().join("a.rs"), "fn a() {}").unwrap();
        // conn_with_file_index leaves `mtime` NULL -- a pre-migration row, or
        // one indexed before this column existed.
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        persist_index_input_snapshot(&conn, &InputCatalog::for_project(root.path())).unwrap();

        let snap = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(
            snap.freshness_class,
            FreshnessClass::Degraded,
            "a NULL stored mtime must fail closed, not be silently treated as no drift"
        );
    }
}
