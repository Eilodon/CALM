use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskOrder {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

impl RiskOrder {
    pub fn parse(s: &str) -> Self {
        match s {
            "medium" => Self::Medium,
            "high" => Self::High,
            "critical" => Self::Critical,
            _ => Self::Low,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Run `cmd` with `args` in `dir`, aborting the wait after `timeout_secs`.
/// `Command::output()` blocks indefinitely with no built-in timeout, so the
/// actual process is spawned on a background thread and the caller waits on
/// a channel with `recv_timeout`. On timeout the spawned thread (and the
/// child process it's blocked on) is simply abandoned — it finishes in the
/// background and its result is dropped when the channel's receiver is gone,
/// since a single `git diff` isn't worth the extra complexity of
/// process-group kill plumbing.
fn run_with_timeout(
    cmd: &str,
    args: Vec<String>,
    dir: &Path,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let cmd_name = cmd.to_string();
    let spawn_cmd = cmd_name.clone();
    let dir = dir.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = Command::new(&spawn_cmd)
            .args(&args)
            .current_dir(&dir)
            .output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("{cmd_name} not found in PATH"))
        }
        Ok(Err(e)) => Err(format!("failed to run {cmd_name}: {e}")),
        Err(_) => Err(format!("{cmd_name} timed out after {timeout_secs}s")),
    }
}

pub fn get_git_diff(
    project_root: &Path,
    staged: bool,
    commits: Option<&str>,
    timeout_secs: u64,
) -> (Option<String>, Option<String>) {
    let cmd_args: Vec<String> = if staged {
        vec!["diff".into(), "--cached".into(), "-M".into()]
    } else if let Some(range) = commits {
        vec!["diff".into(), "-M".into(), range.into()]
    } else {
        return (
            None,
            Some("Provide exactly one of staged=true or commits=<range>.".into()),
        );
    };

    match run_with_timeout("git", cmd_args, project_root, timeout_secs) {
        Ok(output) if output.status.success() => (
            Some(String::from_utf8_lossy(&output.stdout).to_string()),
            None,
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let msg = if stderr.is_empty() {
                format!("git exited {}", output.status.code().unwrap_or(-1))
            } else {
                stderr
            };
            (None, Some(msg))
        }
        Err(msg) => (None, Some(msg)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// (new_start, new_end) inclusive, 1-indexed line ranges touched in the new file.
    pub hunks: Vec<(i64, i64)>,
    /// New-file line numbers that are actual `+` additions, as opposed to
    /// unchanged context lines that merely fall within a hunk's numeric
    /// range. Real diffs carry surrounding context (typically 3 lines) even
    /// around a pure insertion, so a hunk-level "nothing removed" check is
    /// not enough — this is precise per line. A symbol whose signature range
    /// is fully covered by these didn't exist as that text before this diff.
    pub added_lines: std::collections::HashSet<i64>,
    pub is_new_file: bool,
    pub is_deleted_file: bool,
}

/// Minimal unified-diff parser: extracts per-file changed line ranges (new-file side)
/// from `diff --git` / `@@ ... @@` headers. Not a full diff/patch implementation —
/// just enough to overlap against indexed symbol line ranges.
pub fn parse_unified_diff(diff_text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    // New-file line number the next hunk-body line corresponds to; 0 means
    // "not currently inside a hunk body" (reset per file, set by each `@@`).
    let mut new_line_cursor: i64 = 0;

    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(f) = current.take() {
                files.push(f);
            }
            current = Some(FileDiff {
                path: parse_diff_git_header(rest),
                hunks: Vec::new(),
                added_lines: std::collections::HashSet::new(),
                is_new_file: false,
                is_deleted_file: false,
            });
            new_line_cursor = 0;
        } else if line.starts_with("new file mode") {
            if let Some(f) = current.as_mut() {
                f.is_new_file = true;
            }
        } else if line.starts_with("deleted file mode") {
            if let Some(f) = current.as_mut() {
                f.is_deleted_file = true;
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(f) = current.as_mut() {
                let rest = rest.trim();
                if rest != "/dev/null" {
                    f.path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                }
            }
        } else if let Some((new_start, new_end)) =
            line.strip_prefix("@@ ").and_then(parse_hunk_header)
            && let Some(f) = current.as_mut()
        {
            f.hunks.push((new_start, new_end));
            new_line_cursor = new_start;
        } else if new_line_cursor > 0
            && let Some(f) = current.as_mut()
        {
            // Inside a hunk body. `+` lines are additions at the current
            // new-file line (then the cursor advances); ` ` (context) lines
            // also advance the cursor but aren't additions; `-` (old-file
            // only) lines don't touch the new-file cursor at all.
            if line.starts_with('+') {
                f.added_lines.insert(new_line_cursor);
                new_line_cursor += 1;
            } else if line.starts_with(' ') {
                new_line_cursor += 1;
            }
            // Anything else (e.g. `-` lines, "\ No newline at end of file")
            // doesn't correspond to a new-file line — ignored.
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    files
}

fn parse_diff_git_header(rest: &str) -> String {
    if let Some(idx) = rest.find(" b/") {
        rest[idx + 3..].trim().to_string()
    } else {
        rest.split_whitespace()
            .last()
            .map(|s| s.strip_prefix("b/").unwrap_or(s).to_string())
            .unwrap_or_default()
    }
}

/// Parses the new-file `+start,len` range out of a hunk header tail (the part
/// after the leading `"@@ "` has already been stripped by the caller).
fn parse_hunk_header(rest: &str) -> Option<(i64, i64)> {
    let close = rest.find(" @@")?;
    let ranges = &rest[..close];
    let new_part = ranges.split(' ').find(|s| s.starts_with('+'))?;
    let new_part = &new_part[1..];
    let mut parts = new_part.splitn(2, ',');
    let start: i64 = parts.next()?.parse().ok()?;
    let len: i64 = parts
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1);
    let end = if len <= 0 { start } else { start + len - 1 };
    Some((start, end))
}

/// True when at least one line in `signature_range` is an actual `+`
/// addition (`added_lines` — see `FileDiff`), i.e. text that differs from
/// the pre-diff version, rather than unchanged context merely spanned by
/// the same hunk. Line-precise on purpose: real diffs carry several lines
/// of unchanged context around any edit (git's default -U3), so a coarser
/// "does any hunk's overall numeric range overlap this symbol" check flags
/// every symbol sitting near an edit within the same hunk — not just the
/// one actually touched — as "signature changed".
pub fn is_signature_changed(signature_range: (i64, i64), added_lines: &HashSet<i64>) -> bool {
    let (start, end) = signature_range;
    (start..=end).any(|line| added_lines.contains(&line))
}

/// True when `signature_range` falls entirely within territory that didn't
/// exist before this diff: either the whole file is new (`file_is_new`), or
/// every line in the range is an actual `+` addition (`added_lines` — see
/// `FileDiff`; deliberately *not* a hunk-level check, since real diffs carry
/// unchanged context lines around an insertion, so "hunk touches this range"
/// is not the same as "this range is new text"). Distinguishes "this symbol
/// was just created" from "this pre-existing symbol's signature line was
/// edited" — the two `diff_impact` previously conflated under a single
/// `signature_changed` flag, escalating every brand-new function to "high
/// risk — all call sites may need update" even though a symbol with zero
/// prior existence has zero prior call sites.
pub fn is_new_symbol(
    signature_range: (i64, i64),
    file_is_new: bool,
    added_lines: &std::collections::HashSet<i64>,
) -> bool {
    if file_is_new {
        return true;
    }
    let (start, end) = signature_range;
    start <= end && (start..=end).all(|line| added_lines.contains(&line))
}

pub fn compute_aggregate_risk(
    affected_symbols: &[HashMap<String, serde_json::Value>],
    unindexed_files: &[String],
) -> String {
    if !unindexed_files.is_empty() {
        return "unknown".to_string();
    }
    affected_symbols
        .iter()
        .filter_map(|s| {
            s.get("risk_assessment")
                .and_then(|r| r.get("level"))
                .and_then(|l| l.as_str())
        })
        .max_by_key(|level| RiskOrder::parse(level) as u8)
        .unwrap_or("low")
        .to_string()
}

pub fn escalate_risk_if_signature_changed(
    signature_changed: bool,
    level: &str,
    reasons: &mut Vec<String>,
) -> String {
    if signature_changed {
        reasons.push("signature modified — all call sites may need update".to_string());
        let current = RiskOrder::parse(level);
        if (current as u8) < (RiskOrder::High as u8) {
            return "high".to_string();
        }
    }
    level.to_string()
}

pub fn sort_affected_symbols(
    symbols: &mut Vec<HashMap<String, serde_json::Value>>,
    max_affected: usize,
) {
    symbols.sort_by(|a, b| {
        let risk_a = a
            .get("risk_assessment")
            .and_then(|r| r.get("level"))
            .and_then(|l| l.as_str())
            .unwrap_or("low");
        let risk_b = b
            .get("risk_assessment")
            .and_then(|r| r.get("level"))
            .and_then(|l| l.as_str())
            .unwrap_or("low");
        let sig_a = a
            .get("signature_changed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let sig_b = b
            .get("signature_changed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let blast_a = a
            .get("blast_radius")
            .and_then(|br| br.get("direct_callers"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let blast_b = b
            .get("blast_radius")
            .and_then(|br| br.get("direct_callers"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        (RiskOrder::parse(risk_b) as u8, sig_b as u8, blast_b).cmp(&(
            RiskOrder::parse(risk_a) as u8,
            sig_a as u8,
            blast_a,
        ))
    });
    symbols.truncate(max_affected);
}

use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_signature_changed_overlap() {
        assert!(is_signature_changed((5, 10), &HashSet::from([8])));
        assert!(is_signature_changed((5, 10), &HashSet::from([6])));
        assert!(is_signature_changed((5, 10), &HashSet::from([5, 10])));
    }

    #[test]
    fn test_is_signature_changed_no_overlap() {
        assert!(!is_signature_changed((5, 10), &HashSet::from([11, 20])));
        assert!(!is_signature_changed((5, 10), &HashSet::from([1, 4])));
    }

    #[test]
    fn test_is_signature_changed_empty_added_lines() {
        assert!(!is_signature_changed((5, 10), &HashSet::new()));
    }

    /// Regression: a symbol sitting near a real edit — but not itself
    /// touched — must not be flagged just because a hunk's *numeric range*
    /// (context lines included) happens to span it. `line 7` is unchanged
    /// context in the same hunk as an edit at line 3; only line 3 is an
    /// actual addition.
    #[test]
    fn test_is_signature_changed_ignores_unchanged_context_in_same_hunk() {
        let added = HashSet::from([3]);
        assert!(
            !is_signature_changed((7, 9), &added),
            "lines 7-9 are unchanged context, not touched by this diff"
        );
        assert!(
            is_signature_changed((1, 5), &added),
            "line 3 is genuinely new"
        );
    }

    #[test]
    fn test_escalate_risk() {
        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(true, "low", &mut reasons);
        assert_eq!(result, "high");
        assert_eq!(reasons.len(), 1);

        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(true, "critical", &mut reasons);
        assert_eq!(result, "critical");

        let mut reasons = Vec::new();
        let result = escalate_risk_if_signature_changed(false, "low", &mut reasons);
        assert_eq!(result, "low");
        assert!(reasons.is_empty());
    }

    #[test]
    fn test_compute_aggregate_risk_with_unindexed() {
        let result = compute_aggregate_risk(&[], &["new_file.rs".to_string()]);
        assert_eq!(result, "unknown");
    }

    #[test]
    fn test_compute_aggregate_risk_max() {
        let s1 = HashMap::from([(
            "risk_assessment".to_string(),
            serde_json::json!({"level": "low"}),
        )]);
        let s2 = HashMap::from([(
            "risk_assessment".to_string(),
            serde_json::json!({"level": "high"}),
        )]);
        let result = compute_aggregate_risk(&[s1, s2], &[]);
        assert_eq!(result, "high");
    }

    #[test]
    fn test_get_git_diff_no_args() {
        let dir = tempfile::tempdir().unwrap();
        let (diff, err) = get_git_diff(dir.path(), false, None, 10);
        assert!(diff.is_none());
        assert!(err.is_some());
    }

    /// Regression for W7: `Command::output()` blocks indefinitely with no
    /// built-in timeout. Uses `/bin/sleep` directly (bypassing PATH/git
    /// entirely) so the test is deterministic and doesn't depend on git
    /// being slow or on mutating global process state like PATH/env.
    #[test]
    fn test_run_with_timeout_aborts_on_slow_process() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let result = run_with_timeout("/bin/sleep", vec!["5".into()], dir.path(), 1);
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected timeout error, got {result:?}");
        assert!(
            result.unwrap_err().contains("timed out"),
            "error message should mention the timeout"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "should return well before the 5s sleep completes, took {elapsed:?}"
        );
    }

    #[test]
    fn test_run_with_timeout_returns_output_for_fast_process() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout("/bin/sleep", vec!["0".into()], dir.path(), 5);
        let output = result.unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn test_is_new_symbol_whole_new_file() {
        assert!(is_new_symbol(
            (5, 7),
            true,
            &std::collections::HashSet::new()
        ));
    }

    #[test]
    fn test_is_new_symbol_all_lines_added() {
        let added: std::collections::HashSet<i64> = (5..=7).collect();
        assert!(is_new_symbol((5, 7), false, &added));
    }

    #[test]
    fn test_is_new_symbol_false_when_some_lines_are_context() {
        // Line 6 is missing — it's unchanged context, not an addition — so
        // the signature range is not fully new text, even though its other
        // two lines are.
        let added: std::collections::HashSet<i64> = [5, 7].into_iter().collect();
        assert!(!is_new_symbol((5, 7), false, &added));
    }

    #[test]
    fn test_is_new_symbol_false_when_no_lines_added() {
        assert!(!is_new_symbol(
            (5, 7),
            false,
            &std::collections::HashSet::new()
        ));
    }

    /// Regression: a realistic git diff for inserting a new function after an
    /// existing one carries unchanged context lines in the *same* hunk (git's
    /// default -U3), so the hunk header alone (`-10,3 +10,7`) never has
    /// old_len=0 even though 4 of its 7 new-side lines are pure additions.
    /// `added_lines` must track this at line granularity, not hunk granularity.
    #[test]
    fn test_parse_unified_diff_tracks_added_lines_with_realistic_context() {
        // Built via an array + join (not `\`-continued string lines) so each
        // context line's meaningful single leading space survives exactly —
        // `\`-then-newline in a Rust string literal strips *all* leading
        // whitespace on the continuation line, which would silently eat the
        // very space that marks a diff context line.
        let diff = [
            "diff --git a/src/foo.rs b/src/foo.rs",
            "--- a/src/foo.rs",
            "+++ b/src/foo.rs",
            "@@ -10,3 +10,7 @@ fn existing() {",
            " line_a",
            " line_b",
            " line_c",
            "+",
            "+fn brand_new() {",
            "+    2",
            "+}",
        ]
        .join("\n");
        let files = parse_unified_diff(&diff);
        assert_eq!(files[0].hunks, vec![(10, 16)]);
        for context_line in 10..=12 {
            assert!(
                !files[0].added_lines.contains(&context_line),
                "line {context_line} is unchanged context, not an addition"
            );
        }
        for added_line in 13..=16 {
            assert!(
                files[0].added_lines.contains(&added_line),
                "line {added_line} is part of the new function"
            );
        }
    }

    #[test]
    fn test_parse_unified_diff_modified_line_is_not_added() {
        let diff = [
            "diff --git a/src/foo.rs b/src/foo.rs",
            "--- a/src/foo.rs",
            "+++ b/src/foo.rs",
            "@@ -10,1 +10,2 @@ fn foo() {",
            " context",
            "+new_line",
        ]
        .join("\n");
        let files = parse_unified_diff(&diff);
        assert_eq!(files[0].hunks, vec![(10, 11)]);
        assert!(
            !files[0].added_lines.contains(&10),
            "context line is not an addition"
        );
        assert!(
            files[0].added_lines.contains(&11),
            "the +new_line is an addition"
        );
    }

    #[test]
    fn test_parse_unified_diff_single_hunk() {
        let diff = "diff --git a/src/foo.rs b/src/foo.rs\n\
                     index abc..def 100644\n\
                     --- a/src/foo.rs\n\
                     +++ b/src/foo.rs\n\
                     @@ -10,3 +10,4 @@ fn foo() {\n\
                      context\n\
                     +new line\n\
                      context\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/foo.rs");
        assert_eq!(files[0].hunks, vec![(10, 13)]);
        assert!(!files[0].is_new_file);
        assert!(!files[0].is_deleted_file);
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let diff = "diff --git a/src/new.rs b/src/new.rs\n\
                     new file mode 100644\n\
                     index 000..abc\n\
                     --- /dev/null\n\
                     +++ b/src/new.rs\n\
                     @@ -0,0 +1,5 @@\n\
                     +fn new_fn() {}\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/new.rs");
        assert!(files[0].is_new_file);
        assert_eq!(files[0].hunks, vec![(1, 5)]);
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let diff = "diff --git a/src/old.rs b/src/old.rs\n\
                     deleted file mode 100644\n\
                     index abc..000\n\
                     --- a/src/old.rs\n\
                     +++ /dev/null\n\
                     @@ -1,5 +0,0 @@\n\
                     -fn old_fn() {}\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/old.rs");
        assert!(files[0].is_deleted_file);
    }

    #[test]
    fn test_parse_unified_diff_rename() {
        let diff = "diff --git a/src/old.rs b/src/renamed.rs\n\
                     similarity index 95%\n\
                     rename from src/old.rs\n\
                     rename to src/renamed.rs\n\
                     --- a/src/old.rs\n\
                     +++ b/src/renamed.rs\n\
                     @@ -1,2 +1,3 @@\n\
                      context\n\
                     +added\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/renamed.rs");
    }

    #[test]
    fn test_parse_unified_diff_multiple_files_and_hunks() {
        let diff = "diff --git a/a.rs b/a.rs\n\
                     --- a/a.rs\n\
                     +++ b/a.rs\n\
                     @@ -1,2 +1,2 @@\n\
                      x\n\
                     @@ -20,1 +20,1 @@\n\
                      y\n\
                     diff --git a/b.rs b/b.rs\n\
                     --- a/b.rs\n\
                     +++ b/b.rs\n\
                     @@ -5 +5 @@\n\
                      z\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.rs");
        assert_eq!(files[0].hunks, vec![(1, 2), (20, 20)]);
        assert_eq!(files[1].path, "b.rs");
        assert_eq!(files[1].hunks, vec![(5, 5)]);
    }
}
