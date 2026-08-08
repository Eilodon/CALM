//! Authority primitives for the Master Change-Control Kernel
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md,
//! Phase 1). Built up PR by PR: CCK-06 (`EvidenceSnapshot`, this module)
//! is compute-only -- no schema yet. CCK-09 adds `ReviewAuthority` (signed,
//! persisted, single-use) on top of it once CCK-07's state.db v2 exists.

pub mod snapshot;

pub use snapshot::{EvidenceSnapshot, FreshnessClass};
