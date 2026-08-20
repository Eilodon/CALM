//! `PolicyEngine` -- CCK-08
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! **No longer shadow-only** (canonical PolicyDecision, roadmap item 3,
//! 2026-08-20): this module's [`model::RiskVector`] and
//! [`evaluate::PolicyDecision`] are read by `calm-server`'s real write path
//! at 3 production sites -- `edit_lines_impl_gated`'s authority-spend
//! digest check, `review_change`'s Human-tier refusal, and
//! `mint_review_authority_for_edit_context`'s freshness-bar gate -- and its
//! two configurable floors (`manifest_floor`/`uncovered_code_floor`) are
//! ALSO folded directly into `compute_touch_risk`'s own risk escalation
//! (calm-server/tools/edit.rs), the function that feeds `classify_gate`,
//! the real-time gate for `edit_lines`/`edit_symbol` on the plain
//! confirm/reason path. This doc comment previously claimed "nothing in
//! calm-server's write gate reads it yet" -- that was accurate when CCK-10
//! was still pending; it stopped being true once CCK-10 landed and was
//! never updated. Existing `config.risk_rules` path floors are read as-is
//! (`model::RiskVector::risk_rule_floor`) -- this module compiles them
//! into the vector rather than replacing them.

pub mod evaluate;
pub mod loader;
pub mod model;

pub use evaluate::{ApproverClass, PolicyDecision, evaluate};
pub use loader::{Policy, load_policy};
pub use model::{RiskLevel, RiskVector};
