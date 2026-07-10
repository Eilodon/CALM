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
    pub rust: RustConfig,
    pub go: GoConfig,
    pub python: PythonConfig,
    pub js: JsConfig,
    pub java: JavaConfig,
    pub csharp: CSharpConfig,
    pub php: PhpConfig,
    pub clang: ClangConfig,
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
            rust: RustConfig::default(),
            go: GoConfig::default(),
            python: PythonConfig::default(),
            js: JsConfig::default(),
            java: JavaConfig::default(),
            csharp: CSharpConfig::default(),
            php: PhpConfig::default(),
            clang: ClangConfig::default(),
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RustConfig {
    pub scip: ScipConfig,
    /// LSP-backed resolve-time overlay (rust-analyzer `textDocument/definition`
    /// over stdio) — upgrades `ambiguous`/`textual` edges the same way SCIP's
    /// batch overlay does, but resolves interactively per call site instead of
    /// a one-shot `scip` dump. Distinct config block (not reusing `ScipConfig`)
    /// because the two overlays have different cost shapes: SCIP's `enabled`/
    /// `policy` gate a single batch subprocess run, `LspConfig`'s gate a
    /// persistent subprocess plus one request per unresolved call site.
    #[serde(default)]
    pub lsp: LspConfig,
}

/// Config for the LSP resolve-time overlay (`lsp_refresh` MCP tool / `calm lsp
/// run`). Mirrors `ScipConfig`'s three-state `enabled` + `RefreshPolicy`
/// shape deliberately — same reasoning applies here, see `ScipConfig`'s own
/// doc comment. Defaults to `policy: OnDemand` (not `OnSave`, unlike
/// `ScipConfig`'s default): a persistent LSP subprocess plus one
/// `textDocument/definition` round-trip per unresolved call site is a
/// materially heavier per-reindex cost than SCIP's single cached batch dump,
/// and `rebuild_graph` deletes+rebuilds all of `call_edges` on every reindex
/// that touches any file (`pipeline.rs::rebuild_graph`) — so an `OnSave`
/// default here would re-spawn rust-analyzer and re-resolve every ambiguous
/// call site on every single file save. Never auto-upgrade this default to
/// `OnSave` without first measuring real per-save latency at project scale.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LspConfig {
    /// Three-state, same semantics as `ScipConfig::enabled`: unset (default)
    /// auto-detects `rust-analyzer` and silently no-ops if absent; `Some(true)`
    /// forces on and logs once if the binary isn't found; `Some(false)` never
    /// even probes for it.
    pub enabled: Option<bool>,
    /// Optional explicit rust-analyzer binary path (else the same
    /// PATH/rustup/VS-Code auto-detect `ScipConfig::binary` uses, via
    /// `scip::runner::resolve_binary`).
    pub binary: Option<String>,
    /// Gates whether an *automatic* caller (background watcher, `calm index`)
    /// may run this overlay. Never gates an explicit manual refresh (the
    /// `lsp_refresh` MCP tool), which always runs regardless of policy — same
    /// contract as `ScipConfig::policy`. Default `OnDemand`: see this
    /// struct's own doc comment for why `OnSave` is not a safe default here
    /// the way it is for `ScipConfig`.
    #[serde(default = "default_lsp_policy")]
    pub policy: RefreshPolicy,
}

/// Hand-written (not derived) so the Rust-side default agrees with the serde
/// default: `#[derive(Default)]` would fill `policy` from
/// `RefreshPolicy::default()` — `OnSave`, the exact value this config's doc
/// comment forbids as a default — because derive never consults
/// `#[serde(default = "fn")]` attributes (those only apply during
/// deserialization of a present-but-partial `lsp` table). Caught in the
/// 2026-07-10 review pass: every unconfigured project (`RustConfig::default()`,
/// `load_config(...).unwrap_or_default()`) got `OnSave` silently.
impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            binary: None,
            policy: default_lsp_policy(),
        }
    }
}

fn default_lsp_policy() -> RefreshPolicy {
    RefreshPolicy::OnDemand
}

/// Go's overlay config (P2.1) — same `ScipConfig` shape as Rust's (three-state
/// `enabled`/`insert_missing`, optional binary override); a distinct wrapper
/// struct only so `config.json`'s `"go":{"scip":{...}}` doesn't collide with
/// `"rust":{"scip":{...}}` and so each language's block can grow independent
/// fields later without disturbing the other.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct GoConfig {
    pub scip: ScipConfig,
}

/// Python's overlay config (P2.4) — same `ScipConfig` shape and same
/// distinct-wrapper-struct reasoning as `GoConfig`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PythonConfig {
    pub scip: ScipConfig,
}

/// JS/TS's overlay config (P3.2) — same `ScipConfig` shape and same
/// distinct-wrapper-struct reasoning as `GoConfig`/`PythonConfig`. Covers
/// both `file_index.language` values (`"javascript"`/`"typescript"`) under
/// one block since `scip-typescript` indexes both in a single pass — see
/// `provider::TYPESCRIPT`'s `dirty_langs`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct JsConfig {
    pub scip: ScipConfig,
}

/// Java's overlay config (P2.2) — same `ScipConfig` shape and distinct-
/// wrapper-struct reasoning as `GoConfig`/`PythonConfig`/`JsConfig`, but
/// **not** `#[derive(Default)]` like the other three: `scip-java` drives a
/// full Maven/Gradle build (see `runner::JAVA_SCIP_TIMEOUT`'s doc comment),
/// exactly the "heavy future provider (Java/clang)" `ScipConfig::policy`'s
/// own doc comment predicted would need something other than the `OnSave`
/// every other provider defaults to. Default `MinInterval(900)` (15
/// minutes) matches the plan doc's P2.2 row ("Policy: OnDemand/
/// MinInterval(15m+)") — an automatic caller (watcher reindex, one-shot
/// `calm index`) only re-runs the full build-tool invocation at most every
/// 15 minutes; `calm scip run --lang java` / the `scip_refresh` MCP tool
/// always bypass this (see `run_overlay_for`'s `force` parameter).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct JavaConfig {
    pub scip: ScipConfig,
}

impl Default for JavaConfig {
    fn default() -> Self {
        Self {
            scip: ScipConfig {
                policy: RefreshPolicy::MinInterval(900),
                ..Default::default()
            },
        }
    }
}

/// C#'s overlay config (P2.3) — same `ScipConfig` shape and distinct-
/// wrapper-struct reasoning as `GoConfig`/`PythonConfig`/`JsConfig`.
/// `#[derive(Default)]` (plain `OnSave`, unlike `JavaConfig`): `scip-dotnet`
/// runs `dotnet restore` + a Roslyn compile, comparable in cost to
/// `scip-go`/`scip-typescript` rather than a from-scratch Maven/Gradle
/// build, so the default on-save cadence used by every provider except
/// Java is the right fit here too.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CSharpConfig {
    pub scip: ScipConfig,
}

/// PHP's overlay config (P2.5) — same `ScipConfig` shape and distinct-
/// wrapper-struct reasoning as the others. `#[derive(Default)]` (plain
/// `OnSave`): `scip-php` is a pure static AST walk (no build-tool
/// invocation, no compilation), the lightest of every provider added so
/// far — no reason to default it any more conservative than Rust's own
/// baseline.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PhpConfig {
    pub scip: ScipConfig,
}

/// C/C++'s overlay config (P3.1) — same `ScipConfig` shape and distinct-
/// wrapper-struct reasoning as the others, but **not** `#[derive(Default)]`
/// like Go/Python/JS/C#/PHP: `scip-clang` compiles real translation units
/// (see `runner::CLANG_SCIP_TIMEOUT`'s doc comment), the same "heavy future
/// provider (Java/clang)" case `JavaConfig` already exists for. Default
/// `MinInterval(900)` matches `JavaConfig`'s own default and the plan's
/// explicit risk note ("tuyệt đối không nối scip-java/scip-clang vào
/// on-save"). Deliberately does **not** carry the plan's originally
/// sketched `compile_commands: Option<String>` override field — see
/// `provider::CLANG`'s doc comment for why.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClangConfig {
    pub scip: ScipConfig,
}

impl Default for ClangConfig {
    fn default() -> Self {
        Self {
            scip: ScipConfig {
                policy: RefreshPolicy::MinInterval(900),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ScipConfig {
    /// Three-state, not a plain bool, so "unset" is distinguishable from an
    /// explicit choice:
    /// - unset / `null` (the default — `None`): auto-detect. The overlay
    ///   runs when `rust-analyzer` is found on `PATH`/rustup/VS Code, and is
    ///   silently skipped (no log line — this is the expected, common case
    ///   for a checkout without it) when it isn't. No config needed either
    ///   way.
    /// - `true`: force on. Same auto-detect binary search, but logs once at
    ///   `info` if `rust-analyzer` isn't found, since the user explicitly
    ///   asked for this and would want to know why it's a no-op.
    /// - `false`: force off. Never even probes for the binary.
    ///
    /// Backward compatible with the old plain-`bool` shape: existing configs
    /// with `"enabled": true` or `"enabled": false` still deserialize to
    /// `Some(true)`/`Some(false)` — only a config that never mentioned the
    /// key at all changes behavior (was off, is now auto-detect).
    pub enabled: Option<bool>,
    /// Optional explicit rust-analyzer binary path (else auto-detect).
    pub binary: Option<String>,
    /// Gated insert: when SCIP resolves a call site to a definition but no
    /// existing `call_edges` row represents that exact target at all (e.g.
    /// tree-sitter's own candidate selection dropped it for exceeding
    /// `MAX_CALLEE_CANDIDATES`, or the name never matched across crates) —
    /// insert a new `formal`/`formal_source: 'scip'` edge instead of leaving
    /// that call site permanently edge-less. Three-state like `enabled`:
    /// `None` (default) is auto-on — the gates already applied before an
    /// insert (fresh cache key, a uniquely-resolved definition symbol, a
    /// real syntactic `call_sites` row for the call, dedup against existing
    /// edges) are strict enough to be safe by default. `Some(false)` opts
    /// out entirely (e.g. while dogfooding a repo where you don't yet trust
    /// this).
    pub insert_missing: Option<bool>,
    /// When an *automatic* caller (the background watcher's incremental
    /// reindex, or `calm index`'s one-shot pass) may actually invoke this
    /// provider's indexer — never gates an explicit manual refresh (`calm
    /// scip run`, the `scip_refresh` MCP tool), which always runs regardless
    /// of policy. Default `OnSave` reproduces this feature's original
    /// behavior exactly (run whenever the cache key differs) — existing
    /// configs that never mention `policy` see zero behavior change. Real
    /// value for a heavy future provider (Java/clang): `MinInterval`/
    /// `OnDemand` keep a full build-tool invocation off the hot edit-save
    /// path (see the plan's own risk note: "indexer nặng không được chạy
    /// on-save").
    #[serde(default)]
    pub policy: RefreshPolicy,
}

/// See `ScipConfig::policy`. Serializes/deserializes as a plain string so
/// `config.json` stays human-writable: `"on_save"`, `"on_demand"`, or
/// `"min_interval:900"` (seconds).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    /// Run whenever the cache key differs from the last successful run —
    /// today's only behavior, still the default.
    #[default]
    OnSave,
    /// Never run automatically; only an explicit manual refresh does.
    OnDemand,
    /// Run automatically only if at least this many seconds have passed
    /// since the provider's last real (non-cache-skip) run.
    MinInterval(u64),
}

impl std::fmt::Display for RefreshPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshPolicy::OnSave => write!(f, "on_save"),
            RefreshPolicy::OnDemand => write!(f, "on_demand"),
            RefreshPolicy::MinInterval(secs) => write!(f, "min_interval:{secs}"),
        }
    }
}

impl Serialize for RefreshPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RefreshPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "on_save" => Ok(RefreshPolicy::OnSave),
            "on_demand" => Ok(RefreshPolicy::OnDemand),
            _ => s
                .strip_prefix("min_interval:")
                .and_then(|secs| secs.parse::<u64>().ok())
                .map(RefreshPolicy::MinInterval)
                .ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "invalid scip refresh policy {s:?} — expected \
                         \"on_save\", \"on_demand\", or \"min_interval:<seconds>\""
                    ))
                }),
        }
    }
}

#[cfg(test)]
mod refresh_policy_tests {
    use super::*;

    #[test]
    fn default_is_on_save() {
        assert_eq!(RefreshPolicy::default(), RefreshPolicy::OnSave);
    }

    #[test]
    fn round_trips_through_json_string_form() {
        for policy in [
            RefreshPolicy::OnSave,
            RefreshPolicy::OnDemand,
            RefreshPolicy::MinInterval(900),
        ] {
            let json = serde_json::to_string(&policy).unwrap();
            let back: RefreshPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(policy, back);
        }
    }

    #[test]
    fn min_interval_parses_seconds() {
        let policy: RefreshPolicy = serde_json::from_str("\"min_interval:900\"").unwrap();
        assert_eq!(policy, RefreshPolicy::MinInterval(900));
    }

    #[test]
    fn unset_policy_field_defaults_to_on_save() {
        let cfg: ScipConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.policy, RefreshPolicy::OnSave);
    }

    #[test]
    fn invalid_policy_string_is_a_clear_error_not_a_panic() {
        let result: Result<RefreshPolicy, _> = serde_json::from_str("\"bogus\"");
        assert!(result.is_err());
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SemanticSearchConfig {
    pub enabled: bool,
    pub model: String,
    pub dimensions: usize,
    pub index_on_startup: bool,
    /// When the vendored default-model asset is unusable (e.g. an unresolved
    /// Git LFS pointer left by a checkout that never ran `git lfs pull`/had
    /// git-lfs installed — not hypothetical, see the incident this field was
    /// added for), `true` (default) lets `Embedder::load` fall back to a
    /// one-time HuggingFace Hub download of the same default model, cached
    /// locally afterward (`~/.cache/huggingface`) — degrade to *slower on
    /// this run only*, not permanently `failed`. `false` keeps semantic
    /// search strictly zero-network: embeddings report
    /// `embeddings_status: "offline_unavailable"` instead of ever touching
    /// the network, until the vendored asset is fixed locally. Either way,
    /// this governs recovery from a *broken local asset* only — it does not
    /// change plain `search`/`callers`/etc., which never touch the network
    /// and never send code/repo content anywhere; that guarantee is
    /// independent of this flag.
    pub allow_network_fallback: bool,
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
            allow_network_fallback: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SearchConfig {
    pub text_chunk_context_lines: usize,
    pub text_max_chunk_lines: usize,
    pub rrf_k: usize,
    /// Additive weight applied to the session-journey proximity boost (see
    /// `CalmServer::apply_personalization_boost`) before
    /// re-ranking `search`/`locate` results — a result whose file is
    /// import/call-adjacent to something this session recently explored
    /// gets `score += personalization_weight * boost` (`boost` in `(0, 1]`).
    /// Additive-only by construction: it can nudge ordering among
    /// close-scoring results but a low default keeps it from overriding a
    /// strong text/semantic match. `0.0` disables personalization entirely.
    pub personalization_weight: f64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            text_chunk_context_lines: 10,
            text_max_chunk_lines: 50,
            rrf_k: 20,
            personalization_weight: 0.15,
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
/// calm-server). Kept here so `load_config` can validate `Config.preset`
/// without calm-core depending on calm-server.
pub const VALID_PRESETS: &[&str] = &["full", "orient", "trace", "edit", "compound"];

pub fn load_config(project_root: &Path) -> anyhow::Result<Config> {
    for candidate in [
        project_root.join("config.json"),
        project_root.join(".calm").join("config.json"),
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
    /// surface much later as "no tools filtered" behavior in calm-server's
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

#[cfg(test)]
mod scip_config_tests {
    use super::*;

    #[test]
    fn rust_scip_defaults_to_auto_detect_not_forced_on() {
        let c = Config::default();
        assert_eq!(
            c.rust.scip.enabled, None,
            "unset must mean auto-detect, not an explicit true/false"
        );
    }

    #[test]
    fn rust_scip_opt_in_parses() {
        let json = r#"{"rust":{"scip":{"enabled":true}}}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert_eq!(c.rust.scip.enabled, Some(true));
    }

    #[test]
    fn rust_scip_opt_out_parses() {
        let json = r#"{"rust":{"scip":{"enabled":false}}}"#;
        let c: Config = serde_json::from_str(json).unwrap();
        assert_eq!(c.rust.scip.enabled, Some(false));
    }
}
