//! Native decision logic for `calm hooks-check` — a Rust port, by direct
//! subtraction, of `assets/hooks/calm-hooks.sh`'s two hard-gated branches
//! (Stage 5: `edit_context` before native `Edit`/`Write`; Stage 7:
//! `diff_impact` before `git commit`/`push`) plus its `is_prose_file`
//! advisory-only exception. Exists because the bash script's dependency
//! chain (`jq`, `sqlite3` CLI, POSIX `flock`) isn't a safe assumption on
//! native Windows — see
//! `docs/superskills/specs/2026-07-16-calm-hooks-native-cli-subcommand.md`.
//!
//! Deliberately narrower than the bash script: no decision-log JSONL, no
//! native-vs-CALM exploration tally, no tamper-evident downgrade notice.
//! Those are real, valuable features of the *internal* `calm-nudge.sh` this
//! was never meant to fully replace — see that spec's own "Scope boundary"
//! section. This module covers exactly the 2 hard gates plus the mode
//! read, which is what `calm-hooks.sh` itself also limits itself to.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hooks::{HooksMode, read_hooks_mode_file};

/// Raw JSON payload Claude Code feeds a hook on stdin. Every field is
/// optional/defaulted — a payload missing fields this dispatch doesn't
/// need must never be treated as malformed (mirrors `jq -r '... // ""'`'s
/// forgiving default in the bash version).
#[derive(Debug, Deserialize)]
pub struct HookPayload {
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, rename = "tool_input")]
    pub tool_input: ToolInput,
}

#[derive(Debug, Default, Deserialize)]
pub struct ToolInput {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub symbol: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Nudge(String),
    Deny(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionState {
    #[serde(default)]
    edit_context_files: Vec<String>,
    #[serde(default)]
    needs_diff_impact: bool,
}

/// `.md`/`.MD`/`.txt`/`.TXT` — a doc heading is provably never `is_hub`
/// (no call-graph edge exists for it), so Stage 5 stays advisory-only for
/// these regardless of mode. Ported verbatim from `calm-hooks.sh`'s own
/// `is_prose_file`.
fn is_prose_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".txt")
}

fn to_repo_relative(path: &str, project_root: &Path) -> String {
    let p = Path::new(path);
    if p.is_absolute()
        && let Ok(rel) = p.strip_prefix(project_root)
    {
        return rel.to_string_lossy().into_owned();
    }
    path.to_string()
}

fn state_dir(calm_dir: &Path) -> PathBuf {
    calm_dir.join(".hooks-state")
}

fn state_file_path(calm_dir: &Path, session_id: &str) -> PathBuf {
    // session_id arrives from Claude Code's own payload, not attacker-
    // controlled in the typical single-user-CLI threat model this hook
    // runs under — but still sanitized to a safe filename shape (matches
    // the bash version's implicit trust in `$session_id` being a real
    // UUID, made explicit here rather than assumed).
    let safe: String = session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let safe = if safe.is_empty() { "unknown".to_string() } else { safe };
    state_dir(calm_dir).join(format!("{safe}.json"))
}

/// Cross-platform mutual exclusion for the session-state read-modify-write,
/// no crate dependency: `std::fs::create_dir` is atomic create-or-fail on
/// every platform this binary ships for (POSIX mkdir(2) and Windows
/// CreateDirectory are both atomic) — the same "no working `flock` on
/// Windows" gap `calm-hooks.sh` has (see the sibling spec's Problem
/// section) never applies here, since this primitive was never
/// POSIX-only to begin with. Short bounded retry, then proceeds unlocked
/// on contention — same fail-open philosophy as the bash version's own
/// `acquire_state_lock` ("an occasional dropped increment is the
/// acceptable failure mode, a hung hook invocation is not").
struct StateLock {
    dir: Option<PathBuf>,
}

impl StateLock {
    fn acquire(state_file: &Path) -> Self {
        let lock_dir = state_file.with_extension("json.lockdir");
        for _ in 0..20 {
            match std::fs::create_dir(&lock_dir) {
                Ok(()) => return StateLock { dir: Some(lock_dir) },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => return StateLock { dir: None },
            }
        }
        StateLock { dir: None }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn load_state(calm_dir: &Path, session_id: &str) -> SessionState {
    let path = state_file_path(calm_dir, session_id);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(calm_dir: &Path, session_id: &str, state: &SessionState) {
    let dir = state_dir(calm_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = state_file_path(calm_dir, session_id);
    let _lock = StateLock::acquire(&path);
    let Ok(contents) = serde_json::to_string(state) else {
        return;
    };
    // Atomic write (temp + rename), matching this codebase's established
    // edit-path convention — a reader never sees a torn file.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, contents).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Resolves a bare `symbol` argument to a repo-relative path via
/// `.calm/index.db`'s `symbols` table, direct `rusqlite` — mirrors
/// `calm-hooks.sh`'s own `sqlite3 -readonly` query (line ~217-226) but
/// with no external CLI dependency. Read-only connection, `query_only`
/// PRAGMA set immediately (same guarantee `CalmServer::make_read_conn`
/// gives every MCP tool handler — see that function's own doc comment),
/// plus a `busy_timeout` since a one-shot CLI process opening its own
/// connection alongside a live `calm serve` daemon's connections is exactly
/// SQLite WAL mode's supported concurrent-reader case, not a special one —
/// the timeout only matters for the rare window a schema-init writer holds
/// a brief exclusive lock. Returns `None` on ANY failure (missing DB,
/// locked DB, no unique match) — this lookup is advisory (narrows which
/// file a bare `symbol` argument means), never a hard requirement, so it
/// fails open exactly like the bash version's own `command -v sqlite3`
/// soft-guard.
fn resolve_symbol_path(calm_dir: &Path, symbol: &str) -> Option<String> {
    let db_path = calm_dir.join("index.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(500)).ok()?;
    conn.execute_batch("PRAGMA query_only = ON;").ok()?;
    let mut stmt = conn
        .prepare("SELECT path FROM symbols WHERE name = ?1")
        .ok()?;
    let rows: Vec<String> = stmt
        .query_map([symbol], |r| r.get::<_, String>(0))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    match rows.len() {
        1 => Some(rows.into_iter().next().unwrap()),
        _ => None,
    }
}

const EDIT_CONTEXT_POINTER: &str =
    "Call the `calm_workflow` MCP prompt (no arguments) for the full CALM tool workflow.";

/// Main dispatch — one `HookPayload` in, one `Decision` out. Pure function
/// apart from the session-state file I/O (`.calm/.hooks-state/<session>
/// .json`) and the optional `.calm/index.db` read for symbol resolution;
/// no network, no process spawn, no shell. `project_root` is the directory
/// this hook is scoped to (equivalent to `calm-hooks.sh`'s implicit CWD
/// assumption, made an explicit parameter here for testability).
pub fn evaluate(payload: &HookPayload, project_root: &Path) -> Decision {
    let calm_dir = project_root.join(".calm");
    let mode = read_hooks_mode_file(&calm_dir);
    if mode == HooksMode::Off {
        return Decision::Allow;
    }

    let mut state = load_state(&calm_dir, &payload.session_id);

    match payload.tool_name.as_str() {
        "mcp__calm__edit_context" => {
            let mut path = if !payload.tool_input.path.is_empty() {
                Some(to_repo_relative(&payload.tool_input.path, project_root))
            } else {
                None
            };
            if path.is_none() && !payload.tool_input.symbol.is_empty() {
                path = resolve_symbol_path(&calm_dir, &payload.tool_input.symbol)
                    .map(|p| to_repo_relative(&p, project_root));
            }
            if let Some(p) = path
                && !state.edit_context_files.contains(&p)
            {
                state.edit_context_files.push(p);
            }
            save_state(&calm_dir, &payload.session_id, &state);
            Decision::Allow
        }
        "mcp__calm__diff_impact" => {
            state.needs_diff_impact = false;
            save_state(&calm_dir, &payload.session_id, &state);
            Decision::Allow
        }
        "mcp__calm__edit_lines" | "mcp__calm__edit_symbol" => {
            state.needs_diff_impact = true;
            save_state(&calm_dir, &payload.session_id, &state);
            Decision::Allow
        }
        "Edit" | "Write" => {
            let file_path = &payload.tool_input.file_path;
            if file_path.is_empty() {
                return Decision::Allow;
            }
            state.needs_diff_impact = true;
            save_state(&calm_dir, &payload.session_id, &state);

            if is_prose_file(file_path) {
                let rel = to_repo_relative(file_path, project_root);
                if !state.edit_context_files.contains(&rel) {
                    return Decision::Nudge(format!(
                        "RECOMMENDED — call mcp__calm__edit_context before editing {file_path}. \
                         Not required for prose (.md/.txt never carries a call-graph edge). \
                         {EDIT_CONTEXT_POINTER}"
                    ));
                }
                return Decision::Allow;
            }

            let rel = to_repo_relative(file_path, project_root);
            if state.edit_context_files.contains(&rel) {
                return Decision::Allow;
            }
            let msg = format!(
                "MANDATORY — call mcp__calm__edit_context before editing {file_path} \
                 this session. {EDIT_CONTEXT_POINTER}"
            );
            if mode == HooksMode::Enforce {
                Decision::Deny(msg)
            } else {
                Decision::Nudge(msg)
            }
        }
        "Bash" => {
            let cmd = &payload.tool_input.command;
            let is_commit_push = cmd.contains("git commit") || cmd.contains("git push");
            if !is_commit_push {
                return Decision::Allow;
            }
            if !state.needs_diff_impact {
                return Decision::Allow;
            }
            let msg = "MANDATORY — call mcp__calm__diff_impact before commit/push after \
                        any write this session."
                .to_string();
            if mode == HooksMode::Enforce {
                Decision::Deny(msg)
            } else {
                Decision::Nudge(msg)
            }
        }
        _ => Decision::Allow,
    }
}

/// Entrypoint for the `calm hooks-check` CLI subcommand: reads the full
/// hook payload from `stdin`, decides, and reports the result the way
/// Claude Code's hook protocol expects (exit 2 + stderr for a hard deny —
/// see `calm-nudge.sh`'s own 2026-07-14 migration off the JSON
/// `permissionDecision` form for why exit-code is preferred; stderr-only +
/// exit 0 for a nudge; silent exit 0 for allow).
///
/// Malformed/unparseable stdin (Abductive Hypothesis 2 in the spec this
/// implements) degrades to `Decision::Allow` — never a panic, never a
/// nonzero exit for a payload shape this dispatch doesn't understand. This
/// is the Rust equivalent of `jq`'s forgiving-empty-string default the
/// bash version relies on throughout; explicit here rather than implicit.
pub fn run<R: std::io::Read, W: std::io::Write>(
    mut stdin: R,
    mut stderr: W,
    project_root: &Path,
) -> i32 {
    let mut buf = String::new();
    if std::io::Read::read_to_string(&mut stdin, &mut buf).is_err() {
        return 0;
    }
    let payload: HookPayload = match serde_json::from_str(&buf) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    match evaluate(&payload, project_root) {
        Decision::Allow => 0,
        Decision::Nudge(msg) => {
            let _ = writeln!(stderr, "[CALM hooks: nudge — advisory only, never blocks] {msg}");
            0
        }
        Decision::Deny(msg) => {
            let _ = writeln!(
                stderr,
                "[CALM hooks: enforce — best-effort, not a security boundary] {msg}"
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_project() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn set_mode(root: &Path, mode: HooksMode) {
        crate::hooks::write_hooks_mode_file(&root.join(".calm"), mode, "test").unwrap();
    }

    fn payload(tool_name: &str, input: ToolInput, session_id: &str) -> HookPayload {
        HookPayload {
            tool_name: tool_name.into(),
            session_id: session_id.into(),
            tool_input: input,
        }
    }

    #[test]
    fn off_mode_always_allows() {
        let dir = tmp_project();
        // No hooks.mode file at all → Off per read_hooks_mode_file's own
        // contract — never explicitly written here on purpose.
        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "src/main.rs".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert_eq!(out, Decision::Allow);
    }

    #[test]
    fn enforce_mode_denies_native_edit_without_prior_edit_context() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "src/main.rs".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert!(matches!(out, Decision::Deny(_)), "got {out:?}");
    }

    #[test]
    fn nudge_mode_only_nudges_never_denies_same_case() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Nudge);
        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "src/main.rs".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert!(matches!(out, Decision::Nudge(_)), "got {out:?}");
    }

    #[test]
    fn edit_context_then_native_edit_same_session_allows() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let ec = evaluate(
            &payload(
                "mcp__calm__edit_context",
                ToolInput {
                    path: "src/main.rs".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert_eq!(ec, Decision::Allow);

        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "src/main.rs".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert_eq!(out, Decision::Allow);
    }

    #[test]
    fn edit_context_in_one_session_does_not_unlock_another_session() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        evaluate(
            &payload(
                "mcp__calm__edit_context",
                ToolInput {
                    path: "src/main.rs".into(),
                    ..Default::default()
                },
                "session-A",
            ),
            dir.path(),
        );
        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "src/main.rs".into(),
                    ..Default::default()
                },
                "session-B",
            ),
            dir.path(),
        );
        assert!(matches!(out, Decision::Deny(_)), "got {out:?}");
    }

    #[test]
    fn prose_file_never_denies_even_in_enforce_mode() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let out = evaluate(
            &payload(
                "Edit",
                ToolInput {
                    file_path: "README.md".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert!(matches!(out, Decision::Nudge(_)), "got {out:?}");
    }

    #[test]
    fn commit_after_edit_lines_without_diff_impact_is_denied_in_enforce() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        evaluate(
            &payload("mcp__calm__edit_lines", ToolInput::default(), "s1"),
            dir.path(),
        );
        let out = evaluate(
            &payload(
                "Bash",
                ToolInput {
                    command: "git commit -m x".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert!(matches!(out, Decision::Deny(_)), "got {out:?}");
    }

    #[test]
    fn diff_impact_clears_the_commit_gate() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        evaluate(
            &payload("mcp__calm__edit_lines", ToolInput::default(), "s1"),
            dir.path(),
        );
        evaluate(
            &payload("mcp__calm__diff_impact", ToolInput::default(), "s1"),
            dir.path(),
        );
        let out = evaluate(
            &payload(
                "Bash",
                ToolInput {
                    command: "git commit -m x".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert_eq!(out, Decision::Allow);
    }

    #[test]
    fn non_commit_bash_is_always_allowed() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        evaluate(
            &payload("mcp__calm__edit_lines", ToolInput::default(), "s1"),
            dir.path(),
        );
        let out = evaluate(
            &payload(
                "Bash",
                ToolInput {
                    command: "cargo build".into(),
                    ..Default::default()
                },
                "s1",
            ),
            dir.path(),
        );
        assert_eq!(out, Decision::Allow);
    }

    #[test]
    fn malformed_stdin_fails_open_exit_zero_never_panics() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let mut stderr = Vec::new();
        let code = run(
            "{ this is not valid json at all".as_bytes(),
            &mut stderr,
            dir.path(),
        );
        assert_eq!(code, 0);
    }

    #[test]
    fn empty_stdin_fails_open_exit_zero() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let mut stderr = Vec::new();
        let code = run(&[][..], &mut stderr, dir.path());
        assert_eq!(code, 0);
    }

    #[test]
    fn run_deny_path_returns_exit_2_and_writes_stderr() {
        let dir = tmp_project();
        set_mode(dir.path(), HooksMode::Enforce);
        let json = r#"{"tool_name":"Edit","session_id":"s1","tool_input":{"file_path":"src/main.rs"}}"#;
        let mut stderr = Vec::new();
        let code = run(json.as_bytes(), &mut stderr, dir.path());
        assert_eq!(code, 2);
        assert!(!stderr.is_empty());
    }

    #[test]
    fn run_allow_path_returns_exit_0_and_writes_nothing() {
        let dir = tmp_project();
        // Off mode (no hooks.mode file written).
        let json = r#"{"tool_name":"Edit","session_id":"s1","tool_input":{"file_path":"src/main.rs"}}"#;
        let mut stderr = Vec::new();
        let code = run(json.as_bytes(), &mut stderr, dir.path());
        assert_eq!(code, 0);
        assert!(stderr.is_empty());
    }
}
