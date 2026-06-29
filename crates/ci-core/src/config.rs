use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub preset: String,
    pub languages: Vec<String>,
    pub ignore: Vec<String>,
    pub entry_points: Vec<String>,
    pub hub_threshold: HubThresholdConfig,
    pub call_graph: CallGraphConfig,
    pub semantic_search: SemanticSearchConfig,
    pub search: SearchConfig,
    pub path: PathConfig,
    pub callers: DepthConfig,
    pub callees: DepthConfig,
    pub hotspots: HotspotsConfig,
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
            call_graph: CallGraphConfig::default(),
            semantic_search: SemanticSearchConfig::default(),
            search: SearchConfig::default(),
            path: PathConfig::default(),
            callers: DepthConfig::default(),
            callees: DepthConfig::default(),
            hotspots: HotspotsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CallGraphConfig {
    pub resolver: String,
    pub confidence_tracking: bool,
}

impl Default for CallGraphConfig {
    fn default() -> Self {
        Self {
            resolver: "conservative".into(),
            confidence_tracking: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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
            enabled: false,
            model: "BAAI/bge-base-en-v1.5".into(),
            dimensions: 768,
            index_on_startup: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

pub fn load_config(project_root: &Path) -> anyhow::Result<Config> {
    for candidate in [
        project_root.join("config.json"),
        project_root.join(".codeindex").join("config.json"),
    ] {
        if candidate.exists() {
            let text = std::fs::read_to_string(&candidate)?;
            return Ok(serde_json::from_str(&text)?);
        }
    }
    Ok(Config::default())
}
