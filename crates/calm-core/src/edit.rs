//! Line-range text editing primitive for `edit_lines`/`edit_symbol`.
//!
//! Pure logic only — no filesystem or DB access except `atomic_write`, which
//! is a plain fs helper with no DB involvement. The MCP-facing wiring (risk
//! gate, reindex-after-write, response shape) lives in
//! `calm-server/src/tools/edit.rs`.

use std::path::Path;

use crate::indexer::lang_constants::language_for_extension;
use crate::indexer::parser::parse_tree;
use crate::indexer::pipeline::hash_content;

/// One requested change to `[start_line, end_line]` (1-indexed, inclusive)
/// of a file. `expected_hash: None` means "preview only" — the caller wants
/// to see the current hash/content of this range without writing anything
/// (the standard way to learn a range's hash before a real edit, since
/// there is no separate "read with checksum" tool for arbitrary — as
/// opposed to symbol-shaped — ranges).
#[derive(Debug, Clone)]
pub struct HunkRequest {
    pub start_line: usize,
    pub end_line: usize,
    pub expected_hash: Option<String>,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkStatus {
    /// Hash matched (or this hunk had no hash to check); part of a batch
    /// where every hunk matched, and the file was written.
    Applied,
    /// `expected_hash` was `None` — nothing was written for this hunk (or
    /// any other hunk in the same call, since `apply_hunks` is all-or-nothing).
    Preview,
    /// `expected_hash` was `Some` but didn't match the range's current hash.
    Conflict,
}

#[derive(Debug, Clone)]
pub struct HunkResult {
    pub start_line: usize,
    pub end_line: usize,
    /// Hash of the range's content *before* this call (whether or not it
    /// ended up applied) — what a caller should pass as `expected_hash` on
    /// a retry.
    pub current_hash: String,
    /// The range's content *before* this call — doubles as preview content
    /// (on `Preview`/`Conflict`) and as undo material (on `Applied`).
    pub old_text: String,
    pub status: HunkStatus,
    /// Only meaningful when `status == Applied`: the line the replacement
    /// content now ends at (`start_line` is unchanged — bottom-up
    /// application means a hunk's own start position never shifts,
    /// regardless of how many lines hunks below it added or removed).
    pub new_end_line: usize,
}

#[derive(Debug)]
pub enum ApplyError {
    EmptyHunks,
    OutOfRange {
        start_line: usize,
        end_line: usize,
        file_lines: usize,
    },
    InvalidRange {
        start_line: usize,
        end_line: usize,
    },
    OverlappingHunks,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::EmptyHunks => write!(f, "at least one hunk is required"),
            ApplyError::OutOfRange {
                start_line,
                end_line,
                file_lines,
            } => write!(
                f,
                "hunk [{start_line},{end_line}] is out of range — file has {file_lines} lines"
            ),
            ApplyError::InvalidRange {
                start_line,
                end_line,
            } => write!(
                f,
                "invalid range [{start_line},{end_line}] — start_line must be >= 1 and <= end_line"
            ),
            ApplyError::OverlappingHunks => {
                write!(
                    f,
                    "hunks overlap — each call may only touch disjoint ranges"
                )
            }
        }
    }
}

impl std::error::Error for ApplyError {}

#[derive(Debug)]
pub struct ApplyOutcome {
    /// `Some` only when every hunk's hash matched (or every hunk was a
    /// preview) and all were applied — the full new file content to write.
    /// `None` means nothing should be written: some hunk was a preview or
    /// conflict, so the whole batch is reported without touching disk.
    pub new_content: Option<String>,
    /// Per-hunk results, sorted by `start_line` ascending (regardless of
    /// the bottom-up order they were processed in).
    pub results: Vec<HunkResult>,
    pub all_applied: bool,
}

/// Hash of `content`'s `[start_line, end_line]` (1-indexed, inclusive),
/// using the exact same byte-faithful line-splitting `apply_hunks` uses
/// internally — so a checksum reported for a range by e.g. `edit_context`
/// is guaranteed to match what `apply_hunks` computes for that same range.
/// `None` if the range is out of bounds.
pub fn range_checksum(content: &str, start_line: usize, end_line: usize) -> Option<String> {
    let lines = split_lines_inclusive(content);
    if start_line < 1 || end_line < start_line || end_line > lines.len() {
        return None;
    }
    Some(hash_content(&lines[start_line - 1..end_line].concat()))
}

/// Byte-faithful line split: each element keeps its own line terminator
/// (`\n`, or `\r\n` since `\r` isn't a split point and stays attached to
/// the preceding text), and the final element has none if the file doesn't
/// end in a newline. Deliberately not `str::lines()`, which strips
/// terminators and would make `new_text` reconstruction lossy for CRLF
/// files or a missing trailing newline.
fn split_lines_inclusive(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split_inclusive('\n').collect()
}

/// Apply `hunks` to `original` — all or nothing. Every hunk must have a
/// matching `expected_hash` for anything to be written; if any hunk is a
/// preview (`expected_hash: None`) or a conflict, `new_content` is `None`
/// and nothing is written, but every hunk's current hash/content is still
/// reported so the caller can retry with correct hashes.
///
/// Hunks are processed bottom-up (highest `start_line` first) so that
/// splicing one hunk's replacement lines never shifts the line numbers of
/// any hunk still to be processed — they're all above it, and inserting or
/// removing lines only shifts what comes *after* the edit point.
pub fn apply_hunks(original: &str, hunks: &[HunkRequest]) -> Result<ApplyOutcome, ApplyError> {
    if hunks.is_empty() {
        return Err(ApplyError::EmptyHunks);
    }

    let lines = split_lines_inclusive(original);

    let mut sorted: Vec<&HunkRequest> = hunks.iter().collect();
    sorted.sort_by_key(|h| std::cmp::Reverse(h.start_line));

    for h in &sorted {
        if h.start_line < 1 || h.end_line < h.start_line {
            return Err(ApplyError::InvalidRange {
                start_line: h.start_line,
                end_line: h.end_line,
            });
        }
        if h.end_line > lines.len() {
            return Err(ApplyError::OutOfRange {
                start_line: h.start_line,
                end_line: h.end_line,
                file_lines: lines.len(),
            });
        }
    }
    for w in sorted.windows(2) {
        let (later, earlier) = (w[0], w[1]); // sorted descending by start_line
        if earlier.end_line >= later.start_line {
            return Err(ApplyError::OverlappingHunks);
        }
    }

    let mut results = Vec::with_capacity(sorted.len());
    let mut all_applied = true;
    for h in &sorted {
        let old_text: String = lines[h.start_line - 1..h.end_line].concat();
        let current_hash = hash_content(&old_text);
        let status = match &h.expected_hash {
            None => {
                all_applied = false;
                HunkStatus::Preview
            }
            Some(expected) if *expected == current_hash => HunkStatus::Applied,
            Some(_) => {
                all_applied = false;
                HunkStatus::Conflict
            }
        };
        let new_end_line =
            h.start_line + split_lines_inclusive(&h.new_text).len().saturating_sub(1);
        results.push(HunkResult {
            start_line: h.start_line,
            end_line: h.end_line,
            current_hash,
            old_text,
            status,
            new_end_line,
        });
    }

    if !all_applied {
        results.sort_by_key(|r| r.start_line);
        return Ok(ApplyOutcome {
            new_content: None,
            results,
            all_applied: false,
        });
    }

    let mut working: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    for h in &sorted {
        let new_lines: Vec<String> = split_lines_inclusive(&h.new_text)
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        working.splice(h.start_line - 1..h.end_line, new_lines);
    }
    let new_content = working.concat();

    results.sort_by_key(|r| r.start_line);
    Ok(ApplyOutcome {
        new_content: Some(new_content),
        results,
        all_applied: true,
    })
}

/// `Some(true)` = parses clean, `Some(false)` = introduces a tree-sitter
/// `ERROR`/`MISSING` node, `None` = `extension` has no recognized grammar
/// (Cargo.toml, docs/*.md, ...) so validation is skipped — callers must
/// treat `None` as "allow the write", not as a rejection, since `edit_lines`
/// is explicitly meant to also work on files the indexer never parses.
pub fn validate_syntax(new_content: &str, extension: &str) -> Option<bool> {
    let language = language_for_extension(extension)?;
    let tree = parse_tree(new_content, language)?;
    Some(!tree.root_node().has_error())
}

/// Write `content` to `path` atomically: write to a temp file in the same
/// directory, then `rename()` over the target. A concurrent reader (the
/// file watcher's `notify` handler, an editor, `search_grep`) can never
/// observe a half-written file — `rename` within one filesystem is atomic,
/// unlike a direct `fs::write` which truncates-then-writes in place.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp_path = dir.join(format!(".{file_name}.ci-edit-{}.tmp", std::process::id()));

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut f, content.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();

    match write_result {
        Ok(()) => std::fs::rename(&tmp_path, path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_checksum_matches_apply_hunks_hashing() {
        let content = "a\nb\nc\nd\n";
        let checksum = range_checksum(content, 2, 3).unwrap();
        assert_eq!(checksum, hash_content("b\nc\n"));

        // A checksum computed via range_checksum must be accepted by
        // apply_hunks for the exact same range — this is the whole point
        // of exposing it (edit_context's range_checksum must be usable
        // as edit_lines'/edit_symbol's expected_hash).
        let outcome = apply_hunks(
            content,
            &[HunkRequest {
                start_line: 2,
                end_line: 3,
                expected_hash: Some(checksum),
                new_text: "B\nC\n".to_string(),
            }],
        )
        .unwrap();
        assert!(outcome.all_applied);
    }

    #[test]
    fn test_range_checksum_out_of_bounds_is_none() {
        let content = "a\nb\n";
        assert_eq!(range_checksum(content, 1, 5), None);
        assert_eq!(range_checksum(content, 0, 1), None);
    }

    #[test]
    fn test_apply_single_hunk_matching_hash() {
        let original = "line1\nline2\nline3\n";
        let old_hash = hash_content("line2\n");
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some(old_hash),
                new_text: "replaced\n".to_string(),
            }],
        )
        .unwrap();
        assert!(outcome.all_applied);
        assert_eq!(outcome.new_content.unwrap(), "line1\nreplaced\nline3\n");
        assert_eq!(outcome.results[0].status, HunkStatus::Applied);
        assert_eq!(outcome.results[0].old_text, "line2\n");
    }

    #[test]
    fn test_apply_stale_hash_is_conflict_and_writes_nothing() {
        let original = "line1\nline2\nline3\n";
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some("deadbeefdeadbeef".to_string()),
                new_text: "replaced\n".to_string(),
            }],
        )
        .unwrap();
        assert!(!outcome.all_applied);
        assert!(outcome.new_content.is_none());
        assert_eq!(outcome.results[0].status, HunkStatus::Conflict);
        assert_eq!(outcome.results[0].current_hash, hash_content("line2\n"));
    }

    #[test]
    fn test_preview_mode_writes_nothing_but_reports_hash() {
        let original = "line1\nline2\nline3\n";
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: None,
                new_text: "ignored\n".to_string(),
            }],
        )
        .unwrap();
        assert!(!outcome.all_applied);
        assert!(outcome.new_content.is_none());
        assert_eq!(outcome.results[0].status, HunkStatus::Preview);
        assert_eq!(outcome.results[0].current_hash, hash_content("line2\n"));
    }

    #[test]
    fn test_multi_hunk_bottom_up_does_not_shift_upper_hunk() {
        let original = "a\nb\nc\nd\ne\n";
        // Hunk 1 (top): replace line 2 with 3 lines. Hunk 2 (bottom): replace line 4.
        // If applied top-down naively without bottom-up handling, hunk 2's
        // original line 4 would now be line 6 in a half-edited buffer.
        let hunks = vec![
            HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some(hash_content("b\n")),
                new_text: "b1\nb2\nb3\n".to_string(),
            },
            HunkRequest {
                start_line: 4,
                end_line: 4,
                expected_hash: Some(hash_content("d\n")),
                new_text: "D\n".to_string(),
            },
        ];
        let outcome = apply_hunks(original, &hunks).unwrap();
        assert!(outcome.all_applied);
        assert_eq!(outcome.new_content.unwrap(), "a\nb1\nb2\nb3\nc\nD\ne\n");
    }

    #[test]
    fn test_overlapping_hunks_rejected() {
        let original = "a\nb\nc\nd\n";
        let hunks = vec![
            HunkRequest {
                start_line: 1,
                end_line: 2,
                expected_hash: Some(hash_content("a\nb\n")),
                new_text: "x\n".to_string(),
            },
            HunkRequest {
                start_line: 2,
                end_line: 3,
                expected_hash: Some(hash_content("b\nc\n")),
                new_text: "y\n".to_string(),
            },
        ];
        let err = apply_hunks(original, &hunks).unwrap_err();
        assert!(matches!(err, ApplyError::OverlappingHunks));
    }

    #[test]
    fn test_out_of_range_rejected() {
        let original = "a\nb\n";
        let err = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 5,
                end_line: 5,
                expected_hash: Some("x".to_string()),
                new_text: "z\n".to_string(),
            }],
        )
        .unwrap_err();
        assert!(matches!(err, ApplyError::OutOfRange { .. }));
    }

    #[test]
    fn test_crlf_round_trip_preserves_line_endings() {
        let original = "line1\r\nline2\r\nline3\r\n";
        let old_hash = hash_content("line2\r\n");
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some(old_hash),
                new_text: "replaced\r\n".to_string(),
            }],
        )
        .unwrap();
        assert_eq!(
            outcome.new_content.unwrap(),
            "line1\r\nreplaced\r\nline3\r\n"
        );
    }

    #[test]
    fn test_no_trailing_newline_preserved() {
        let original = "line1\nline2\nline3";
        let old_hash = hash_content("line3");
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 3,
                end_line: 3,
                expected_hash: Some(old_hash),
                new_text: "replaced".to_string(),
            }],
        )
        .unwrap();
        assert_eq!(outcome.new_content.unwrap(), "line1\nline2\nreplaced");
    }

    #[test]
    fn test_unicode_multibyte_line_boundary_safe() {
        let original = "日本語\n中文测试\nEnglish\n";
        let old_hash = hash_content("中文测试\n");
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some(old_hash),
                new_text: "한국어\n".to_string(),
            }],
        )
        .unwrap();
        assert_eq!(outcome.new_content.unwrap(), "日本語\n한국어\nEnglish\n");
    }

    #[test]
    fn test_validate_syntax_detects_error_node() {
        assert_eq!(validate_syntax("def f():\n    pass\n", "py"), Some(true));
        assert_eq!(validate_syntax("def f(:\n    pass\n", "py"), Some(false));
    }

    #[test]
    fn test_validate_syntax_none_for_unrecognized_extension() {
        assert_eq!(validate_syntax("[dependencies]\nfoo = 1\n", "toml"), None);
    }

    #[test]
    fn test_atomic_write_then_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("ci_edit_atomic_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "old\n").unwrap();

        atomic_write(&path, "new content\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
