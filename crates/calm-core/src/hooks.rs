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
/// than living only as a Rust string literal — see
/// `assets/hooks/calm-hooks.sh`.
pub const HOOKS_SCRIPT: &str = include_str!("../assets/hooks/calm-hooks.sh");

/// Exact invocation string used both to write the settings.json block and
/// to detect/remove it later — one constant, so the installer and the
/// detector/remover can never independently drift out of sync (the exact
/// failure mode a hand-duplicated string in two places risks).
pub const HOOKS_WIRE_COMMAND: &str = "bash .claude/hooks/calm-hooks.sh";

/// `PreToolUse` matcher covering exactly the tool names `calm-hooks.sh`
/// branches on — kept in sync with the script by hand (both are small and
/// change together); a mismatch here would only widen or narrow which
/// calls invoke an otherwise-correct script, not silently break the mode
/// logic itself.
pub const HOOKS_MATCHER: &str = "Edit|Write|Bash|mcp__calm__edit_context|mcp__calm__diff_impact|mcp__calm__edit_lines|mcp__calm__edit_symbol";

/// Relative path to the scaffolded script inside a project, and the
/// settings.json file it's wired into.
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
    let tmp_path = calm_dir.join(format!(
        "hooks.mode.tmp.{}",
        std::process::id()
    ));
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

/// The one-block `{matcher, hooks: [...]}` entry this module ever writes
/// into `.claude/settings.json`'s `hooks.PreToolUse` array — same shape
/// `write_mcp_config_entry`'s neighbors already use elsewhere in
/// `calm-cli`, just for a different top-level key.
fn hooks_settings_block() -> serde_json::Value {
    serde_json::json!({
        "matcher": HOOKS_MATCHER,
        "hooks": [
            { "type": "command", "command": HOOKS_WIRE_COMMAND, "timeout": 5 }
        ]
    })
}

fn block_command(block: &serde_json::Value) -> Option<&str> {
    block.get("hooks")?.as_array()?.iter().find_map(|h| h.get("command")?.as_str())
}

/// Merges CALM's hook block into `.claude/settings.json`'s
/// `hooks.PreToolUse` array. Never touches any other key at any level —
/// mirrors `write_mcp_config_entry`'s own "leave everything else alone"
/// contract, adapted for an array-of-blocks shape instead of a
/// single-object-keyed-by-name shape (`PreToolUse`/`PostToolUse` are
/// independent `{matcher, hooks}` blocks per Claude Code's own hooks
/// schema — confirmed via this repo's own `.claude/settings.json` and via
/// official docs that all matching blocks fire in parallel, not
/// first-match-only, so appending a new block is safe regardless of what
/// else is already there). Idempotent: a block whose `command` already
/// equals `HOOKS_WIRE_COMMAND` means "already wired," no rewrite.
pub fn write_hooks_settings_block(path: &Path) -> Result<&'static str> {
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

    let root_obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("existing file's top level isn't a JSON object — leaving it untouched"))?;
    let hooks_obj = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("existing \"hooks\" field isn't a JSON object — leaving it untouched"))?;
    let pre = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("existing \"hooks.PreToolUse\" field isn't a JSON array — leaving it untouched"))?;

    let already_wired = pre.iter().any(|b| block_command(b) == Some(HOOKS_WIRE_COMMAND));
    if already_wired {
        return Ok("up to date");
    }

    pre.push(hooks_settings_block());
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    Ok("wrote")
}

/// Inverse of `write_hooks_settings_block` — removes only the one block
/// this module itself would have added (identified by the exact same
/// `HOOKS_WIRE_COMMAND` constant), leaving every other `PreToolUse` entry
/// and every other top-level key untouched. No-ops cleanly if the file, the
/// `hooks`/`PreToolUse` keys, or the block itself don't exist.
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
    pre.retain(|b| block_command(b) != Some(HOOKS_WIRE_COMMAND));
    if pre.len() == before {
        return Ok("nothing to remove");
    }

    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    Ok("removed")
}

/// FM2's status cross-check: does the mode file's claim (`configured`)
/// match what's actually wired in `.claude/settings.json`, and does the
/// script file itself exist? Returns a `HooksStatus` with an explicit
/// `active` flag rather than letting a caller assume "mode file present"
/// means "actually enforced" — the specific gap the spec's audit named as
/// worse than no feature if left unaddressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HooksStatus {
    pub configured_mode: HooksMode,
    pub settings_wired: bool,
    pub script_present: bool,
    pub disabled_by_env: bool,
}

impl HooksStatus {
    /// True only when the configured mode is actually live: not `Off`,
    /// settings.json wiring found, script file present, and the
    /// `CALM_HOOKS_DISABLE` escape hatch isn't set in the environment.
    pub fn active(&self) -> bool {
        self.configured_mode != HooksMode::Off
            && self.settings_wired
            && self.script_present
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
        if !self.script_present {
            return format!(
                "hooks: wiring present but {} is missing. Run `calm init --hooks={}` to reinstall.",
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
    let settings_wired = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("hooks")?
                .get("PreToolUse")?
                .as_array()
                .map(|arr| arr.iter().any(|b| block_command(b) == Some(HOOKS_WIRE_COMMAND)))
        })
        .unwrap_or(false);
    let script_present = project_root.join(HOOKS_SCRIPT_REL_PATH).is_file();
    let disabled_by_env = std::env::var("CALM_HOOKS_DISABLE").as_deref() == Ok("1");
    HooksStatus {
        configured_mode,
        settings_wired,
        script_present,
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
        std::fs::write(dir.path().join("hooks.mode"), "schema=1 \r\nmode=enforce \r\n").unwrap();
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
        assert_eq!(write_hooks_settings_block(&path).unwrap(), "wrote");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(HOOKS_WIRE_COMMAND));
    }

    #[test]
    fn settings_block_rerun_is_idempotent() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path).unwrap();
        assert_eq!(write_hooks_settings_block(&path).unwrap(), "up to date");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(HOOKS_WIRE_COMMAND).count(), 1);
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

        write_hooks_settings_block(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(v["hooks"]["PostToolUse"][0]["hooks"][0]["command"], "echo bye");
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert!(v["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["matcher"] == "SomeOtherTool"));
    }

    #[test]
    fn settings_block_invalid_json_is_left_untouched() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not valid json").unwrap();
        assert!(write_hooks_settings_block(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not valid json");
    }

    #[test]
    fn removing_block_from_absent_file_is_a_clean_noop() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        assert_eq!(remove_hooks_settings_block(&path).unwrap(), "nothing to remove");
    }

    #[test]
    fn remove_settings_block_leaves_other_blocks_alone() {
        let dir = tmp_dir();
        let path = dir.path().join(".claude/settings.json");
        write_hooks_settings_block(&path).unwrap();
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
        write_hooks_settings_block(&path).unwrap();
        assert_eq!(remove_hooks_settings_block(&path).unwrap(), "removed");
        assert_eq!(remove_hooks_settings_block(&path).unwrap(), "nothing to remove");
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
        // No settings.json, no script file written.
        let status = check_hooks_status(dir.path());
        assert_eq!(status.configured_mode, HooksMode::Enforce);
        assert!(!status.settings_wired);
        assert!(!status.active());
        assert!(status.summary().contains("CONFIGURED BUT NOT ACTIVE"));
    }

    #[test]
    fn status_active_when_mode_wiring_and_script_all_present() {
        let dir = tmp_dir();
        let calm_dir = dir.path().join(".calm");
        write_hooks_mode_file(&calm_dir, HooksMode::Enforce, "test").unwrap();
        write_hooks_settings_block(&dir.path().join(CLAUDE_SETTINGS_REL_PATH)).unwrap();
        let script_path = dir.path().join(HOOKS_SCRIPT_REL_PATH);
        std::fs::create_dir_all(script_path.parent().unwrap()).unwrap();
        std::fs::write(&script_path, HOOKS_SCRIPT).unwrap();

        let status = check_hooks_status(dir.path());
        assert!(status.active());
        assert_eq!(status.summary(), "hooks: enforce mode, active");
    }
}
