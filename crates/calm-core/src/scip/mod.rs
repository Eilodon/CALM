pub mod cache;
pub mod ingest;
pub mod parse;
pub mod provider;
pub mod runner;

use std::path::Path;

use rusqlite::Connection;

use crate::config::{RustConfig, ScipConfig};

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
/// Caches on (rust-analyzer version, active toolchain fingerprint, Cargo.lock
/// hash, Cargo.toml hash, `dirty`): an unchanged toolchain, dependency set,
/// edition/workspace shape, and Rust source state means a re-run would find
/// the same call graph, so the (comparatively expensive) rust-analyzer pass
/// is skipped and the previous upgrades — already persisted as
/// `formal`/`ruled_out_by_scip` in the DB — stand. `dirty` is the caller's
/// current Rust-source fingerprint (see `rust_source_dirty_keys`) — pass it
/// so a source-only change (no lockfile/toolchain difference, e.g. editing a
/// function body) still invalidates the cache instead of silently standing
/// forever; an empty slice degrades to the old (lockfile/toolchain-only) key,
/// which remains safe on its own because it can only widen a "skip" into a
/// "run", never the reverse.
pub fn run_overlay_for(
    provider: &provider::ScipProvider,
    conn: &Connection,
    root: &Path,
    sub_root: &Path,
    cfg: &ScipConfig,
    dirty: &[String],
) -> anyhow::Result<ingest::IngestStats> {
    if cfg.enabled == Some(false) {
        return Ok(ingest::IngestStats::default());
    }
    let Some(bin) = (provider.resolve_binary)(cfg.binary.as_deref(), root) else {
        if cfg.enabled == Some(true) {
            tracing::info!(
                "SCIP overlay enabled but no {} indexer found — skipping",
                provider.lang
            );
        }
        return Ok(ingest::IngestStats::default());
    };

    let cache_path = root.join(".calm").join(provider.cache_file_name);
    let key = (provider.cache_key)(&bin, root, dirty);
    if std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key) {
        tracing::info!(
            "SCIP overlay ({}): cache key unchanged, skipping indexer run",
            provider.lang
        );
        return Ok(ingest::IngestStats::default());
    }

    let tmp = tempfile::Builder::new().suffix(".scip").tempfile()?;
    if let Err(e) = runner::run_indexer(provider, &bin, root, tmp.path()) {
        tracing::warn!(
            "SCIP overlay ({}) run failed, keeping syntactic graph: {e}",
            provider.lang
        );
        return Ok(ingest::IngestStats::default());
    }
    let occ = match parse::parse_scip_file(tmp.path(), sub_root) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("SCIP parse failed ({}): {e}", provider.lang);
            return Ok(ingest::IngestStats::default());
        }
    };
    let insert_missing = cfg.insert_missing != Some(false);
    let stats = ingest::ingest_occurrences(conn, &occ, insert_missing)?;
    tracing::info!(
        "SCIP overlay ({}): {} edges upgraded to formal, {} fan-out siblings ruled out, \
         {} edges inserted, match_rate={:.2}",
        provider.lang,
        stats.upgraded,
        stats.ruled_out,
        stats.inserted,
        stats.match_rate
    );
    // Best-effort: a failed cache write just means the next run pays the cost
    // of re-running this provider's indexer again, never a correctness issue.
    let _ = std::fs::write(&cache_path, &key);
    // Best-effort sidecar so `indexing_status`/`overlay_status` can surface
    // this run's `inserted`/`match_rate` without re-running the overlay —
    // those two fields aren't derivable from `call_edges` alone the way
    // `available`/`up_to_date` are (there's no column recording "how many
    // SCIP-resolved sites exist" once the pass is done). Stands until the
    // next real (non-cache-skip) run overwrites it; reading code should
    // treat it as "as of the last real run", not "live". Shared across
    // providers for now (single filename) — fine while `RUST` is the only
    // entry in the table; Phase 2 may need to key this per-language too.
    let stats_path = root.join(".calm").join("scip-stats.json");
    let _ = std::fs::write(
        &stats_path,
        serde_json::json!({
            "upgraded": stats.upgraded,
            "ruled_out": stats.ruled_out,
            "inserted": stats.inserted,
            "match_rate": stats.match_rate,
        })
        .to_string(),
    );
    Ok(stats)
}

/// Run the full SCIP overlay for Rust — thin wrapper around
/// `run_overlay_for(&provider::RUST, ...)`. Kept as its own function (rather
/// than inlining the `RustConfig` unwrap at every call site) because all 3
/// production callers (`lib.rs`, `watcher.rs`, `main.rs`) already call this
/// exact signature; changing it would touch 3 files for zero behavior gain.
/// See `run_overlay_for`'s doc comment for the actual contract (fail-silent,
/// caching, three-state `enabled`).
pub fn run_overlay(
    conn: &Connection,
    root: &Path,
    rust: &RustConfig,
    dirty: &[String],
) -> anyhow::Result<ingest::IngestStats> {
    run_overlay_for(
        &provider::RUST,
        conn,
        root,
        Path::new(""),
        &rust.scip,
        dirty,
    )
}
/// Fingerprint of every currently-indexed file's content for one or more
/// `file_index.language` values, for `run_overlay_for`'s `dirty` parameter —
/// one `"path@hash"` entry per file (`hash` already computed by the indexer,
/// so this is a cheap read, no re-hashing). Changes whenever any matching
/// file's content differs from what was indexed at the last successful
/// overlay run, regardless of whether the lockfile/toolchain also changed —
/// see `run_overlay_for`'s doc comment for why that matters. `langs` lets a
/// future multi-language provider (e.g. a combined C/C++ one) scope this to
/// more than one `file_index.language` value at once.
pub fn source_dirty_keys(conn: &Connection, langs: &[&str]) -> Vec<String> {
    if langs.is_empty() {
        return Vec::new();
    }
    let placeholders = langs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT path, hash FROM file_index WHERE language IN ({placeholders}) ORDER BY path"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params_from_iter(langs.iter()), |r| {
        Ok(format!(
            "{}@{}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Rust-only convenience wrapper around `source_dirty_keys` — all existing
/// callers (`lib.rs`, `watcher.rs`, `main.rs`, this module's own tests) call
/// this exact name; kept so P0.4 touches zero call sites.
pub fn rust_source_dirty_keys(conn: &Connection) -> Vec<String> {
    source_dirty_keys(conn, &["rust"])
}

/// Cheap, non-invoking snapshot of the overlay's readiness — never spawns an
/// external indexer, just checks binary presence and compares the cache key
/// that `run_overlay_for` would compute against what's already on disk.
/// Backs `indexing_status`'s `scip_overlay` field so an agent can tell
/// whether the call graph for currently-edited files has actually been
/// upgraded by SCIP yet, without waiting on or triggering a real run. `None`
/// when `cfg.enabled == Some(false)` — overlay is off, nothing to report.
pub fn overlay_status_for(
    provider: &provider::ScipProvider,
    conn: &Connection,
    root: &Path,
    cfg: &ScipConfig,
) -> Option<OverlayStatus> {
    if cfg.enabled == Some(false) {
        return None;
    }
    let bin = (provider.resolve_binary)(cfg.binary.as_deref(), root);
    let available = bin.is_some();
    let up_to_date = match &bin {
        Some(bin) => {
            let dirty = source_dirty_keys(conn, &[provider.lang]);
            let key = (provider.cache_key)(bin, root, &dirty);
            let cache_path = root.join(".calm").join(provider.cache_file_name);
            std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key)
        }
        None => false,
    };
    let (last_match_rate, last_inserted) = read_last_stats(root);
    Some(OverlayStatus {
        available,
        up_to_date,
        last_match_rate,
        last_inserted,
    })
}

/// Rust-only convenience wrapper around `overlay_status_for` — kept so the
/// one production caller (`recover.rs`'s `indexing_status`) doesn't need to
/// know about `ScipProvider` yet.
pub fn overlay_status(conn: &Connection, root: &Path, rust: &RustConfig) -> Option<OverlayStatus> {
    overlay_status_for(&provider::RUST, conn, root, &rust.scip)
}

/// Best-effort read of the sidecar `run_overlay` writes after a real
/// (non-cache-skip) run — `inserted`/`match_rate` aren't derivable from
/// `call_edges` alone at read time the way `available`/`up_to_date` are, so
/// they have to come from whatever the last real run actually observed.
/// Absent (never run) or corrupt — both `(None, None)`, not an error; this is
/// a diagnostic nicety, not load-bearing.
fn read_last_stats(root: &Path) -> (Option<f64>, Option<usize>) {
    let path = root.join(".calm").join("scip-stats.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (None, None);
    };
    (
        v.get("match_rate").and_then(|x| x.as_f64()),
        v.get("inserted")
            .and_then(|x| x.as_u64())
            .map(|n| n as usize),
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayStatus {
    /// `rust-analyzer` binary was found (PATH/rustup/VS Code) at last check.
    pub available: bool,
    /// The current Rust source fingerprint + toolchain + lockfile match the
    /// last successful overlay run's cache key — `false` means the next
    /// `run_overlay` call (or the next non-noop incremental reindex, if
    /// wired to call it) would actually invoke rust-analyzer again rather
    /// than cache-skip. Always `false` when `available` is `false`.
    pub up_to_date: bool,
    /// `IngestStats::match_rate` from the last real (non-cache-skip)
    /// `run_overlay` invocation, or `None` if it has never actually run
    /// (a fresh checkout, or `available == false`). Stale the moment
    /// `up_to_date` is `false` — read that first.
    pub last_match_rate: Option<f64>,
    /// `IngestStats::inserted` from that same last real run, or `None` for
    /// the same reasons as `last_match_rate`.
    pub last_inserted: Option<usize>,
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
                insert_missing: None,
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
                insert_missing: None,
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
                insert_missing: None,
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
