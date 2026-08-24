//! Authority primitives for the Master Change-Control Kernel
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md,
//! Phase 1). Built up PR by PR: CCK-06 (`EvidenceSnapshot`) is
//! compute-only, persisted by CCK-07. CCK-09 (`ReviewAuthority`, this
//! module's `review`/`key` submodules) is the signed, single-use,
//! snapshot-bound authority object #65 asks for, built on top of both.

pub mod key;
pub mod pending_review;
pub mod receipt;
pub mod review;
pub mod snapshot;

pub use pending_review::{
    AgentRelayOutcome, NewPendingReview, PENDING_REVIEW_DEFAULT_TTL_SECS, PendingReview,
    approve_pending_review, claim_approved_matching, decide_via_agent_relay,
    decline_pending_review, find_approved_matching, get_pending_review, insert_pending_review,
    list_pending_reviews, release_claimed_review,
};
pub use receipt::{ApprovalReceipt, insert_approval_receipt};
pub use review::{
    AUTHORITY_TTL_MAX_SECS, AuthorityError, AuthorityTtl, AuthorityTtlError, AuthorizeEditError,
    CurrentState, MintParams, ReviewAuthority, current_analysis_version,
};
pub use snapshot::{EvidenceSnapshot, FreshnessClass};
