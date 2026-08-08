//! `.calm/policy.toml` loader -- CCK-08. Mirrors `config::load_config`'s
//! shape (`Result` variant that fails loudly on a malformed file, plus an
//! `_or_warn` variant that falls back to defaults with a logged reason) so
//! a broken `policy.toml` behaves the same way a broken `config.json`
//! already does, rather than introducing a second silent-fallback hazard.

use std::path::Path;

use crate::policy::model::RiskLevel;

/// How [`evaluate::evaluate`](super::evaluate::evaluate) escalates on each
/// new-in-this-PR signal. All three default to `High`: "mismatch never
/// silently accepted" (blueprint's own wording for `kind_mismatch`)
/// generalizes to every axis this PR adds, since none of them existed
/// before and a project that hasn't tuned `policy.toml` yet should get the
/// most conservative behavior, not the least.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Policy {
    pub kind_mismatch_floor: RiskLevel,
    pub manifest_floor: RiskLevel,
    pub uncovered_code_floor: RiskLevel,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            kind_mismatch_floor: RiskLevel::High,
            manifest_floor: RiskLevel::High,
            uncovered_code_floor: RiskLevel::High,
        }
    }
}

const POLICY_FILE_RELATIVE: &str = ".calm/policy.toml";

/// `Ok(Policy::default())` when `.calm/policy.toml` doesn't exist (the
/// common, zero-config case) -- an `Err` only for a file that exists but
/// fails to parse, or whose values aren't valid TOML for this shape.
pub fn load_policy(project_root: &Path) -> anyhow::Result<Policy> {
    let candidate = project_root.join(POLICY_FILE_RELATIVE);
    if !candidate.exists() {
        return Ok(Policy::default());
    }
    let text = std::fs::read_to_string(&candidate)?;
    let policy: Policy = toml::from_str(&text)?;
    Ok(policy)
}

/// Same as `load_policy(project_root).unwrap_or_default()`, but logs why a
/// load fell back to defaults -- see `config::load_config_or_warn`'s doc
/// comment for the exact hazard this avoids.
pub fn load_policy_or_warn(project_root: &Path) -> Policy {
    match load_policy(project_root) {
        Ok(policy) => policy,
        Err(e) => {
            tracing::warn!(
                "failed to load {POLICY_FILE_RELATIVE} for {}, falling back to defaults: {e}",
                project_root.display()
            );
            Policy::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_policy_file_yields_defaults() {
        let root = tempfile::tempdir().unwrap();
        let policy = load_policy(root.path()).unwrap();
        assert_eq!(policy, Policy::default());
    }

    #[test]
    fn defaults_are_all_high_the_maximally_conservative_setting() {
        let d = Policy::default();
        assert_eq!(d.kind_mismatch_floor, RiskLevel::High);
        assert_eq!(d.manifest_floor, RiskLevel::High);
        assert_eq!(d.uncovered_code_floor, RiskLevel::High);
    }

    #[test]
    fn partial_policy_toml_fills_missing_fields_from_defaults() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".calm")).unwrap();
        std::fs::write(root.path().join(".calm/policy.toml"), "manifest_floor = \"medium\"\n").unwrap();
        let policy = load_policy(root.path()).unwrap();
        assert_eq!(policy.manifest_floor, RiskLevel::Medium);
        assert_eq!(policy.kind_mismatch_floor, RiskLevel::High, "unset fields keep the default");
    }

    #[test]
    fn malformed_policy_toml_is_a_loud_error_not_a_silent_default() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".calm")).unwrap();
        std::fs::write(root.path().join(".calm/policy.toml"), "not valid toml {{{").unwrap();
        assert!(load_policy(root.path()).is_err());
    }

    #[test]
    fn load_policy_or_warn_falls_back_on_malformed_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".calm")).unwrap();
        std::fs::write(root.path().join(".calm/policy.toml"), "not valid toml {{{").unwrap();
        assert_eq!(load_policy_or_warn(root.path()), Policy::default());
    }
}
