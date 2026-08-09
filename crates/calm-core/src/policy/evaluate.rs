//! Pure `RiskVector -> PolicyDecision` evaluation -- CCK-08. Deliberately
//! side-effect-free (no I/O, no clock, no RNG): the blueprint's own
//! determinism requirement ("same inputs+digest ⇒ byte-identical
//! decision") falls out for free from that, rather than needing to be
//! separately engineered.
//!
//! **Shadow only.** Nothing calls this from a write path yet -- see the
//! module doc comment in `policy/mod.rs`.

use crate::policy::loader::Policy;
use crate::policy::model::{RiskLevel, RiskVector};

/// The aggregate decision plus a human-readable trail of which axes
/// contributed to it -- the trail exists so a shadow-mode caller can *log
/// disagreement* against the legacy aggregate risk string (blueprint's own
/// wording for this PR's shadow posture) with an actual explanation
/// attached, not just a differing number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PolicyDecision {
    pub aggregate_risk: RiskLevel,
    pub reasons: Vec<String>,
}

/// Folds every axis of `vector` into one [`PolicyDecision`], starting from
/// `caller_count_level` (the existing structural floor) and only ever
/// raising the level -- an axis firing can escalate risk, never lower it,
/// matching `compute_touch_risk`'s own "rules never lower structural risk"
/// invariant (`crates/calm-server/src/tools.rs::
/// compute_touch_risk_rules_never_lower_structural_risk`).
pub fn evaluate(vector: &RiskVector, policy: &Policy) -> PolicyDecision {
    let mut level = vector.caller_count_level;
    let mut reasons = Vec::new();

    if let Some(floor) = vector.risk_rule_floor {
        if floor > level {
            level = floor;
        }
        reasons.push(format!("risk_rules path floor: {}", floor.as_str()));
    }
    if vector.is_hub {
        reasons.push(format!(
            "touches a hub symbol{}",
            vector
                .hub_kind
                .as_deref()
                .map(|k| format!(" ({k})"))
                .unwrap_or_default()
        ));
    }
    if vector.signature_changed {
        level = level.max(RiskLevel::High);
        reasons.push("touched symbol's own signature changed meaning".to_string());
    }
    if vector.uncertain_zero_caller {
        reasons.push("zero-caller symbol with uncertain dead-code confidence".to_string());
    }
    if vector.kind_mismatch {
        level = level.max(policy.kind_mismatch_floor);
        reasons.push(format!(
            "declared change kind disagrees with observed diff (floor: {})",
            policy.kind_mismatch_floor.as_str()
        ));
    }
    if vector.touches_manifest {
        level = level.max(policy.manifest_floor);
        reasons.push(format!(
            "touches a dependency manifest (floor: {})",
            policy.manifest_floor.as_str()
        ));
    }
    if vector.touches_uncovered_code {
        level = level.max(policy.uncovered_code_floor);
        reasons.push(format!(
            "touched range has no recorded test coverage (floor: {})",
            policy.uncovered_code_floor.as_str()
        ));
    }

    PolicyDecision {
        aggregate_risk: level,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_vector() -> RiskVector {
        RiskVector {
            caller_count_level: RiskLevel::Low,
            is_hub: false,
            hub_kind: None,
            signature_changed: false,
            uncertain_zero_caller: false,
            risk_rule_floor: None,
            kind_mismatch: false,
            touches_manifest: false,
            touches_uncovered_code: false,
        }
    }

    #[test]
    fn no_axes_fired_keeps_the_caller_count_level() {
        let mut v = base_vector();
        v.caller_count_level = RiskLevel::Medium;
        let decision = evaluate(&v, &Policy::default());
        assert_eq!(decision.aggregate_risk, RiskLevel::Medium);
    }

    #[test]
    fn signature_change_always_escalates_to_high() {
        let mut v = base_vector();
        v.signature_changed = true;
        let decision = evaluate(&v, &Policy::default());
        assert_eq!(decision.aggregate_risk, RiskLevel::High);
    }

    #[test]
    fn kind_mismatch_escalates_to_the_policy_configured_floor() {
        let mut v = base_vector();
        v.kind_mismatch = true;
        let lenient = Policy {
            kind_mismatch_floor: RiskLevel::Medium,
            ..Policy::default()
        };
        let decision = evaluate(&v, &lenient);
        assert_eq!(decision.aggregate_risk, RiskLevel::Medium);
    }

    #[test]
    fn an_axis_never_lowers_risk_below_the_structural_floor() {
        let mut v = base_vector();
        v.caller_count_level = RiskLevel::High;
        v.kind_mismatch = true;
        let permissive = Policy {
            kind_mismatch_floor: RiskLevel::Low,
            manifest_floor: RiskLevel::Low,
            uncovered_code_floor: RiskLevel::Low,
        };
        let decision = evaluate(&v, &permissive);
        assert_eq!(
            decision.aggregate_risk,
            RiskLevel::High,
            "a low policy floor must not undercut a higher structural risk"
        );
    }

    #[test]
    fn risk_rule_floor_raises_but_never_lowers_the_level() {
        let mut v = base_vector();
        v.caller_count_level = RiskLevel::High;
        v.risk_rule_floor = Some(RiskLevel::Low);
        let decision = evaluate(&v, &Policy::default());
        assert_eq!(decision.aggregate_risk, RiskLevel::High);
    }

    #[test]
    fn multiple_axes_compose_to_the_highest_applicable_level() {
        let mut v = base_vector();
        v.touches_manifest = true;
        v.touches_uncovered_code = true;
        let policy = Policy {
            manifest_floor: RiskLevel::Medium,
            uncovered_code_floor: RiskLevel::High,
            ..Policy::default()
        };
        let decision = evaluate(&v, &policy);
        assert_eq!(decision.aggregate_risk, RiskLevel::High);
        assert_eq!(decision.reasons.len(), 2);
    }

    #[test]
    fn evaluation_is_deterministic_for_identical_inputs() {
        let v = base_vector();
        let policy = Policy::default();
        let a = evaluate(&v, &policy);
        let b = evaluate(&v, &policy);
        assert_eq!(a, b);
    }
}
