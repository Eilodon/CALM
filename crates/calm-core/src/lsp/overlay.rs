//! The DB-driven overlay pass itself, in three strictly separated phases:
//!
//! 1. **DB read** (caller thread, sync): load candidate edges + the source
//!    text of their files, then let go of the connection.
//! 2. **LSP session** (dedicated OS thread, its own single-threaded tokio
//!    runtime): spawn the provider's server, resolve each call site, return
//!    plain data. A dedicated thread — NOT an inline `block_on` — because
//!    the MCP tool that calls this already runs on the server's ambient
//!    tokio runtime, where a nested `block_on` panics ("Cannot start a
//!    runtime from within a runtime"; reproduced 2026-07-10). The thread
//!    boundary also keeps `rusqlite::Connection` (`!Sync`) entirely out of
//!    async code.
//! 3. **DB write** (caller thread, sync): re-verify each hit is still an
//!    upgradable row and apply it, counting rows actually changed — a
//!    concurrent `rebuild_graph` (`DELETE FROM call_edges` + reinsert) makes
//!    snapshot ids stale, so `to_upgrade.len()` would over-report.
//!
//! Generalized (D.0, 2026-07-11) from a Rust-only pass into a table-driven
//! one over `LspProvider` (`lsp::provider`) — same shape/reasoning as
//! `scip::provider::ScipProvider` — so `resolve_binary`, the candidate-edge
//! language filter, and the `MinInterval` sidecar are all per-provider
//! instead of hardcoded to rust-analyzer.
//!
//! Fail-silent like `scip::run_overlay_for`: every failure mode returns
//! `Ok(LspIngestStats::default())`, leaving the graph untouched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{Connection, Transaction};

use crate::config::LspConfig;
use crate::lsp::client::{
    DefinitionOutcome, LspClient, LspClientProfile, PositionEncoding, path_to_uri, uri_to_path,
};
use crate::lsp::provider::LspProvider;

/// Bounds every individual LSP request round-trip. Live probe data
/// (2026-07-10, rust-analyzer 1.96 on the `rust_workspace` fixture): replies
/// stall up to ~4s while initial indexing runs, so this must comfortably
/// exceed that; after warm-up, replies are milliseconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds the whole LSP phase (spawn → last definition). ADR-0004 proposed
/// 60s for an enrichment pass; the probe showed ~5.4s cold-start on a tiny
/// fixture, so a CALM-sized repo plausibly needs 30-60s of warm-up alone —
/// 180s keeps the hard-cap guarantee without starving the first real run.
const PASS_BUDGET: Duration = Duration::from_secs(180);
/// How long the warm-up loop keeps re-asking before concluding the server
/// is as ready as it will get (see `resolve_all_on_thread`).
const WARMUP_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LspRunStatus {
    #[default]
    NotRun,
    Disabled,
    NoMatchingFiles,
    AutomaticDenied,
    Unavailable,
    FingerprintUnavailable,
    NoCandidates,
    Failed,
    Cancelled,
    Stale,
    Succeeded,
}

impl LspRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Disabled => "disabled",
            Self::NoMatchingFiles => "no_matching_files",
            Self::AutomaticDenied => "automatic_denied",
            Self::Unavailable => "unavailable",
            Self::FingerprintUnavailable => "fingerprint_unavailable",
            Self::NoCandidates => "no_candidates",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Stale => "stale",
            Self::Succeeded => "succeeded",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LspIngestStats {
    /// Edges whose row was actually changed to `formal`/`formal_source='lsp'`
    /// this run — counted from `UPDATE` rowcounts, not from resolution hits,
    /// so ids gone stale under a concurrent rebuild don't inflate it.
    pub upgraded: usize,
    /// Call sites actually queried against the live server.
    pub attempted: usize,
    /// `upgraded / attempted` (0.0 when nothing was attempted).
    pub match_rate: f64,
    pub status: LspRunStatus,
}

impl LspIngestStats {
    fn with_status(status: LspRunStatus) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }
}

/// One `call_edges` row eligible for LSP resolution: not yet formal, not
/// already ruled out by SCIP's exact evidence (241 of 1013 otherwise-eligible
/// rows on the 2026-07-10 self-repo measurement — re-querying those wastes
/// round-trips, and a divergent answer would let LSP contradict SCIP's
/// stronger verdict), and carrying a current exact CallSite identity.
struct CandidateEdge {
    id: i64,
    call_site_id: i64,
    from_path: String,
    callee_start_byte: i64,
    callee_end_byte: i64,
    source_file_hash: String,
    to_symbol: String,
}

/// A resolution the LSP phase produced, pending phase-3 verification.
struct ResolvedSite {
    edge_id: i64,
    call_site_id: i64,
    callee_start_byte: i64,
    callee_end_byte: i64,
    source_file_hash: String,
    to_symbol: String,
    def_uri: lsp_types::Uri,
    def_line_zero_based: u32,
}

/// One mutable LSP provider slot. A proof baseline is the tuple of provider
/// and resolution-context fingerprints; its workspace is normalized so two
/// equivalent relative roots cannot run competing sessions.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LspRunKey {
    workspace: PathBuf,
    provider: String,
}

impl LspRunKey {
    fn new(root: &Path, provider: &str) -> Self {
        Self {
            workspace: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            provider: provider.to_owned(),
        }
    }
}

#[derive(Clone)]
struct QueuedLspGeneration {
    generation: String,
    sequence: u64,
    waiters: usize,
}

#[derive(Clone)]
struct LspRunState {
    generation: String,
    cancelled: Arc<AtomicBool>,
    running: bool,
    current_waiters: usize,
    result: Option<Result<LspIngestStats, String>>,
    queued: Option<QueuedLspGeneration>,
    obsolete_queued_waiters: usize,
    next_sequence: u64,
    latest_sequence: u64,
}

/// In-process coordinator for resolve-time LSP overlays. It deliberately owns
/// no database connection: the leader keeps that `!Sync` resource on its
/// caller thread, while same-baseline callers join the leader's result.
#[derive(Default)]
struct LspRunCoordinator {
    slots: Mutex<HashMap<LspRunKey, LspRunState>>,
    changed: Condvar,
}

impl LspRunCoordinator {
    fn run<F>(&self, key: LspRunKey, generation: &str, task: F) -> anyhow::Result<LspIngestStats>
    where
        F: FnOnce(Arc<AtomicBool>) -> anyhow::Result<LspIngestStats>,
    {
        let generation = generation.to_owned();
        let mut task = Some(task);
        let mut queued_sequence = None;
        loop {
            let mut slots = self.slots.lock().expect("LSP coordinator mutex poisoned");
            match slots.get_mut(&key) {
                Some(state) if state.running && state.generation == generation => {
                    state.current_waiters += 1;
                    while slots.get(&key).is_some_and(|current| current.running) {
                        slots = self
                            .changed
                            .wait(slots)
                            .expect("LSP coordinator mutex poisoned");
                    }
                    let state = slots
                        .get_mut(&key)
                        .expect("leader retains completed result for registered waiters");
                    let result = state
                        .result
                        .clone()
                        .expect("completed LSP run has a result");
                    state.current_waiters -= 1;
                    if state.current_waiters == 0 {
                        if state.queued.is_none() {
                            slots.remove(&key);
                        }
                        self.changed.notify_all();
                    }
                    return result.map_err(anyhow::Error::msg);
                }
                Some(state) if state.running => {
                    // Latest-wins queue: at most one generation is retained
                    // after the active child stops. An older queued caller
                    // observes its sequence has been superseded and returns a
                    // no-op instead of starting a second rerun.
                    if queued_sequence.is_some_and(|sequence| sequence < state.latest_sequence) {
                        return Ok(LspIngestStats::default());
                    }
                    let sequence = match &mut state.queued {
                        Some(queued) if queued.generation == generation => {
                            queued.waiters += 1;
                            queued.sequence
                        }
                        _ => {
                            if let Some(previous) = state.queued.take() {
                                state.obsolete_queued_waiters += previous.waiters;
                            }
                            state.next_sequence += 1;
                            let sequence = state.next_sequence;
                            state.latest_sequence = sequence;
                            state.queued = Some(QueuedLspGeneration {
                                generation: generation.clone(),
                                sequence,
                                waiters: 1,
                            });
                            state.cancelled.store(true, Ordering::SeqCst);
                            sequence
                        }
                    };
                    queued_sequence = Some(sequence);
                    slots = self
                        .changed
                        .wait(slots)
                        .expect("LSP coordinator mutex poisoned");
                    drop(slots);
                    continue;
                }
                Some(state) if state.current_waiters > 0 => {
                    // A completed run stays available until every actual
                    // same-baseline joiner has received its result. Pending
                    // newer work cannot steal that hand-off state.
                    slots = self
                        .changed
                        .wait(slots)
                        .expect("LSP coordinator mutex poisoned");
                    drop(slots);
                    continue;
                }
                Some(state) if state.queued.is_some() => {
                    let queued = state.queued.as_ref().expect("guard checked queued");
                    if queued.generation != generation || queued_sequence != Some(queued.sequence) {
                        // This request was queued before a newer baseline.
                        // It must not run after the newer baseline is known.
                        state.obsolete_queued_waiters = state
                            .obsolete_queued_waiters
                            .checked_sub(1)
                            .expect("every superseded queued request is counted");
                        self.changed.notify_all();
                        return Ok(LspIngestStats::default());
                    }
                    if state.obsolete_queued_waiters > 0 {
                        slots = self
                            .changed
                            .wait(slots)
                            .expect("LSP coordinator mutex poisoned");
                        drop(slots);
                        continue;
                    }
                    let cancelled = Arc::new(AtomicBool::new(false));
                    state.generation = generation.clone();
                    state.cancelled = Arc::clone(&cancelled);
                    state.running = true;
                    state.result = None;
                    state.queued = None;
                    drop(slots);
                    return self.finish_leader(
                        key,
                        cancelled,
                        task.take().expect("task runs once"),
                    );
                }
                Some(_) => {
                    // A new explicit refresh after a completed baseline is a
                    // new run, never a cached result.
                    slots.remove(&key);
                    continue;
                }
                None => {
                    let cancelled = Arc::new(AtomicBool::new(false));
                    slots.insert(
                        key.clone(),
                        LspRunState {
                            generation: generation.clone(),
                            cancelled: Arc::clone(&cancelled),
                            running: true,
                            current_waiters: 0,
                            result: None,
                            queued: None,
                            obsolete_queued_waiters: 0,
                            next_sequence: 0,
                            latest_sequence: 0,
                        },
                    );
                    drop(slots);
                    return self.finish_leader(
                        key,
                        cancelled,
                        task.take().expect("task runs once"),
                    );
                }
            }
        }
    }

    fn finish_leader<F>(
        &self,
        key: LspRunKey,
        cancelled: Arc<AtomicBool>,
        task: F,
    ) -> anyhow::Result<LspIngestStats>
    where
        F: FnOnce(Arc<AtomicBool>) -> anyhow::Result<LspIngestStats>,
    {
        let outcome = task(cancelled).map_err(|error| error.to_string());
        let mut slots = self.slots.lock().expect("LSP coordinator mutex poisoned");
        if let Some(state) = slots.get_mut(&key) {
            state.running = false;
            state.result = Some(outcome.clone());
            if state.current_waiters == 0 && state.queued.is_none() {
                slots.remove(&key);
            }
        }
        self.changed.notify_all();
        outcome.map_err(anyhow::Error::msg)
    }

    /// Runs the short phase-3 database transaction only while this leader is
    /// still current. Holding the coordinator lock prevents a newer baseline
    /// from setting its cancellation flag between this check and the commit.
    /// The lock is deliberately held only for the local SQLite transaction,
    /// never for the LSP session or provenance probe.
    fn commit_if_current<T>(
        &self,
        key: &LspRunKey,
        generation: &str,
        cancelled: &Arc<AtomicBool>,
        commit: impl FnOnce() -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Option<T>> {
        let slots = self.slots.lock().expect("LSP coordinator mutex poisoned");
        let Some(state) = slots.get(key) else {
            return Ok(None);
        };
        if !state.running
            || state.generation != generation
            || state.cancelled.load(Ordering::SeqCst)
            || !Arc::ptr_eq(&state.cancelled, cancelled)
        {
            return Ok(None);
        }
        commit().map(Some)
    }

    #[cfg(test)]
    fn waiting_count(&self, key: &LspRunKey) -> usize {
        self.slots
            .lock()
            .expect("LSP coordinator mutex poisoned")
            .get(key)
            .map_or(0, |state| state.current_waiters)
    }

    #[cfg(test)]
    fn queued_generation(&self, key: &LspRunKey) -> Option<String> {
        self.slots
            .lock()
            .expect("LSP coordinator mutex poisoned")
            .get(key)
            .and_then(|state| state.queued.as_ref())
            .map(|queued| queued.generation.clone())
    }
}

fn lsp_run_coordinator() -> &'static LspRunCoordinator {
    static COORDINATOR: OnceLock<LspRunCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(LspRunCoordinator::default)
}

/// Run the LSP resolve-time overlay for one `provider` — the `lsp_refresh`
/// MCP tool's `force: true` entry point (via `lsp::refresh_language`). D4 keeps
/// this resolver strictly on-demand: a `force: false` caller is always a no-op,
/// regardless of a legacy automatic policy value in configuration.
pub fn run_lsp_overlay(
    conn: &Connection,
    root: &Path,
    provider: &LspProvider,
    cfg: &LspConfig,
    force: bool,
) -> anyhow::Result<LspIngestStats> {
    if cfg.enabled == Some(false) {
        return Ok(LspIngestStats::with_status(LspRunStatus::Disabled));
    }
    // Cheap DB check before ever probing for a binary or spawning anything —
    // same reasoning as scip::run_overlay_for's provider_has_any_files gate.
    if !has_any_lang_files(conn, provider.langs) {
        return Ok(LspIngestStats::with_status(LspRunStatus::NoMatchingFiles));
    }
    if !force {
        tracing::info!(
            "LSP overlay ({}): automatic runs are disabled by D4 — \
             use the `lsp_refresh` MCP tool",
            provider.name
        );
        return Ok(LspIngestStats::with_status(LspRunStatus::AutomaticDenied));
    }
    let Some(bin) = (provider.resolve_binary)(cfg.binary.as_deref(), root) else {
        if cfg.enabled == Some(true) {
            tracing::info!(
                "LSP overlay enabled but no {} found — skipping",
                provider.name
            );
        }
        return Ok(LspIngestStats::with_status(LspRunStatus::Unavailable));
    };

    let Some(proof_context) = lsp_proof_context(provider, &bin, root) else {
        tracing::warn!(
            "LSP overlay ({}) skipped: version probe did not yield an auditable provider fingerprint",
            provider.name
        );
        return Ok(LspIngestStats::with_status(
            LspRunStatus::FingerprintUnavailable,
        ));
    };
    let proof_generation: i64 = conn.query_row(
        "SELECT generation FROM graph_generation_state WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let proof_context = proof_context.at_graph_generation(proof_generation);
    let key = LspRunKey::new(root, provider.name);
    let generation = lsp_run_baseline_key(&proof_context);
    let commit_key = key.clone();
    let commit_generation = generation.clone();
    lsp_run_coordinator().run(key, &generation, move |cancelled| {
        run_lsp_overlay_prepared(
            conn,
            root,
            provider,
            &bin,
            &proof_context,
            &commit_key,
            &commit_generation,
            cancelled,
        )
    })
}

/// Coordinator identity must include the graph generation, not only provider
/// provenance. A rebuilt graph with the same provider/context needs a fresh
/// run; otherwise it would join an obsolete in-flight resolver and never queue
/// the required newer-baseline rerun.
fn lsp_run_baseline_key(context: &crate::scip::ingest::ExternalProofContext) -> String {
    let generation = context.graph_generation.map_or_else(
        || "missing-generation".to_owned(),
        |value| value.to_string(),
    );
    format!(
        "{}\u{0}{}\u{0}{generation}",
        context.provider_fingerprint, context.context_fingerprint
    )
}

enum PhaseThreeOutcome {
    Stale,
    Applied(usize),
}

/// Leader-only portion of an LSP refresh. The coordinator guarantees that no
/// same-baseline peer starts another server, and its cancellation token fences
/// a superseded generation before it can write evidence in phase 3.
fn run_lsp_overlay_prepared(
    conn: &Connection,
    root: &Path,
    provider: &LspProvider,
    bin: &Path,
    proof_context: &crate::scip::ingest::ExternalProofContext,
    key: &LspRunKey,
    generation: &str,
    cancelled: Arc<AtomicBool>,
) -> anyhow::Result<LspIngestStats> {
    // ---- Phase 1: DB read (sync, caller thread) ----
    let rows = load_candidate_edges(conn, provider.langs)?;
    if rows.is_empty() {
        return Ok(LspIngestStats::with_status(LspRunStatus::NoCandidates));
    }
    let proof_binary = bin.to_path_buf();
    let mut by_file: HashMap<String, Vec<CandidateEdge>> = HashMap::new();
    for row in rows {
        by_file.entry(row.from_path.clone()).or_default().push(row);
    }
    // Read file contents up front too: phase 2 then touches nothing but its
    // own inputs, and a file that changed on disk mid-pass can't desync the
    // didOpen text from the lines we compute columns against.
    let mut files: Vec<(PathBuf, String, Vec<CandidateEdge>)> = Vec::with_capacity(by_file.len());
    for (from_path, edges) in by_file {
        let abs = root.join(&from_path);
        match std::fs::read_to_string(&abs) {
            Ok(text)
                if edges.iter().all(|edge| {
                    edge.source_file_hash == crate::indexer::pipeline::hash_content(&text)
                }) =>
            {
                files.push((abs, text, edges))
            }
            Ok(_) => {
                tracing::debug!(
                    path = %from_path,
                    "LSP overlay skipped a file whose disk bytes no longer match the index"
                );
            }
            Err(_) => continue, // deleted/unreadable since indexing — skip
        }
    }

    // ---- Phase 2: LSP session on a dedicated thread ----
    let root_owned = root.to_path_buf();
    let bin_owned = bin.to_path_buf();
    let client_profile = provider.client_profile;
    let cancellation_for_thread = Arc::clone(&cancelled);
    let handle = std::thread::Builder::new()
        .name("calm-lsp-overlay".into())
        .spawn(move || {
            resolve_all_on_thread(
                &bin_owned,
                &root_owned,
                client_profile,
                files,
                cancellation_for_thread,
            )
        })
        .map_err(anyhow::Error::from)?;
    let (resolutions, attempted) = match handle.join() {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            tracing::warn!("LSP overlay run failed, keeping prior graph state: {e}");
            return Ok(LspIngestStats::with_status(LspRunStatus::Failed));
        }
        Err(_) => {
            tracing::warn!("LSP overlay thread panicked, keeping prior graph state");
            return Ok(LspIngestStats::with_status(LspRunStatus::Failed));
        }
    };
    if cancelled.load(Ordering::SeqCst) {
        tracing::info!(
            "LSP overlay ({}) discarded: superseded by a newer baseline before proof write",
            provider.name
        );
        return Ok(LspIngestStats::with_status(LspRunStatus::Cancelled));
    }
    let Some(current_context) = lsp_proof_context(provider, &proof_binary, root) else {
        tracing::warn!(
            "LSP overlay ({}) discarded: provider fingerprint became unavailable during the run",
            provider.name
        );
        return Ok(LspIngestStats::with_status(
            LspRunStatus::FingerprintUnavailable,
        ));
    };
    let context_changed = !current_context.has_same_provenance_as(proof_context);
    // Phase 3 owns both stale-proof invalidation and new proof writes in one
    // transaction, while the coordinator lock makes "current" and commit one
    // atomic decision with respect to a newly queued baseline.
    let phase_three =
        lsp_run_coordinator().commit_if_current(key, generation, &cancelled, || {
            let tx = conn.unchecked_transaction()?;
            if context_changed {
                invalidate_stale_lsp_context_in_tx(&tx, &current_context)?;
                tx.commit()?;
                return Ok(PhaseThreeOutcome::Stale);
            }
            invalidate_stale_lsp_context_in_tx(&tx, proof_context)?;
            let upgraded = apply_lsp_resolutions_in_tx(&tx, root, &resolutions, proof_context)?;
            tx.commit()?;
            Ok(PhaseThreeOutcome::Applied(upgraded))
        })?;
    let Some(phase_three) = phase_three else {
        tracing::info!(
            "LSP overlay ({}) discarded: superseded by a newer baseline before proof write",
            provider.name
        );
        return Ok(LspIngestStats::with_status(LspRunStatus::Cancelled));
    };
    let upgraded = match phase_three {
        PhaseThreeOutcome::Stale => {
            tracing::warn!(
                "LSP overlay ({}) discarded: provider or resolution context changed during the run",
                provider.name
            );
            return Ok(LspIngestStats::with_status(LspRunStatus::Stale));
        }
        PhaseThreeOutcome::Applied(upgraded) => upgraded,
    };
    if upgraded > 0 {
        // Same contract as scip::run_and_refresh: caller_count (and the
        // hub/coreness/dead-code signals derived from it) counts by
        // confidence tier, so flipping ambiguous→formal changes it.
        crate::indexer::pipeline::refresh_caller_counts(conn)?;
    }

    let match_rate = if attempted == 0 {
        0.0
    } else {
        upgraded as f64 / attempted as f64
    };
    let stats = LspIngestStats {
        upgraded,
        attempted,
        match_rate,
        status: LspRunStatus::Succeeded,
    };
    tracing::info!(
        "LSP overlay ({}): {} of {} attempted call sites upgraded to formal (match_rate={:.2})",
        provider.name,
        stats.upgraded,
        stats.attempted,
        stats.match_rate
    );
    write_last_run_stats(root, provider, &stats);
    Ok(stats)
}

/// Applies phase-3 LSP resolutions as one generation-fenced transaction. A
/// stale generation therefore changes neither an edge nor its proof, even when
/// another indexer connection is concurrently rebuilding the graph.
#[cfg(test)]
fn apply_lsp_resolutions(
    conn: &Connection,
    root: &Path,
    resolutions: &[ResolvedSite],
    proof_context: &crate::scip::ingest::ExternalProofContext,
) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let upgraded = apply_lsp_resolutions_in_tx(&tx, root, resolutions, proof_context)?;
    tx.commit()?;
    Ok(upgraded)
}

fn apply_lsp_resolutions_in_tx(
    tx: &Transaction<'_>,
    root: &Path,
    resolutions: &[ResolvedSite],
    proof_context: &crate::scip::ingest::ExternalProofContext,
) -> rusqlite::Result<usize> {
    let upgraded_edge_ids = {
        let mut update = tx.prepare(
            // Re-verify the row is still in an upgradable state: a concurrent
            // rebuild_graph DELETEs+reinserts call_edges with fresh ids, so a
            // stale id must update 0 rows, and an id that survived must still
            // not be formal/ruled-out.
            "UPDATE call_edges AS ce SET edge_confidence = 'formal', formal_source = 'lsp', \
                 evidence_state = 'unverified' \
             WHERE ce.id = ?1 AND ce.call_site_id = ?2 \
               AND ce.edge_confidence IN ('ambiguous', 'textual') \
               AND ce.formal_source IS NULL AND ce.ruled_out_by_scip = 0 \
               AND EXISTS ( \
                   SELECT 1 FROM call_sites cs \
                   JOIN file_index fi ON fi.path = cs.from_path \
                   WHERE cs.id = ce.call_site_id \
                     AND cs.callee_start_byte = ?3 \
                     AND cs.callee_end_byte = ?4 \
                     AND cs.identity_version >= 2 \
                     AND fi.hash = ?5 \
                ) \
                AND EXISTS ( \
                    SELECT 1 FROM graph_generation_state \
                    WHERE id = 1 AND generation = ?6 \
                )",
        )?;
        let mut changed_edge_ids = Vec::new();
        for site in resolutions {
            let Some(def_path) = uri_to_repo_path(&site.def_uri, root) else {
                continue;
            };
            // def_line from LSP is 0-indexed; symbols.line_start is 1-indexed.
            let resolved = crate::scip::ingest::resolve_unique_symbol_at_filtered(
                tx,
                &def_path,
                site.def_line_zero_based as i64 + 1,
                true, // markdown headings are never call targets
            )?;
            if resolved.as_deref() == Some(site.to_symbol.as_str())
                && update.execute(rusqlite::params![
                    site.edge_id,
                    site.call_site_id,
                    site.callee_start_byte,
                    site.callee_end_byte,
                    site.source_file_hash,
                    proof_context.graph_generation,
                ])? > 0
            {
                changed_edge_ids.push(site.edge_id);
            }
        }
        changed_edge_ids
    };
    let mut upgraded = 0usize;
    for edge_id in upgraded_edge_ids {
        crate::scip::ingest::record_external_proof_for_edge(tx, proof_context, edge_id, "lsp")?;
        upgraded += tx.execute(
            "UPDATE call_edges SET evidence_state = 'fresh'
             WHERE id = ?1 AND formal_source = 'lsp' AND evidence_state = 'unverified'",
            [edge_id],
        )?;
    }
    Ok(upgraded)
}

/// Phase 2 entry: builds this thread's own single-threaded runtime (safe —
/// no ambient runtime exists on a fresh OS thread), applies the overall
/// pass budget, and always tears the server down before returning.
fn resolve_all_on_thread(
    bin: &Path,
    root: &Path,
    profile: LspClientProfile,
    files: Vec<(PathBuf, String, Vec<CandidateEdge>)>,
    cancelled: Arc<AtomicBool>,
) -> anyhow::Result<(Vec<ResolvedSite>, usize)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut client = LspClient::spawn_with_profile(bin, root, REQUEST_TIMEOUT, profile).await?;
        let mut resolutions = Vec::new();
        let mut attempted = 0usize;
        // Budget expiry keeps whatever resolved so far — `resolutions` and
        // `attempted` live outside the timed future.
        let run = tokio::time::timeout(
            PASS_BUDGET,
            resolve_loop(
                &mut client,
                root,
                &files,
                &mut resolutions,
                &mut attempted,
                cancelled.as_ref(),
            ),
        )
        .await;
        client.shutdown().await; // every path, including budget expiry
        match run {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("LSP resolve loop ended early: {e}"),
            Err(_) => tracing::info!(
                "LSP overlay pass budget ({PASS_BUDGET:?}) expired — keeping partial results"
            ),
        }
        Ok((resolutions, attempted))
    })
}

async fn resolve_loop(
    client: &mut LspClient,
    root: &Path,
    files: &[(PathBuf, String, Vec<CandidateEdge>)],
    resolutions: &mut Vec<ResolvedSite>,
    attempted: &mut usize,
    cancelled: &AtomicBool,
) -> anyhow::Result<()> {
    // Warm-up: a freshly spawned server answers `null` (not "please retry"!)
    // to definition requests until initial indexing settles — observed live
    // on rust-analyzer: null, null, -32801, then correct, over ~5.4s on a
    // tiny fixture. An early `null` is therefore not authoritative; keep
    // re-asking the first few sites until one resolves or the warm-up budget
    // expires, and only then trust `NotFound` answers.
    let warmup_deadline = tokio::time::Instant::now() + WARMUP_BUDGET;
    let mut warmed_up = false;

    for (abs_path, text, edges) in files {
        if cancelled.load(Ordering::SeqCst) {
            anyhow::bail!("LSP resolution cancelled by a newer baseline");
        }
        let Ok(uri) = path_to_uri(abs_path) else {
            continue;
        };
        if client.open_file(abs_path, &uri, text).await.is_err() {
            continue;
        }
        for edge in edges {
            if cancelled.load(Ordering::SeqCst) {
                anyhow::bail!("LSP resolution cancelled by a newer baseline");
            }
            let Ok(start_byte) = usize::try_from(edge.callee_start_byte) else {
                continue;
            };
            let Some((line_idx, character)) =
                lsp_position_from_call_site_byte(text, start_byte, client.encoding)
            else {
                continue;
            };
            *attempted += 1;
            let mut outcome = client.definition(&uri, line_idx, character).await;
            // Retry loop: -32801 is always "ask again"; during warm-up a
            // `null` is too (see above).
            loop {
                match &outcome {
                    Ok(DefinitionOutcome::Retryable) => {}
                    Ok(DefinitionOutcome::NotFound)
                        if !warmed_up && tokio::time::Instant::now() < warmup_deadline => {}
                    _ => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                outcome = client.definition(&uri, line_idx, character).await;
            }
            match outcome {
                Ok(DefinitionOutcome::Resolved(def_uri, def_line)) => {
                    warmed_up = true;
                    // Only keep in-repo definitions; std/deps can't match a
                    // graph symbol anyway.
                    if uri_to_path(&def_uri).is_some_and(|p| p.starts_with(root)) {
                        resolutions.push(ResolvedSite {
                            edge_id: edge.id,
                            call_site_id: edge.call_site_id,
                            callee_start_byte: edge.callee_start_byte,
                            callee_end_byte: edge.callee_end_byte,
                            source_file_hash: edge.source_file_hash.clone(),
                            to_symbol: edge.to_symbol.clone(),
                            def_uri,
                            def_line_zero_based: def_line,
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    // One hard protocol error (closed pipe, timeout) ends the
                    // pass — later requests would fail identically.
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

/// `langs`-filtered candidate edges: joins `file_index` on `from_path` so a
/// provider only ever sees call sites in files of the languages it claims
/// (`provider.langs`) — a gopls session must never be asked to open a `.rs`
/// file just because that file also happened to have an unresolved edge.
fn load_candidate_edges(conn: &Connection, langs: &[&str]) -> rusqlite::Result<Vec<CandidateEdge>> {
    let placeholders = langs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT ce.id, ce.call_site_id, cs.from_path, cs.callee_start_byte, \
                cs.callee_end_byte, fi.hash, ce.to_symbol \
         FROM call_edges ce \
         JOIN call_sites cs ON cs.id = ce.call_site_id \
         JOIN symbols s ON s.qualified_name = ce.to_symbol \
         JOIN file_index fi ON fi.path = cs.from_path \
         WHERE ce.edge_confidence IN ('ambiguous', 'textual') \
         AND ce.formal_source IS NULL \
         AND ce.ruled_out_by_scip = 0 \
         AND ce.call_site_id IS NOT NULL \
         AND cs.identity_version >= 2 \
         AND cs.callee_start_byte IS NOT NULL \
         AND cs.callee_end_byte IS NOT NULL \
         AND s.kind != 'heading' \
           AND fi.language IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map(rusqlite::params_from_iter(langs.iter()), |r| {
        Ok(CandidateEdge {
            id: r.get(0)?,
            call_site_id: r.get(1)?,
            from_path: r.get(2)?,
            callee_start_byte: r.get(3)?,
            callee_end_byte: r.get(4)?,
            source_file_hash: r.get(5)?,
            to_symbol: r.get(6)?,
        })
    })?
    .collect()
}

/// Whether the project has at least one indexed file in any of `langs` —
/// same idiom (and the same fail-open-on-error posture) as
/// `scip::provider_has_any_files`.
fn has_any_lang_files(conn: &Connection, langs: &[&str]) -> bool {
    if langs.is_empty() {
        return true;
    }
    let placeholders = langs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT EXISTS(SELECT 1 FROM file_index WHERE language IN ({placeholders}))");
    conn.query_row(&sql, rusqlite::params_from_iter(langs.iter()), |r| {
        r.get::<_, i64>(0)
    })
    .map(|n| n != 0)
    .unwrap_or(true) // fail open, same posture as scip::provider_has_any_files
}

/// Hash exactly the reviewed profile inputs beneath `root`. Missing inputs are
/// represented explicitly so creating a manifest/config also invalidates an
/// old proof. Absolute/traversing paths fail closed into the fingerprint rather
/// than allowing a provider profile to read outside the workspace.
fn resolution_context_fingerprint(root: &Path, inputs: &[&str]) -> String {
    let mut entries = Vec::with_capacity(inputs.len());
    for input in inputs {
        let relative = Path::new(input);
        let safe = !relative.is_absolute()
            && !relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir));
        let value = if !safe {
            "<rejected-path>".to_string()
        } else {
            match std::fs::read_to_string(root.join(relative)) {
                Ok(text) => crate::indexer::pipeline::hash_content(&text),
                Err(_) => "<missing>".to_string(),
            }
        };
        entries.push(format!("{input}@{value}"));
    }
    crate::indexer::pipeline::hash_content(&entries.join("\n"))
}

/// Probe the exact executable and snapshot its reviewed resolution inputs
/// before a live LSP run. A failed probe is not evidence and must not produce a
/// fresh proof, even if the server binary happened to be discoverable.
fn lsp_proof_context(
    provider: &LspProvider,
    binary: &Path,
    root: &Path,
) -> Option<crate::scip::ingest::ExternalProofContext> {
    let output = std::process::Command::new(binary)
        .args(provider.version_args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut version = String::from_utf8_lossy(&output.stdout).into_owned();
    version.push_str(&String::from_utf8_lossy(&output.stderr));
    let version = version.trim();
    if version.is_empty() {
        return None;
    }
    Some(crate::scip::ingest::ExternalProofContext::new(
        format!("lsp:{}", provider.name),
        crate::lsp::provider::proof_provider_fingerprint(provider, binary, version),
        resolution_context_fingerprint(root, provider.context_inputs),
    ))
}

/// A changed profile/context invalidates LSP's only formal authority. Reopen
/// those rows as residual textual candidates; the next explicit run may prove
/// them again, while an abandoned refresh cannot leave stale proof presented as
/// current. Other formal sources never match this predicate.
#[cfg(test)]
fn invalidate_stale_lsp_context(
    conn: &Connection,
    context: &crate::scip::ingest::ExternalProofContext,
) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    invalidate_stale_lsp_context_in_tx(&tx, context)?;
    tx.commit()
}

fn invalidate_stale_lsp_context_in_tx(
    tx: &Transaction<'_>,
    context: &crate::scip::ingest::ExternalProofContext,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE external_proofs SET status = 'stale'
         WHERE provider = ?1
           AND status = 'fresh'
           AND (provider_fingerprint != ?2 OR context_fingerprint != ?3)",
        rusqlite::params![
            context.provider,
            context.provider_fingerprint,
            context.context_fingerprint,
        ],
    )?;
    tx.execute(
        "UPDATE call_edges AS ce
         SET edge_confidence = 'textual', formal_source = NULL, evidence_state = 'stale'
         WHERE ce.edge_confidence = 'formal'
           AND ce.formal_source = 'lsp'
           AND EXISTS (
               SELECT 1 FROM external_proofs proof
               WHERE proof.call_site_id = ce.call_site_id
                 AND proof.to_symbol = ce.to_symbol
                 AND proof.provider = ?1
                 AND proof.status = 'stale'
           )",
        [context.provider.as_str()],
    )?;
    Ok(())
}

/// Normalizes a persisted, zero-based UTF-8 byte span into the LSP position
/// negotiated for this request. The byte must be a UTF-8 boundary; CRLF is
/// naturally handled because the current-line prefix starts after `\n`.
fn lsp_position_from_call_site_byte(
    text: &str,
    callee_start_byte: usize,
    encoding: PositionEncoding,
) -> Option<(u32, u32)> {
    let preceding = text.get(..callee_start_byte)?;
    let line_start = preceding.rfind('\n').map_or(0, |offset| offset + 1);
    let line = preceding.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_prefix = text.get(line_start..callee_start_byte)?;
    let character = match encoding {
        PositionEncoding::Utf8 => line_prefix.len() as u32,
        PositionEncoding::Utf16 => line_prefix.encode_utf16().count() as u32,
    };
    Some((line, character))
}

/// `file://` `Uri` -> repo-relative path string, matching the convention
/// `call_edges.from_path`/`symbols.path` are stored in. `None` if `uri`
/// isn't a `file://` URI or doesn't fall under `root`.
fn uri_to_repo_path(uri: &lsp_types::Uri, root: &Path) -> Option<String> {
    // Strip-prefix alone is lexical: `root/../secret.rs` starts with `root`
    // but escapes it after normalization. Canonicalize both existing paths
    // before deriving the DB-relative definition path.
    let root = std::fs::canonicalize(root).ok()?;
    let abs = std::fs::canonicalize(uri_to_path(uri)?).ok()?;
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
fn read_last_run_unix(provider: &LspProvider, root: &Path) -> Option<u64> {
    let path = root.join(".calm").join(provider.stats_file_name);
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("last_run_unix").and_then(|x| x.as_u64())
}

fn write_last_run_stats(root: &Path, provider: &LspProvider, stats: &LspIngestStats) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = root.join(".calm").join(provider.stats_file_name);
    let _ = std::fs::write(
        &path,
        serde_json::json!({
            "upgraded": stats.upgraded,
            "attempted": stats.attempted,
            "match_rate": stats.match_rate,
            "last_run_unix": now_unix,
        })
        .to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RefreshPolicy;
    use crate::lsp::provider;

    #[test]
    fn explicit_off_is_a_noop_even_when_rust_analyzer_is_on_path() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cfg = LspConfig {
            enabled: Some(false),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &provider::RUST_ANALYZER, &cfg, false)
                .unwrap()
                .status,
            LspRunStatus::Disabled
        );
    }

    #[test]
    fn zero_rust_files_is_a_noop_even_when_forced_on() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.py', 'h', 'python', 0.0)",
            [],
        )
        .unwrap();
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &provider::RUST_ANALYZER, &cfg, false)
                .unwrap()
                .status,
            LspRunStatus::NoMatchingFiles
        );
    }

    /// The generalization's own gate: a project with ONLY Rust files must be
    /// a no-op for the GOPLS provider — proves `langs`-based filtering
    /// actually discriminates between providers, not just that the old
    /// Rust-only gate still works.
    #[test]
    fn zero_go_files_is_a_noop_for_gopls_even_with_rust_files_present() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.rs', 'h', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &provider::GOPLS, &cfg, true)
                .unwrap()
                .status,
            LspRunStatus::NoMatchingFiles,
            "gopls must not run against a project with zero .go files"
        );
    }

    /// The single most important behavior this whole feature exists to get
    /// right (see the roadmap's gating requirement): an automatic
    /// (`force: false`) caller under the default `OnDemand` policy must
    /// never even reach the binary probe, regardless of what's on `PATH`.
    #[test]
    fn on_demand_policy_skips_automatic_runs_even_with_candidate_edges() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.rs', 'h', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &provider::RUST_ANALYZER, &cfg, false)
                .unwrap()
                .status,
            LspRunStatus::AutomaticDenied,
            "OnDemand must block an automatic (force=false) run"
        );
    }

    #[test]
    fn automatic_policy_values_cannot_probe_or_run_an_lsp_server() {
        fn panic_if_probed(_: Option<&str>, _: &std::path::Path) -> Option<std::path::PathBuf> {
            panic!("an automatic LSP run reached the binary resolver")
        }

        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) \
             VALUES ('main.rs', 'h', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let provider = provider::LspProvider {
            name: "panic-test-server",
            langs: &["rust"],
            resolve_binary: panic_if_probed,
            version_args: &["--version"],
            context_inputs: &["Cargo.toml"],
            client_profile: provider::RUST_ANALYZER.client_profile,
            stats_file_name: "unused.json",
        };
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnSave,
        };

        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &provider, &cfg, false)
                .unwrap()
                .status,
            LspRunStatus::AutomaticDenied,
        );
    }

    /// Locks the 2026-07-10 review's config finding: `LspConfig::default()`
    /// must agree with the serde default (`OnDemand`) — a derived `Default`
    /// silently resolves to `RefreshPolicy::default()` = `OnSave`, the exact
    /// value the config's own doc comment forbids as a default.
    #[test]
    fn default_policy_is_on_demand_not_on_save() {
        assert_eq!(LspConfig::default().policy, RefreshPolicy::OnDemand);
        assert_eq!(
            crate::config::RustConfig::default().lsp.policy,
            RefreshPolicy::OnDemand,
            "an unconfigured project must land on OnDemand"
        );
        // And the serde path for a config.json that never mentions `lsp`:
        let parsed: crate::config::RustConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.lsp.policy, RefreshPolicy::OnDemand);
    }

    /// The `MinInterval`-sidecar fix this generalization exists to make
    /// safe: two providers running back-to-back must not clobber each
    /// other's last-run timestamp (the pre-generalization code had exactly
    /// one hardcoded `lsp-stats.json` for all of them).
    #[test]
    fn definition_uri_must_resolve_under_the_workspace_root() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("src").join("lib.rs");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "fn inside() {}\n").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();

        assert_eq!(
            uri_to_repo_path(&path_to_uri(&inside).unwrap(), root.path()).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            uri_to_repo_path(&path_to_uri(outside.path()).unwrap(), root.path()),
            None,
            "an LSP definition outside the configured workspace is never trusted"
        );
    }

    #[test]
    fn stats_files_are_provider_specific_not_shared() {
        let dir = std::env::temp_dir().join(format!(
            "calm_lsp_stats_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".calm")).unwrap();

        write_last_run_stats(&dir, &provider::RUST_ANALYZER, &LspIngestStats::default());
        assert!(
            read_last_run_unix(&provider::GOPLS, &dir).is_none(),
            "gopls must not see rust-analyzer's sidecar"
        );
        assert!(read_last_run_unix(&provider::RUST_ANALYZER, &dir).is_some());

        write_last_run_stats(&dir, &provider::GOPLS, &LspIngestStats::default());
        assert!(
            read_last_run_unix(&provider::CLANGD, &dir).is_none(),
            "clangd must not see gopls's sidecar"
        );
        assert!(read_last_run_unix(&provider::GOPLS, &dir).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lsp_position_uses_exact_span_with_unicode_and_crlf() {
        let text = "let café = 1;\r\ncafé(); foo();\r\n";
        let foo_start_byte = text.rfind("foo").unwrap();

        assert_eq!(
            lsp_position_from_call_site_byte(text, foo_start_byte, PositionEncoding::Utf8),
            Some((1, 9)),
        );
        assert_eq!(
            lsp_position_from_call_site_byte(text, foo_start_byte, PositionEncoding::Utf16),
            Some((1, 8)),
        );
    }

    #[test]
    fn lsp_candidates_require_a_current_exact_call_site() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('main.rs', 'source-hash', 'rust', 1, 0);
             INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('target.rs::target', 'target', 'function', 'rust', 'target.rs', 1, 1);
             INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line, edge_kind)
             VALUES ('main.rs', 'main.rs::main', 'target', 2, 'call');
             INSERT INTO call_sites
                 (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                  callee_end_byte, identity_version, edge_kind)
             VALUES ('main.rs', 'main.rs::main', 'target', 2, 14, 20, 2, 'call');
             INSERT INTO call_edges
                 (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
             VALUES
                 ('main.rs::main', 'target.rs::target', 2, 1, 'textual', 'main.rs', 'target.rs', 'call'),
                 ('main.rs::main', 'target.rs::target', 2, 2, 'textual', 'main.rs', 'target.rs', 'call');",
        )
        .unwrap();

        let candidates = load_candidate_edges(&conn, &["rust"]).unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "legacy line-only rows must not reach LSP"
        );
        assert_eq!(candidates[0].call_site_id, 2);
        assert_eq!(candidates[0].callee_start_byte, 14);
        assert_eq!(candidates[0].callee_end_byte, 20);
        assert_eq!(candidates[0].source_file_hash, "source-hash");
    }

    #[test]
    fn stale_lsp_generation_cannot_mutate_edges_or_proofs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let target = root.join("target.rs");
        std::fs::write(&target, "fn target() {}\n").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO file_index (path, hash, language, symbol_count, last_indexed) VALUES
                 ('main.rs', 'source-hash', 'rust', 1, 0),
                 ('target.rs', 'target-hash', 'rust', 1, 0);
             INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
                 VALUES ('target.rs::target', 'target', 'function', 'rust', 'target.rs', 1, 1);
             INSERT INTO call_sites
                 (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                  callee_end_byte, identity_version, edge_kind)
                 VALUES ('main.rs', 'main.rs::main', 'target', 1, 4, 10, 2, 'call');
             INSERT INTO call_edges
                 (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
                 VALUES ('main.rs::main', 'target.rs::target', 1, 1, 'textual', 'main.rs', 'target.rs', 'call');
             UPDATE graph_generation_state SET generation = 2 WHERE id = 1;",
        )
        .unwrap();
        let context =
            crate::scip::ingest::ExternalProofContext::new("lsp:test", "provider", "context")
                .at_graph_generation(1);
        let upgraded = apply_lsp_resolutions(
            &conn,
            root,
            &[ResolvedSite {
                edge_id: 1,
                call_site_id: 1,
                callee_start_byte: 4,
                callee_end_byte: 10,
                source_file_hash: "source-hash".into(),
                to_symbol: "target.rs::target".into(),
                def_uri: path_to_uri(&target).unwrap(),
                def_line_zero_based: 0,
            }],
            &context,
        )
        .unwrap();

        assert_eq!(upgraded, 0);
        assert_eq!(
            conn.query_row(
                "SELECT edge_confidence || ':' || COALESCE(formal_source, '') FROM call_edges WHERE id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "textual:"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM external_proofs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn resolve_symbol_at_picks_narrowest_span_and_skips_headings() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('a.rs::Outer', 'Outer', 'impl', 'rust', 'a.rs', 1, 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('a.rs::Outer::inner', 'inner', 'method', 'rust', 'a.rs', 3, 5)",
            [],
        )
        .unwrap();
        assert_eq!(
            crate::scip::ingest::resolve_unique_symbol_at_filtered(&conn, "a.rs", 4, true).unwrap(),
            Some("a.rs::Outer::inner".to_string())
        );
    }

    /// Live integration: real rust-analyzer against the same fixture the SCIP
    /// overlay's ignored test uses. Ignored by default — needs rust-analyzer
    /// on PATH/rustup/VS Code and a real `cargo metadata` resolve. Exercises
    /// the full three-phase pipeline including warm-up (the fixture takes
    /// ~5s before rust-analyzer answers definitions — see module docs).
    #[test]
    #[ignore]
    fn lsp_overlay_upgrades_a_real_edge_on_the_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust_workspace");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE edge_confidence IN ('ambiguous','textual') \
                 AND formal_source IS NULL AND call_site_line IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(before > 0, "fixture must start with unresolved edges");

        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        let stats = run_lsp_overlay(&conn, &fixture, &provider::RUST_ANALYZER, &cfg, true).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal (attempted={})",
            stats.attempted
        );
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE formal_source = 'lsp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n as usize, stats.upgraded);
    }

    /// Live integration: real `gopls` against `multi_lang_workspace/go`
    /// (Phase D.4, 2026-07-11). Ignored by default — needs `gopls` on PATH
    /// and a real `go build`-capable module (this fixture's own `go.mod`).
    ///
    /// `route`'s type switch (`switch h := v.(type) { case AlphaHandler: ...
    /// case BetaHandler: ... }`) is the Go analogue of Kotlin's `is X ->`
    /// smart cast: CALM's syntactic resolver can't follow the type switch,
    /// so `h.Process()` in both branches lands as `ambiguous` with the same
    /// 2 candidates (`AlphaHandler::Process`/`BetaHandler::Process`) before
    /// this overlay runs. `gopls`'s real `go/types` analysis narrows `h`'s
    /// static type per-branch and resolves each call to its own specific
    /// target, ruling out the other branch's candidate — verified once by
    /// hand before this test was written (indexed the fixture, ran gopls
    /// through the LSP overlay: both ambiguous call sites resolved to the
    /// correct branch-specific target, the wrong cross-pairing at each site
    /// stayed ambiguous).
    #[test]
    #[ignore]
    fn gopls_overlay_upgrades_ambiguous_type_switch_calls_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/go");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before_alpha: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.go::route' \
                   AND to_symbol = 'handlers.go::AlphaHandler::Process' \
                   AND call_site_line = 32",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            before_alpha, "ambiguous",
            "fixture must start ambiguous — CALM's syntactic resolver can't follow a Go type switch"
        );

        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        let stats = run_lsp_overlay(&conn, &fixture, &provider::GOPLS, &cfg, true).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal (attempted={})",
            stats.attempted
        );

        // Line 32 (`case AlphaHandler: return h.Process()`) must resolve to
        // AlphaHandler's own Process, not BetaHandler's.
        let (alpha_conf, alpha_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'handlers.go::route' \
                   AND to_symbol = 'handlers.go::AlphaHandler::Process' \
                   AND call_site_line = 32",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(alpha_conf, "formal");
        assert_eq!(alpha_src.as_deref(), Some("lsp"));

        // Line 34 (`case BetaHandler: return h.Process()`) must resolve to
        // BetaHandler's own Process, not AlphaHandler's.
        let (beta_conf, beta_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'handlers.go::route' \
                   AND to_symbol = 'handlers.go::BetaHandler::Process' \
                   AND call_site_line = 34",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(beta_conf, "formal");
        assert_eq!(beta_src.as_deref(), Some("lsp"));

        // The wrong cross-pairing at each site must still be ambiguous —
        // the actual disambiguation proof, not just "something changed".
        let wrong_at_32: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.go::route' \
                   AND to_symbol = 'handlers.go::BetaHandler::Process' \
                   AND call_site_line = 32",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_32, "ambiguous");
        let wrong_at_34: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.go::route' \
                   AND to_symbol = 'handlers.go::AlphaHandler::Process' \
                   AND call_site_line = 34",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_34, "ambiguous");
    }

    /// Live integration: real `clangd` against `multi_lang_workspace/cpp`
    /// (Phase D.3, 2026-07-11). Ignored by default — needs `clangd` on
    /// PATH; no `compile_commands.json` required for this fixture (a
    /// single self-contained translation unit with no external
    /// dependencies beyond its own local header — clangd's fallback
    /// compilation database handles this fine).
    ///
    /// `process(int)`/`process(double)` are real C++ overloads with the
    /// SAME name — CALM's syntactic resolver has no type-checker, so it
    /// can't perform overload resolution and lands the single bare call
    /// `process(x)` (`x: int`) as `ambiguous` with both candidates before
    /// this overlay runs. `clangd`'s real Clang-based overload resolution
    /// picks the exact `int` overload and rules out the `double` one —
    /// verified once by hand before this test was written (indexed the
    /// fixture, ran clangd through the LSP overlay: the call resolved to
    /// the `int` overload only).
    ///
    /// (An earlier fixture design tried `auto*` + `static_cast` dispatch,
    /// mirroring the Kotlin/Go smart-cast tests — but CALM's C++ resolver
    /// doesn't do `auto` type deduction at all, so it creates NO
    /// `call_edges` row for that pattern, not even an `ambiguous` one,
    /// leaving nothing for this overlay to upgrade. Free-function overload
    /// name collision is the pattern that actually produces an eligible
    /// `ambiguous` row for C++, confirmed empirically, not assumed.)
    #[test]
    #[ignore]
    fn clangd_overlay_upgrades_ambiguous_overload_call_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/cpp");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before_int: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'Overloads.cpp::dispatchOverload' \
                   AND to_symbol = 'Overloads.cpp::process' \
                   AND call_site_line = 12",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            before_int, "ambiguous",
            "fixture must start ambiguous — CALM has no C++ overload resolution"
        );

        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        let stats = run_lsp_overlay(&conn, &fixture, &provider::CLANGD, &cfg, true).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal (attempted={})",
            stats.attempted
        );

        // The int overload (declared first, lines 3-5) must resolve —
        // matches the call site's actual argument type.
        let (int_conf, int_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'Overloads.cpp::dispatchOverload' \
                   AND to_symbol = 'Overloads.cpp::process' \
                   AND call_site_line = 12",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(int_conf, "formal");
        assert_eq!(int_src.as_deref(), Some("lsp"));

        // The double overload (lines 7-9, qualified_name suffixed #7) must
        // still be ambiguous — the actual disambiguation proof.
        let double_conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'Overloads.cpp::dispatchOverload' \
                   AND to_symbol = 'Overloads.cpp::process#7' \
                   AND call_site_line = 12",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(double_conf, "ambiguous");
    }

    #[test]
    fn resolution_context_fingerprint_changes_with_a_declared_input() {
        let root = std::env::temp_dir().join(format!(
            "calm_lsp_context_fingerprint_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = 'one'\n").unwrap();

        let first = resolution_context_fingerprint(&root, &["Cargo.toml", "Cargo.lock"]);
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = 'two'\n").unwrap();
        let second = resolution_context_fingerprint(&root, &["Cargo.toml", "Cargo.lock"]);

        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn connection_with_fresh_lsp_proof(provider: &str, context_fingerprint: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('main.rs', 'main.rs::main', 'target', 1, 0, 6, 2, 'call')",
            [],
        )
        .unwrap();
        let call_site_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_id, edge_confidence, formal_source,
                 evidence_state, edge_kind)
             VALUES ('main.rs::main', 'lib.rs::target', ?1, 'formal', 'lsp', 'fresh', 'call')",
            [call_site_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO external_proofs
                (call_site_id, to_symbol, provider, source_file_hash, callee_start_byte,
                 callee_end_byte, provider_fingerprint, context_fingerprint, status, observed_at)
             VALUES (?1, 'lib.rs::target', ?2, 'h', 0, 6,
                     'fixed-provider', ?3, 'fresh', 1.0)",
            rusqlite::params![call_site_id, provider, context_fingerprint],
        )
        .unwrap();
        conn
    }

    #[test]
    fn every_declared_provider_input_changes_only_its_context_and_stales_prior_proof() {
        let root = tempfile::tempdir().unwrap();
        let providers = [
            &provider::RUST_ANALYZER,
            &provider::GOPLS,
            &provider::CLANGD,
        ];
        let initial_contexts: Vec<(&str, String)> = providers
            .iter()
            .map(|provider| {
                (
                    provider.name,
                    resolution_context_fingerprint(root.path(), provider.context_inputs),
                )
            })
            .collect();

        for provider in providers {
            for input in provider.context_inputs {
                let missing = resolution_context_fingerprint(root.path(), provider.context_inputs);
                let conn =
                    connection_with_fresh_lsp_proof(&format!("lsp:{}", provider.name), &missing);
                let path = root.path().join(input);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&path, format!("first:{}:{input}\n", provider.name)).unwrap();
                let present = resolution_context_fingerprint(root.path(), provider.context_inputs);
                assert_ne!(missing, present, "{input} missing→present must invalidate");
                for (other_name, initial) in &initial_contexts {
                    if *other_name != provider.name {
                        let other = providers
                            .iter()
                            .find(|other| other.name == *other_name)
                            .unwrap();
                        assert_eq!(
                            resolution_context_fingerprint(root.path(), other.context_inputs),
                            *initial,
                            "{input} must not perturb {other_name}'s context"
                        );
                    }
                }
                invalidate_stale_lsp_context(
                    &conn,
                    &crate::scip::ingest::ExternalProofContext::new(
                        format!("lsp:{}", provider.name),
                        "fixed-provider",
                        present.clone(),
                    ),
                )
                .unwrap();
                assert_eq!(
                    conn.query_row("SELECT status FROM external_proofs", [], |row| row
                        .get::<_, String>(0))
                        .unwrap(),
                    "stale",
                    "{input} must stale the proof established before its change"
                );
                assert_eq!(
                    conn.query_row("SELECT evidence_state FROM call_edges", [], |row| row
                        .get::<_, String>(0))
                        .unwrap(),
                    "stale",
                    "{input} must reopen the LSP formal edge"
                );

                std::fs::write(&path, format!("second:{}:{input}\n", provider.name)).unwrap();
                let changed = resolution_context_fingerprint(root.path(), provider.context_inputs);
                assert_ne!(present, changed, "{input} present→changed must invalidate");
                std::fs::remove_file(&path).unwrap();
                assert_eq!(
                    resolution_context_fingerprint(root.path(), provider.context_inputs),
                    missing,
                    "removing {input} must restore its explicit missing-state fingerprint"
                );
            }
        }
    }

    #[test]
    fn changed_lsp_context_stales_its_proof_and_reopens_the_edge() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('main.rs', 'main.rs::main', 'target', 1, 0, 6, 2, 'call')",
            [],
        )
        .unwrap();
        let call_site_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_id, edge_confidence, formal_source,
                 evidence_state, edge_kind)
             VALUES ('main.rs::main', 'lib.rs::target', ?1, 'formal', 'lsp', 'fresh', 'call')",
            [call_site_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO external_proofs
                (call_site_id, to_symbol, provider, source_file_hash, callee_start_byte,
                 callee_end_byte, provider_fingerprint, context_fingerprint, status, observed_at)
             VALUES (?1, 'lib.rs::target', 'lsp:rust-analyzer', 'h', 0, 6,
                     'old-provider', 'old-context', 'fresh', 1.0)",
            [call_site_id],
        )
        .unwrap();
        let context = crate::scip::ingest::ExternalProofContext::new(
            "lsp:rust-analyzer",
            "new-provider",
            "new-context",
        );

        invalidate_stale_lsp_context(&conn, &context).unwrap();

        let proof_status: String = conn
            .query_row("SELECT status FROM external_proofs", [], |row| row.get(0))
            .unwrap();
        let edge: (String, Option<String>, String) = conn
            .query_row(
                "SELECT edge_confidence, formal_source, evidence_state FROM call_edges",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(proof_status, "stale");
        assert_eq!(edge, ("textual".into(), None, "stale".into()));
    }

    #[test]
    fn coordinator_coalesces_same_baseline_and_fences_an_older_generation() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let coordinator = Arc::new(LspRunCoordinator::default());
        let key = LspRunKey::new(Path::new("/tmp/calm-d4-coordinator"), "rust-analyzer");
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let first = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-a", move |cancelled| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    while !cancelled.load(Ordering::SeqCst) {
                        if release_rx.try_recv().is_ok() {
                            return Ok(LspIngestStats {
                                upgraded: 1,
                                attempted: 1,
                                match_rate: 1.0,
                                status: LspRunStatus::Succeeded,
                            });
                        }
                        std::thread::yield_now();
                    }
                    anyhow::bail!("cancelled by a newer LSP baseline")
                })
            })
        };
        started_rx.recv().unwrap();

        let joined = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-a", move |_| {
                    calls.fetch_add(100, Ordering::SeqCst);
                    Ok(LspIngestStats::default())
                })
            })
        };
        while coordinator.waiting_count(&key) != 1 {
            std::thread::yield_now();
        }

        let newer = {
            let coordinator = Arc::clone(&coordinator);
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-b", |_| {
                    Ok(LspIngestStats {
                        upgraded: 2,
                        attempted: 2,
                        match_rate: 1.0,
                        status: LspRunStatus::Succeeded,
                    })
                })
            })
        };

        assert!(first.join().unwrap().is_err());
        assert!(joined.join().unwrap().is_err());
        assert_eq!(newer.join().unwrap().unwrap().upgraded, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = release_tx.send(());
    }

    #[test]
    fn lsp_run_baseline_key_changes_with_graph_generation() {
        let first = crate::scip::ingest::ExternalProofContext::new(
            "lsp:rust-analyzer",
            "provider",
            "context",
        )
        .at_graph_generation(41);
        let newer = crate::scip::ingest::ExternalProofContext::new(
            "lsp:rust-analyzer",
            "provider",
            "context",
        )
        .at_graph_generation(42);

        assert_ne!(lsp_run_baseline_key(&first), lsp_run_baseline_key(&newer));
    }

    #[test]
    fn coordinator_runs_only_the_latest_queued_baseline() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let coordinator = Arc::new(LspRunCoordinator::default());
        let key = LspRunKey::new(Path::new("/tmp/calm-d4-latest-wins"), "rust-analyzer");
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let first = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-a", move |cancelled| {
                    started_tx.send(()).unwrap();
                    while !cancelled.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    cancelled_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    anyhow::bail!("cancelled by a newer LSP baseline")
                })
            })
        };
        started_rx.recv().unwrap();

        let older_pending = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-b", move |_| {
                    calls.fetch_add(10, Ordering::SeqCst);
                    Ok(LspIngestStats::default())
                })
            })
        };
        while coordinator.queued_generation(&key).as_deref() != Some("baseline-b") {
            std::thread::yield_now();
        }
        cancelled_rx.recv().unwrap();

        let newest_pending = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            let calls = Arc::clone(&calls);
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-c", move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(LspIngestStats {
                        upgraded: 3,
                        attempted: 3,
                        match_rate: 1.0,
                        status: LspRunStatus::Succeeded,
                    })
                })
            })
        };
        while coordinator.queued_generation(&key).as_deref() != Some("baseline-c") {
            std::thread::yield_now();
        }
        release_tx.send(()).unwrap();

        assert!(first.join().unwrap().is_err());
        assert_eq!(
            older_pending.join().unwrap().unwrap(),
            LspIngestStats::default()
        );
        assert_eq!(newest_pending.join().unwrap().unwrap().upgraded, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn coordinator_refuses_phase_three_commit_after_a_newer_baseline_is_queued() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        use std::sync::mpsc;

        let coordinator = Arc::new(LspRunCoordinator::default());
        let key = LspRunKey::new(Path::new("/tmp/calm-d4-phase-three-fence"), "rust-analyzer");
        let did_commit = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = mpsc::channel();

        let predecessor = {
            let coordinator = Arc::clone(&coordinator);
            let commit_coordinator = Arc::clone(&coordinator);
            let run_key = key.clone();
            let commit_key = key.clone();
            let did_commit = Arc::clone(&did_commit);
            std::thread::spawn(move || {
                coordinator.run(run_key, "baseline-a", move |cancelled| {
                    started_tx.send(()).unwrap();
                    while !cancelled.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    let phase_three = commit_coordinator
                        .commit_if_current(&commit_key, "baseline-a", &cancelled, || {
                            did_commit.store(true, Ordering::SeqCst);
                            Ok(())
                        })
                        .unwrap();
                    assert!(
                        phase_three.is_none(),
                        "a cancelled baseline must be fenced before its phase-three mutation"
                    );
                    Ok(LspIngestStats::with_status(LspRunStatus::Cancelled))
                })
            })
        };
        started_rx.recv().unwrap();

        let successor = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-b", |_| Ok(LspIngestStats::default()))
            })
        };
        while coordinator.queued_generation(&key).as_deref() != Some("baseline-b") {
            std::thread::yield_now();
        }

        assert_eq!(
            predecessor.join().unwrap().unwrap().status,
            LspRunStatus::Cancelled
        );
        successor.join().unwrap().unwrap();
        assert!(
            !did_commit.load(Ordering::SeqCst),
            "only the newest baseline may reach phase-three persistence"
        );
    }

    #[test]
    fn coordinator_latest_wins_allows_only_current_baseline_to_persist_proof() {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("target.rs");
        std::fs::write(&target, "fn target() {}\n").unwrap();
        let db_path = tmp.path().join("graph.sqlite");
        let conn = Connection::open(&db_path).unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO file_index (path, hash, language, symbol_count, last_indexed) VALUES
                 ('main.rs', 'source-hash', 'rust', 1, 0),
                 ('target.rs', 'target-hash', 'rust', 1, 0);
             INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
                 VALUES ('target.rs::target', 'target', 'function', 'rust', 'target.rs', 1, 1);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
                 VALUES ('main.rs', 'main.rs::main', 'target', 1, 4, 10, 2, 'call');
             INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence,
                 from_path, to_path, edge_kind)
                 VALUES ('main.rs::main', 'target.rs::target', 1, 1, 'textual',
                         'main.rs', 'target.rs', 'call');",
        )
        .unwrap();

        let coordinator = Arc::new(LspRunCoordinator::default());
        let key = LspRunKey::new(&root, "rust-analyzer");
        let (started_tx, started_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let predecessor = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-a", move |cancelled| {
                    started_tx.send(()).unwrap();
                    while !cancelled.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    cancelled_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(LspIngestStats::with_status(LspRunStatus::Cancelled))
                })
            })
        };
        started_rx.recv().unwrap();

        let superseded = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-b", |_| {
                    panic!("the superseded queued baseline must never execute")
                })
            })
        };
        while coordinator.queued_generation(&key).as_deref() != Some("baseline-b") {
            std::thread::yield_now();
        }

        let current = {
            let coordinator = Arc::clone(&coordinator);
            let key = key.clone();
            let db_path = db_path.clone();
            let root = root.clone();
            let target_uri = path_to_uri(&target).unwrap();
            std::thread::spawn(move || {
                coordinator.run(key, "baseline-c", move |_| {
                    let conn = Connection::open(db_path).unwrap();
                    let upgraded = apply_lsp_resolutions(
                        &conn,
                        &root,
                        &[ResolvedSite {
                            edge_id: 1,
                            call_site_id: 1,
                            callee_start_byte: 4,
                            callee_end_byte: 10,
                            source_file_hash: "source-hash".into(),
                            to_symbol: "target.rs::target".into(),
                            def_uri: target_uri,
                            def_line_zero_based: 0,
                        }],
                        &crate::scip::ingest::ExternalProofContext::new(
                            "lsp:baseline-c",
                            "current-provider",
                            "current-context",
                        )
                        .at_graph_generation(0),
                    )
                    .unwrap();
                    Ok(LspIngestStats {
                        upgraded,
                        attempted: 1,
                        match_rate: upgraded as f64,
                        status: LspRunStatus::Succeeded,
                    })
                })
            })
        };
        while coordinator.queued_generation(&key).as_deref() != Some("baseline-c") {
            std::thread::yield_now();
        }
        cancelled_rx.recv().unwrap();
        release_tx.send(()).unwrap();

        assert_eq!(
            predecessor.join().unwrap().unwrap().status,
            LspRunStatus::Cancelled
        );
        assert_eq!(
            superseded.join().unwrap().unwrap(),
            LspIngestStats::default(),
            "baseline B must be coalesced away before it can mutate persistence"
        );
        assert_eq!(current.join().unwrap().unwrap().upgraded, 1);
        let proof_rows: Vec<(String, String, i64)> = conn
            .prepare("SELECT provider, status, graph_generation FROM external_proofs")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            proof_rows,
            vec![("lsp:baseline-c".into(), "fresh".into(), 0)]
        );
        let edge: (String, String, i64) = conn
            .query_row(
                "SELECT edge_confidence, formal_source, ruled_out_by_scip FROM call_edges WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(edge, ("formal".into(), "lsp".into(), 0));
    }
}
