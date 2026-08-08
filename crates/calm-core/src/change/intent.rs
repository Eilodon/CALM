//! `ChangeIntent` -- CCK-07
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! What a caller *declared* they were about to do, bound to the
//! `EvidenceSnapshot` (CCK-06) in effect at declaration time. Persisted by
//! `change::store` into the `change_intents`/`change_intent_targets`
//! tables `db::state_migrations`'s v1->v2 step creates.
//!
//! Invariant #3 (blueprint §2): natural language is never a permission
//! primitive. `reason` on this struct is exactly that -- free text for a
//! human/reviewer to read, never compared or matched against to authorize
//! anything. The only field with authority-relevant meaning is `kind`
//! (checked against the observed diff via `change::classify::kinds_mismatch`)
//! and, once CCK-09 exists, `snapshot_id`.

use crate::change::classify::ChangeIntentKind;

/// One file (optionally symbol-scoped) a `ChangeIntent` declares as its
/// target. A `ChangeIntent` can already name several of these ahead of
/// Phase 2's multi-file `ChangeSet` actually landing -- see
/// `change_intent_targets`'s own doc comment in `STATE_SCHEMA_SQL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeIntentTarget {
    pub path: String,
    pub qualified_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangeIntent {
    /// `"INT-<hex nanos>-<hex counter>-<pid>"` -- same best-effort-unique,
    /// roughly time-sortable shape as `txn::new_tx_id`/`edit::write_nonce`
    /// (uniqueness, not unpredictability, is the only property relied on;
    /// `change_intents.intent_id` is a real PRIMARY KEY so a residual
    /// collision fails loudly rather than silently overwriting another
    /// intent).
    pub intent_id: String,
    pub kind: ChangeIntentKind,
    pub reason: String,
    pub snapshot_id: String,
    pub targets: Vec<ChangeIntentTarget>,
    pub created_at: f64,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_intent_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("INT-{nanos:016x}-{counter:08x}-{}", std::process::id())
}

impl ChangeIntent {
    /// Mints a fresh `ChangeIntent` with a new `intent_id` and
    /// `created_at` set to now. `snapshot_id` is taken as a plain
    /// `String`, not an `&EvidenceSnapshot` -- the caller is expected to
    /// have already persisted the snapshot (`authority::snapshot::persist`)
    /// before minting an intent that references it, since
    /// `change_intents.snapshot_id` is a foreign key.
    pub fn new(
        kind: ChangeIntentKind,
        reason: impl Into<String>,
        snapshot_id: impl Into<String>,
        targets: Vec<ChangeIntentTarget>,
    ) -> Self {
        Self {
            intent_id: new_intent_id(),
            kind,
            reason: reason.into(),
            snapshot_id: snapshot_id.into(),
            targets,
            created_at: now_epoch_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::classify::ChangeKind;

    #[test]
    fn new_intent_gets_a_unique_id_and_a_positive_timestamp() {
        let a = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-x", vec![]);
        let b = ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-x", vec![]);
        assert_ne!(a.intent_id, b.intent_id);
        assert!(a.intent_id.starts_with("INT-"));
        assert!(a.created_at > 0.0);
    }

    #[test]
    fn targets_are_preserved_verbatim() {
        let targets = vec![
            ChangeIntentTarget { path: "a.rs".to_string(), qualified_name: Some("a.rs::f".to_string()) },
            ChangeIntentTarget { path: "b.rs".to_string(), qualified_name: None },
        ];
        let intent =
            ChangeIntent::new(ChangeIntentKind(ChangeKind::Body), "test", "SNP-x", targets.clone());
        assert_eq!(intent.targets, targets);
    }
}
