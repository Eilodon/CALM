//! `PolicyEngine` -- CCK-08
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! **Shadow only**: this module computes a [`model::RiskVector`] and a
//! [`evaluate::PolicyDecision`] from already-known signals; nothing in
//! `calm-server`'s write gate reads it yet (that wiring, and the shadow-vs-
//! enforce promotion decision, is CCK-10). Existing `config.risk_rules`
//! path floors are read as-is (`model::RiskVector::risk_rule_floor`) --
//! this module compiles them into the vector rather than replacing them.

pub mod evaluate;
pub mod loader;
pub mod model;

pub use evaluate::{PolicyDecision, evaluate};
pub use loader::{Policy, load_policy};
pub use model::{RiskLevel, RiskVector};
