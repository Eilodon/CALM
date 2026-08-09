//! Change intent and classification for the Master Change-Control Kernel
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! `classify` (CCK-08) builds the `ObservedChangeKind` half; `intent` +
//! `store` (CCK-07) are the *declared* half plus its persistence, once
//! `db::state_migrations`'s v1->v2 step gives it somewhere to live.
//! `ChangeIntentKind` and `ObservedChangeKind` wrap the same [`ChangeKind`]
//! variant set so they can never silently drift apart, but stay distinct
//! Rust types so a caller can't compare "what was declared" to itself and
//! call it a check.

pub mod classify;
pub mod intent;
pub mod store;

pub use classify::{
    ChangeIntentKind, ChangeKind, ObservedChangeInput, ObservedChangeKind, kinds_mismatch,
};
pub use intent::{ChangeIntent, ChangeIntentTarget};
pub use store::{find_change_intent_by_idempotency_key, get_change_intent, insert_change_intent};
