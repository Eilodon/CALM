//! Authority primitives for the Master Change-Control Kernel
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md,
//! Phase 1). Built up PR by PR: CCK-06 (`EvidenceSnapshot`) is
//! compute-only, persisted by CCK-07. CCK-09 (`ReviewAuthority`, this
//! module's `review`/`key` submodules) is the signed, single-use,
//! snapshot-bound authority object #65 asks for, built on top of both.

pub mod key;
pub mod review;
pub mod snapshot;

pub use review::{
    AUTHORITY_TTL_MAX_SECS, AuthorityError, AuthorityTtl, AuthorityTtlError, CurrentState,
    MintParams, ReviewAuthority, current_analysis_version,
};
pub use snapshot::{EvidenceSnapshot, FreshnessClass};
