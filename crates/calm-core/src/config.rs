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
    pub ruby: RubyConfig,
    pub indexing: IndexingConfig,
    pub edit: EditConfig,
    pub orientation: OrientationConfig,
    pub verification: VerificationConfig,
    /// Path-based floor on top of `compute_touch_risk`'s purely structural
    /// (caller-count/hub) signal -- see `RiskRule`'s own doc comment for
    /// why this exists and what it can/can't do. Empty by default: zero
    /// behavior change unless a project opts in.
    pub risk_rules: Vec<RiskRule>,
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
            // Bare-name conventions for common language entry points
            // (matched by exact symbol name in `extract_file_data`, kind-
            // gated to function/method only — see pipeline.rs). Previously
            // empty, so a fresh install never flagged any entry point via
            // this path at all (per-language `detect_entry_point` in
            // parser.rs still covers `fn main`/`func main`/`public static
            // void main` etc. directly; this list is the user-configurable
            // supplement for framework entry conventions like `serve`/
            // `handler`/`cli` that vary too much per project to hardcode).
            entry_points: vec![
                "main".into(),
                "__main__".into(),
                "serve".into(),
                "run".into(),
                "handler".into(),
                "cli".into(),
            ],
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
            ruby: RubyConfig::default(),
            indexing: IndexingConfig::default(),
            edit: EditConfig::default(),
            orientation: OrientationConfig::default(),
            verification: VerificationConfig::default(),
            risk_rules: Vec::new(),
        }
    }
}

/// One `.calm/config.json` `risk_rules` entry: any edit touching a path
/// matching `glob` can never be classified below `minimum` risk by the
/// write gate (`compute_touch_risk`/`classify_gate`,
/// crates/calm-server/src/tools/edit.rs), regardless of what the purely
/// structural caller-count/hub signal alone would say. Closes the gap
/// where a low-fan-in but security-sensitive file (an auth handler, a CI
/// workflow, a migration) reads as "low risk" simply because few other
/// symbols happen to call it yet.
///
/// Deliberately a FLOOR, never a ceiling: `risk_floor_for_path` only ever
/// raises the structural risk to `minimum`, never lowers it, so a
/// misconfigured or malicious `risk_rules` entry can weaken this gate for
/// no path -- the worst a bad entry can do is over-gate, never under-gate.
///
/// Distinct from the ownership-entropy/dead-code-confidence escalations
/// `edit_context`'s advisory `risk`/`risk_reasons` field also applies
/// (guardrails.rs) -- those are deliberately kept OUT of the hard
/// write-blocking gate (see that code's own doc comment: entropy measures
/// review coverage, not danger-of-being-wrong). A `risk_rules` glob match
/// is the opposite case: it says "getting this file wrong is dangerous
/// regardless of how many callers happen to exist today", which is
/// exactly the axis the hard gate already exists to protect, so it feeds
/// the gate directly instead of staying advisory-only.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RiskRule {
    /// Glob matched against the edited file's repo-relative path (forward
    /// slashes) -- `globset::Glob` syntax, e.g. `"**/auth/**"`,
    /// `".github/workflows/**"`, `"**/migrations/**"`.
    pub glob: String,
    /// One of `VALID_RISK_LEVELS` (`"low"`/`"medium"`/`"high"`) -- must be
    /// a level `classify_gate` actually understands; validated at config
    /// load (`load_config`), not silently clamped, since a silently
    /// no-op'd rule is worse than a loud config error for a
    /// security-relevant control.
    pub minimum: String,
}

/// Risk levels `classify_gate`'s hard write-blocking gate actually
/// understands -- `RiskRule.minimum` is validated against this list at
/// config load, not against `RiskOrder`'s broader set (which also has
/// `"critical"`, understood by `diff_impact`'s advisory reporting but NOT
/// by the write gate, which only ever checks `risk == Some("high")`).
pub const VALID_RISK_LEVELS: &[&str] = &["low", "medium", "high"];

fn risk_rule_severity(level: &str) -> u8 {
    match level {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

/// The highest-severity `rules` entry whose `glob` matches `path`, as
/// `(minimum, glob)` -- `None` if no rule matches or `rules` is empty (the
/// default, zero-behavior-change case). `path` is expected repo-relative
/// with forward slashes, same convention every other glob match in this
/// crate uses. Compiles each glob fresh per call rather than caching a
/// `GlobSet` -- `risk_rules` lists are expected to be small (a handful of
/// security-sensitive path patterns, not thousands), the same "small list,
/// compile inline" precedent `search.rs`'s own glob matching already sets;
/// an unparseable glob is treated as a non-match here (validated loudly at
/// config load instead, see `load_config`, so a bad glob is caught long
/// before it would ever reach this silent-skip path in practice).
pub fn risk_floor_for_path<'a>(rules: &'a [RiskRule], path: &str) -> Option<(&'a str, &'a str)> {
    rules
        .iter()
        .filter(|r| {
            globset::Glob::new(&r.glob)
                .map(|g| g.compile_matcher().is_match(path))
                .unwrap_or(false)
        })
        .max_by_key(|r| risk_rule_severity(&r.minimum))
        .map(|r| (r.minimum.as_str(), r.glob.as_str()))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HubThresholdConfig {
    pub top_pct: f64,
    pub min_callers: i64,
    pub min_callers_bridge: i64,
    pub coreness_pct: f64,
}

/// Phase B (`docs/plans/2026-07-13-phase-b-incremental-graph-update.md`)
/// Edit-tool behavior flags (`[edit]` in `.calm/config.json`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EditConfig {
    /// Human-in-the-loop veto for hub/high-risk edits: when `true` AND the
    /// connected MCP client declared form-mode elicitation support, an edit
    /// that passed the full machine gate (confirm:true + edit_context +
    /// grounded reason) additionally asks the human at the client UI before
    /// writing. Default `false` — zero behavior change unless opted in.
    /// The veto is fail-closed: decline, timeout, or a broken elicitation
    /// round-trip all refuse the edit; it can never approve an edit the
    /// machine gate would have refused.
    pub elicit_hub_confirm: bool,
    /// How long to wait for the human's answer before refusing with
    /// ELICITATION_TIMEOUT. Only read when `elicit_hub_confirm` is on.
    pub elicit_timeout_secs: u64,
    /// Widens `edit_lines`/`edit_symbol`'s write-time risk gate
    /// (`EDIT_CONTEXT_REQUIRED`/`CONFIRM_REQUIRED`/`REASON_NOT_GROUNDED`,
    /// see `crates/calm-server/src/tools/edit.rs`) to fire on every touched
    /// symbol, not just one that's `is_hub`/`risk_assessment: "high"`/an
    /// uncertain-zero-caller symbol. Default `false` — zero behavior change
    /// unless opted in; the existing hub/high-risk-only gate already applies
    /// regardless of this flag. This is a protocol-level gate (the tool's
    /// own JSON-RPC error response), so unlike a Claude-Code-only hook it
    /// applies identically to every MCP client.
    pub always_require_edit_context: bool,
}

impl Default for EditConfig {
    fn default() -> Self {
        Self {
            elicit_hub_confirm: false,
            elicit_timeout_secs: 120,
            always_require_edit_context: false,
        }
    }
}

/// `[verification]` in `.calm/config.json` -- WS-6 first slice (docs/plans/
/// 2026-08-03-ws6-verification-pipeline-execution-plan.md). Gates whether
/// `edit_lines`/`edit_symbol` route a transaction through
/// `TxState::VerifyPending` (crates/calm-core/src/txn.rs) instead of
/// straight to `Done`, and whether `verify_change` has anything to do.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VerificationConfig {
    /// When `true` AND the touched file is `.rs`: after a write reaches
    /// `IndexCommitted`, the transaction advances to `VerifyPending`
    /// instead of `Done`, and the `edit_lines`/`edit_symbol` response
    /// includes a `tx_id` with a suggestion to call `verify_change` on it.
    /// Default `false` -- zero behavior change unless opted in; every
    /// transaction still advances straight to `Done` exactly as before.
    /// Only Rust is supported today; a non-`.rs` file is unaffected by this
    /// flag regardless of its value.
    pub rust_check_on_write: bool,
    /// Wall-clock budget for one `verify_change` run of `cargo check`
    /// (build.rs/proc-macros/git-dependency fetches can all hang or run
    /// long) -- exceeding it kills the child process and reports a failed
    /// verification rather than blocking the tool call indefinitely.
    /// `DEFAULT_VERIFY_TIMEOUT_SECS` (120s) if unset.
    pub timeout_secs: u64,
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            rust_check_on_write: false,
            timeout_secs: crate::verify::DEFAULT_VERIFY_TIMEOUT_SECS,
        }
    }
}

/// `mode` values for `[orientation]` in `.calm/config.json` — see
/// `OrientationConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OrientationMode {
    /// No session-start orientation behavior at all — today's pre-2026-07-22
    /// behavior (only the advisory `get_info().with_instructions()` push and
    /// tool descriptions, neither of them enforced).
    Off,
    /// The first tool call of a session that isn't already orientation-
    /// adjacent (`repo_overview`, `indexing_status`, `session_context`) still
    /// runs normally, but the server merges a compact orientation summary
    /// into that same response — impossible to miss, never fails the call.
    /// Default: a session that ignores the advisory push still can't avoid
    /// seeing this on its very first real tool response, on any MCP client.
    #[default]
    Inject,
    /// The first non-orientation-adjacent tool call of a session is refused
    /// outright with `ORIENTATION_REQUIRED` until `repo_overview` has been
    /// called. Automatically no-ops (falls back to `Inject`) when the
    /// active tool_router (preset-scoped, see `resolve_preset`) doesn't
    /// register any orientation-adjacent tool at all — e.g. a single-toolset
    /// preset like `--preset "security"` never includes `repo_overview`, and
    /// a literal block with no escape hatch would deadlock every call for
    /// that session permanently.
    Block,
}

impl OrientationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Inject => "inject",
            Self::Block => "block",
        }
    }
}

/// Session-start orientation gate (`[orientation]` in `.calm/config.json`) —
/// the single, client-agnostic dispatch chokepoint every `tools/call` request
/// passes through (`CalmServer::call_tool`), regardless of MCP client
/// (Claude Code, Cursor, Windsurf, Codex CLI, or a hand-rolled client), since
/// it's server-side protocol logic rather than a Claude-Code-only hook.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OrientationConfig {
    pub mode: OrientationMode,
    /// Whether every tool response (not just `session_context`, which
    /// already surfaces this today) should carry a reminder while this
    /// connection has files written (`written_files_snapshot`) that haven't
    /// had `diff_impact` run on them since. Default `true`. Always advisory
    /// — `diff_impact`-before-commit can never be a hard server-side gate at
    /// all (an MCP server has no visibility into a client's own native
    /// Bash/Edit tool calls, e.g. `git commit`), so this only makes an
    /// already-real signal harder to miss, on every client uniformly,
    /// instead of adding new enforcement.
    pub remind_pending_diff_impact: bool,
}

impl Default for OrientationConfig {
    fn default() -> Self {
        Self {
            mode: OrientationMode::default(),
            remind_pending_diff_impact: true,
        }
    }
}

/// indexing-pipeline flags.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IndexingConfig {
    /// Route `reindex_paths`/`reindex_changed_cancellable`'s non-noop passes
    /// through `incremental_graph_update` (scoped re-resolve, keeping the
    /// formal edges of unchanged files alive) instead of the full
    /// `rebuild_graph` sweep. Default `true` since plan T6: golden-
    /// equivalence (incremental == full) passed on both the fixture driver
    /// and a copy of the real CALM DB, and live dogfooding put the
    /// reindex+graph path at 88–188 ms vs ~337 ms for a full rebuild. `calm
    /// index` (CLI one-shot / fresh index) always goes through
    /// `run_indexing_pipeline_cancellable`, which never reads this flag — a
    /// permanent full-rebuild path. Set `false` in config.json to force
    /// every reindex back to full rebuild (the rollback switch).
    pub incremental_graph: bool,
}

impl Default for IndexingConfig {
    fn default() -> Self {
        // Flipped false → true at plan T6 (see the field's doc comment) once
        // golden-equivalence passed on the real indexed DB — before that,
        // this whole code path was dead weight no build ever executed.
        Self {
            incremental_graph: true,
        }
    }
}

impl Default for HubThresholdConfig {
    fn default() -> Self {
        Self {
            top_pct: 5.0,
            min_callers: 5,
            min_callers_bridge: 4, // Plan 3 §3.3 (F10): was 2, measured 9.8% hub_pct on CALM itself
            coreness_pct: 75.0, // Plan 3 §3.3 (F10): unchanged — min_callers_bridge=4 alone reached 4.37% on CALM (target 3-5%); an earlier calibration run showing 6.4%/2.71% at pct=75/90 was invalidated by a stale local .calm/config.json hub_threshold override (min_callers_bridge:2) that had nothing to do with this value — see plan doc
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
    /// LSP resolve-time overlay (gopls `textDocument/definition`) — same
    /// role and gating contract as `RustConfig.lsp`, added when the LSP
    /// overlay was generalized from a Rust-only pass into a per-language
    /// provider table (D.0, 2026-07-11). `#[derive(Default)]` on this
    /// struct already calls `LspConfig::default()` for this field, so no
    /// manual `impl Default` is needed here the way `RustConfig` doesn't
    /// need one either.
    #[serde(default)]
    pub lsp: LspConfig,
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

/// Ruby's overlay config (Phase D.1) — same `ScipConfig` shape and
/// distinct-wrapper-struct reasoning as the others, but **not**
/// `#[derive(Default)]` like Go/Python/JS/C#/PHP: `scip-ruby` runs a real
/// (if best-effort on untyped files) Sorbet type-check pass over the whole
/// project, closer in cost to Java/Clang's heavier providers than PHP's
/// pure AST walk. Default `MinInterval(900)` matches `JavaConfig`'s/
/// `ClangConfig`'s own default for the same reason.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct RubyConfig {
    pub scip: ScipConfig,
}

impl Default for RubyConfig {
    fn default() -> Self {
        Self {
            scip: ScipConfig {
                policy: RefreshPolicy::MinInterval(900),
                ..Default::default()
            },
        }
    }
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
    /// LSP resolve-time overlay (clangd `textDocument/definition`) — same
    /// role as `RustConfig.lsp`/`GoConfig.lsp`, added when the LSP overlay
    /// was generalized (D.0, 2026-07-11). Unlike `scip`, this field's
    /// default policy is `OnDemand` (via `LspConfig::default()`), not
    /// `MinInterval(900)` — see `LspConfig`'s own doc comment for why an
    /// LSP overlay's per-reindex cost profile differs from a batch SCIP
    /// indexer's, independent of which language it's for.
    #[serde(default)]
    pub lsp: LspConfig,
}

impl Default for ClangConfig {
    fn default() -> Self {
        Self {
            scip: ScipConfig {
                policy: RefreshPolicy::MinInterval(900),
                ..Default::default()
            },
            lsp: LspConfig::default(),
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
    /// Max entries kept in `callers`/`callees`'s own `direct`/`ambiguous`
    /// lists (single-hop, not the `transitive` BFS `max_depth_cap` already
    /// caps above) before truncating. A real hub symbol can have 50-200+
    /// direct callers with zero cap — `direct_count`/`ambiguous_count`
    /// always report the true total regardless of this cap, so nothing
    /// about scale is lost, only the raw per-entry dump beyond this size.
    pub direct_list_cap: usize,
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            max_depth_cap: 4,
            transitive_timeout_ms: 3000,
            direct_list_cap: 25,
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
/// calm-server), plus `"remote-safe"` -- a virtual, capability-derived
/// token (every tool with `read_only_hint = true`, computed live off the
/// real tool router by calm-server's `remote_safe_tool_names`, not a
/// `#[tool_router]` module like the `VALID_TOOLSET_NAMES` entries below)
/// that only `resolve_preset`'s composable parser understands, never
/// `preset_tools` directly. Included here anyway so this lighter
/// syntax-only check (see `preset_syntax_is_plausible`) doesn't reject a
/// `.calm/config.json` that legitimately sets `"preset": "remote-safe"`
/// before calm-server's authoritative validator ever gets a chance to
/// accept it. Kept here so `load_config` can validate `Config.preset`
/// without calm-core depending on calm-server.
pub const VALID_PRESETS: &[&str] = &["full", "orient", "trace", "edit", "compound", "remote-safe"];

/// Toolset (module-domain) names for the composable half of the preset
/// registry (2026-07-14 upgrade item) — mirrors calm-server's
/// `tools::common::TOOLSET_NAMES` exactly, one name per `#[tool_router]`
/// module. Duplicated here (not imported) for the same reason
/// `VALID_PRESETS` is: calm-core cannot depend on calm-server. The two
/// lists WILL silently drift if either is edited alone — calm-server's
/// `toolset_names_match_calm_core_valid_toolset_names` test is the one
/// thing that catches that; if this list changes, that test must too.
pub const VALID_TOOLSET_NAMES: &[&str] = &[
    "trace",
    "locate",
    "orient",
    "memory",
    "guardrails",
    "recover",
    "scip",
    "lsp",
    "security",
    "testgap",
    "inspect",
    "edit",
    "patterndebt",
    "txn",
    "change",
];
pub fn load_config(project_root: &Path) -> anyhow::Result<Config> {
    match resolve_config_path(project_root) {
        Some(candidate) => {
            let text = std::fs::read_to_string(&candidate)?;
            let config: Config = serde_json::from_str(&text)?;
            if !config.preset.is_empty() && !preset_syntax_is_plausible(&config.preset) {
                anyhow::bail!(
                    "Unknown preset {:?} in {}. Valid presets: {}. Valid toolsets \
                     (comma-composable, e.g. \"trace,security\" or \"full,-edit\"): {}",
                    config.preset,
                    candidate.display(),
                    VALID_PRESETS.join(", "),
                    VALID_TOOLSET_NAMES.join(", ")
                );
            }
            for rule in &config.risk_rules {
                if !VALID_RISK_LEVELS.contains(&rule.minimum.as_str()) {
                    anyhow::bail!(
                        "Unknown risk_rules minimum {:?} for glob {:?} in {}. Valid levels: {}",
                        rule.minimum,
                        rule.glob,
                        candidate.display(),
                        VALID_RISK_LEVELS.join(", ")
                    );
                }
                if let Err(e) = globset::Glob::new(&rule.glob) {
                    anyhow::bail!(
                        "Invalid risk_rules glob {:?} in {}: {e}",
                        rule.glob,
                        candidate.display()
                    );
                }
            }
            Ok(config)
        }
        None => Ok(Config::default()),
    }
}

/// Same as `load_config(project_root).unwrap_or_default()`, but logs why a
/// load fell back to defaults instead of failing silently — a malformed
/// `config.json` (JSON syntax error, unknown preset) during an incremental
/// reindex would otherwise silently re-enable every default (ignore
/// patterns, entry points, hub thresholds, ...) with no signal to the user
/// that their config was never actually read. See `LspConfig::default`'s
/// doc comment above for a previous instance of this same silent-fallback
/// hazard.
pub fn load_config_or_warn(project_root: &Path) -> Config {
    match load_config(project_root) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(
                "failed to load config for {}, falling back to defaults: {e}",
                project_root.display()
            );
            Config::default()
        }
    }
}

/// Syntactic-only check: does `preset` look like either a legacy bare
/// name or a comma-composed toolset spec? A single unrecognized bare
/// token (the common typo case — `"preset": "flul"`) is still rejected
/// right here, matching the exact UX this check has always had. A
/// composed spec's individual TOOLSET tokens are also checked against
/// `VALID_TOOLSET_NAMES` (kept in sync with calm-server's own
/// `common::TOOLSET_NAMES` — see that const's doc comment for the
/// cross-check test), so a typo'd toolset name in config.json is still
/// caught here, not just at server startup.
///
/// What this does NOT validate: individual bare tool names composed into
/// a spec (e.g. a hypothetical `"trace,+scan_text"`) — calm-core has no
/// visibility into individual registered tool names without depending on
/// calm-server (see `VALID_PRESETS`'s doc comment on why that dependency
/// direction is off the table). `calm_server::tools::common::resolve_preset`
/// is the full, authoritative validator; it runs at server startup
/// (`CalmServer::new_with_preset`) and fails loudly there for anything
/// this lighter check let through — see that function's own doc comment.
fn preset_syntax_is_plausible(preset: &str) -> bool {
    let is_valid_token = |t: &str| {
        let name = t.strip_prefix('-').unwrap_or(t);
        name == "full" || VALID_PRESETS.contains(&name) || VALID_TOOLSET_NAMES.contains(&name)
    };
    if !preset.contains(',') {
        return VALID_PRESETS.contains(&preset) || VALID_TOOLSET_NAMES.contains(&preset);
    }
    preset.split(',').map(str::trim).all(is_valid_token)
}

/// The `config.json`/`.calm/config.json` candidate `load_config` would
/// currently read (first existing one wins), or `None` when neither
/// exists — single source of truth for that resolution order so
/// `config_mtime` (audit F12's cache-key helper) can never drift from
/// what `load_config` itself would actually load.
pub fn resolve_config_path(project_root: &Path) -> Option<std::path::PathBuf> {
    [
        project_root.join("config.json"),
        project_root.join(".calm").join("config.json"),
    ]
    .into_iter()
    .find(|p| p.exists())
}
/// mtime of whichever config file `load_config` would currently read
/// (`None` when neither exists) — a cache-invalidation key: unchanged
/// mtime means the previously-loaded `Config` is still current, without
/// re-reading or re-parsing the file (audit F12). Note: filesystem mtime
/// resolution (commonly ~1s) means two writes to config.json within the
/// same tick could be missed; acceptable for a file edited by a human at
/// human timescale, not by an automated rewrite loop.
pub fn config_mtime(project_root: &Path) -> Option<std::time::SystemTime> {
    resolve_config_path(project_root).and_then(|p| std::fs::metadata(p).ok()?.modified().ok())
}

/// Root-cause fix for the F10 calibration bug: a local `config.json`/
/// `.calm/config.json` can silently override any nested field of
/// `Config::default()` (struct-level `#[serde(default)]` means the file
/// only needs to mention the fields it overrides), with previously zero
/// visibility into which fields were actually shadowed. Diffs `loaded`
/// against `Config::default()` generically via `serde_json::Value` (dot-
/// separated field paths, e.g. `"hub_threshold.min_callers_bridge"`) so
/// this never rots when new `Config` fields are added later — no manual
/// `PartialEq`/diff maintenance required.
pub fn diff_from_default(loaded: &Config) -> Vec<String> {
    let default_value = serde_json::to_value(Config::default()).unwrap_or(serde_json::Value::Null);
    let loaded_value = serde_json::to_value(loaded).unwrap_or(serde_json::Value::Null);
    let mut paths = Vec::new();
    collect_diff_paths(&default_value, &loaded_value, "", &mut paths);
    paths
}

fn collect_diff_paths(
    default: &serde_json::Value,
    loaded: &serde_json::Value,
    prefix: &str,
    out: &mut Vec<String>,
) {
    if let (serde_json::Value::Object(d), serde_json::Value::Object(l)) = (default, loaded) {
        let mut keys: Vec<&String> = d.keys().chain(l.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            let next_prefix = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match (d.get(key), l.get(key)) {
                (Some(dv), Some(lv)) => collect_diff_paths(dv, lv, &next_prefix, out),
                _ => out.push(next_prefix),
            }
        }
    } else if default != loaded {
        out.push(prefix.to_string());
    }
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
    fn config_load_rejects_unknown_risk_rule_minimum() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_badrisk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"risk_rules": [{"glob": "**/auth/**", "minimum": "critical"}]}"#,
        )
        .unwrap();

        let result = crate::config::load_config(&tmp);
        assert!(
            result.is_err(),
            "an unrecognized risk_rules minimum must fail to load, got: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_load_rejects_invalid_risk_rule_glob() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_badglob_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"risk_rules": [{"glob": "[", "minimum": "high"}]}"#,
        )
        .unwrap();

        let result = crate::config::load_config(&tmp);
        assert!(
            result.is_err(),
            "an unparseable glob must fail to load, got: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_load_accepts_valid_risk_rules() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_goodrisk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"risk_rules": [{"glob": "**/auth/**", "minimum": "high"}]}"#,
        )
        .unwrap();

        let config = crate::config::load_config(&tmp).unwrap();
        assert_eq!(config.risk_rules.len(), 1);
        assert_eq!(config.risk_rules[0].minimum, "high");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn risk_floor_for_path_matches_the_configured_glob() {
        let rules = vec![RiskRule {
            glob: "**/auth/**".to_string(),
            minimum: "high".to_string(),
        }];
        assert_eq!(
            risk_floor_for_path(&rules, "src/auth/login.rs"),
            Some(("high", "**/auth/**"))
        );
        assert_eq!(risk_floor_for_path(&rules, "src/db/query.rs"), None);
    }

    #[test]
    fn risk_floor_for_path_picks_the_strictest_of_several_matching_rules() {
        let rules = vec![
            RiskRule {
                glob: "**/auth/**".to_string(),
                minimum: "medium".to_string(),
            },
            RiskRule {
                glob: "**/*.rs".to_string(),
                minimum: "high".to_string(),
            },
        ];
        // Both rules match src/auth/login.rs -- the stricter ("high") wins,
        // regardless of which rule appears first in the list.
        assert_eq!(
            risk_floor_for_path(&rules, "src/auth/login.rs"),
            Some(("high", "**/*.rs"))
        );
    }

    #[test]
    fn risk_floor_for_path_is_none_for_an_empty_rule_list() {
        assert_eq!(risk_floor_for_path(&[], "src/auth/login.rs"), None);
    }

    #[test]
    fn config_load_accepts_composed_toolset_preset() {
        let tmp =
            std::env::temp_dir().join(format!("ci_cfg_composedpreset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.json"), r#"{"preset": "trace,security"}"#).unwrap();

        let config = crate::config::load_config(&tmp);
        assert!(
            config.is_ok(),
            "comma-composed toolset preset should load, got: {config:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_load_accepts_bare_toolset_name_and_exclusion_syntax() {
        for preset in ["security", "full,-edit"] {
            let tmp = std::env::temp_dir().join(format!(
                "ci_cfg_toolsetsyntax_{}_{}",
                preset.replace([',', '-'], "_"),
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp).unwrap();
            std::fs::write(
                tmp.join("config.json"),
                format!(r#"{{"preset": "{preset}"}}"#),
            )
            .unwrap();

            let config = crate::config::load_config(&tmp);
            assert!(
                config.is_ok(),
                "preset {preset:?} should load, got: {config:?}"
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    #[test]
    fn config_load_rejects_unknown_token_in_composed_preset() {
        let tmp =
            std::env::temp_dir().join(format!("ci_cfg_badcomposedpreset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"preset": "trace,not_a_real_toolset"}"#,
        )
        .unwrap();

        let result = crate::config::load_config(&tmp);
        assert!(
            result.is_err(),
            "unknown toolset token in a composed preset should fail to load, got: {result:?}"
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

    #[test]
    fn config_mtime_none_when_no_config_file() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_mtime_absent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        assert_eq!(crate::config::config_mtime(&tmp), None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_mtime_matches_config_json_over_dotcalm_variant() {
        // Same candidate order as load_config: bare config.json wins over
        // .calm/config.json when both exist (audit F12 -- config_mtime must
        // never pick a different file than load_config would).
        let tmp =
            std::env::temp_dir().join(format!("ci_cfg_mtime_precedence_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".calm")).unwrap();
        std::fs::write(tmp.join(".calm").join("config.json"), "{}").unwrap();
        std::fs::write(tmp.join("config.json"), "{}").unwrap();

        let expected = std::fs::metadata(tmp.join("config.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(crate::config::config_mtime(&tmp), Some(expected));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_mtime_changes_when_file_is_touched() {
        let tmp = std::env::temp_dir().join(format!("ci_cfg_mtime_touch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("config.json"), "{}").unwrap();
        let first = crate::config::config_mtime(&tmp);
        assert!(first.is_some());

        // Force a visibly later mtime regardless of filesystem timestamp
        // resolution (some filesystems only track whole seconds).
        let later = first.unwrap() + std::time::Duration::from_secs(2);
        let f = std::fs::File::open(tmp.join("config.json")).unwrap();
        f.set_modified(later).unwrap();

        assert_eq!(crate::config::config_mtime(&tmp), Some(later));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn diff_from_default_is_empty_for_pure_defaults() {
        assert!(diff_from_default(&Config::default()).is_empty());
    }

    #[test]
    fn edit_elicit_defaults_off_and_parses_override() {
        // Elicitation human-veto is opt-in: absent [edit] table = off, and
        // an old config.json without the section must keep parsing.
        let d = Config::default();
        assert!(!d.edit.elicit_hub_confirm);
        assert_eq!(d.edit.elicit_timeout_secs, 120);

        let tmp = std::env::temp_dir().join(format!("ci_cfg_elicit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"edit": {"elicit_hub_confirm": true, "elicit_timeout_secs": 5}}"#,
        )
        .unwrap();
        let loaded = load_config(&tmp).unwrap();
        assert!(loaded.edit.elicit_hub_confirm);
        assert_eq!(loaded.edit.elicit_timeout_secs, 5);
        let diff = diff_from_default(&loaded);
        assert!(
            diff.contains(&"edit.elicit_hub_confirm".to_string()),
            "{diff:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn edit_always_require_edit_context_defaults_off_and_parses_override() {
        // Widened edit gate is opt-in: absent [edit] table = off (today's
        // hub/high-risk-only behavior unchanged), and an old config.json
        // without this field must keep parsing.
        let d = Config::default();
        assert!(!d.edit.always_require_edit_context);

        let tmp =
            std::env::temp_dir().join(format!("ci_cfg_always_edit_ctx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"edit": {"always_require_edit_context": true}}"#,
        )
        .unwrap();
        let loaded = load_config(&tmp).unwrap();
        assert!(loaded.edit.always_require_edit_context);
        let diff = diff_from_default(&loaded);
        assert!(
            diff.contains(&"edit.always_require_edit_context".to_string()),
            "{diff:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn orientation_config_defaults_to_inject_and_parses_override() {
        let d = Config::default();
        assert_eq!(d.orientation.mode, OrientationMode::Inject);
        assert!(d.orientation.remind_pending_diff_impact);

        let tmp = std::env::temp_dir().join(format!("ci_cfg_orientation_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"orientation": {"mode": "block", "remind_pending_diff_impact": false}}"#,
        )
        .unwrap();
        let loaded = load_config(&tmp).unwrap();
        assert_eq!(loaded.orientation.mode, OrientationMode::Block);
        assert!(!loaded.orientation.remind_pending_diff_impact);
        let diff = diff_from_default(&loaded);
        assert!(diff.contains(&"orientation.mode".to_string()), "{diff:?}");
        assert!(
            diff.contains(&"orientation.remind_pending_diff_impact".to_string()),
            "{diff:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn orientation_mode_off_round_trips_through_json() {
        let tmp =
            std::env::temp_dir().join(format!("ci_cfg_orientation_off_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"orientation": {"mode": "off"}}"#,
        )
        .unwrap();
        let loaded = load_config(&tmp).unwrap();
        assert_eq!(loaded.orientation.mode, OrientationMode::Off);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn diff_from_default_reports_only_the_overridden_nested_field() {
        // Regression test for the F10 calibration bug: a config.json that
        // only overrides one nested field (hub_threshold.min_callers_bridge)
        // must be reported precisely -- not the whole hub_threshold object,
        // and nothing from unrelated sections.
        let tmp = std::env::temp_dir().join(format!("ci_cfg_diff_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("config.json"),
            r#"{"hub_threshold": {"min_callers_bridge": 2}}"#,
        )
        .unwrap();

        let loaded = load_config(&tmp).unwrap();
        let diff = diff_from_default(&loaded);
        assert_eq!(diff, vec!["hub_threshold.min_callers_bridge".to_string()]);

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

    #[test]
    fn incremental_graph_defaults_on_and_opt_out_parses() {
        // Plan T6.4 flipped this default false → true after golden-equivalence
        // passed on the real DB. Lock both: the default, and that config.json
        // can still force it off (the rollback switch).
        assert!(
            IndexingConfig::default().incremental_graph,
            "default must be true since T6.4"
        );
        assert!(Config::default().indexing.incremental_graph);
        let off: Config =
            serde_json::from_str(r#"{"indexing":{"incremental_graph":false}}"#).unwrap();
        assert!(
            !off.indexing.incremental_graph,
            "config.json must be able to force it off"
        );
    }
}
