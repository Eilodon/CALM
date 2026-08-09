//! `RiskVector` -- CCK-08's 9-axis replacement for a single aggregate
//! risk string, kept alongside (not instead of) that aggregate: see
//! `evaluate::PolicyDecision::aggregate_risk`.
//!
//! Six axes mirror signals `calm-server`'s existing
//! `compute_touch_risk` already computes (caller-count level, hub
//! centrality, signature change, uncertain-zero-caller, and the
//! `risk_rules` path floor) -- `calm-core` cannot depend on `calm-server`,
//! so this module never recomputes them itself; a `calm-server` caller
//! (CCK-10) is expected to fill them in from what it already has. Three
//! axes are new in this PR: `kind_mismatch` (declared vs. observed
//! `ChangeKind`, see `change::classify`), `touches_manifest`, and
//! `touches_uncovered_code`.

/// The 3 severity levels `classify_gate`'s write-blocking check already
/// understands (`crates/calm-server/src/tools/edit.rs::risk_severity`) --
/// deliberately not a 4th `Critical` level (that belongs to
/// `diff_impact`'s advisory-only `RiskOrder`, a different scale for a
/// different consumer).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Parses the same 3 strings `RiskRule.minimum` is validated against
    /// (`config::VALID_RISK_LEVELS`) -- `None` for anything else, so a
    /// caller can propagate "not a level this scale understands" instead
    /// of guessing.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// 9 independently-inspectable risk signals for one proposed touch.
/// Compute-only, like `EvidenceSnapshot` (CCK-06) -- nothing here is
/// persisted yet.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RiskVector {
    /// From the touched symbol(s)' max caller count, via whatever scale
    /// the caller already uses (mirrors `risk_level_from_caller_count` in
    /// `calm-server`).
    pub caller_count_level: RiskLevel,
    /// Any touched symbol has `is_hub == true`.
    pub is_hub: bool,
    /// Strongest hub kind among touched symbols (e.g. `"degree"`,
    /// `"bridge"`, `"both"`), if any -- passed through verbatim, not
    /// re-interpreted, since only `calm-server` knows the exact strings
    /// its own `hub_kind_strength` ranks.
    pub hub_kind: Option<String>,
    /// A touched function/method's own signature meaning changed (per
    /// `is_signature_semantically_changed`), not just a line inside its
    /// range.
    pub signature_changed: bool,
    /// A touched zero-caller symbol's dead-code confidence couldn't rule
    /// out it being live (`classify_uncertain_zero_caller` fired).
    pub uncertain_zero_caller: bool,
    /// The highest-severity `config.risk_rules` glob match for the
    /// touched path, if any -- existing mechanism, read as-is (see module
    /// doc comment).
    pub risk_rule_floor: Option<RiskLevel>,
    /// Declared `ChangeIntentKind` disagreed with the observed
    /// `ChangeKind` (`change::classify::kinds_mismatch`). New in this PR.
    pub kind_mismatch: bool,
    /// The observed change classified as `ChangeKind::Manifest`. New in
    /// this PR.
    pub touches_manifest: bool,
    /// The touched range has no recorded test coverage
    /// (`CoverageData::is_covered` returned `false`). New in this PR.
    pub touches_uncovered_code: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_level_ordering_matches_the_existing_low_medium_high_scale() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
    }

    #[test]
    fn risk_level_round_trips_through_its_string_form() {
        for level in [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High] {
            assert_eq!(RiskLevel::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn risk_level_parse_rejects_unknown_strings() {
        assert_eq!(RiskLevel::parse("critical"), None);
        assert_eq!(RiskLevel::parse(""), None);
    }
}
