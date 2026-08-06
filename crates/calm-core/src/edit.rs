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

/// Render `body` (already sanitized) with `cat -n`-style absolute
/// line-number gutters, matching the `<n>\t<line>` shape a coding agent
/// reads from a native file read — so a CALM `source` read is directly
/// usable to pick an `edit_lines`/`edit_symbol` hunk without counting
/// lines by hand. `first_line` is the 1-indexed absolute line number of
/// `body`'s first line (a symbol's `line_start`, or a range read's start).
///
/// Gutters are added AFTER sanitize/injection detection so they are never
/// scanned as content, and they never affect the etag — which hashes the
/// raw file range (`range_checksum`), independent of this rendering. An
/// empty `body` renders to an empty string.
pub fn with_line_gutters(body: &str, first_line: i64) -> String {
    if body.is_empty() {
        return String::new();
    }
    body.lines()
        .enumerate()
        .map(|(i, line)| format!("{}\t{}", first_line + i as i64, line))
        .collect::<Vec<_>>()
        .join("\n")
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

/// Byte range of every ERROR/MISSING node in `node`'s subtree, converted to
/// 1-indexed inclusive LINE ranges (tree-sitter positions are 0-indexed
/// rows) so they can be compared against the caller's own line-based hunk
/// coordinates without a byte-offset round trip.
fn collect_error_line_ranges(node: tree_sitter::Node, out: &mut Vec<(i64, i64)>) {
    if node.is_error() || node.is_missing() {
        let start = node.start_position().row as i64 + 1;
        let end = (node.end_position().row as i64 + 1).max(start);
        out.push((start, end));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_error_line_ranges(child, out);
    }
}

/// True if line range `r` intersects any zone in `zones`.
fn intersects_any(r: &(i64, i64), zones: &[(i64, i64)]) -> bool {
    zones.iter().any(|&(zs, ze)| !(r.1 < zs || r.0 > ze))
}

/// Root-cause fix for a real false-positive found 2026-07-14: `validate_syntax`
/// re-parses the WHOLE resulting file and rejects the write if `has_error()`
/// is true anywhere in it — including pre-existing regions the edit never
/// touched. This goes wrong whenever the file already contains a construct
/// newer than the vendored grammar (verified case: `&raw const`/`&raw mut`,
/// stable Rust syntax that `tree-sitter-rust` 0.23.3 has no rule for and
/// marks as an `ERROR` node at every occurrence — confirmed the file still
/// compiles clean with `rustc`, and confirmed a byte-identical no-op
/// "replacement" of an unrelated line range was rejected with the exact
/// same `PARSE_ERROR`, proving the failure had nothing to do with what was
/// being written). Bumping the grammar isn't a safe narrow fix here: this
/// workspace's `tree-sitter` core is deliberately held at an ABI level
/// (13-14) that ~15 other exact-pinned language grammars (see this crate's
/// workspace `Cargo.toml` comments) depend on; jumping it to unlock a newer
/// `tree-sitter-rust` risks an ABI cliff across all of them.
///
/// Audit 5.4: comparing bare error-node COUNTS (`new_errors <=
/// original_errors`) doesn't actually prove "no new error" — a hunk that
/// coincidentally fixes one pre-existing error while introducing a
/// DIFFERENT one nearby keeps the same total and silently passed. Now
/// compares SPANS: any error node whose line range intersects the edited
/// region (`touched_new_lines`, each widened by a small resync margin — an
/// unmatched delimiter can misparse a few lines past the actual edit before
/// the grammar recovers) rejects outright, regardless of the old count —
/// there's no valid "it was already broken exactly there" excuse for text
/// this call just wrote. Errors OUTSIDE the touched region still use a
/// count comparison (scoped to `touched_old_lines` on the original side),
/// same spirit as before, so a pre-existing error elsewhere in the file
/// that simply shifted line numbers (the edit changed the file's total
/// line count) still correctly reads as "already there", not "new".
///
/// `touched_old_lines`/`touched_new_lines`: the edited hunks' line ranges
/// in the ORIGINAL and resulting NEW file respectively (1-indexed,
/// inclusive) — e.g. from `apply_hunks`' `HunkResult::{start_line,
/// end_line}` (old) and a cumulative-shift-adjusted `new_end_line` (new).
/// Empty is treated as "nothing known to be touched" (no zone rejection,
/// falls back to the old global-count comparison) rather than an error —
/// defensive for any future caller that hasn't threaded hunk positions
/// through yet.
///
/// `Some(true)` = clean, or no more errors than `original` already had
/// outside the touched region, and none inside it.
/// `Some(false)` = this edit introduced an error intersecting the touched
/// region, or strictly increased the outside-region error count.
/// `None` = `extension` has no recognized grammar — callers must treat this
/// as "allow the write", same convention as `validate_syntax`.
pub fn validate_syntax_diff(
    original: &str,
    new_content: &str,
    extension: &str,
    touched_old_lines: &[(i64, i64)],
    touched_new_lines: &[(i64, i64)],
) -> Option<bool> {
    let language = language_for_extension(extension)?;
    let new_tree = parse_tree(new_content, language)?;
    let mut new_errors = Vec::new();
    collect_error_line_ranges(new_tree.root_node(), &mut new_errors);
    if new_errors.is_empty() {
        return Some(true);
    }

    // Audit 5.4: a small margin, not the touched range alone -- tree-sitter
    // often attributes a resync-recovery error node to the line immediately
    // adjacent to the actual unmatched construct rather than the exact
    // touched line. Deliberately narrow (1, not e.g. 3): real code usually
    // has enough spacing between unrelated symbols that a wider margin
    // would start swallowing genuinely distant, unrelated pre-existing
    // errors into "must be caused by this edit".
    const RESYNC_MARGIN: i64 = 1;
    let new_zone: Vec<(i64, i64)> = touched_new_lines
        .iter()
        .map(|&(s, e)| (s - RESYNC_MARGIN, e + RESYNC_MARGIN))
        .collect();
    if !new_zone.is_empty() && new_errors.iter().any(|r| intersects_any(r, &new_zone)) {
        return Some(false);
    }

    let original_errors_all = parse_tree(original, language).map(|t| {
        let mut v = Vec::new();
        collect_error_line_ranges(t.root_node(), &mut v);
        v
    });
    let Some(original_errors) = original_errors_all else {
        // No parseable original tree to compare against -- fall back to
        // the pre-fix global comparison (against zero, since there's no
        // baseline) rather than guessing.
        return Some(new_errors.is_empty());
    };

    if new_zone.is_empty() {
        // No hunk position info supplied -- preserve the old, coarser
        // global-count behavior exactly.
        return Some(new_errors.len() <= original_errors.len());
    }
    let old_zone: Vec<(i64, i64)> = touched_old_lines
        .iter()
        .map(|&(s, e)| (s - RESYNC_MARGIN, e + RESYNC_MARGIN))
        .collect();
    let new_outside = new_errors
        .iter()
        .filter(|r| !intersects_any(r, &new_zone))
        .count();
    let original_outside = original_errors
        .iter()
        .filter(|r| !intersects_any(r, &old_zone))
        .count();
    Some(new_outside <= original_outside)
}

/// Write `content` to `path` atomically: write to a temp file in the same
/// directory, then `rename()` over the target. A concurrent reader (the
/// file watcher's `notify` handler, an editor, `search_grep`) can never
/// observe a half-written file — `rename` within one filesystem is atomic,
/// unlike a direct `fs::write` which truncates-then-writes in place.
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    atomic_write_with(path, content, WriteAssurance::Fast)
}

/// Controls how `atomic_write_with` treats a failure that doesn't affect the
/// written content itself — currently just permission preservation.
/// `Fast` (what `atomic_write` uses) keeps the original best-effort
/// behavior: a permission-preservation failure never fails a write whose
/// content already landed correctly (see `atomic_write`'s doc comment,
/// audit F5). `HighAssurance` surfaces that same failure as an `Err`
/// instead of silently dropping it — for callers that need to know when
/// metadata was lost rather than finding out later. See
/// docs/plans/2026-08-02-phase1-p0-execution-plan.md §3.2; WS-1's
/// transaction commit path is the first planned `HighAssurance` caller,
/// nothing calls it yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAssurance {
    Fast,
    HighAssurance,
}

/// Same atomic-rename contract as `atomic_write` (temp file in the same
/// directory, then `rename()` over the target — never a half-written file
/// visible to a concurrent reader), plus fixes for audit A06 applied
/// unconditionally regardless of `assurance`:
/// - the temp file name carries a random nonce instead of `process::id()`
///   — a reused PID, or two edits racing in the same process, could
///   otherwise collide on the same temp path.
/// - the temp file is opened with `create_new(true)` (`O_EXCL` on every
///   std-supported platform), so a name collision is a loud retry instead
///   of silently truncating another in-flight write's temp file.
/// - on Unix, the parent directory is fsync'd after `rename()` succeeds —
///   `sync_all` on the temp file only durably persists the file's
///   *content*; the new name→inode link itself needs the directory fsync'd
///   too. No-op on non-Unix targets: there is no portable directory-fsync
///   via `std`.
///
/// `assurance` changes exactly one thing: what happens when permission
/// preservation fails after the content write already succeeded. `Fast`
/// matches `atomic_write`'s existing behavior byte-for-byte; `HighAssurance`
/// surfaces it as an `Err` (and removes the orphaned temp file) instead.
pub fn atomic_write_with(
    path: &Path,
    content: &str,
    assurance: WriteAssurance,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Captured before the write so a set_permissions failure below can
    // never make this function fail a write that already succeeded
    // content-wise (Fast mode) — same rationale as the original
    // `atomic_write` (audit F5).
    let original_perms = std::fs::metadata(path).ok().map(|m| m.permissions());

    const MAX_NONCE_RETRIES: u32 = 8;
    let (tmp_path, mut file) = {
        let mut last_err = None;
        let mut created = None;
        for _ in 0..MAX_NONCE_RETRIES {
            let candidate = dir.join(format!(".{file_name}.ci-edit-{}.tmp", write_nonce()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => {
                    created = Some((candidate, f));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    last_err = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        match created {
            Some(pair) => pair,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "atomic_write_with: exhausted nonce retries for temp file name",
                    )
                }));
            }
        }
    };

    let write_result = (|| -> std::io::Result<()> {
        std::io::Write::write_all(&mut file, content.as_bytes())?;
        file.sync_all()
    })();
    drop(file);
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Some(perms) = original_perms
        && let Err(e) = std::fs::set_permissions(&tmp_path, perms)
        && assurance == WriteAssurance::HighAssurance
    {
        // Fast falls through here and stays best-effort, matching the
        // original atomic_write exactly.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    std::fs::rename(&tmp_path, path)?;
    fsync_parent_dir(dir);
    Ok(())
}

/// fsync's `dir` itself so a just-`rename()`d name→inode link is durable,
/// not just the renamed file's content. Best-effort: an `Err` here (e.g.
/// permission denied opening the directory) must never fail a write whose
/// content and rename already succeeded — it only weakens the durability
/// guarantee for that one write, the same trade-off `atomic_write` already
/// makes for permission preservation in `Fast` mode.
#[cfg(unix)]
fn fsync_parent_dir(dir: &Path) {
    if let Ok(dir_file) = std::fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }
}

#[cfg(not(unix))]
fn fsync_parent_dir(_dir: &Path) {
    // No portable directory-fsync via std on non-Unix targets.
}

/// Best-effort-unique suffix for a temp file name: a process-local
/// monotonic counter combined with wall-clock nanoseconds and the PID.
/// Not a CSPRNG nonce — doesn't need to be, since uniqueness (not
/// unpredictability) is the only property `atomic_write_with` relies on,
/// and `create_new(true)` turns any residual collision into a loud retry
/// rather than silent data loss.
fn write_nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}-{counter:x}", std::process::id())
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
    fn test_with_line_gutters_numbers_from_first_line() {
        // Absolute numbering starts at `first_line`, tab-separated, matching
        // the `<n>\t<line>` shape a native file read emits.
        let body = "fn foo() {\n    bar();\n}";
        assert_eq!(
            with_line_gutters(body, 64),
            "64\tfn foo() {\n65\t    bar();\n66\t}"
        );
        // Empty body renders to empty (no phantom "1\t" line).
        assert_eq!(with_line_gutters("", 5), "");
        // A blank interior line still gets its own number.
        assert_eq!(with_line_gutters("a\n\nb", 1), "1\ta\n2\t\n3\tb");
        // Single line.
        assert_eq!(with_line_gutters("solo", 42), "42\tsolo");
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
    fn test_replacing_one_function_with_multiple_functions_parses_cleanly() {
        // Repro for the reported "PARSE_ERROR false positive when replacing
        // 1 function with N functions via edit_lines/edit_symbol"
        // (2026-07-13 session, crates/calm-cli/src/main.rs split of
        // write_mcp_config into calm_entry + write_mcp_config +
        // write_mcp_config_entry). Checks whether apply_hunks + validate_syntax
        // correctly accept a hash-matched hunk whose new_text holds several
        // top-level functions instead of exactly one.
        let original =
            "fn before() {}\n\nfn old_impl(x: i32) -> i32 {\n    x + 1\n}\n\nfn after() {}\n";
        let old_text = "fn old_impl(x: i32) -> i32 {\n    x + 1\n}\n";
        let old_hash = hash_content(old_text);
        let new_text = "fn helper_a() -> i32 {\n    1\n}\n\nfn helper_b(x: i32) -> i32 {\n    x + helper_a()\n}\n\nfn old_impl(x: i32) -> i32 {\n    helper_b(x)\n}\n";
        let outcome = apply_hunks(
            original,
            &[HunkRequest {
                start_line: 3,
                end_line: 5,
                expected_hash: Some(old_hash),
                new_text: new_text.to_string(),
            }],
        )
        .unwrap();
        let new_content = outcome.new_content.expect("hash matched, should apply");
        assert_eq!(
            validate_syntax(&new_content, "rs"),
            Some(true),
            "new_content was:\n{new_content}"
        );
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

    /// Audit 5.4 core regression: the OLD `new_errors <= original_errors`
    /// count comparison is gameable -- an edit that leaves the TOUCHED line
    /// just as broken as before (a different invalid construct, not a fix)
    /// still passes if an unrelated pre-existing error elsewhere keeps the
    /// total count identical. Two broken functions; the edit touches only
    /// `g`'s line and replaces it with a DIFFERENT still-broken line, while
    /// `f`'s pre-existing error is untouched -- total count stays at 2
    /// either way, but the touched line itself is still broken.
    #[test]
    fn test_validate_syntax_diff_rejects_a_new_error_in_the_touched_region_even_at_equal_count() {
        let original = "def f(:\n    pass\n\ndef g(:\n    pass\n";
        let new_content = "def f(:\n    pass\n\ndef h(:\n    pass\n";
        // Sanity: both have exactly 2 errors (f's and g's/h's) -- the old
        // count-only check would see 2 <= 2 and pass.
        assert_eq!(
            validate_syntax_diff(original, new_content, "py", &[], &[]),
            Some(true),
            "sanity check: old global-count behavior (no hunk positions) must still pass"
        );
        // The edit touched line 4 (old) / line 4 (new, same line count) --
        // still broken there, so must now be rejected.
        assert_eq!(
            validate_syntax_diff(original, new_content, "py", &[(4, 4)], &[(4, 4)]),
            Some(false),
            "an error surviving inside the touched region must reject, even at equal total count"
        );
    }

    /// Audit 5.4 (no regression): editing one broken function to be clean
    /// while a DIFFERENT, untouched broken function elsewhere is left alone
    /// must still pass -- the fix must not become overly conservative.
    #[test]
    fn test_validate_syntax_diff_still_passes_when_touched_region_is_genuinely_fixed() {
        let original = "def f(:\n    pass\n\ndef g(:\n    pass\n";
        let new_content = "def f(:\n    pass\n\ndef g():\n    pass\n";
        assert_eq!(
            validate_syntax_diff(original, new_content, "py", &[(4, 4)], &[(4, 4)]),
            Some(true),
            "a genuinely fixed touched region with an untouched pre-existing error elsewhere \
             must still pass"
        );
    }

    /// Audit 5.4: a brand new error strictly OUTSIDE the touched region
    /// (and its resync margin) still uses the count comparison, and a
    /// strict increase there must still reject -- the span check narrows
    /// what's rejected outright, it doesn't just turn the whole function
    /// into "was the touched region clean".
    #[test]
    fn test_validate_syntax_diff_rejects_new_error_outside_touched_region_when_count_increases() {
        let original = "def f():\n    pass\n\ndef g():\n    pass\n";
        // Touch line 4 (a clean edit, g stays valid) but ALSO corrupt f's
        // untouched line 1 in the same new_content -- simulates a caller
        // passing a new_content that changed more than the hunk it
        // declared (or a second, undeclared change slipping in).
        let new_content = "def f(:\n    pass\n\ndef g():\n    pass\n";
        assert_eq!(
            validate_syntax_diff(original, new_content, "py", &[(4, 4)], &[(4, 4)]),
            Some(false),
            "a new error outside the touched region that increases the outside count must \
             still reject"
        );
    }

    /// Audit 5.4: empty `touched_*_lines` (a caller with no hunk-position
    /// info yet) preserves the original coarse global-count behavior
    /// exactly, rather than becoming stricter or panicking.
    #[test]
    fn test_validate_syntax_diff_empty_touched_lines_falls_back_to_global_count() {
        assert_eq!(
            validate_syntax_diff(
                "def f():\n    pass\n",
                "def f(:\n    pass\n",
                "py",
                &[],
                &[]
            ),
            Some(false),
            "introducing an error with zero pre-existing errors must still reject under the \
             fallback path"
        );
        assert_eq!(
            validate_syntax_diff(
                "def f(:\n    pass\n",
                "def f():\n    pass\n",
                "py",
                &[],
                &[]
            ),
            Some(true),
            "fixing the only error with zero hunk-position info must still pass under the \
             fallback path"
        );
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
    fn atomic_write_with_fast_and_high_assurance_both_round_trip() {
        for (label, assurance) in [
            ("fast", WriteAssurance::Fast),
            ("high_assurance", WriteAssurance::HighAssurance),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "ci_edit_atomic_with_{label}_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("f.txt");
            std::fs::write(&path, "old\n").unwrap();

            atomic_write_with(&path, "new content\n", assurance).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "new content\n",
                "{label} mode must write the new content"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn atomic_write_with_leaves_no_orphaned_temp_files_after_success() {
        let dir =
            std::env::temp_dir().join(format!("ci_edit_atomic_with_notemp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "old\n").unwrap();

        atomic_write_with(&path, "new\n", WriteAssurance::Fast).unwrap();

        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "f.txt")
            .collect();
        assert!(
            leftover.is_empty(),
            "no temp/nonce file should survive a successful write, found: {leftover:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_with_concurrent_writes_to_distinct_paths_all_succeed() {
        // Exercises the random-nonce + create_new(O_EXCL) retry path under
        // real concurrency (not just single-threaded sequential calls) —
        // if two threads ever raced onto the same temp name without the
        // retry loop handling it, one of these would fail instead of
        // silently corrupting the other's write.
        let dir = std::env::temp_dir().join(format!(
            "ci_edit_atomic_with_concurrent_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let path = dir.join(format!("f{i}.txt"));
                    let content = format!("content-{i}\n");
                    atomic_write_with(&path, &content, WriteAssurance::Fast).unwrap();
                    (path, content)
                })
            })
            .collect();

        for handle in handles {
            let (path, expected) = handle.join().unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn atomic_write_high_assurance_preserves_permissions_like_fast() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "ci_edit_atomic_with_perms_ha_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        atomic_write_with(
            &path,
            "#!/bin/sh\necho new\n",
            WriteAssurance::HighAssurance,
        )
        .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "HighAssurance must preserve permissions on the success path exactly like Fast"
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
    fn test_insertion_hunk_before_sandwiches_between_leading_doc_comment_and_symbol() {
        // Deeper-dig finding for the reported "PARSE_ERROR false positive
        // when replacing 1 function with N functions" (2026-07-13 main.rs
        // session): `insertion_hunk`'s `Before` position anchors at
        // `line_start`, which for any indexed symbol is the item's OWN
        // line (see walk_symbols, crates/calm-core/src/indexer/parser.rs:
        // 587 — `node.start_position().row + 1`, the raw tree-sitter node
        // span). A leading `///` doc comment is a separate sibling node,
        // never folded into that span. So `edit_symbol(position="before")`
        // on a symbol that has its own doc comment lands the new content
        // BETWEEN the doc comment and the symbol, not above the doc
        // comment — the doc comment silently ends up describing whatever
        // was just inserted, not the symbol it was written for. Always
        // syntactically valid (doc comments precede anything), so this
        // can never trigger PARSE_ERROR itself — it's a real but
        // low-severity documentation-association gap, not a correctness
        // bug, and does not on its own explain the original PARSE_ERROR.
        let src = "/// Old doc for write_mcp_config.\nfn write_mcp_config() {}\n";
        let h = insertion_hunk(src, 2, 2, InsertPosition::Before, "fn calm_entry() {}").unwrap();
        let out = apply_hunks(src, &[h]).unwrap();
        assert_eq!(
            out.new_content.unwrap(),
            "/// Old doc for write_mcp_config.\nfn calm_entry() {}\nfn write_mcp_config() {}\n",
            "the old doc comment now sits above calm_entry, not write_mcp_config"
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
