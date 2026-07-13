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
    /// How many same-length line windows of the pre-edit file (this range
    /// included) are byte-identical to `old_text`. Anything above 1 means
    /// `expected_hash` can only vouch for the CONTENT at this range, not
    /// its POSITION — a stale line number that happens to point at another
    /// identical window (a lone `}` line, say) still hash-matches and the
    /// edit lands there instead.
    pub content_occurrences: usize,
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
        let window = &lines[h.start_line - 1..h.end_line];
        let old_text: String = window.concat();
        let current_hash = hash_content(&old_text);
        let content_occurrences = lines
            .windows(window.len())
            .filter(|w| **w == *window)
            .count();
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
            content_occurrences,
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
        let mut new_lines: Vec<String> = split_lines_inclusive(&h.new_text)
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        // A non-EOF hunk whose `new_text` is missing its trailing newline
        // would otherwise fuse onto whatever untouched line follows it --
        // silently merging two adjacent symbols onto one physical line (the
        // root cause behind a real PARSE_ERROR landmine found in
        // crates/calm-server/src/tools/orient.rs). Only normalize when the
        // hunk doesn't reach the true end of the original file, so
        // `test_no_trailing_newline_preserved`'s EOF behavior stays intact.
        if h.end_line < lines.len()
            && let Some(last) = new_lines.last_mut()
            && !last.ends_with('\n')
        {
            last.push('\n');
        }
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

#[derive(Debug, PartialEq)]
pub enum MatchOutcome {
    NotFound,
    /// 1-indexed line numbers of every occurrence found, for a caller to
    /// report back (mirrors `SymbolResolution::Ambiguous`'s shape).
    Ambiguous(Vec<usize>),
}

/// Small-text-match mode: search for `old_text` within `content`'s
/// `[line_start, line_end]` window (1-indexed, inclusive — same convention
/// as `HunkRequest`), and if it occurs exactly once, build a `HunkRequest`
/// that replaces just that occurrence with `new_text`. Reads the real
/// current content to find the match, so `expected_hash` is computed here
/// too — a stale match is structurally impossible, same guarantee
/// `insertion_hunk` already provides for its anchor line.
pub fn find_and_replace_hunk(
    content: &str,
    line_start: usize,
    line_end: usize,
    old_text: &str,
    new_text: &str,
) -> Result<HunkRequest, MatchOutcome> {
    let lines = split_lines_inclusive(content);
    if line_start < 1 || line_end < line_start || line_end > lines.len() {
        return Err(MatchOutcome::NotFound);
    }
    let window_start_byte: usize = lines[..line_start - 1].iter().map(|l| l.len()).sum();
    let window: String = lines[line_start - 1..line_end].concat();

    let match_lines: Vec<usize> = window
        .match_indices(old_text)
        .map(|(byte_off, _)| {
            let abs_byte = window_start_byte + byte_off;
            content[..abs_byte].matches('\n').count() + 1
        })
        .collect();

    match match_lines.len() {
        0 => Err(MatchOutcome::NotFound),
        1 => {
            let full_new = window.replace(old_text, new_text);
            Ok(HunkRequest {
                start_line: line_start,
                end_line: line_end,
                expected_hash: Some(hash_content(&window)),
                new_text: full_new,
            })
        }
        _ => Err(MatchOutcome::Ambiguous(match_lines)),
    }
}

/// Where `insertion_hunk` places `new_text` relative to a symbol's
/// `[line_start, line_end]` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertPosition {
    /// Directly above `line_start` — the symbol shifts down untouched.
    Before,
    /// Directly below `line_end` — a new sibling after the symbol.
    After,
    /// At the end of the symbol's body: above `line_end` when that line is
    /// a bare closing delimiter (`}`/`)`/`]`, or `end` for Ruby/Lua/
    /// Elixir), below it otherwise (Python-style bodies with no closer).
    AppendInside,
}

/// Builds a pure-insertion hunk pinned to a single anchor line of
/// `content`, so callers add code relative to structure instead of doing
/// line arithmetic against a possibly-stale snapshot: the anchor line is
/// re-emitted verbatim inside the hunk's `new_text` and its current hash is
/// pre-filled as `expected_hash`, so `apply_hunks` still conflict-checks
/// the write against exactly what was read here. An insertion below a
/// final line lacking a trailing newline adds one (the inserted text must
/// start on its own line). Returns `None` when the range is out of bounds.
pub fn insertion_hunk(
    content: &str,
    line_start: usize,
    line_end: usize,
    position: InsertPosition,
    new_text: &str,
) -> Option<HunkRequest> {
    let lines = split_lines_inclusive(content);
    if line_start < 1 || line_end < line_start || line_end > lines.len() {
        return None;
    }
    let mut insert = new_text.to_string();
    if !insert.ends_with('\n') {
        insert.push('\n');
    }
    let insert_above = match position {
        InsertPosition::Before => true,
        InsertPosition::After => false,
        InsertPosition::AppendInside => {
            let last = lines[line_end - 1].trim();
            last.starts_with('}')
                || last.starts_with(')')
                || last.starts_with(']')
                || last == "end"
                || last.starts_with("end ")
        }
    };
    let anchor = match position {
        InsertPosition::Before => line_start,
        _ => line_end,
    };
    let anchor_line = lines[anchor - 1];
    let combined = if insert_above {
        format!("{insert}{anchor_line}")
    } else if anchor_line.ends_with('\n') {
        format!("{anchor_line}{insert}")
    } else {
        format!("{anchor_line}\n{insert}")
    };
    Some(HunkRequest {
        start_line: anchor,
        end_line: anchor,
        expected_hash: Some(hash_content(anchor_line)),
        new_text: combined,
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

    // Captured before the write so a set_permissions failure below can never
    // make this function fail a write that already succeeded content-wise —
    // audit F5: File::create(tmp) + rename() used to always hand the new
    // file umask-derived perms, silently dropping the executable bit off
    // scripts/*.sh (or any other non-default mode) on every edit_lines/
    // edit_symbol write.
    let original_perms = std::fs::metadata(path).ok().map(|m| m.permissions());

    let write_result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp_path)?;
        std::io::Write::write_all(&mut f, content.as_bytes())?;
        f.sync_all()?;
        Ok(())
    })();

    match write_result {
        Ok(()) => {
            if let Some(perms) = original_perms {
                // Best-effort: a permissions mismatch (e.g. read-only fs,
                // owner mismatch) must not fail a write whose content
                // already landed correctly.
                let _ = std::fs::set_permissions(&tmp_path, perms);
            }
            std::fs::rename(&tmp_path, path)
        }
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
    fn test_mid_file_hunk_missing_trailing_newline_does_not_fuse_next_line() {
        // Regression test for the root cause behind the orient.rs:251 /
        // trace.rs:539 PARSE_ERROR landmines: a replace hunk that doesn't
        // reach EOF and whose new_text lacks a trailing newline must NOT
        // fuse onto the next untouched line.
        let original = "line1\nline2\nline3\n";
        let old_hash = hash_content("line2\n");
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: Some(old_hash),
                new_text: "replaced".to_string(), // deliberately no trailing \n
            }],
        )
        .unwrap();
        assert_eq!(outcome.new_content.unwrap(), "line1\nreplaced\nline3\n");
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
    fn find_and_replace_hunk_unique_match_produces_correct_hunk() {
        let content = "fn f() {\n    let x = 1;\n    let y = 2;\n}\n";
        let hunk = find_and_replace_hunk(content, 1, 4, "let x = 1;", "let x = 99;").unwrap();
        let outcome = apply_hunks(content, &[hunk]).unwrap();
        assert_eq!(
            outcome.new_content.unwrap(),
            "fn f() {\n    let x = 99;\n    let y = 2;\n}\n"
        );
    }

    #[test]
    fn find_and_replace_hunk_zero_matches_is_not_found() {
        let content = "fn f() {\n    let x = 1;\n}\n";
        let err = find_and_replace_hunk(content, 1, 3, "nope", "x").unwrap_err();
        assert!(matches!(err, MatchOutcome::NotFound));
    }

    #[test]
    fn find_and_replace_hunk_multiple_matches_is_ambiguous_with_locations() {
        let content = "fn f() {\n    let x = 1;\n    let x = 2;\n}\n";
        let err = find_and_replace_hunk(content, 1, 4, "let x", "let z").unwrap_err();
        match err {
            MatchOutcome::Ambiguous(locations) => assert_eq!(locations, vec![2, 3]),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn find_and_replace_hunk_scopes_search_to_the_given_range() {
        // A match outside [line_start, line_end] must not count.
        let content = "let x = 1;\nfn f() {\n    let y = 2;\n}\n";
        let err = find_and_replace_hunk(content, 2, 4, "let x", "let z").unwrap_err();
        assert!(matches!(err, MatchOutcome::NotFound));
    }

    #[test]
    fn find_and_replace_hunk_old_text_spanning_a_line_boundary() {
        // Regression guard from task-risk-score's B3 gap: find_and_replace_hunk
        // computes its own hash from the same window its own line-arithmetic
        // derives, so a boundary bug here would be self-consistent and NOT
        // caught by apply_hunks' hash check downstream -- must be verified
        // directly against a real multi-line match.
        let content = "fn f() {\n    let x =\n        1;\n}\n";
        let hunk =
            find_and_replace_hunk(content, 1, 4, "let x =\n        1;", "let x = 2;").unwrap();
        let outcome = apply_hunks(content, &[hunk]).unwrap();
        assert_eq!(
            outcome.new_content.unwrap(),
            "fn f() {\n    let x = 2;\n}\n"
        );
    }

    #[test]
    fn find_and_replace_hunk_multi_byte_utf8_old_text() {
        let content = "fn f() {\n    let s = \"café 中文\";\n}\n";
        let hunk = find_and_replace_hunk(content, 1, 3, "café 中文", "bar").unwrap();
        let outcome = apply_hunks(content, &[hunk]).unwrap();
        assert_eq!(
            outcome.new_content.unwrap(),
            "fn f() {\n    let s = \"bar\";\n}\n"
        );
    }

    proptest::proptest! {
        #[test]
        fn apply_hunks_never_fuses_two_untouched_lines(
            prefix_lines in proptest::collection::vec("[a-z]{1,8}", 1..5),
            hunk_new_text in proptest::collection::vec("[a-z]{1,8}", 0..3),
            suffix_lines in proptest::collection::vec("[a-z]{1,8}", 1..5),
            drop_trailing_newline in proptest::bool::ANY,
        ) {
            // Regression guard for this session's real bug (a mid-file
            // replace hunk whose new_text lacked a trailing newline silently
            // fused with the next untouched line -- root cause of the
            // orient.rs:251/trace.rs:539 landmines). 20+ hand-written unit
            // tests in this file missed this case; this fuzzes it directly.
            let original: String = prefix_lines.iter()
                .chain(std::iter::once(&"REPLACE_ME".to_string()))
                .chain(suffix_lines.iter())
                .map(|l| format!("{l}\n"))
                .collect();
            let replace_line = prefix_lines.len() + 1;

            let mut new_text: String = hunk_new_text.iter().map(|l| format!("{l}\n")).collect();
            if new_text.is_empty() {
                new_text = "x\n".to_string();
            }
            if drop_trailing_newline && new_text.ends_with('\n') {
                new_text.pop();
            }

            let old_hash = hash_content("REPLACE_ME\n");
            let outcome = apply_hunks(
                &original,
                &[HunkRequest {
                    start_line: replace_line,
                    end_line: replace_line,
                    expected_hash: Some(old_hash),
                    new_text,
                }],
            ).unwrap();

            let new_content = outcome.new_content.unwrap();
            // The invariant this session's real bug violated: every line that
            // was NOT part of the hunk must still be its own, intact physical
            // line in the output -- specifically, the first suffix line must
            // appear as a whole line, never fused onto the hunk's replacement.
            if let Some(first_suffix) = suffix_lines.first() {
                let expected_line = format!("{first_suffix}\n");
                proptest::prop_assert!(
                    new_content.split_inclusive('\n').any(|l| l == expected_line),
                    "suffix line {first_suffix:?} was not preserved intact in {new_content:?}"
                );
            }
        }
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
    #[test]
    #[cfg(unix)]
    fn test_atomic_write_preserves_original_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("ci_edit_atomic_perms_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        atomic_write(&path, "#!/bin/sh\necho new\n").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "atomic_write must preserve the original file's mode, not hand the replacement umask-derived perms"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_content_occurrences_flags_generic_ranges() {
        let src = "fn a() {\n}\nfn b() {\n}\nfn c() {\n}\n";
        // previewing the lone `}` on line 2 — lines 4 and 6 are identical
        let out = apply_hunks(
            src,
            &[HunkRequest {
                start_line: 2,
                end_line: 2,
                expected_hash: None,
                new_text: String::new(),
            }],
        )
        .unwrap();
        assert_eq!(out.results[0].content_occurrences, 3);

        // a distinctive line matches only itself
        let out = apply_hunks(
            src,
            &[HunkRequest {
                start_line: 1,
                end_line: 1,
                expected_hash: None,
                new_text: String::new(),
            }],
        )
        .unwrap();
        assert_eq!(out.results[0].content_occurrences, 1);

        // multi-line windows count too: [`}`, `fn b() {`] appears once
        let out = apply_hunks(
            src,
            &[HunkRequest {
                start_line: 2,
                end_line: 3,
                expected_hash: None,
                new_text: String::new(),
            }],
        )
        .unwrap();
        assert_eq!(out.results[0].content_occurrences, 1);
    }

    #[test]
    fn test_insertion_hunk_append_inside_brace_body() {
        let src = "mod tests {\n    fn old() {}\n}\n";
        let h =
            insertion_hunk(src, 1, 3, InsertPosition::AppendInside, "    fn newer() {}").unwrap();
        assert_eq!((h.start_line, h.end_line), (3, 3));
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(
            out.new_content.unwrap(),
            "mod tests {\n    fn old() {}\n    fn newer() {}\n}\n"
        );
    }

    #[test]
    fn test_insertion_hunk_append_inside_end_keyword_body() {
        let src = "def f\n  x = 1\nend\n";
        let h = insertion_hunk(src, 1, 3, InsertPosition::AppendInside, "  y = 2").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(out.new_content.unwrap(), "def f\n  x = 1\n  y = 2\nend\n");
    }

    #[test]
    fn test_insertion_hunk_append_inside_no_closer_appends_below() {
        let src = "def f():\n    x = 1\n";
        let h = insertion_hunk(src, 1, 2, InsertPosition::AppendInside, "    y = 2").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(out.new_content.unwrap(), "def f():\n    x = 1\n    y = 2\n");
    }

    #[test]
    fn test_insertion_hunk_before_and_after() {
        let src = "fn a() {}\nfn b() {}\n";
        let h = insertion_hunk(src, 2, 2, InsertPosition::Before, "fn mid() {}").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(
            out.new_content.unwrap(),
            "fn a() {}\nfn mid() {}\nfn b() {}\n"
        );

        let h = insertion_hunk(src, 2, 2, InsertPosition::After, "fn tail() {}").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(
            out.new_content.unwrap(),
            "fn a() {}\nfn b() {}\nfn tail() {}\n"
        );
    }

    #[test]
    fn test_insertion_hunk_after_eof_without_trailing_newline() {
        let src = "fn a() {}";
        let h = insertion_hunk(src, 1, 1, InsertPosition::After, "fn b() {}").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(out.new_content.unwrap(), "fn a() {}\nfn b() {}\n");
    }

    #[test]
    fn test_insertion_hunk_stale_anchor_is_conflict() {
        let src = "fn a() {\n}\n";
        let h = insertion_hunk(src, 1, 2, InsertPosition::AppendInside, "    let x = 1;").unwrap();
        // the file changes under us before the hunk is applied
        let changed = "fn a() {\n    let y = 2;\n}\n";
        let out = apply_hunks(changed, &[h]).unwrap();
        assert!(!out.all_applied);
        assert!(matches!(out.results[0].status, HunkStatus::Conflict));
    }

    #[test]
    fn test_insertion_hunk_out_of_bounds_is_none() {
        assert!(insertion_hunk("one\n", 1, 2, InsertPosition::After, "x").is_none());
        assert!(insertion_hunk("one\n", 0, 1, InsertPosition::Before, "x").is_none());
    }
}
