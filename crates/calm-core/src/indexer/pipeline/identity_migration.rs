//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 9, final slice): behavior-preserving extraction from
//! `pipeline.rs` (issue #67 hotspot). The D4 CallSite-identity migration: the
//! predicate that detects a database still on the pre-D4 (line-only) identity
//! scheme, the diagnostic-status recorder, and the one-time full-baseline
//! reparse that upgrades it. Move-only -- no logic changed, only relocated.
//!
//! Same sibling-module wrinkle as slice 8: `driver.rs` (a sibling of this
//! new `identity_migration` module, not an ancestor) calls
//! `needs_call_site_identity_baseline`/`rebuild_call_site_identity_baseline`
//! from `reindex_changed_cancellable`/`reindex_paths` -- both are
//! `pub(super)` here, and `pipeline.rs` pulls them back in via a plain `use
//! identity_migration::{...}` that `driver.rs`'s existing `use super::{...}`
//! block already reaches unchanged (same ancestor-reexport-cascade pattern
//! confirmed working in slice 8 -- verified via callers() before this move:
//! real callers of both are exactly `driver.rs::reindex_changed_cancellable`/
//! `reindex_paths`, 2 call sites each).
//! `record_call_site_identity_migration_status` stays plain private -- its
//! only caller, `rebuild_call_site_identity_baseline`, moves into this same
//! file.
//!
//! Reverse dependency: `rebuild_call_site_identity_baseline` itself calls
//! `run_indexing_pipeline_cancellable`, which slice 7 already moved into
//! `driver.rs` (`pub` there) -- pulled in via `use super::driver::
//! run_indexing_pipeline_cancellable`, along with `PipelineOutcome`/
//! `ReindexOutcome` (also `pub` in `driver.rs` since slice 7).
//! `ReindexSummary`/`GraphMode`/`now_secs` stay defined in `pipeline.rs`
//! (not moved) -- pulled in via `super::` as usual.
//!
//! After this slice, `pipeline.rs`'s PR#7 split is complete: the file holds
//! only shared struct/type/const definitions, the 9 `mod` + import/
//! re-export blocks, `now_secs`/`signature_returns_option_or_result`, and
//! the untouched `#[cfg(test)] mod tests` block.

use rusqlite::Connection;
use std::path::Path;

use super::driver::{PipelineOutcome, ReindexOutcome, run_indexing_pipeline_cancellable};
use super::{GraphMode, ReindexSummary, now_secs};

/// Whether an existing database still contains CallSites whose line-only
/// identity predates D4. Incremental indexing cannot repair these rows because
/// their file hashes are unchanged, so it must take the full transactional
/// baseline path instead of reporting a no-op.
pub(super) fn needs_call_site_identity_baseline(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM call_sites
             WHERE identity_version < 2
                OR callee_start_byte IS NULL
                OR callee_end_byte IS NULL
         )",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
}

/// Update D4's diagnostic-only migration status.  It is intentionally outside
/// the graph transaction: a failed/cancelled baseline must preserve the old
/// graph while still leaving a useful reason for operators.
fn record_call_site_identity_migration_status(
    conn: &Connection,
    status: &str,
    failure_reason: Option<&str>,
    metrics: Option<(i64, i64, i64, Option<i64>)>,
) -> rusqlite::Result<()> {
    let now = now_secs();
    let (started_at, completed_at, failed_at) = match status {
        "running" => (Some(now), None, None),
        "baseline_ready" => (None, Some(now), None),
        "failed" => (None, None, Some(now)),
        _ => (None, None, None),
    };
    let (duration_ms, rows_rebuilt, busy_retries, graph_generation) = metrics
        .map(
            |(duration_ms, rows_rebuilt, busy_retries, graph_generation)| {
                (
                    Some(duration_ms),
                    Some(rows_rebuilt),
                    Some(busy_retries),
                    graph_generation,
                )
            },
        )
        .unwrap_or((None, None, None, None));
    conn.execute(
        "INSERT INTO identity_migration_state
            (id, target_version, status, started_at, completed_at, failed_at, failure_reason,
             duration_ms, rows_rebuilt, busy_retries, graph_generation)
         VALUES (1, 2, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             target_version = excluded.target_version,
             status = excluded.status,
             started_at = CASE WHEN excluded.status = 'running'
                               THEN excluded.started_at
                               ELSE identity_migration_state.started_at END,
             completed_at = excluded.completed_at,
             failed_at = excluded.failed_at,
             failure_reason = excluded.failure_reason,
             duration_ms = COALESCE(excluded.duration_ms, identity_migration_state.duration_ms),
             rows_rebuilt = COALESCE(excluded.rows_rebuilt, identity_migration_state.rows_rebuilt),
             busy_retries = COALESCE(excluded.busy_retries, identity_migration_state.busy_retries),
             graph_generation = COALESCE(excluded.graph_generation, identity_migration_state.graph_generation)",
        rusqlite::params![
            status,
            started_at,
            completed_at,
            failed_at,
            failure_reason,
            duration_ms,
            rows_rebuilt,
            busy_retries,
            graph_generation,
        ],
    )?;
    Ok(())
}

/// Run the one-time D4 baseline through the normal full-pipeline transaction.
/// Both incremental entry points must share this path: an unchanged source hash
/// cannot prove its persisted CallSite identity is current.
pub(super) fn rebuild_call_site_identity_baseline(
    conn: &mut Connection,
    project_root: &Path,
    cancel: &dyn Fn() -> bool,
) -> rusqlite::Result<ReindexOutcome> {
    let started = std::time::Instant::now();
    tracing::info!("D4 CallSite identity migration detected — forcing a full baseline reparse");
    let phase = std::sync::Arc::new(std::sync::RwLock::new(crate::types::IndexingPhase::Parsing));
    if let Err(error) = record_call_site_identity_migration_status(conn, "running", None, None) {
        tracing::warn!(%error, "could not record D4 CallSite identity migration start");
    }

    match run_indexing_pipeline_cancellable(conn, project_root, phase, cancel) {
        Ok(PipelineOutcome::Completed) => {
            let rebuilt_files: usize =
                conn.query_row("SELECT COUNT(*) FROM file_index", [], |row| {
                    row.get::<_, i64>(0)
                })? as usize;
            if let Err(error) = record_call_site_identity_migration_status(
                conn,
                "baseline_ready",
                None,
                Some((
                    started.elapsed().as_millis().try_into().unwrap_or(i64::MAX),
                    rebuilt_files.try_into().unwrap_or(i64::MAX),
                    0,
                    conn.query_row(
                        "SELECT generation FROM graph_generation_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok(),
                )),
            ) {
                tracing::warn!(%error, "could not record D4 CallSite identity migration completion");
            }
            Ok(ReindexOutcome::Completed(ReindexSummary {
                changed: rebuilt_files,
                graph_mode: GraphMode::FullFallback("call_site_identity_v2".to_string()),
                ..ReindexSummary::default()
            }))
        }
        Ok(PipelineOutcome::Cancelled) => {
            if let Err(error) = record_call_site_identity_migration_status(
                conn,
                "failed",
                Some("baseline cancelled"),
                Some((
                    started.elapsed().as_millis().try_into().unwrap_or(i64::MAX),
                    0,
                    0,
                    conn.query_row(
                        "SELECT generation FROM graph_generation_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok(),
                )),
            ) {
                tracing::warn!(%error, "could not record cancelled D4 CallSite identity migration");
            }
            Ok(ReindexOutcome::Cancelled)
        }
        Err(error) => {
            let failure_reason = error.to_string();
            if let Err(status_error) = record_call_site_identity_migration_status(
                conn,
                "failed",
                Some(&failure_reason),
                Some((
                    started.elapsed().as_millis().try_into().unwrap_or(i64::MAX),
                    0,
                    0,
                    conn.query_row(
                        "SELECT generation FROM graph_generation_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok(),
                )),
            ) {
                tracing::warn!(%status_error, "could not record failed D4 CallSite identity migration");
            }
            Err(error)
        }
    }
}
