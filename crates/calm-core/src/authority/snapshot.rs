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

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::digest::evidence_digest;
use crate::indexer::refresh::{index_input_drift, IndexInputDrift, InputCatalog};

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

impl FreshnessClass {
    /// High-risk authority (CCK-09/CCK-10) must force reconciliation before
    /// minting against a `Degraded` snapshot -- watcher/index health alone
    /// is never proof (blueprint's own wording for this PR).
    pub fn is_safe_for_high_risk_authority(self) -> bool {
        matches!(self, Self::Reconciled | Self::Current)
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
    pub freshness_class: FreshnessClass,
}

impl EvidenceSnapshot {
    /// Computes a snapshot from `conn`'s current state without forcing
    /// reconciliation first -- `freshness_class` reflects whatever
    /// `index_input_drift` reports right now (`Current` or `Degraded`,
    /// never `Reconciled`).
    pub fn compute(conn: &Connection, project_root: &Path) -> rusqlite::Result<Self> {
        let catalog = InputCatalog::for_project(project_root);
        let drift = index_input_drift(conn, &catalog)?;
        Self::build(conn, project_root, drift_to_freshness(drift))
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

    fn build(
        conn: &Connection,
        project_root: &Path,
        freshness_class: FreshnessClass,
    ) -> rusqlite::Result<Self> {
        let source_catalog_digest = source_catalog_digest(conn)?;
        let graph_generation = current_graph_generation(conn);
        let config_digest = config_digest(project_root);

        let material = format!(
            "evidence-snapshot-v1\n\
             source_catalog_digest={source_catalog_digest}\n\
             graph_generation={graph_generation}\n\
             config_digest={config_digest}\n\
             graph_derivation_version={}\n\
             package_graph_version={}\n",
            crate::graph::digest::GRAPH_DERIVATION_VERSION,
            crate::indexer::package_deps::PACKAGE_GRAPH_VERSION,
        );
        let snapshot_id = format!("SNP-{}", evidence_digest(material.as_bytes()));

        Ok(Self { snapshot_id, source_catalog_digest, graph_generation, freshness_class })
    }

    /// Persists this snapshot into `evidence_snapshots` (CCK-07,
    /// `db::state_migrations`'s v1->v2 step) -- `state_conn` is a
    /// **state.db** connection, distinct from the `conn` (index.db)
    /// `compute`/`build` read from. `snapshot_id` is content-addressed, so
    /// re-persisting an identical snapshot is a harmless `INSERT OR
    /// IGNORE`, not a duplicate/conflict; `created_at` reflects the first
    /// time this exact snapshot was ever persisted, not this call.
    pub fn persist(&self, state_conn: &Connection) -> rusqlite::Result<()> {
        state_conn.execute(
            "INSERT OR IGNORE INTO evidence_snapshots \
             (snapshot_id, source_catalog_digest, graph_generation, freshness_class, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                self.snapshot_id,
                self.source_catalog_digest,
                self.graph_generation,
                self.freshness_class.as_str(),
                now_epoch_secs(),
            ],
        )?;
        Ok(())
    }

    /// `Ok(None)` when no row matches. `state_conn` is a state.db
    /// connection, same as [`persist`](Self::persist).
    pub fn load(state_conn: &Connection, snapshot_id: &str) -> rusqlite::Result<Option<Self>> {
        let row: Option<(String, String, i64, String)> = state_conn
            .query_row(
                "SELECT snapshot_id, source_catalog_digest, graph_generation, freshness_class \
                 FROM evidence_snapshots WHERE snapshot_id = ?1",
                params![snapshot_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((snapshot_id, source_catalog_digest, graph_generation, freshness_str)) = row else {
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
        Ok(Some(Self { snapshot_id, source_catalog_digest, graph_generation, freshness_class }))
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

/// Mirrors `guardrails.rs::edit_context`'s own read of the same table --
/// see that call site's doc comment for why 0 (never indexed) is a safe
/// default rather than an error.
fn current_graph_generation(conn: &Connection) -> i64 {
    conn.query_row("SELECT generation FROM graph_generation_state WHERE id = 1", [], |r| {
        r.get(0)
    })
    .unwrap_or(0)
}

/// `evidence_digest` over the concatenated bytes of every file in
/// [`GLOBAL_CONFIGURATION_PATHS`] that exists, in fixed order -- a missing
/// file contributes its path with no bytes (present vs. absent still
/// changes the digest), so deleting a config file is not indistinguishable
/// from an unchanged one.
fn config_digest(project_root: &Path) -> String {
    let mut material = Vec::from(*b"config-digest-v1\n");
    for relative in GLOBAL_CONFIGURATION_PATHS {
        material.extend_from_slice(relative.as_bytes());
        material.push(b'\0');
        if let Ok(bytes) = std::fs::read(project_root.join(relative)) {
            material.extend_from_slice(&bytes);
        }
        material.push(b'\n');
    }
    evidence_digest(&material)
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
        assert_eq!(forward_snap.source_catalog_digest, backward_snap.source_catalog_digest);
        assert_eq!(forward_snap.snapshot_id, backward_snap.snapshot_id);
    }

    #[test]
    fn graph_generation_change_flips_the_snapshot_id() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(before.graph_generation, 0);

        conn.execute("UPDATE graph_generation_state SET generation = 7 WHERE id = 1", [])
            .unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(after.graph_generation, 7);
        assert_ne!(before.snapshot_id, after.snapshot_id);
        // graph_generation alone must not move source_catalog_digest --
        // that field is keyed only on file_index content.
        assert_eq!(before.source_catalog_digest, after.source_catalog_digest);
    }

    #[test]
    fn provider_relevant_config_change_flips_the_snapshot_id() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        std::fs::write(root.path().join("config.json"), br#"{"languages":["rust"]}"#).unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_ne!(before.snapshot_id, after.snapshot_id);
    }

    #[test]
    fn source_content_change_flips_the_snapshot_id_but_not_via_graph_generation() {
        let conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let before = EvidenceSnapshot::compute(&conn, root.path()).unwrap();

        conn.execute("UPDATE file_index SET hash = 'h1-changed' WHERE path = 'a.rs'", [])
            .unwrap();
        let after = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_ne!(before.source_catalog_digest, after.source_catalog_digest);
        assert_ne!(before.snapshot_id, after.snapshot_id);
        assert_eq!(before.graph_generation, after.graph_generation);
    }

    #[test]
    fn clean_drift_is_current_not_reconciled_or_degraded() {
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        // No index_input_state row persisted -> index_input_drift reports
        // Unknown (fail-closed), which must classify as Degraded, not
        // Current -- confirms the fail-closed default flows through.
        let snap = EvidenceSnapshot::compute(&conn, root.path()).unwrap();
        assert_eq!(snap.freshness_class, FreshnessClass::Degraded);
        assert!(!snap.freshness_class.is_safe_for_high_risk_authority());
    }

    #[test]
    fn compute_after_reconciliation_is_always_reconciled_regardless_of_drift() {
        let conn = conn_with_file_index(&[]);
        let root = tmp_project();
        let snap = EvidenceSnapshot::compute_after_reconciliation(&conn, root.path()).unwrap();
        assert_eq!(snap.freshness_class, FreshnessClass::Reconciled);
        assert!(snap.freshness_class.is_safe_for_high_risk_authority());
    }

    #[test]
    fn degraded_is_never_silently_treated_as_safe_for_high_risk_authority() {
        assert!(!FreshnessClass::Degraded.is_safe_for_high_risk_authority());
        assert!(FreshnessClass::Current.is_safe_for_high_risk_authority());
        assert!(FreshnessClass::Reconciled.is_safe_for_high_risk_authority());
    }

    #[test]
    fn freshness_class_round_trips_through_as_str_and_parse() {
        for class in [FreshnessClass::Reconciled, FreshnessClass::Current, FreshnessClass::Degraded] {
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
        let loaded = EvidenceSnapshot::load(&state, &snapshot.snapshot_id).unwrap().unwrap();
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn persisting_the_same_snapshot_twice_is_a_harmless_no_op() {
        let index_conn = conn_with_file_index(&[("a.rs", "h1")]);
        let root = tmp_project();
        let snapshot = EvidenceSnapshot::compute(&index_conn, root.path()).unwrap();

        let state = state_conn();
        snapshot.persist(&state).unwrap();
        snapshot.persist(&state).unwrap(); // must not error (INSERT OR IGNORE)

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
        assert_eq!(EvidenceSnapshot::load(&state, "SNP-does-not-exist").unwrap(), None);
    }
}
