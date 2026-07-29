//! DB-backed replacement for the old `.calm/<provider>.cache` +
//! `.calm/<provider>-stats.json` sidecar files (2026-07-28 benchmark
//! root-cause, B4).
//!
//! A sidecar file outlives the database it describes: deleting or rebuilding
//! `.calm/index.db` while leaving a stale `.calm/scip-ts.cache` behind made
//! `run_overlay_for`'s cache-key check believe an overlay had already run
//! for the fresh DB and skip it — permanently, since nothing ever
//! invalidated the leftover file. Confirmed live while re-indexing the
//! `express` corpus for a benchmark: scip-typescript was newly available,
//! but `calm index` reported "cache key unchanged, skipping indexer run"
//! against a brand-new, zero-SCIP-edge database.
//!
//! Storing this same state as a row in the database the overlay itself
//! writes to makes that class of bug structurally impossible: wiping the DB
//! wipes the cache-key/stats state with it, by construction, with no
//! separate file to remember to delete.

use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayRunState {
    pub cache_key: Option<String>,
    pub upgraded: usize,
    pub ruled_out: usize,
    pub inserted: usize,
    pub match_rate: f64,
    pub last_run_unix: Option<u64>,
}

/// The cache key recorded by this provider's last real (non-skip) run, or
/// `None` if it has never run against this database — same "never run"
/// meaning a missing sidecar file used to carry.
pub fn read_cache_key(conn: &Connection, provider_lang: &str) -> Option<String> {
    conn.query_row(
        "SELECT cache_key FROM scip_overlay_state WHERE provider = ?1",
        params![provider_lang],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// Full last-run state for `overlay_status_for`/`policy_allows_automatic_run`
/// — `None` if this provider has never run against this database.
pub fn read_state(conn: &Connection, provider_lang: &str) -> Option<OverlayRunState> {
    conn.query_row(
        "SELECT cache_key, upgraded, ruled_out, inserted, match_rate, last_run_unix \
         FROM scip_overlay_state WHERE provider = ?1",
        params![provider_lang],
        |r| {
            Ok(OverlayRunState {
                cache_key: r.get(0)?,
                upgraded: r.get::<_, i64>(1)? as usize,
                ruled_out: r.get::<_, i64>(2)? as usize,
                inserted: r.get::<_, i64>(3)? as usize,
                match_rate: r.get(4)?,
                last_run_unix: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            })
        },
    )
    .optional()
    .unwrap_or(None)
}

/// Record a real (non-skip) overlay run. Best-effort — same posture as the
/// sidecar-file writes this replaces: a failed write here only costs the
/// next run a redundant re-index, never a correctness issue, so errors are
/// swallowed rather than propagated.
#[allow(clippy::too_many_arguments)]
pub fn write_state(
    conn: &Connection,
    provider_lang: &str,
    cache_key: &str,
    upgraded: usize,
    ruled_out: usize,
    inserted: usize,
    match_rate: f64,
) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = conn.execute(
        "INSERT INTO scip_overlay_state \
            (provider, cache_key, upgraded, ruled_out, inserted, match_rate, last_run_unix) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(provider) DO UPDATE SET \
            cache_key = excluded.cache_key, \
            upgraded = excluded.upgraded, \
            ruled_out = excluded.ruled_out, \
            inserted = excluded.inserted, \
            match_rate = excluded.match_rate, \
            last_run_unix = excluded.last_run_unix",
        params![
            provider_lang,
            cache_key,
            upgraded as i64,
            ruled_out as i64,
            inserted as i64,
            match_rate,
            now_unix as i64,
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn read_cache_key_none_when_never_run() {
        let conn = db();
        assert_eq!(read_cache_key(&conn, "rust"), None);
        assert_eq!(read_state(&conn, "rust"), None);
    }

    #[test]
    fn write_then_read_round_trips() {
        let conn = db();
        write_state(&conn, "rust", "key-abc", 5, 2, 3, 0.75);
        assert_eq!(read_cache_key(&conn, "rust"), Some("key-abc".to_string()));
        let state = read_state(&conn, "rust").unwrap();
        assert_eq!(state.cache_key.as_deref(), Some("key-abc"));
        assert_eq!(state.upgraded, 5);
        assert_eq!(state.ruled_out, 2);
        assert_eq!(state.inserted, 3);
        assert_eq!(state.match_rate, 0.75);
        assert!(state.last_run_unix.is_some());
    }

    #[test]
    fn second_write_overwrites_not_duplicates() {
        let conn = db();
        write_state(&conn, "rust", "key-1", 1, 0, 0, 0.1);
        write_state(&conn, "rust", "key-2", 9, 9, 9, 0.9);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scip_overlay_state WHERE provider = 'rust'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "must upsert, not accumulate rows per provider");
        assert_eq!(read_cache_key(&conn, "rust"), Some("key-2".to_string()));
    }

    #[test]
    fn providers_are_independent() {
        let conn = db();
        write_state(&conn, "rust", "rust-key", 1, 0, 0, 0.5);
        write_state(&conn, "go", "go-key", 2, 0, 0, 0.6);
        assert_eq!(read_cache_key(&conn, "rust"), Some("rust-key".to_string()));
        assert_eq!(read_cache_key(&conn, "go"), Some("go-key".to_string()));
    }

    #[test]
    // The exact bug this module fixes (B4): a fresh/rebuilt DB (new
    // in-memory connection, same as `index.db` being deleted and
    // recreated) has no `scip_overlay_state` row for a provider even if an
    // old `.calm/*.cache` sidecar file from a PRIOR database still exists
    // on disk somewhere — there is nothing here for a leftover file to
    // desynchronize from, because the state lives in the DB itself.
    fn fresh_database_has_no_stale_state() {
        let conn = db();
        assert_eq!(
            read_cache_key(&conn, "typescript"),
            None,
            "a brand-new database must never look up-to-date for a provider \
             that has never actually run against it"
        );
    }
}
