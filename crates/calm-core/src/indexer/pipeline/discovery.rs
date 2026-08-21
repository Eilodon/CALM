//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 first slice): behavior-preserving extraction from `pipeline.rs`
//! (issue #67 hotspot). File discovery + the small content/path/hash
//! primitives every indexing entry point shares. Move-only -- no logic
//! changed, only relocated. `pub(super)` (not plain private) on the four
//! internal helpers because Rust's module privacy is "visible in the
//! defining module and its descendants" -- `pipeline.rs` is this module's
//! ANCESTOR, not a descendant, so plain `fn` would be invisible to it.
//! `hash_content`/`collect_source_files` stay `pub` and are re-exported by
//! `pipeline.rs` unchanged, since both have real external callers reaching
//! them via the stable `crate::indexer::pipeline::hash_content` /
//! `calm_core::indexer::pipeline::collect_source_files` paths (verified via
//! `callers()` before this move: hash_content has 48, collect_source_files
//! has 3, one of which is outside calm-core entirely).

use std::path::{Path, PathBuf};

use crate::indexer::lang_constants::{is_recognized_unparsed_extension, language_for_extension};

use super::MAX_INDEXABLE_FILE_BYTES;

/// `std::fs::read_to_string`, but skipping the read entirely for a file over
/// `MAX_INDEXABLE_FILE_BYTES` (checked via a cheap `metadata()` stat, not by
/// reading the file first) -- the shared choke point for all three
/// indexing entry points below (full reindex, changed-file reindex,
/// targeted path reindex) so the cap can't be forgotten on any one path.
/// `std::fs::read_to_string`, but skipping the read entirely for a file over
/// `MAX_INDEXABLE_FILE_BYTES` (checked via a cheap `metadata()` stat, not by
/// reading the file first) -- the shared choke point for all three
/// indexing entry points below (full reindex, changed-file reindex,
/// targeted path reindex) so the cap can't be forgotten on any one path.
///
/// Returns `Err(reason)` instead of `None` (truth-kernel-hardening plan,
/// Wave 4 item 4.1b) so a caller can persist *why* a file was skipped, not
/// just *that* it was: `"too_large:<byte len>"` for the cap, or
/// `"unreadable:<io::ErrorKind Debug repr>"` for a metadata/read failure
/// (permission denied, TOCTOU delete, a non-UTF-8 write mid-flight --
/// `read_to_string` itself reports invalid UTF-8 as `io::ErrorKind::
/// InvalidData`, so that case is already distinguishable via `kind` without
/// a dedicated variant).
pub(super) fn read_source_capped(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("unreadable:{:?}", e.kind()))?;
    let len = meta.len();
    if len > MAX_INDEXABLE_FILE_BYTES {
        return Err(format!("too_large:{len}"));
    }
    std::fs::read_to_string(path).map_err(|e| format!("unreadable:{:?}", e.kind()))
}

/// Records why a file was skipped this pass (`file_index.skip_reason`)
/// without disturbing whatever was already known about it -- `hash`/
/// `symbol_count`/`language`/`mtime` from a prior successful index (if any)
/// are left exactly as they were, since nothing was actually re-extracted
/// this pass. A file skipped before it was ever successfully indexed gets a
/// placeholder row (empty hash, `symbol_count` 0, `language` NULL, `mtime`
/// NULL) purely so its `skip_reason` is discoverable (`fitness_report`) --
/// a successful `upsert_file_index` afterwards always clears `skip_reason`
/// back to NULL, since `INSERT OR REPLACE` deletes the old row first and
/// the fresh INSERT there never lists this column.
pub(super) fn mark_file_index_skip_reason(
    tx: &rusqlite::Transaction,
    rel: &str,
    skip_reason: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO file_index (path, hash, language, symbol_count, last_indexed, mtime, skip_reason) \
         VALUES (?1, '', NULL, 0, ?2, NULL, ?3) \
         ON CONFLICT(path) DO UPDATE SET skip_reason = excluded.skip_reason, last_indexed = excluded.last_indexed",
        rusqlite::params![rel, now, skip_reason],
    )?;
    Ok(())
}

/// Collect tier-0 source files under `root` via the shared `crate::walk`
/// walker (built-in `IGNORE_DIRS`, dot-directories, user-configured `ignore`
/// patterns, and real `.gitignore`), filtered down to extensions
/// `language_for_extension` recognizes. Deterministic order is imposed by
/// the caller.
pub fn collect_source_files(root: &Path, ignore: &[String], out: &mut Vec<PathBuf>) {
    for result in crate::walk::build_walker(root, ignore, false) {
        let Ok(entry) = result else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && (language_for_extension(ext).is_some() || is_recognized_unparsed_extension(ext))
        {
            out.push(path);
        }
    }
}

/// Portable FNV-1a 64-bit hash. `DefaultHasher` is explicitly *not* stable
/// across Rust versions/platforms per the std docs — using it for the
/// persisted `file_index.hash` column meant a toolchain upgrade could
/// invalidate every cached hash and force a full re-parse. FNV-1a has a
/// fixed, documented algorithm so the same content always hashes the same
/// way regardless of toolchain. `pub` so `crate::edit` can reuse it as the
/// same stale-write conflict guard for arbitrary line ranges.
pub fn hash_content(s: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET_BASIS;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

pub(crate) fn mtime_secs(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Relative path of `file` under `project_root`, normalised to forward slashes.
pub(super) fn rel_path(project_root: &Path, file: &Path) -> String {
    file.strip_prefix(project_root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `lang` is `None` for a recognized-but-unparsed extension (see
/// `is_recognized_unparsed_extension`) — persisted as SQL `NULL`, matching
/// `file_index.language`'s nullable column.
pub(super) fn upsert_file_index(
    tx: &rusqlite::Transaction,
    rel: &str,
    lang: Option<&str>,
    hash: &str,
    mtime: f64,
    symbol_count: usize,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO file_index (path, hash, language, symbol_count, last_indexed, mtime) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![rel, hash, lang, symbol_count as i64, now, mtime],
    )?;
    Ok(())
}
