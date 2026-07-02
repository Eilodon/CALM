use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub preset: String,
    pub languages: Vec<String>,
    pub ignore: Vec<String>,
    pub entry_points: Vec<String>,
    pub hub_threshold: HubThresholdConfig,
    pub semantic_search: SemanticSearchConfig,
    pub search: SearchConfig,
    pub path: PathConfig,
    pub callers: DepthConfig,
    pub callees: DepthConfig,
    pub hotspots: HotspotsConfig,
    pub dependencies: DependenciesConfig,
    pub session: SessionConfig,
    pub cochange: CoChangeConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            preset: "full".into(),
            languages: vec![
                "python".into(),
                "typescript".into(),
                "javascript".into(),
                "java".into(),
                "rust".into(),
                "go".into(),
            ],
            ignore: vec![
                "node_modules".into(),
                ".git".into(),
                "__pycache__".into(),
                "*.min.js".into(),
                "dist".into(),
                "build".into(),
                ".venv".into(),
            ],
            entry_points: Vec::new(),
            hub_threshold: HubThresholdConfig::default(),
            semantic_search: SemanticSearchConfig::default(),
            search: SearchConfig::default(),
            path: PathConfig::default(),
            callers: DepthConfig::default(),
            callees: DepthConfig::default(),
            hotspots: HotspotsConfig::default(),
            dependencies: DependenciesConfig::default(),
            session: SessionConfig::default(),
            cochange: CoChangeConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HubThresholdConfig {
    pub top_pct: f64,
    pub min_callers: i64,
    pub min_callers_bridge: i64,
    pub coreness_pct: f64,
}

impl Default for HubThresholdConfig {
    fn default() -> Self {
        Self {
            top_pct: 5.0,
            min_callers: 5,
            min_callers_bridge: 2,
            coreness_pct: 75.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SemanticSearchConfig {
    pub enabled: bool,
    pub model: String,
    pub dimensions: usize,
    pub index_on_startup: bool,
}

impl Default for SemanticSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Pure-Rust static code embeddings (model2vec-rs); distilled from
            // nomic CodeRankEmbed, 256-dim. Weights for this default model
            // are vendored into the binary — see `embedding::DEFAULT_MODEL_ID`.
            model: crate::embedding::DEFAULT_MODEL_ID.into(),
            dimensions: 256,
            index_on_startup: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SearchConfig {
    pub text_chunk_context_lines: usize,
    pub text_max_chunk_lines: usize,
    pub rrf_k: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            text_chunk_context_lines: 10,
            text_max_chunk_lines: 50,
            rrf_k: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PathConfig {
    pub default_max_hops: usize,
    pub max_allowed_hops: usize,
    pub timeout_ms: u64,
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            default_max_hops: 8,
            max_allowed_hops: 20,
            timeout_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DepthConfig {
    pub max_depth_cap: usize,
    pub transitive_timeout_ms: u64,
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            max_depth_cap: 4,
            transitive_timeout_ms: 3000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HotspotsConfig {
    pub default_top_n: usize,
    pub default_since: String,
    pub default_min_churn: usize,
    pub risk_critical_threshold: f64,
    pub risk_high_threshold: f64,
    pub risk_medium_threshold: f64,
}

impl Default for HotspotsConfig {
    fn default() -> Self {
        Self {
            default_top_n: 10,
            default_since: "6 months ago".into(),
            default_min_churn: 2,
            risk_critical_threshold: 0.75,
            risk_high_threshold: 0.50,
            risk_medium_threshold: 0.25,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DependenciesConfig {
    /// Cap on `imports` entries returned by the `dependencies` tool — a
    /// generous default since per-file import counts are normally small.
    pub max_imports: usize,
    /// Cap on `imported_by` entries — fan-in can be large for hub files, so
    /// this is the one more likely to actually bite.
    pub max_imported_by: usize,
}

impl Default for DependenciesConfig {
    fn default() -> Self {
        Self {
            max_imports: 200,
            max_imported_by: 200,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Cap on `explored_symbols`/`explored_files` returned by
    /// `session_context` — without this, a very long session dumps an
    /// unbounded list into every `session_context` call.
    pub max_fetched: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { max_fetched: 200 }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CoChangeConfig {
    /// `git log --since` window mined for co-change pairs — same syntax as
    /// `hotspots.default_since`, deliberately not shared with it: churn
    /// (single-file risk) and co-change (cross-file coupling) are different
    /// enough questions that a project may reasonably want different
    /// windows for each.
    pub since: String,
    /// Minimum number of shared commits before a file is reported — filters
    /// out one-off coincidences (e.g. a repo-wide reformat commit).
    pub min_co_changes: usize,
    pub top_n: usize,
}

impl Default for CoChangeConfig {
    fn default() -> Self {
        Self {
            since: "6 months ago".into(),
            min_co_changes: 3,
            top_n: 5,
        }
    }
}

/// Preset names recognized by the MCP tool router (`preset_tools` in
/// ci-server). Kept here so `load_config` can validate `Config.preset`
/// without ci-core depending on ci-server.
pub const VALID_PRESETS: &[&str] = &["full", "orient", "trace", "edit", "compound"];

pub fn load_config(project_root: &Path) -> anyhow::Result<Config> {
    for candidate in [
        project_root.join("config.json"),
        project_root.join(".codeindex").join("config.json"),
    ] {
        if candidate.exists() {
            let text = std::fs::read_to_string(&candidate)?;
            let config: Config = serde_json::from_str(&text)?;
            if !config.preset.is_empty() && !VALID_PRESETS.contains(&config.preset.as_str()) {
                anyhow::bail!(
                    "Unknown preset {:?} in {}. Valid presets: {}",
                    config.preset,
                    candidate.display(),
                    VALID_PRESETS.join(", ")
                );
            }
            return Ok(config);
        }
    }
    Ok(Config::default())
}

pub fn default_config_json() -> String {
    serde_json::to_string_pretty(&Config::default()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_preset_defaults_to_full() {
        let config = Config::default();
        assert_eq!(config.preset, "full");
    }

    #[test]
    fn config_preset_from_json() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_preset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.json"), r#"{"preset": "orient"}"#).unwrap();

        let config = crate::config::load_config(&tmp).unwrap();
        assert_eq!(
            config.preset, "orient",
            "config.json preset must be loaded, got: {}",
            config.preset
        );
    }

    /// Regression for Task 15: an unrecognized preset in config.json used to
    /// be silently accepted (no validation existed at all) and would only
    /// surface much later as "no tools filtered" behavior in ci-server's
    /// `preset_tools`, with no indication the value was a typo.
    #[test]
    fn config_load_rejects_unknown_preset() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_badpreset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.json"), r#"{"preset": "yolo"}"#).unwrap();

        let result = crate::config::load_config(&tmp);
        assert!(
            result.is_err(),
            "unknown preset should fail to load, got: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_load_accepts_every_valid_preset() {
        for preset in VALID_PRESETS {
            let tmp = std::env::temp_dir().join(format!(
                "ci_cfg_validpreset_{preset}_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(
                tmp.join("config.json"),
                format!(r#"{{"preset": "{preset}"}}"#),
            )
            .unwrap();

            let result = crate::config::load_config(&tmp);
            assert!(
                result.is_ok(),
                "preset {preset:?} should be valid: {result:?}"
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    #[test]
    fn config_load_accepts_empty_preset() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_emptypreset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.json"), r#"{"preset": ""}"#).unwrap();

        let result = crate::config::load_config(&tmp);
        assert!(
            result.is_ok(),
            "empty preset means 'full', should be valid: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
