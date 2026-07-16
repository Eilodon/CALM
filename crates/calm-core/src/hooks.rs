//! Generic, project-agnostic CALM hook for external repos — scaffolded by
//! `calm init --hooks[=nudge|enforce|off]`, not this repo's own dev-only
//! `.claude/hooks/calm-nudge.sh` (which stays internal). See the embedded
//! script's own header comment (`assets/hooks/calm-hooks.sh`) for the full
//! best-effort framing and the exact bypass this mechanism cannot close,
//! and `docs/superskills/specs/2026-07-15-calm-hooks-transparent-
//! reactivation.md` (in the CALM project itself) for the design rationale.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result};

/// The hook script content, embedded verbatim from the canonical source
/// file so it stays directly shellcheck'able/testable in this repo rather
/// than living only as a Rust string literal — see `assets/hooks/calm-
/// hooks.sh`. Kept around for `--hooks=off` cleanup of a pre-existing
/// legacy (shell-form) install and for anyone who still wants the bash
/// version, but no longer written by a fresh `calm init --hooks` run —
/// see `HOOKS_WIRE_ARGS` below.
pub const HOOKS_SCRIPT: &str = include_str!("../assets/hooks/calm-hooks.sh");

/// Legacy shell-form invocation string (pre-2026-07-16) — no longer
/// written by a fresh install, kept only to detect and clean up an
/// existing one during migration (`block_is_calm_hook_block`) so
/// `--hooks=enforce`/`--hooks=off` on an old install still finds and
/// removes it by exact identity, same guarantee this constant always gave.
pub const HOOKS_WIRE_COMMAND: &str = "bash .claude/hooks/calm-hooks.sh";

/// Current (2026-07-16+) exec-form invocation: `{"command": "<path to the
/// calm binary>", "args": HOOKS_WIRE_ARGS}` — bypasses the shell entirely,
/// so no `jq`/`sqlite3` CLI/POSIX `flock` dependency (see
/// `docs/superskills/specs/2026-07-16-calm-hooks-native-cli-subcommand.md`).
/// The binary path varies per install (can't be a single constant the way
/// the legacy command string was), so identity for idempotent-write/detect/
/// remove purposes is keyed on `args` matching exactly instead — a
/// binary's absolute path changing (rebuilt, moved) between runs is a
/// legitimate self-heal case, not a "different hook" case.
pub const HOOKS_WIRE_ARGS: &[&str] = &["hooks-check"];

/// `PreToolUse` matcher covering exactly the tool names the hook branches
/// on (both the legacy script and `hooks_check::evaluate` agree on this
/// set) — kept in sync by hand; a mismatch here would only widen or narrow
/// which calls invoke an otherwise-correct decision, not silently break
/// the mode logic itself.
pub const HOOKS_MATCHER: &str = "Edit|Write|Bash|mcp__calm__edit_context|mcp__calm__diff_impact|mcp__calm__edit_lines|mcp__calm__edit_symbol";

/// Relative path to the legacy scaffolded script inside a project (only
/// relevant for an old shell-form install), and the settings.json file
/// the hook block is wired into either way.
pub const HOOKS_SCRIPT_REL_PATH: &str = ".claude/hooks/calm-hooks.sh";
pub const CLAUDE_SETTINGS_REL_PATH: &str = ".claude/settings.json";

/// Schema version for `.calm/hooks.mode`'s own file format — bumped only if
/// the format itself changes shape. `read_hooks_mode_file`'s safe-default
/// behavior on any other value (including a schema this binary doesn't
/// recognize) is the FM1 mitigation this exists for; see that function.
pub const HOOKS_MODE_SCHEMA: &str = "1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HooksMode {
    Nudge,
    Enforce,
    Off,
}

impl HooksMode {
    pub fn as_str(self) -> &'static str {
        match self {
            HooksMode::Nudge => "nudge",
            HooksMode::Enforce => "enforce",
            HooksMode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "nudge" => Some(HooksMode::Nudge),
            "enforce" => Some(HooksMode::Enforce),
            "off" => Some(HooksMode::Off),
            _ => None,
        }
    }
}

impl fmt::Display for HooksMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn hooks_mode_path(calm_dir: &Path) -> std::path::PathBuf {
    calm_dir.join("hooks.mode")
}

/// FM1's safe state machine, Rust side — MUST agree with `calm-hooks.sh`'s
/// own `read_hooks_mode()` shell function (same contract, cross-checked by
/// `hooks_mode_parser_matches_shell_contract` in this module's tests): a
/// missing file means never-installed (`Off`); any parse failure or
/// unrecognized value (wrong/missing schema, garbage content, a mode name
/// a future or older binary doesn't recognize) resolves to `Nudge`, never
/// silently to `Enforce`. This function never panics on malformed input —
/// every branch has an explicit fallback.
pub fn read_hooks_mode_file(calm_dir: &Path) -> HooksMode {
    let path = hooks_mode_path(calm_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return HooksMode::Off,
    };
    let mut schema: Option<&str> = None;
    let mut mode: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("schema=") {
            schema = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("mode=") {
            mode = Some(v.trim());
        }
    }
    if schema != Some(HOOKS_MODE_SCHEMA) {
        return HooksMode::Nudge;
    }
    mode.and_then(HooksMode::parse).unwrap_or(HooksMode::Nudge)
}

/// Atomic write (temp file + rename, matching this codebase's established
/// edit-path convention — see `docs/architecture.md`'s "Atomic writes"
/// note) so a hook script reading this file mid-write never sees a torn
/// result. `written_by` should be `calm_core::BUILD_INFO` or a version
/// string — kept as a plain caller-supplied string here to avoid this
/// module depending on `calm-cli`'s own version plumbing.
pub fn write_hooks_mode_file(calm_dir: &Path, mode: HooksMode, written_by: &str) -> Result<()> {
    std::fs::create_dir_all(calm_dir)
        .with_context(|| format!("creating {}", calm_dir.display()))?;
    let written_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let contents = format!(
        "schema={HOOKS_MODE_SCHEMA}\nmode={mode}\nwritten_by={written_by}\nwritten_at={written_at}\n"
    );
    let path = hooks_mode_path(calm_dir);
    let tmp_path = calm_dir.join(format!("hooks.mode.tmp.{}", std::process::id()));
    std::fs::write(&tmp_path, contents)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Removes `.calm/hooks.mode` if present. No-ops cleanly (`Ok(false)`) if
/// it was never there — `--hooks=off` on an uninstalled project must not
/// error (L7 idempotency requirement from the spec's audit).
pub fn remove_hooks_mode_file(calm_dir: &Path) -> Result<bool> {
    let path = hooks_mode_path(calm_dir);
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(true)
}

/// The exec-form `{matcher, hooks: [...]}` entry this module writes into
/// `.claude/settings.json`'s `hooks.PreToolUse` array for a fresh install
/// — `command`/`args` bypass the shell entirely (Claude Code's exec form:
/// a command with an `args` array spawns the executable directly, no
/// `sh -c`/Git-Bash indirection — see the sibling spec's Windows-execution
/// research). `bin_path` is normally `std::env::current_exe()` at scaffold
/// time (the exact binary that's running `calm init --hooks` right now is
/// guaranteed to exist and work).
fn hooks_settings_block(bin_path: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": HOOKS_MATCHER,
        "hooks": [
            {
                "type": "command",
                "command": bin_path,
                "args": HOOKS_WIRE_ARGS,
                "timeout": 5
            }
        ]
    })
}

fn block_command(block: &serde_json::Value) -> Option<&str> {
    block
        .get("hooks")?
        .as_array()?
        .iter()
        .find_map(|h| h.get("command")?.as_str())
}

fn block_args(block: &serde_json::Value) -> Option<Vec<&str>> {
    block
        .get("hooks")?
        .as_array()?
        .iter()
        .find_map(|h| h.get("args")?.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
}

/// True for EITHER a legacy shell-form block (`command` == the old
/// `HOOKS_WIRE_COMMAND` string) OR a current exec-form block (`args` ==
/// `HOOKS_WIRE_ARGS`) — the one predicate write/remove/detect all share, so
/// a stale legacy install and a stale/previous exec-form install are
/// always recognized and cleanly swapped, never left to coexist (the
/// sibling spec's Failure Mode 3 — dual-entrypoint transition window).
fn block_is_calm_hook_block(block: &serde_json::Value) -> bool {
    block_command(block) == Some(HOOKS_WIRE_COMMAND)
        || block_args(block).as_deref() == Some(HOOKS_WIRE_ARGS)
}

/// Merges CALM's hook block into `.claude/settings.json`'s
/// `hooks.PreToolUse` array — atomic swap, not append: any existing calm
/// hook block (legacy shell-form OR a previous exec-form entry with a
/// stale binary path) is removed first, then the current exec-form block
/// for `bin_path` is added, in the same write, so there is never a window
/// where two calm hooks are simultaneously wired. Never touches any other
/// key at any level — mirrors `write_mcp_config_entry`'s own "leave
/// everything else alone" contract, adapted for an array-of-blocks shape
/// (`PreToolUse`/`PostToolUse` are independent `{matcher, hooks}` blocks
/// per Claude Code's own hooks schema — confirmed via this repo's own
/// `.claude/settings.json` and via official docs that all matching blocks
/// fire in parallel, not first-match-only, so appending a new block is
/// safe regardless of what else is already there). Idempotent: if an
/// existing block already has this exact `bin_path` + `HOOKS_WIRE_ARGS`,
/// no rewrite.
pub fn write_hooks_settings_block(path: &Path, bin_path: &str) -> Result<&'static str> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut root: serde_json::Value = if path.exists() {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("existing file isn't valid JSON ({e}) — leaving it untouched")
        })?
    } else {
        serde_json::json!({})
    };

    let root_obj = root.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!("existing file's top level isn't a JSON object — leaving it untouched")
    })?;
    let hooks_obj = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            anyhow::anyhow!("existing \"hooks\" field isn't a JSON object — leaving it untouched")
        })?;
    let pre = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "existing \"hooks.PreToolUse\" field isn't a JSON array — leaving it untouched"
            )
        })?;

    let already_current = pre.iter().any(|b| {
        block_command(b) == Some(bin_path) && block_args(b).as_deref() == Some(HOOKS_WIRE_ARGS)
    });
    if already_current {
        return Ok("up to date");
    }

    pre.retain(|b| !block_is_calm_hook_block(b));
    pre.push(hooks_settings_block(bin_path));
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    Ok("wrote")
}

/// Inverse of `write_hooks_settings_block` — removes any calm hook block
/// (legacy shell-form or current exec-form, via `block_is_calm_hook_block`),
/// leaving every other `PreToolUse` entry and every other top-level key
/// untouched. No-ops cleanly if the file, the `hooks`/`PreToolUse` keys, or
/// the block itself don't exist.
pub fn remove_hooks_settings_block(path: &Path) -> Result<&'static str> {
    if !path.exists() {
        return Ok("nothing to remove");
    }
    let text = std::fs::read_to_string(path)?;
    let mut root: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!("existing file isn't valid JSON ({e}) — leaving it untouched")
    })?;

    let Some(pre) = root
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
        .and_then(|h| h.get_mut("PreToolUse"))
        .and_then(|p| p.as_array_mut())
    else {
        return Ok("nothing to remove");
    };

    let before = pre.len();
    pre.retain(|b| !block_is_calm_hook_block(b));
    if pre.len() == before {
        return Ok("nothing to remove");
    }

    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    Ok("removed")
}

/// FM2's status cross-check: does the mode file's claim (`configured`)
/// match what's actually wired in `.claude/settings.json`, and does the
/// entrypoint it points at actually exist? Returns a `HooksStatus` with an
/// explicit `active` flag rather than letting a caller assume "mode file
/// present" means "actually enforced" — the specific gap the spec's audit
/// named as worse than no feature if left unaddressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksStatus {
    pub configured_mode: HooksMode,
    pub settings_wired: bool,
    /// For a current exec-form install, whether the wired block's
    /// `command` path exists on disk right now (the binary could have
    /// been moved/removed since scaffolding). For a legacy shell-form
    /// install, whether `.claude/hooks/calm-hooks.sh` exists. `false` if
    /// `settings_wired` is false (nothing to check).
    pub entrypoint_present: bool,
    pub disabled_by_env: bool,
}

impl HooksStatus {
    /// True only when the configured mode is actually live: not `Off`,
    /// settings.json wiring found, the entrypoint it points at present,
    /// and the `CALM_HOOKS_DISABLE` escape hatch isn't set.
    pub fn active(&self) -> bool {
        self.configured_mode != HooksMode::Off
            && self.settings_wired
            && self.entrypoint_present
            && !self.disabled_by_env
    }

    pub fn summary(&self) -> String {
        if self.configured_mode == HooksMode::Off {
            return "hooks: not installed".to_string();
        }
        if self.disabled_by_env {
            return format!(
                "hooks: {} mode configured, but CALM_HOOKS_DISABLE is set in this environment — inert regardless",
                self.configured_mode
            );
        }
        if !self.settings_wired {
            return format!(
                "hooks: {} mode CONFIGURED BUT NOT ACTIVE — {} wiring missing. Run `calm init --hooks={}` to reinstall.",
                self.configured_mode, CLAUDE_SETTINGS_REL_PATH, self.configured_mode
            );
        }
        if !self.entrypoint_present {
            return format!(
                "hooks: wiring present but its entrypoint (binary, or {} for a legacy install) is missing. Run `calm init --hooks={}` to reinstall.",
                HOOKS_SCRIPT_REL_PATH, self.configured_mode
            );
        }
        format!("hooks: {} mode, active", self.configured_mode)
    }
}

/// `project_root` — not `calm_dir` — since this also needs to look at
/// `.claude/settings.json`, which lives at the project root alongside
/// `.calm/`.
pub fn check_hooks_status(project_root: &Path) -> HooksStatus {
    let calm_dir = project_root.join(".calm");
    let configured_mode = read_hooks_mode_file(&calm_dir);
    let settings_path = project_root.join(CLAUDE_SETTINGS_REL_PATH);
    let wired_block: Option<serde_json::Value> = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("hooks")?
                .get("PreToolUse")?
                .as_array()?
                .iter()
                .find(|b| block_is_calm_hook_block(b))
                .cloned()
        });
    let settings_wired = wired_block.is_some();
    let entrypoint_present = match &wired_block {
        Some(b) if block_args(b).as_deref() == Some(HOOKS_WIRE_ARGS) => block_command(b)
            .map(|cmd| Path::new(cmd).is_file())
            .unwrap_or(false),
        Some(_) => project_root.join(HOOKS_SCRIPT_REL_PATH).is_file(),
        None => false,
    };
    let disabled_by_env = std::env::var("CALM_HOOKS_DISABLE").as_deref() == Ok("1");
    HooksStatus {
        configured_mode,
        settings_wired,
        entrypoint_present,
        disabled_by_env,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // --- FM1: read_hooks_mode_file safe-default matrix ---

    #[test]
    fn missing_file_is_off() {
        let dir = tmp_dir();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Off);
    }

    #[test]
    fn empty_file_is_nudge() {
        let dir = tmp_dir();
        std::fs::write(dir.path().join("hooks.mode"), "").unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Nudge);
    }

    #[test]
    fn garbage_content_is_nudge() {
        let dir = tmp_dir();
        std::fs::write(dir.path().join("hooks.mode"), "not a real file\n\x00\x01").unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Nudge);
    }

    #[test]
    fn missing_schema_is_nudge() {
        let dir = tmp_dir();
        std::fs::write(dir.path().join("hooks.mode"), "mode=enforce\n").unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Nudge);
    }

    #[test]
    fn wrong_schema_never_escalates_to_enforce() {
        let dir = tmp_dir();
        std::fs::write(dir.path().join("hooks.mode"), "schema=999\nmode=enforce\n").unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Nudge);
    }

    #[test]
    fn unrecognized_future_mode_name_is_nudge() {
        let dir = tmp_dir();
        std::fs::write(dir.path().join("hooks.mode"), "schema=1\nmode=quarantine\n").unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Nudge);
    }

    #[test]
    fn each_valid_mode_round_trips() {
        let dir = tmp_dir();
        for m in [HooksMode::Nudge, HooksMode::Enforce, HooksMode::Off] {
            write_hooks_mode_file(dir.path(), m, "test").unwrap();
            assert_eq!(read_hooks_mode_file(dir.path()), m);
        }
    }

    #[test]
    fn trailing_whitespace_and_crlf_still_parse() {
        let dir = tmp_dir();
        std::fs::write(
            dir.path().join("hooks.mode"),
            "schema=1 \r\nmode=enforce \r\n",
        )
        .unwrap();
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Enforce);
    }

    #[test]
    fn removing_absent_mode_file_is_a_clean_noop() {
        let dir = tmp_dir();
        assert!(!remove_hooks_mode_file(dir.path()).unwrap());
    }

    #[test]
    fn removing_present_mode_file_reports_true_and_deletes() {
        let dir = tmp_dir();
        write_hooks_mode_file(dir.path(), HooksMode::Enforce, "test").unwrap();
        assert!(remove_hooks_mode_file(dir.path()).unwrap());
        assert_eq!(read_hooks_mode_file(dir.path()), HooksMode::Off);
    }

    // --- Cross-check against the shell script's own contract, so the two
    // parsers (bash in calm-hooks.sh, Rust here) can never silently drift
    // apart on what counts as valid. ---
    #[test]
    fn hooks_mode_parser_matches_shell_contract() {
        assert!(HOOKS_SCRIPT.contains(&format!("HOOKS_MODE_SCHEMA=\"{HOOKS_MODE_SCHEMA}\"")));
        for m in ["nudge", "enforce", "off"] {
            assert!(
                HOOKS_SCRIPT.contains(m),
                "shell script no longer mentions mode value {m:?} — Rust/shell contract drifted"
            );
        }
    }

    // --- settings.json merge: idempotency, isolation from unrelated keys ---

    #[test]
    fn settings_block_writes_new_file() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        assert_eq!(
            write_hooks_settings_block(&path, "/usr/bin/calm").unwrap(),
            "wrote"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hooks-check"));
        assert!(text.contains("/usr/bin/calm"));
    }

    #[test]
    fn settings_block_rerun_is_idempotent() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path, "/usr/bin/calm").unwrap();
        assert_eq!(
            write_hooks_settings_block(&path, "/usr/bin/calm").unwrap(),
            "up to date"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("hooks-check").count(), 1);
    }

    #[test]
    fn settings_block_rerun_with_a_moved_binary_self_heals_the_path() {
        // A rebuilt/moved binary between two `calm init --hooks` runs is a
        // legitimate self-heal case (ADR-0005's daemon.meta stale-build
        // pattern, same shape) — not treated as "already up to date".
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path, "/old/path/calm").unwrap();
        assert_eq!(
            write_hooks_settings_block(&path, "/new/path/calm").unwrap(),
            "wrote"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("/old/path/calm"));
        assert!(text.contains("/new/path/calm"));
        assert_eq!(text.matches("hooks-check").count(), 1);
    }

    #[test]
    fn settings_block_preserves_unrelated_keys_and_blocks() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "permissions": {"allow": ["Bash(ls:*)"]},
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "SomeOtherTool", "hooks": [{"type": "command", "command": "echo hi"}]}
                    ],
                    "PostToolUse": [
                        {"matcher": "*", "hooks": [{"type": "command", "command": "echo bye"}]}
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        write_hooks_settings_block(&path, "/usr/bin/calm").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(
            v["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "echo bye"
        );
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert!(
            v["hooks"]["PreToolUse"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b["matcher"] == "SomeOtherTool")
        );
    }

    #[test]
    fn settings_block_invalid_json_is_left_untouched() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not valid json").unwrap();
        assert!(write_hooks_settings_block(&path, "/usr/bin/calm").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not valid json");
    }

    #[test]
    fn removing_block_from_absent_file_is_a_clean_noop() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        assert_eq!(
            remove_hooks_settings_block(&path).unwrap(),
            "nothing to remove"
        );
    }

    #[test]
    fn remove_settings_block_leaves_other_blocks_alone() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path, "/usr/bin/calm").unwrap();
        // Add an unrelated block by hand, same array.
        let text = std::fs::read_to_string(&path).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["hooks"]["PreToolUse"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"matcher": "Other", "hooks": [{"type": "command", "command": "echo hi"}]}));
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();

        assert_eq!(remove_hooks_settings_block(&path).unwrap(), "removed");
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"], "Other");
    }

    #[test]
    fn remove_settings_block_twice_is_idempotent() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path, "/usr/bin/calm").unwrap();
        assert_eq!(remove_hooks_settings_block(&path).unwrap(), "removed");
        assert_eq!(
            remove_hooks_settings_block(&path).unwrap(),
            "nothing to remove"
        );
    }

    #[test]
    fn write_settings_block_atomically_swaps_away_a_legacy_shell_form_block() {
        // Spec FM3 (docs/superskills/specs/2026-07-16-calm-hooks-native-
        // cli-subcommand.md): a project that scaffolded the OLD bash-
        // script hook before this migration must never end up with BOTH
        // the legacy shell-form block and the new exec-form block wired
        // at once — Claude Code fires every matching PreToolUse block in
        // parallel, so two simultaneously-wired calm hooks is a real
        // decision-conflict risk, not just clutter.
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "hooks": {
                    "PreToolUse": [hooks_settings_block_legacy_for_test()]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        write_hooks_settings_block(&path, "/usr/bin/calm").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(HOOKS_WIRE_COMMAND),
            "legacy block must be gone, got: {text}"
        );
        assert_eq!(text.matches("hooks-check").count(), 1);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["hooks"]["PreToolUse"].as_array().unwrap().len(),
            1,
            "exactly one calm hook block after the swap, not two"
        );
    }

    // --- FM2: status cross-check ---

    #[test]
    fn status_reports_not_installed_when_nothing_present() {
        let dir = tmp_dir();
        let status = check_hooks_status(dir.path());
        assert_eq!(status.configured_mode, HooksMode::Off);
        assert!(!status.active());
        assert_eq!(status.summary(), "hooks: not installed");
    }

    #[test]
    fn status_flags_configured_but_not_active_when_settings_wiring_missing() {
        let dir = tmp_dir();
        let calm_dir = dir.path().join(".calm");
        write_hooks_mode_file(&calm_dir, HooksMode::Enforce, "test").unwrap();
        // No settings.json, no entrypoint written.
        let status = check_hooks_status(dir.path());
        assert_eq!(status.configured_mode, HooksMode::Enforce);
        assert!(!status.settings_wired);
        assert!(!status.active());
        assert!(status.summary().contains("CONFIGURED BUT NOT ACTIVE"));
    }

    #[test]
    fn status_active_when_mode_wiring_and_binary_all_present() {
        let dir = tmp_dir();
        let calm_dir = dir.path().join(".calm");
        write_hooks_mode_file(&calm_dir, HooksMode::Enforce, "test").unwrap();
        // The exec-form entrypoint check looks at whether the wired
        // `command` path actually exists on disk — any real file works
        // for the test, it doesn't need to be a real executable.
        let bin_path = dir.path().join("fake-calm-binary");
        std::fs::write(&bin_path, b"not a real binary, just needs to exist").unwrap();
        write_hooks_settings_block(
            &dir.path().join(CLAUDE_SETTINGS_REL_PATH),
            bin_path.to_str().unwrap(),
        )
        .unwrap();

        let status = check_hooks_status(dir.path());
        assert!(status.active(), "status: {status:?}");
        assert_eq!(status.summary(), "hooks: enforce mode, active");
    }

    #[test]
    fn status_not_active_when_wired_binary_path_no_longer_exists() {
        let dir = tmp_dir();
        let calm_dir = dir.path().join(".calm");
        write_hooks_mode_file(&calm_dir, HooksMode::Enforce, "test").unwrap();
        write_hooks_settings_block(
            &dir.path().join(CLAUDE_SETTINGS_REL_PATH),
            "/no/such/path/calm",
        )
        .unwrap();

        let status = check_hooks_status(dir.path());
        assert!(!status.entrypoint_present);
        assert!(!status.active());
    }

    /// Test-only helper building a legacy shell-form block, kept separate
    /// from `hooks_settings_block` (which only ever builds the current
    /// exec-form shape) so the migration test above can seed a realistic
    /// pre-2026-07-16 install without resurrecting the old code path.
    fn hooks_settings_block_legacy_for_test() -> serde_json::Value {
        serde_json::json!({
            "matcher": HOOKS_MATCHER,
            "hooks": [
                { "type": "command", "command": HOOKS_WIRE_COMMAND, "timeout": 5 }
            ]
        })
    }
}
