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
    /// CCK-26 (audit follow-up on the master blueprint): who must approve a
    /// change at this risk level, derived deterministically from
    /// `aggregate_risk` -- `evaluate` computing this (not the caller
    /// guessing it separately) is what makes it possible for
    /// `ReviewAuthority` to bind a `required_approver_class` that's
    /// actually backed by a real risk evaluation, not just a policy config
    /// digest.
    pub required_approver_class: ApproverClass,
}

/// Who must approve a change at a given risk level. `SelfReviewed` --
/// `review_change`'s existing `approved: bool` self-attestation is
/// sufficient. `Human` -- self-attestation is NOT sufficient; only a real
/// elicitation round-trip (`ElicitGate::Ask -> Approved`, same MRTR/legacy
/// mechanism the write-path gate already uses) counts.
/// `TrustedPolicyService` reserved for a future automated-approver
/// integration (e.g. a CI policy bot) -- not producible by `evaluate` yet,
/// included now so `ReviewAuthority`'s bound field doesn't need to widen
/// again when that lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApproverClass {
    SelfReviewed,
    Human,
    TrustedPolicyService,
}

impl ApproverClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfReviewed => "self_reviewed",
            Self::Human => "human",
            Self::TrustedPolicyService => "trusted_policy_service",
        }
    }

    /// Inverse of [`Self::as_str`] -- CCK-26: round-trips the value stored
    /// in `review_authorities.required_approver_class`. `None` for
    /// anything else, so a caller can propagate "not a class this scale
    /// understands" instead of guessing (mirrors `RiskLevel::parse`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "self_reviewed" => Some(Self::SelfReviewed),
            "human" => Some(Self::Human),
            "trusted_policy_service" => Some(Self::TrustedPolicyService),
            _ => None,
        }
    }

    /// Low/medium risk: self-review is an adequate approver class. High
    /// risk: nothing short of a human counts -- mirrors
    /// `edit_lines`/`edit_symbol`'s own unconditional
    /// `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` check (CCK-23), so the two
    /// gates agree on where the line is.
    fn from_risk(level: RiskLevel) -> Self {
        match level {
            RiskLevel::Low | RiskLevel::Medium => Self::SelfReviewed,
            RiskLevel::High => Self::Human,
        }
    }
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
        // Parity with the legacy `classify_gate` (calm-server/tools/edit.rs):
        // `hub_hit` alone forces the full independent-review gate there,
        // regardless of caller-count risk -- this axis must never authorize
        // weaker than that.
        level = level.max(RiskLevel::High);
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
        // Parity with `classify_gate`: `uncertain_zero_caller.is_some()`
        // alone forces the full gate there too, independent of `risk`.
        level = level.max(RiskLevel::High);
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
        required_approver_class: ApproverClass::from_risk(level),
    }
}

impl PolicyDecision {
    /// Canonical digest bound into `ReviewAuthority` (CCK-26) -- changes if
    /// and only if the decision itself (risk level, reasons, or required
    /// approver) changes, so `verify_and_consume` can detect a decision
    /// that's drifted since mint time the same way it already detects a
    /// drifted snapshot/policy-config digest.
    pub fn digest(&self) -> String {
        let material = serde_json::to_string(self).unwrap_or_else(|_| format!("{:?}", self));
        crate::digest::evidence_digest(format!("policy-decision-v1\n{material}").as_bytes())
    }
}

impl RiskVector {
    /// Canonical digest bound into `ReviewAuthority` (CCK-26) -- the raw
    /// signal inputs `evaluate` folded into the `PolicyDecision` above.
    /// Bound separately (not just the decision) so a future policy-config
    /// change that alters HOW a given vector is judged is still
    /// distinguishable from the underlying facts about the touch changing.
    pub fn digest(&self) -> String {
        let material = serde_json::to_string(self).unwrap_or_else(|_| format!("{:?}", self));
        crate::digest::evidence_digest(format!("risk-vector-v1\n{material}").as_bytes())
    }

    /// Wave 10 (item 2): a formally-grounded, per-field "no riskier than"
    /// check -- `self` (the current, spend-time vector) is covered by
    /// `minted` (the vector `evaluate()` already approved at mint time)
    /// iff every axis `evaluate()` itself reads is no more escalatory in
    /// `self` than in `minted`. Because `evaluate()` (above) is provably
    /// monotone -- it starts from `caller_count_level` and every
    /// subsequent axis only ever RAISES the running level via `.max(...)`,
    /// never lowers it -- `self.is_covered_by(minted) == true` proves
    /// `evaluate(self, policy).aggregate_risk <= evaluate(minted,
    /// policy).aggregate_risk` for any single `policy` both vectors are
    /// judged under (`kind_mismatch_floor`/`manifest_floor`/
    /// `uncovered_code_floor` come from `policy`, not the vector, so this
    /// alone doesn't prove anything across a policy change -- callers must
    /// independently confirm the policy digest still matches, exactly as
    /// `verify_only` already does via its own `StalePolicy` check).
    /// `risk_rule_floor` compares via `Option<RiskLevel>`'s own derived
    /// `Ord` (`None` sorts below every `Some(_)`), which already matches
    /// `evaluate()`'s "no floor configured contributes nothing" reading.
    ///
    /// `hub_kind` is deliberately excluded: `evaluate()` never reads its
    /// STRING value for the risk level, only `is_hub`'s bool (`hub_kind`
    /// only feeds `evaluate()`'s own human-readable `reasons` text, plus a
    /// separate, independently-and-freshly-recomputed bridge-downgrade
    /// eligibility check in calm-server that never trusts a stored
    /// authority's claimed `hub_kind` in the first place) -- including it
    /// here would reject spends `evaluate()` itself would judge identical.
    pub fn is_covered_by(&self, minted: &RiskVector) -> bool {
        self.caller_count_level <= minted.caller_count_level
            && self.risk_rule_floor <= minted.risk_rule_floor
            && (!self.is_hub || minted.is_hub)
            && (!self.signature_changed || minted.signature_changed)
            && (!self.uncertain_zero_caller || minted.uncertain_zero_caller)
            && (!self.kind_mismatch || minted.kind_mismatch)
            && (!self.touches_manifest || minted.touches_manifest)
            && (!self.touches_uncovered_code || minted.touches_uncovered_code)
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

    #[test]
    fn required_approver_class_is_human_for_high_risk_self_reviewed_otherwise() {
        let policy = Policy::default();
        let mut low = base_vector();
        low.caller_count_level = RiskLevel::Low;
        assert_eq!(
            evaluate(&low, &policy).required_approver_class,
            ApproverClass::SelfReviewed
        );

        let mut high = base_vector();
        high.is_hub = true;
        assert_eq!(
            evaluate(&high, &policy).required_approver_class,
            ApproverClass::Human
        );
    }

    #[test]
    fn policy_decision_digest_changes_when_the_decision_changes() {
        let policy = Policy::default();
        let low = evaluate(&base_vector(), &policy);
        let mut hub_vector = base_vector();
        hub_vector.is_hub = true;
        let high = evaluate(&hub_vector, &policy);
        assert_ne!(low.digest(), high.digest());
        assert_eq!(low.digest(), evaluate(&base_vector(), &policy).digest());
    }

    #[test]
    fn risk_vector_digest_changes_when_the_vector_changes() {
        let a = base_vector();
        let mut b = base_vector();
        b.touches_manifest = true;
        assert_ne!(a.digest(), b.digest());
        assert_eq!(a.digest(), base_vector().digest());
    }

    #[test]
    fn is_covered_by_is_reflexive() {
        let v = base_vector();
        assert!(v.is_covered_by(&v));
    }

    #[test]
    fn is_covered_by_true_when_spend_is_strictly_less_risky_on_every_axis() {
        let spend = base_vector(); // everything at its weakest
        let mut mint = base_vector();
        mint.caller_count_level = RiskLevel::High;
        mint.is_hub = true;
        mint.signature_changed = true;
        mint.uncertain_zero_caller = true;
        mint.risk_rule_floor = Some(RiskLevel::High);
        mint.kind_mismatch = true;
        mint.touches_manifest = true;
        mint.touches_uncovered_code = true;
        assert!(
            spend.is_covered_by(&mint),
            "a strictly weaker spend must be covered by a strictly stronger mint"
        );
    }

    #[test]
    fn is_covered_by_false_when_spend_caller_count_level_exceeds_mint() {
        let mut spend = base_vector();
        spend.caller_count_level = RiskLevel::High;
        let mut mint = base_vector();
        mint.caller_count_level = RiskLevel::Low;
        assert!(!spend.is_covered_by(&mint));
    }

    #[test]
    fn is_covered_by_false_when_spend_has_a_bool_axis_mint_lacks() {
        let mut spend = base_vector();
        spend.touches_uncovered_code = true;
        let mint = base_vector(); // touches_uncovered_code still false
        assert!(
            !spend.is_covered_by(&mint),
            "spend touching uncovered code must not be covered by a mint that never saw it"
        );
    }

    #[test]
    fn is_covered_by_respects_risk_rule_floor_ordering() {
        let mut spend_high_floor = base_vector();
        spend_high_floor.risk_rule_floor = Some(RiskLevel::High);
        let mint_no_floor = base_vector();
        assert!(
            !spend_high_floor.is_covered_by(&mint_no_floor),
            "a spend-time floor absent at mint time must not be covered"
        );

        let spend_no_floor = base_vector();
        let mut mint_low_floor = base_vector();
        mint_low_floor.risk_rule_floor = Some(RiskLevel::Low);
        assert!(
            spend_no_floor.is_covered_by(&mint_low_floor),
            "no spend-time floor is covered by any mint-time floor, including none at all"
        );
    }

    #[test]
    fn is_covered_by_ignores_hub_kind_text_when_is_hub_agrees() {
        let mut spend = base_vector();
        spend.is_hub = true;
        spend.hub_kind = Some("bridge".to_string());
        let mut mint = base_vector();
        mint.is_hub = true;
        mint.hub_kind = Some("degree".to_string());
        assert!(
            spend.is_covered_by(&mint),
            "hub_kind text drift alone must not block coverage -- evaluate() never reads it \
             for the risk level, only is_hub's bool"
        );
    }

    #[test]
    fn is_covered_by_soundly_implies_evaluate_is_no_riskier() {
        // Direct check of the property is_covered_by's own doc comment
        // claims: for a handful of concrete (spend, mint) pairs where
        // is_covered_by holds, evaluate(spend) really is <= evaluate(mint)
        // under the same policy -- not just asserted, verified.
        let policy = Policy {
            kind_mismatch_floor: RiskLevel::Medium,
            manifest_floor: RiskLevel::Medium,
            uncovered_code_floor: RiskLevel::High,
        };
        let pairs: Vec<(RiskVector, RiskVector)> = vec![
            (base_vector(), {
                let mut m = base_vector();
                m.touches_uncovered_code = true;
                m
            }),
            (
                {
                    let mut s = base_vector();
                    s.caller_count_level = RiskLevel::Medium;
                    s
                },
                {
                    let mut m = base_vector();
                    m.caller_count_level = RiskLevel::Medium;
                    m.is_hub = true;
                    m
                },
            ),
            (base_vector(), base_vector()),
        ];
        for (spend, mint) in pairs {
            assert!(
                spend.is_covered_by(&mint),
                "test fixture itself must be covered"
            );
            let spend_decision = evaluate(&spend, &policy);
            let mint_decision = evaluate(&mint, &policy);
            assert!(
                spend_decision.aggregate_risk <= mint_decision.aggregate_risk,
                "is_covered_by held but evaluate(spend)={:?} > evaluate(mint)={:?}",
                spend_decision.aggregate_risk,
                mint_decision.aggregate_risk
            );
        }
    }

    #[test]
    fn is_hub_alone_escalates_to_high_matching_classify_gates_unconditional_gate() {
        // Legacy `classify_gate` (calm-server/tools/edit.rs) gates on
        // `hub_hit` alone, independent of caller-count risk -- this axis
        // must reach the same conclusion here.
        let mut v = base_vector();
        v.is_hub = true;
        v.hub_kind = Some("degree".to_string());
        let decision = evaluate(&v, &Policy::default());
        assert_eq!(decision.aggregate_risk, RiskLevel::High);
    }

    #[test]
    fn uncertain_zero_caller_alone_escalates_to_high_matching_classify_gates_unconditional_gate() {
        // Legacy `classify_gate` gates on `uncertain_zero_caller.is_some()`
        // alone too, independent of `risk`.
        let mut v = base_vector();
        v.uncertain_zero_caller = true;
        let decision = evaluate(&v, &Policy::default());
        assert_eq!(decision.aggregate_risk, RiskLevel::High);
    }

    /// Mirrors `classify_gate`'s own unconditional-gate predicate
    /// (`hub_hit || risk == Some("high") || uncertain_zero_caller.is_some()
    /// || force_gate_always`) from `calm-server/tools/edit.rs`, minus
    /// `force_gate_always` (a config flag `evaluate` doesn't see) --
    /// `calm-core` cannot depend on `calm-server` to call the real function,
    /// so this predicate is the parity contract the two must independently
    /// satisfy. If `classify_gate`'s predicate ever changes, this comment
    /// and the assertions below must change with it.
    fn legacy_will_gate(hub_hit: bool, risk_high: bool, uncertain_zero_caller: bool) -> bool {
        hub_hit || risk_high || uncertain_zero_caller
    }

    #[test]
    fn evaluate_never_authorizes_weaker_than_classify_gates_unconditional_axes() {
        for hub_hit in [false, true] {
            for uncertain_zero_caller in [false, true] {
                let mut v = base_vector();
                v.is_hub = hub_hit;
                v.uncertain_zero_caller = uncertain_zero_caller;
                let decision = evaluate(&v, &Policy::default());
                let legacy_gates = legacy_will_gate(hub_hit, false, uncertain_zero_caller);
                let evaluate_gates = decision.aggregate_risk == RiskLevel::High;
                assert!(
                    evaluate_gates || !legacy_gates,
                    "classify_gate would gate (hub_hit={hub_hit}, uncertain_zero_caller={uncertain_zero_caller}) but evaluate() did not escalate to High"
                );
            }
        }
    }
}
