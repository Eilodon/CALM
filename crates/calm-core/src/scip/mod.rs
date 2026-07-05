pub mod cache;
pub mod ingest;
pub mod parse;
pub mod runner;

use std::path::Path;

use rusqlite::Connection;

use crate::config::RustConfig;

/// Run the full SCIP overlay: detect rust-analyzer, run batch scip into a temp
/// file, parse, and upgrade edges. Fail-silent — every failure mode (disabled,
/// no binary, timeout, parse error) returns `Ok(IngestStats::default())`,
/// leaving the syntactic graph untouched.
///
/// `rust.scip.enabled` is three-state (see `ScipConfig`): `Some(false)` skips
/// without even probing for a binary; unset (`None`, the default) or
/// `Some(true)` both probe for `rust-analyzer` and run if found — the only
/// difference is `Some(true)` logs once when the probe comes up empty (the
/// user explicitly asked, so a no-op is worth explaining), while unset stays
/// silent (finding nothing is the common, expected case for a checkout that
/// never configured this at all, not worth a log line every session).
///
/// Caches on (rust-analyzer version, Cargo.lock hash, `dirty`): an unchanged
/// toolchain, dependency set, and Rust source state means a re-run would find
/// the same call graph, so the (comparatively expensive) rust-analyzer pass
/// is skipped and the previous upgrades — already persisted as
/// `formal`/`ruled_out_by_scip` in the DB — stand. `dirty` is the caller's
/// current Rust-source fingerprint (see `rust_source_dirty_keys`) — pass it
/// so a source-only change (no lockfile/toolchain difference, e.g. editing a
/// function body) still invalidates the cache instead of silently standing
/// forever; an empty slice degrades to the old (lockfile/toolchain-only) key,
/// which remains safe on its own because it can only widen a "skip" into a
/// "run", never the reverse.
pub fn run_overlay(
    conn: &Connection,
    root: &Path,
    rust: &RustConfig,
    dirty: &[String],
) -> anyhow::Result<ingest::IngestStats> {
    if rust.scip.enabled == Some(false) {
        return Ok(ingest::IngestStats::default());
    }
    let Some(bin) = runner::resolve_binary(rust.scip.binary.as_deref()) else {
        if rust.scip.enabled == Some(true) {
            tracing::info!("SCIP overlay enabled but no rust-analyzer found — skipping");
        }
        return Ok(ingest::IngestStats::default());
    };

    let cache_path = root.join(".calm").join("scip.cache");
    let key = cache::overlay_cache_key(&runner::binary_version(&bin), &lockfile_hash(root), dirty);
    if std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key) {
        tracing::info!("SCIP overlay: cache key unchanged, skipping rust-analyzer run");
        return Ok(ingest::IngestStats::default());
    }

    let tmp = tempfile::Builder::new().suffix(".scip").tempfile()?;
    if let Err(e) = runner::run_scip(&bin, root, tmp.path()) {
        tracing::warn!("SCIP overlay run failed, keeping syntactic graph: {e}");
        return Ok(ingest::IngestStats::default());
    }
    let occ = match parse::parse_scip_file(tmp.path()) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("SCIP parse failed: {e}");
            return Ok(ingest::IngestStats::default());
        }
    };
    let stats = ingest::ingest_occurrences(conn, &occ)?;
    tracing::info!(
        "SCIP overlay: {} edges upgraded to formal, {} fan-out siblings ruled out",
        stats.upgraded,
        stats.ruled_out
    );
    // Best-effort: a failed cache write just means the next run pays the cost
    // of rust-analyzer again, never a correctness issue.
    let _ = std::fs::write(&cache_path, &key);
    Ok(stats)
}

/// Hash of `Cargo.lock`'s contents, or `""` when absent (e.g. a virtual
/// workspace without a checked-in lockfile) — a stable "no lockfile" key that
/// still changes the moment one appears.
fn lockfile_hash(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.lock"))
        .map(|s| crate::indexer::pipeline::hash_content(&s))
        .unwrap_or_default()
}

/// Fingerprint of every currently-indexed Rust file's content, for
/// `run_overlay`'s `dirty` parameter — one `"path@hash"` entry per file
/// (`hash` already computed by the indexer, so this is a cheap read, no
/// re-hashing). Changes whenever any Rust file's content differs from what
/// was indexed at the last successful overlay run, regardless of whether
/// `Cargo.lock` or the rust-analyzer version also changed — see
/// `run_overlay`'s doc comment for why that matters.
pub fn rust_source_dirty_keys(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn
        .prepare("SELECT path, hash FROM file_index WHERE language = 'rust' ORDER BY path")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| {
        Ok(format!(
            "{}@{}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Cheap, non-invoking snapshot of the overlay's readiness — never spawns
/// rust-analyzer, just checks binary presence and compares the cache key that
/// `run_overlay` would compute against what's already on disk. Backs
/// `indexing_status`'s `scip_overlay` field so an agent can tell whether the
/// call graph for currently-edited Rust files has actually been upgraded by
/// SCIP yet, without waiting on or triggering a real run. `None` when
/// `rust.scip.enabled == Some(false)` — overlay is off, nothing to report.
pub fn overlay_status(conn: &Connection, root: &Path, rust: &RustConfig) -> Option<OverlayStatus> {
    if rust.scip.enabled == Some(false) {
        return None;
    }
    let bin = runner::resolve_binary(rust.scip.binary.as_deref());
    let available = bin.is_some();
    let up_to_date = match &bin {
        Some(bin) => {
            let dirty = rust_source_dirty_keys(conn);
            let key = cache::overlay_cache_key(
                &runner::binary_version(bin),
                &lockfile_hash(root),
                &dirty,
            );
            let cache_path = root.join(".calm").join("scip.cache");
            std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key)
        }
        None => false,
    };
    Some(OverlayStatus {
        available,
        up_to_date,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayStatus {
    /// `rust-analyzer` binary was found (PATH/rustup/VS Code) at last check.
    pub available: bool,
    /// The current Rust source fingerprint + toolchain + lockfile match the
    /// last successful overlay run's cache key — `false` means the next
    /// `run_overlay` call (or the next non-noop incremental reindex, if
    /// wired to call it) would actually invoke rust-analyzer again rather
    /// than cache-skip. Always `false` when `available` is `false`.
    pub up_to_date: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Some(false)` (explicit force-off) must be a no-op regardless of what's
    /// actually on this machine's `PATH` — unlike unset/auto-detect, this
    /// path skips before ever probing for a binary, so it's safe to assert
    /// deterministically even on a dev box with rust-analyzer installed (this
    /// one is; see `runner::tests::detect_returns_none_when_binary_absent`
    /// for why the unset/auto-detect case can't be tested the same way).
    #[test]
    fn explicit_off_is_a_noop_even_when_rust_analyzer_is_on_path() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let rust = RustConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
            },
        };
        assert_eq!(
            run_overlay(&conn, Path::new("."), &rust, &[]).unwrap(),
            ingest::IngestStats::default()
        );
    }

    #[test]
    fn rust_source_dirty_keys_reflects_path_and_hash_rust_only() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, 0.0)",
            rusqlite::params!["src/a.rs", "hashA", "rust"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, 0.0)",
            rusqlite::params!["src/main.py", "hashP", "python"],
        )
        .unwrap();

        let keys = rust_source_dirty_keys(&conn);
        assert_eq!(
            keys,
            vec!["src/a.rs@hashA".to_string()],
            "must include only rust files, keyed by path+hash"
        );
    }

    #[test]
    fn rust_source_dirty_keys_changes_when_a_file_hash_changes() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('src/a.rs', 'hash1', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let before = rust_source_dirty_keys(&conn);

        conn.execute(
            "UPDATE file_index SET hash = 'hash2' WHERE path = 'src/a.rs'",
            [],
        )
        .unwrap();
        let after = rust_source_dirty_keys(&conn);

        assert_ne!(
            before, after,
            "editing a rust file's content must change its dirty-key entry"
        );
    }

    #[test]
    fn overlay_status_none_when_explicitly_disabled() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let rust = RustConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
            },
        };
        assert_eq!(overlay_status(&conn, Path::new("."), &rust), None);
    }

    /// Live integration: real rust-analyzer against the Rust fixture workspace
    /// used throughout Phase A. Ignored by default -- requires rust-analyzer
    /// on PATH/rustup/VS Code, and a real `cargo metadata` resolve, neither of
    /// which CI is guaranteed to have for this opt-in feature.
    #[test]
    #[ignore]
    fn overlay_upgrades_a_real_edge_on_the_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust_workspace");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let rust = RustConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
            },
        };
        let dirty = rust_source_dirty_keys(&conn);
        let stats = run_overlay(&conn, &fixture, &rust, &dirty).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'app/src/main.rs::main' \
                   AND to_symbol = 'core/src/engine.rs::Engine::start'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }
}
