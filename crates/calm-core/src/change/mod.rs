//! Change classification for the Master Change-Control Kernel
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md,
//! CCK-08). `classify` builds the `ObservedChangeKind` half of that PR;
//! `ChangeIntentKind` is the *declared* half a future caller states up
//! front (CCK-11's `plan_change`, once CCK-07 gives it somewhere to
//! persist that declaration) -- both wrap the same [`ChangeKind`] variant
//! set so they can never silently drift apart, but stay distinct Rust
//! types so a caller can't compare "what was declared" to itself and call
//! it a check.

pub mod classify;

pub use classify::{
    kinds_mismatch, ChangeIntentKind, ChangeKind, ObservedChangeInput, ObservedChangeKind,
};
