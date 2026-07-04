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
/// Caches on (rust-analyzer version, Cargo.lock hash): an unchanged toolchain
/// and dependency set means a re-run would find the same call graph, so the
/// (comparatively expensive) rust-analyzer pass is skipped and the previous
/// upgrades — already persisted as `formal`/`ruled_out_by_scip` in the DB —
/// stand. Per-file dirty tracking isn't wired here (would need a change-set
/// the caller doesn't have at this point); this key alone is safe because it
/// can only widen a "skip" into a "run" (any lockfile/toolchain difference
/// invalidates it), never the reverse.
pub fn run_overlay(
    conn: &Connection,
    root: &Path,
    rust: &RustConfig,
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

    let cache_path = root.join(".codeindex").join("scip.cache");
    let key = cache::overlay_cache_key(&runner::binary_version(&bin), &lockfile_hash(root), &[]);
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
            run_overlay(&conn, Path::new("."), &rust).unwrap(),
            ingest::IngestStats::default()
        );
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
        let stats = run_overlay(&conn, &fixture, &rust).unwrap();
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
