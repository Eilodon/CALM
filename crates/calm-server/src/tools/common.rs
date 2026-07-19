use super::inspect::*;
use super::*;

/// Poison-tolerant lock accessors (audit F4): every `Mutex`/`RwLock` field
/// these are used on guards state whose only invariant is "contains a
/// valid `T`" (a counter, an `Option`, a map, or `()`) — no cross-field
/// invariant that a panic mid-update could leave torn, so recovering the
/// guard on poison and carrying on is strictly better than letting one
/// panicking tool call brick every subsequent call that needs the same
/// lock for the rest of the process's life. If a lock this is used on ever
/// grows a real cross-field invariant (e.g. "these two fields must stay in
/// sync"), that lock must stop using these and go back to `.unwrap()` (or
/// an explicit poison check) instead — poison-tolerance would silently
/// hide a torn invariant rather than fail loudly.
pub(crate) trait LockExt<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn lock_ok(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) trait RwLockExt<T> {
    fn read_ok(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_ok(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for std::sync::RwLock<T> {
    fn read_ok(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn write_ok(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl CalmServer {
    pub fn new(project_root: PathBuf, db_path: PathBuf) -> anyhow::Result<Self> {
        Self::new_with_preset(project_root, db_path, "full".into())
    }

    pub fn new_with_preset(
        project_root: PathBuf,
        db_path: PathBuf,
        preset: String,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // `open_writer` (not a bare `Connection::open`) so this sets
        // `busy_timeout` before running schema DDL: every *other* writer site
        // in this codebase goes through `open_writer` for exactly this
        // reason (see its doc comment), but this one-time schema-init
        // connection didn't, and it's reachable from every `calm serve`
        // process's own startup, not just the indexer-lock owner's. Without
        // `busy_timeout`, two processes launched at the same moment against
        // a brand-new project (no schema yet — the widest DDL burst, and the
        // likeliest moment for two sessions to start together) can race:
        // SQLite's default no-retry `SQLITE_BUSY` on the loser propagates
        // straight through this `?`, failing `new_with_preset` entirely —
        // that `calm serve` process never starts, surfacing to the user as
        // "MCP server failed to connect" instead of the brief, silent wait
        // `busy_timeout` gives every other writer.
        let conn = calm_core::db::conn::open_writer(&db_path)?;
        calm_core::db::schema::init_db(&conn)?;
        drop(conn);
        let coverage = calm_core::analysis::coverage::load_coverage(&project_root);
        let tool_router = CalmServer::tool_router_for_preset(&preset)?;
        Ok(Self {
            project_root,
            db_path,
            phase: Arc::new(RwLock::new(IndexingPhase::Scanning)),
            last_index_error: Arc::new(RwLock::new(None)),
            last_graph_mode: Arc::new(RwLock::new(None)),
            embedder: Arc::new(RwLock::new(None)),
            embed_status: Arc::new(RwLock::new(EmbedStatus::Disabled)),
            last_embed_error: Arc::new(RwLock::new(None)),
            owns_indexer_lock: Arc::new(RwLock::new(false)),
            coverage: Arc::new(RwLock::new(coverage)),
            config_cache: Arc::new(RwLock::new(None)),
            co_change_cache: Arc::new(RwLock::new(None)),
            session_log: Arc::new(Mutex::new(SessionLog::default())),
            // `0` is never a real `for_connection`-allocated id (that
            // counter starts at 1 — see `next_session_id` below), so this
            // instance's own entry never collides with, and is never
            // confused for, a connection's.
            session_id: 0,
            next_session_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            active_sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
            edit_lock: Arc::new(Mutex::new(())),
            preset,
            tool_router,
        })
    }
    /// Builds a fresh per-connection `CalmServer` from a daemon-shared
    /// instance — every field is cloned (cheap: everything but
    /// `session_log`/`session_id`/`preset`/`project_root`/`db_path`/
    /// `tool_router` is already `Arc<RwLock/Mutex<_>>`) except two
    /// deliberately-private ones this call resets: `session_log` gets a
    /// brand-new `SessionLog` so one connection's explored-files/explored-
    /// symbols history can never leak into another session sharing the same
    /// daemon, and `session_id` gets a fresh id (allocated here, from the
    /// still-shared `next_session_id` counter) with a matching entry
    /// inserted into the still-shared `active_sessions` map — the mirror
    /// image: `session_log` stays private per connection, `active_sessions`
    /// stays visible across all of them, on purpose, so `session_context`
    /// can answer "who else is here" without leaking any one session's full
    /// exploration history to the others. `edit_lock` is deliberately NOT
    /// reset here — it must stay the one lock shared by every connection to
    /// keep serializing `edit_lines`/`edit_symbol` writes against the one
    /// shared DB writer (today, each `calm serve` process has its own
    /// `edit_lock`, only soft-serialized across processes via SQLite's
    /// `busy_timeout` — a daemon sharing one real `edit_lock` is a strict
    /// improvement, real mutual exclusion instead of best-effort).
    /// `preset`/`project_root`/`db_path`/`tool_router` also stay
    /// shared/frozen at whatever the daemon was spawned with —
    /// first-writer-wins, per ADR-0005.
    pub fn for_connection(&self) -> Self {
        let session_id = self
            .next_session_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut sessions) = self.active_sessions.lock() {
            sessions.insert(
                session_id,
                SessionSummary {
                    session_id,
                    last_touched_file: None,
                    last_touched_at: utc_now_iso8601(),
                    tool_calls: 0,
                    reviewing_symbol: None,
                },
            );
        }
        Self {
            session_id,
            session_log: Arc::new(Mutex::new(SessionLog::default())),
            ..self.clone()
        }
    }

    /// Clone of the shared `active_sessions` map plus this connection's own
    /// `session_id` — for the daemon's accept loop to deregister this
    /// connection's entry when it ends, without needing broader field
    /// access. Mirrors the existing `phase_handle`/`embed_status_handle`
    /// pattern (a narrow accessor instead of a `pub(crate)` field).
    pub(crate) fn session_registry_handle(
        &self,
    ) -> (
        Arc<Mutex<std::collections::HashMap<u64, SessionSummary>>>,
        u64,
    ) {
        (self.active_sessions.clone(), self.session_id)
    }
    /// Opens a new dedicated read-only connection to the same DB file.
    /// Sets `PRAGMA query_only = ON` immediately so any accidental write in a
    /// tool handler is rejected at the SQLite level.
    ///
    /// SINGLE_WRITER enforcement: all tool handlers must use this for reads.
    /// Schema init uses a short-lived local connection in `new_with_preset`.
    pub(crate) fn make_read_conn(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch("PRAGMA query_only = ON;")?;
        Ok(conn)
    }

    /// Cached `load_config` (audit F12): checks `config.json`'s current
    /// mtime against what's cached; a match serves the cached `Config`
    /// clone without touching disk beyond the one `stat()` inside
    /// `calm_core::config::config_mtime`. A miss (including a config file
    /// appearing/disappearing since the last call) reloads and replaces the
    /// whole `(mtime, Config)` pair in one atomic `write_ok()` — never
    /// mutates the cached `Config` in place, so a concurrent `read_ok()`
    /// can never observe a torn pair. Behavior is otherwise identical to
    /// `calm_core::config::load_config_or_warn(&self.project_root)` (same
    /// file, same defaulting, same on-error log), just cached.
    pub(crate) fn config(&self) -> calm_core::config::Config {
        let current_mtime = calm_core::config::config_mtime(&self.project_root);
        if let Some((cached_mtime, cfg)) = self.config_cache.read_ok().as_ref()
            && *cached_mtime == current_mtime
        {
            return cfg.clone();
        }
        let cfg = calm_core::config::load_config_or_warn(&self.project_root);
        // Root-cause fix for the F10 calibration bug: a local config.json/
        // .calm/config.json override previously shadowed Config::default()
        // with zero visibility, so a stale forgotten override file could
        // silently mask a code-level default change indefinitely (see
        // calm_core::config::diff_from_default's doc comment). Logged once
        // per cache miss -- rare, gated by config_mtime -- not every call.
        if let Some(override_path) = calm_core::config::resolve_config_path(&self.project_root) {
            let diff = calm_core::config::diff_from_default(&cfg);
            if !diff.is_empty() {
                tracing::info!(
                    "config: local override active at {} — {} field(s) differ from built-in defaults: {}",
                    override_path.display(),
                    diff.len(),
                    diff.join(", ")
                );
            }
        }
        *self.config_cache.write_ok() = Some((current_mtime, cfg.clone()));
        cfg
    }
    /// Cached `compute_co_changes` (audit F11b): `edit_context` is the
    /// mandatory-before-every-edit tool, and used to spawn a `git log`
    /// subprocess on every single call regardless of whether the same file
    /// was just inspected a moment ago. A cache hit (same target_path/since/
    /// min_co_changes/top_n, within `CO_CHANGE_CACHE_TTL`) returns the
    /// cached `CoChangeResult` clone without touching git at all. Git
    /// history only changes on a new commit, so a short TTL is plenty fresh
    /// for this tool's advisory purpose (co-changed files are a coupling
    /// hint, not ground truth the caller acts on blindly).
    pub(crate) fn co_changes_cached(
        &self,
        target_path: &str,
        since: &str,
        min_co_changes: usize,
        top_n: usize,
    ) -> calm_core::analysis::cochange::CoChangeResult {
        const CO_CHANGE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
        let key = (
            target_path.to_string(),
            since.to_string(),
            min_co_changes,
            top_n,
        );
        if let Some((cached_key, at, result)) = self.co_change_cache.read_ok().as_ref()
            && *cached_key == key
            && at.elapsed() < CO_CHANGE_CACHE_TTL
        {
            return result.clone();
        }
        let result = calm_core::analysis::cochange::compute_co_changes(
            &self.project_root,
            target_path,
            since,
            min_co_changes,
            top_n,
        );
        *self.co_change_cache.write_ok() = Some((key, std::time::Instant::now(), result.clone()));
        result
    }

    /// Test-only write connection for seeding fixture data.
    /// Production tool handlers must use `make_read_conn()` instead.
    #[cfg(test)]
    pub(crate) fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db_path).unwrap()
    }

    /// Write connection for `remember` — the one tool handler that isn't
    /// read-only (every other tool must use `make_read_conn()`). Scoped
    /// narrowly: `project_memory` is never touched by the indexer/watcher,
    /// so this doesn't contend with indexing writes in practice; the
    /// `busy_timeout` covers the rare case where SQLite's single-writer-per-
    /// file lock is briefly held by an indexing transaction anyway, rather
    /// than failing the note immediately.
    pub(crate) fn memory_write_conn(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        calm_core::db::conn::open_writer(&self.db_path)
    }
    /// Wraps `telemetry::timed_tool`, additionally bumping the session's tool-call
    /// counter. Kept as a method (rather than changing `timed_tool`'s signature)
    /// since only this type has access to `session_log`.
    pub(crate) fn timed_tool<T: serde::Serialize>(
        &self,
        name: &str,
        body: impl FnOnce() -> T,
    ) -> T {
        if let Ok(mut log) = self.session_log.lock() {
            log.tool_calls += 1;
        }
        crate::telemetry::timed_tool(name, body)
    }
    pub(crate) fn track_symbol(&self, qualified_name: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            let now = log.tool_calls;
            if !log.explored_symbols.contains_key(qualified_name) {
                log.last_progress_at = now;
            }
            log.explored_symbols.insert(qualified_name.to_string(), now);
        }
    }

    pub(crate) fn track_file(&self, path: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            let now = log.tool_calls;
            if !log.explored_files.contains_key(path) {
                log.last_progress_at = now;
            }
            log.explored_files.insert(path.to_string(), now);
        }
        self.touch_active_session(Some(path));
    }

    /// Current `session_log.tool_calls` count — the freshness clock
    /// `record_edit_context_review`/`edit_context_review` compare against.
    pub(crate) fn session_tool_calls(&self) -> u64 {
        self.session_log
            .lock()
            .map(|log| log.tool_calls)
            .unwrap_or(0)
    }

    /// Records that `edit_context` just ran for `qualified_name` this
    /// session — the structural half of `edit_symbol`/`edit_lines`' confirm
    /// gate (docs/superskills/specs/2026-07-11-superskills-inspired-features.md
    /// #5 v2). `caller_qns` should be the same confidence-ordered list
    /// `edit_context` itself returned (capped upstream); this stores at most 5.
    pub(crate) fn record_edit_context_review(
        &self,
        qualified_name: &str,
        caller_qns: &[String],
        risk_level: &str,
    ) {
        if let Ok(mut log) = self.session_log.lock() {
            let at = log.tool_calls;
            log.edit_context_reviewed.insert(
                qualified_name.to_string(),
                EditContextReview {
                    at,
                    caller_qns: caller_qns.iter().take(5).cloned().collect(),
                    risk_level: risk_level.to_string(),
                },
            );
        }
    }

    /// Looks up `qualified_name`'s most recent `edit_context` review this
    /// session, if any — `None` when it was never reviewed (or a prior review
    /// exists for a *different* qualified_name, e.g. after a rename). Cloned
    /// out from behind the lock rather than returning a guard, matching every
    /// other `session_log` accessor in this file (`session_context`,
    /// `written_files_snapshot`).
    pub(crate) fn edit_context_review(&self, qualified_name: &str) -> Option<EditContextReview> {
        self.session_log
            .lock()
            .ok()
            .and_then(|log| log.edit_context_reviewed.get(qualified_name).cloned())
    }

    /// Records that `path` was written via `edit_lines`/`edit_symbol` — see
    /// `SessionLog::written_files`. Call once per successful write.
    pub(crate) fn mark_written(&self, path: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            log.written_files.insert(path.to_string());
        }
        self.touch_active_session(Some(path));
    }

    /// Refreshes this connection's own entry in the shared `active_sessions`
    /// map — `last_touched_file` (when `path` is `Some`), `last_touched_at`,
    /// and `tool_calls` (read from `session_log`, already bumped by
    /// `timed_tool` before any handler body runs). Called from `track_file`/
    /// `mark_written` rather than `track_symbol`, since a qualified symbol
    /// name isn't reliably path-shaped across every indexed language — file-
    /// level granularity is what `session_context.other_active_sessions`
    /// promises, not symbol-level. A no-op whenever this entry was never
    /// inserted in the first place (a bare `new`/`new_with_preset` instance,
    /// `session_id == 0` — see `for_connection`).
    /// audit H6: lock order invariant, codebase-wide — `session_log` is
    /// always locked BEFORE `active_sessions` (see this function for the
    /// canonical example). Any function that touches both must preserve
    /// this order; reversing it is a deadlock waiting to happen against
    /// another function that also locks both.
    fn touch_active_session(&self, path: Option<&str>) {
        let tool_calls = self
            .session_log
            .lock()
            .map(|log| log.tool_calls)
            .unwrap_or(0);
        if let Ok(mut sessions) = self.active_sessions.lock()
            && let Some(entry) = sessions.get_mut(&self.session_id)
        {
            if let Some(path) = path {
                entry.last_touched_file = Some(path.to_string());
            }
            entry.last_touched_at = utc_now_iso8601();
            entry.tool_calls = tool_calls;
        }
    }

    /// Publishes "this session is currently reviewing `qualified_name`" to
    /// the *shared* `active_sessions` registry — the multi-agent-visible
    /// counterpart to `record_edit_context_review`'s session-local record.
    /// Called from `edit_context` (the mandatory pre-edit tool), so another
    /// concurrent session calling `session_context` can see *intent*
    /// ("session 3 just reviewed `foo` — probably about to edit it"), not
    /// just the *past* touches `touch_active_session` already tracked.
    /// Deliberately advisory only, same posture as the rest of
    /// `SessionSummary`: this never blocks, reserves, or locks anything —
    /// two sessions can review (or even edit) the same symbol regardless.
    pub(crate) fn note_reviewing(&self, qualified_name: &str) {
        if let Ok(mut sessions) = self.active_sessions.lock()
            && let Some(entry) = sessions.get_mut(&self.session_id)
        {
            entry.reviewing_symbol = Some(qualified_name.to_string());
            entry.last_touched_at = utc_now_iso8601();
        }
    }

    /// Read-only snapshot of paths written since the last `diff_impact` call
    /// — for `session_context` to report without clearing anything (only
    /// `diff_impact` itself, via `clear_written_files`, does that).
    pub(crate) fn written_files_snapshot(&self) -> Vec<String> {
        self.session_log
            .lock()
            .map(|log| log.written_files.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Clears the written-files set — `diff_impact` calls this only from its
    /// single success point (audit F6, previously called unconditionally at
    /// entry). A *failed* call (bad input, git failure, DB error) proves
    /// nothing about whether a blast-radius check actually happened, so it
    /// must leave the gate set; only a genuine analysis satisfies it. Note
    /// this is stricter than the Claude-Code hook's own (host-specific,
    /// PreToolUse-only) equivalent gate, which still resets on every call
    /// regardless of outcome since it fires before the result is known —
    /// see the AUDIT NOTE on Item 1.3 in docs/plans/2026-07-12-upgrade-plan-1-correctness-safety.md.
    pub(crate) fn clear_written_files(&self) {
        if let Ok(mut log) = self.session_log.lock() {
            log.written_files.clear();
        }
    }

    /// Additive, session-scoped relevance boost for `search`/`locate`
    /// results: a result whose file is import/call-adjacent to something
    /// this session recently explored gets nudged up, so results lean
    /// toward the current work context without ever overriding a strong
    /// text/semantic match. Mutates `results[i].score` in place and re-sorts
    /// — never touches `symbols.is_hub`/`coreness` or any other
    /// DB-persisted, cross-session-shared ranking signal. Returns `true`
    /// when personalization actually adjusted anything, so callers can
    /// report it transparently rather than silently.
    ///
    /// Guaranteed no-op (identical results, in identical order) when this
    /// session hasn't explored anything yet, `personalization_weight` is
    /// configured to `0.0` — the common case for a session's first calls —
    /// or none of the computed boosts' paths appear in this particular
    /// result set. The actual score math lives in `normalize_then_boost`
    /// (a pure, `&self`-free function) so it's directly unit-testable
    /// without a full `CalmServer`/DB fixture — see Plan 3 §3.2 and
    /// `personalization_tests::normalize_then_boost_never_flips_a_large_gap`.
    pub(crate) fn apply_personalization_boost(
        &self,
        conn: &rusqlite::Connection,
        results: &mut [calm_core::search::SearchResult],
    ) -> bool {
        if results.is_empty() {
            return false;
        }
        let weight = self.config().search.personalization_weight;
        if weight <= 0.0 {
            return false;
        }
        let (explored_files, explored_symbols, tool_calls) = {
            let log = self.session_log.lock_ok();
            (
                log.explored_files.clone(),
                log.explored_symbols.clone(),
                log.tool_calls,
            )
        };
        if explored_files.is_empty() && explored_symbols.is_empty() {
            return false;
        }

        let boosts = compute_proximity_boosts(conn, &explored_files, &explored_symbols, tool_calls);
        normalize_then_boost(results, &boosts, weight)
    }

    /// A handle the background indexer uses to advance the phase as it works.
    pub fn phase_handle(&self) -> Arc<RwLock<IndexingPhase>> {
        Arc::clone(&self.phase)
    }

    /// A handle the background indexer uses to publish an error message
    /// when `phase` transitions to `Failed` (see `IndexingPhase::Failed`).
    pub fn last_index_error_handle(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.last_index_error)
    }
    /// Shared handle to `last_graph_mode` so the file watcher (which has no
    /// `CalmServer`) can record which rebuild path each incremental reindex
    /// took — mirrors `last_index_error_handle`. See `run_watch_loop`.
    pub fn last_graph_mode_handle(&self) -> Arc<RwLock<Option<String>>> {
        Arc::clone(&self.last_graph_mode)
    }
    /// Handles the background indexer uses to publish the loaded model + status.
    pub fn embedder_handle(&self) -> Arc<RwLock<Option<Arc<Embedder>>>> {
        Arc::clone(&self.embedder)
    }
    pub fn embed_status_handle(&self) -> Arc<RwLock<EmbedStatus>> {
        self.embed_status.clone()
    }

    pub fn coverage_handle(&self) -> Arc<RwLock<calm_core::analysis::coverage::CoverageData>> {
        self.coverage.clone()
    }

    /// The loaded embedder, if semantic search is ready.
    pub(crate) fn embedder(&self) -> Option<Arc<Embedder>> {
        self.embedder.read_ok().clone()
    }

    pub(crate) fn filter_sn(&self, sn: Option<SuggestedNext>) -> Option<SuggestedNext> {
        filter_suggested_next(sn, &self.tool_router)
    }

    pub(crate) fn embed_status_str(&self) -> String {
        self.embed_status.read_ok().as_str().to_string()
    }

    /// Re-runs the embedding bootstrap in the background when it previously
    /// failed (model load, vector-table creation, or embedding all set status
    /// to `Failed`) or was blocked by offline policy (`OfflineUnavailable` —
    /// e.g. the caller since flipped `semantic_search.allow_network_fallback`
    /// to `true` or ran `git lfs pull` and wants to try again). No-op for any
    /// other status: `Ready`/`Embedding`/`Downloading` are already done or in
    /// flight, and `Disabled` means semantic search isn't turned on in
    /// config. Opens its own DB connection so the retry doesn't hold the
    /// shared connection mutex for its duration.
    pub(crate) fn retry_embeddings_if_failed(&self) {
        // Claim the retry synchronously (Failed/OfflineUnavailable ->
        // Downloading) so two overlapping `retry_embeddings` requests can't
        // both spawn a bootstrap.
        {
            let mut status = self.embed_status.write_ok();
            if *status != EmbedStatus::Failed && *status != EmbedStatus::OfflineUnavailable {
                return;
            }
            *status = EmbedStatus::Downloading;
        }
        let semantic = self.config().semantic_search;
        let db_path = self.db_path.clone();
        let embedder = Arc::clone(&self.embedder);
        let status = Arc::clone(&self.embed_status);
        let last_embed_error = Arc::clone(&self.last_embed_error);
        // Only the process that actually won the indexer-lock race is
        // allowed to write new embedding rows to the shared DB — a
        // non-owning process just reloads its own local `Embedder` for
        // query-time embedding instead (see `load_embedder_readonly`);
        // calling the write-capable path here would race the real owner's
        // writes.
        let owns_lock = *self.owns_indexer_lock.read_ok();
        std::thread::spawn(move || {
            // Catches a panic inside the bootstrap so a bug there (or in a
            // future change to it) can't leave `status` stuck on
            // `Downloading` forever with no thread left to ever flip it —
            // the discarded `JoinHandle` means nothing else would notice.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if owns_lock {
                    match calm_core::db::conn::open_writer(&db_path) {
                        Ok(conn) => crate::bootstrap_embeddings(
                            &conn,
                            &semantic,
                            &embedder,
                            &status,
                            &last_embed_error,
                        ),
                        Err(e) => {
                            tracing::error!("Embeddings retry: failed to open DB: {e}");
                            *status.write_ok() = EmbedStatus::Failed;
                            *last_embed_error.write_ok() =
                                Some(format!("Embeddings retry: failed to open DB: {e}"));
                        }
                    }
                } else {
                    crate::load_embedder_readonly(&semantic, &embedder, &status, &last_embed_error);
                }
            }));
            if outcome.is_err() {
                tracing::error!("Embeddings retry thread panicked");
                *status.write_ok() = EmbedStatus::Failed;
            }
        });
    }

    pub fn last_embed_error_handle(&self) -> Arc<RwLock<Option<String>>> {
        self.last_embed_error.clone()
    }

    pub fn owns_indexer_lock_handle(&self) -> Arc<RwLock<bool>> {
        self.owns_indexer_lock.clone()
    }
    pub(crate) fn current_phase(&self) -> IndexingPhase {
        *self.phase.read_ok()
    }

    /// Canonical `indexing_phase` string for tool responses.
    pub(crate) fn phase_str(&self) -> String {
        self.current_phase().as_str().to_string()
    }

    /// `edges_ready` is true only once the full graph is built.
    pub(crate) fn edges_ready(&self) -> bool {
        self.current_phase() == IndexingPhase::Ready
    }
}

// ---------------------------------------------------------------------------
// Shared output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct SuggestedNext {
    pub(crate) tool: String,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<serde_json::Value>,
    /// Plan 3 §3.5(b): `Some(true)` iff skipping `tool` is actually
    /// hook-enforced (currently only the edit_context/edit_lines/edit_symbol
    /// → diff_impact hints set this, via `suggested_gated`) — every other
    /// hint is left unset (`None`), meaning advisory-only. Lets an agent
    /// tell "you'll be blocked if you skip this" apart from "you probably
    /// want this next" without re-deriving it from AGENTS.md prose each time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gate: Option<bool>,
}

pub(crate) fn suggested(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: None,
    })
}

pub(crate) fn suggested_with_args(
    tool: &str,
    reason: &str,
    args: serde_json::Value,
) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: Some(args),
        gate: None,
    })
}

/// Plan 3 §3.5(b): same as `suggested`, but for the 2 hints backed by an
/// actual hook enforcement (the pending-diff_impact gate) rather than a
/// convention — sets `gate: Some(true)` so an agent can tell "mandatory,
/// you will be blocked" from "recommended" without re-reading AGENTS.md.
pub(crate) fn suggested_gated(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: Some(true),
    })
}

// ---------------------------------------------------------------------------
// Tool Presets — selective tool set definitions
// ---------------------------------------------------------------------------

pub(crate) fn preset_tools(preset: &str) -> Option<&'static [&'static str]> {
    match preset {
        "orient" => Some(&[
            "repo_overview",
            "locate",
            "dependencies",
            "hotspots",
            "fitness_report",
            "indexing_status",
        ]),
        "trace" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "path",
            "dependencies",
            "indexing_status",
        ]),
        "edit" => Some(&[
            "repo_overview",
            "search",
            "locate",
            "symbol_info",
            "source",
            "callers",
            "callees",
            "edit_context",
            "edit_lines",
            "edit_symbol",
            "diff_impact",
            "indexing_status",
        ]),
        "compound" => Some(&[
            "repo_overview",
            "locate",
            "hotspots",
            "fitness_report",
            "source",
            "understand",
            "edit_context",
            "diff_impact",
            "session_context",
            "indexing_status",
            "remember",
            "recall",
        ]),
        "full" | "" => None, // None = all tools, no filtering
        _ => None,
    }
}

/// Toolset (module-domain) names for the composable preset registry
/// (2026-07-14 upgrade item, ported from github/github-mcp-server's
/// `pkg/inventory` toolset registry — verified against its real source:
/// a fixed, hand-chosen taxonomy of tool groups, not something read back
/// off individual tool metadata). One name per `#[tool_router]` module
/// merged into `CalmServer::full_tool_router()` — `toolset_tools` below
/// resolves a name to its actual tools by calling that module's own
/// router directly, so toolset MEMBERSHIP can never drift from what's
/// really registered even though this NAME list is hand-maintained (a
/// stale/renamed entry here just means that name resolves to zero tools,
/// caught immediately by `every_toolset_name_is_nonempty_and_real`
/// below). Mirrored in `calm_core::config::VALID_TOOLSET_NAMES` for
/// config.json's own light-weight preset syntax check (calm-core can't
/// depend on calm-server to validate this dynamically — see that const's
/// doc comment); `toolset_names_match_calm_core_valid_toolset_names`
/// below is the trip-wire if the two ever drift apart.
pub(crate) const TOOLSET_NAMES: &[&str] = &[
    "trace",
    "locate",
    "orient",
    "memory",
    "guardrails",
    "recover",
    "scip",
    "lsp",
    "security",
    "testgap",
    "inspect",
    "edit",
    "patterndebt",
];

/// Tool names belonging to one toolset, read directly off that module's
/// own `#[tool_router]`-generated router — the same accessor
/// `full_tool_router()` itself merges, so this can never name a tool the
/// toolset doesn't really have. `None` for an unrecognized toolset name.
fn toolset_tools(name: &str) -> Option<Vec<String>> {
    let router = match name {
        "trace" => CalmServer::trace_tool_router(),
        "locate" => CalmServer::locate_tool_router(),
        "orient" => CalmServer::orient_tool_router(),
        "memory" => CalmServer::memory_tool_router(),
        "guardrails" => CalmServer::guardrails_tool_router(),
        "recover" => CalmServer::recover_tool_router(),
        "scip" => CalmServer::scip_tool_router(),
        "lsp" => CalmServer::lsp_tool_router(),
        "security" => CalmServer::security_tool_router(),
        "testgap" => CalmServer::testgap_tool_router(),
        "inspect" => CalmServer::inspect_tool_router(),
        "edit" => CalmServer::edit_tool_router(),
        "patterndebt" => CalmServer::patterndebt_tool_router(),
        _ => return None,
    };
    Some(
        router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect(),
    )
}

fn calm_all_tool_names() -> std::collections::BTreeSet<String> {
    CalmServer::full_tool_router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

/// Resolves a `preset` spec to the concrete tool-name allow-list it
/// grants — `Ok(None)` means "every tool, no filtering" (the `full`/`""`
/// sentinel `tool_router_for_preset` already treats specially).
///
/// Two syntaxes, both accepted:
/// - A single bare legacy name (`"orient"`/`"trace"`/`"edit"`/
///   `"compound"`/`"full"`/`""`) resolves EXACTLY as `preset_tools`
///   always has — unchanged, byte-for-byte, so every existing
///   config.json and `--preset` invocation keeps working identically.
///   These 4 names are hand-curated cross-cutting workflow bundles (e.g.
///   `"edit"`'s 12 tools span 6 different toolsets), not toolset unions —
///   that's deliberate and predates this function; see `preset_tools`.
/// - Anything else is parsed as a composable spec: a comma-separated list
///   of `TOOLSET_NAMES` entries (or `"full"`), each optionally prefixed
///   with `-` to SUBTRACT that toolset's tools from the accumulated set
///   instead of adding it (e.g. `"trace,security"` unions two toolsets;
///   `"full,-edit"` is every tool except the edit toolset's). This is the
///   new, actually-composable half of the registry — deliberately scoped
///   to whole-toolset granularity, not individual tool names, both to
///   keep `calm_core::config::VALID_TOOLSET_NAMES` a small syntactically
///   checkable list and because per-tool `-`-tokens are a natural, non-
///   breaking extension of this same grammar if ever needed later.
///
/// A single bare token that ISN'T one of the 5 legacy names now goes
/// through the composable parser too (e.g. `"security"` alone resolves
/// to just the security toolset) rather than silently falling through to
/// `preset_tools`'s catch-all `_ => None` (= unfiltered full access) —
/// that silent-fallback-to-everything behavior for an unrecognized name
/// was the actual footgun motivating this function: an unrecognized
/// *composable* token is now a hard `Err`, never a silent grant.
pub(crate) fn resolve_preset(
    preset: &str,
) -> anyhow::Result<Option<std::collections::BTreeSet<String>>> {
    let trimmed = preset.trim();
    if matches!(
        trimmed,
        "" | "full" | "orient" | "trace" | "edit" | "compound"
    ) {
        return Ok(preset_tools(trimmed).map(|list| list.iter().map(|s| s.to_string()).collect()));
    }

    let mut include_all = false;
    let mut included: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut excluded: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for raw_token in trimmed.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            anyhow::bail!("empty token in preset spec {preset:?} (stray comma?)");
        }
        let (is_exclude, name) = match token.strip_prefix('-') {
            Some(rest) => (true, rest.trim()),
            None => (false, token),
        };
        if name == "full" {
            if is_exclude {
                excluded.extend(calm_all_tool_names());
            } else {
                include_all = true;
            }
            continue;
        }
        let Some(tools) = toolset_tools(name) else {
            anyhow::bail!(
                "unknown toolset {name:?} in preset spec {preset:?} — valid toolsets: {}. \
                 (Legacy bare presets full/orient/trace/edit/compound are also still accepted, \
                 alone, with no comma.)",
                TOOLSET_NAMES.join(", ")
            );
        };
        if is_exclude {
            excluded.extend(tools);
        } else {
            included.extend(tools);
        }
    }

    let base = if include_all {
        calm_all_tool_names()
    } else {
        included
    };
    Ok(Some(base.difference(&excluded).cloned().collect()))
}

#[cfg(test)]
mod preset_registry_tests {
    use super::*;

    #[test]
    fn every_toolset_name_is_nonempty_and_real() {
        let all_tools = calm_all_tool_names();
        for name in TOOLSET_NAMES {
            let tools = toolset_tools(name)
                .unwrap_or_else(|| panic!("TOOLSET_NAMES entry {name:?} does not resolve"));
            assert!(!tools.is_empty(), "toolset {name:?} resolved to zero tools");
            for t in &tools {
                assert!(
                    all_tools.contains(t),
                    "toolset {name:?} claims tool {t:?}, which full_tool_router() doesn't have"
                );
            }
        }
    }

    #[test]
    fn toolset_names_match_calm_core_valid_toolset_names() {
        let here: std::collections::BTreeSet<&str> = TOOLSET_NAMES.iter().copied().collect();
        let there: std::collections::BTreeSet<&str> = calm_core::config::VALID_TOOLSET_NAMES
            .iter()
            .copied()
            .collect();
        assert_eq!(
            here, there,
            "calm-server's TOOLSET_NAMES and calm-core's VALID_TOOLSET_NAMES have drifted apart"
        );
    }

    #[test]
    fn resolve_preset_legacy_bare_tokens_are_unchanged() {
        for name in ["", "full", "orient", "trace", "edit", "compound"] {
            let legacy = preset_tools(name).map(|l| {
                l.iter()
                    .map(|s| s.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
            });
            assert_eq!(
                resolve_preset(name).unwrap(),
                legacy,
                "resolve_preset({name:?}) must match preset_tools({name:?}) exactly"
            );
        }
    }

    #[test]
    fn resolve_preset_composes_toolsets_by_union() {
        let trace_only = toolset_tools("trace").unwrap();
        let security_only = toolset_tools("security").unwrap();
        let combined = resolve_preset("trace,security").unwrap().unwrap();
        for t in trace_only.iter().chain(security_only.iter()) {
            assert!(combined.contains(t), "combined preset missing {t:?}");
        }
        assert_eq!(
            combined.len(),
            trace_only.len() + security_only.len(),
            "trace and security toolsets should not overlap"
        );
    }

    #[test]
    fn resolve_preset_supports_toolset_exclusion() {
        let edit_tools = toolset_tools("edit").unwrap();
        let resolved = resolve_preset("full,-edit").unwrap().unwrap();
        for t in &edit_tools {
            assert!(
                !resolved.contains(t),
                "excluded toolset tool {t:?} still present"
            );
        }
        assert!(
            resolved.contains("repo_overview"),
            "non-excluded tool should remain"
        );
    }

    #[test]
    fn resolve_preset_rejects_unknown_toolset() {
        assert!(resolve_preset("not_a_real_toolset").is_err());
        assert!(resolve_preset("trace,not_a_real_toolset").is_err());
    }

    #[test]
    fn resolve_preset_single_bare_toolset_no_longer_silently_grants_full_access() {
        // Before this upgrade, an unrecognized bare preset name silently
        // fell through `preset_tools`'s `_ => None` catch-all -> every
        // tool available, no error. A bare toolset name must now resolve
        // to just that toolset, not everything.
        let resolved = resolve_preset("security").unwrap().unwrap();
        let expected: std::collections::BTreeSet<String> =
            toolset_tools("security").unwrap().into_iter().collect();
        assert_eq!(resolved, expected);
    }
}

/// `router.has_route(tool)` already means exactly "registered and not
/// disabled" (rmcp's own definition) — this used to re-derive the same
/// answer from the `preset` STRING via a separate `preset_tools` match, a
/// second mechanism that could disagree with what `tool_router_for_preset`
/// actually did to the router. Querying the router directly makes it the
/// single source of truth for "is this tool available", matching what
/// `tool_router_for_preset`'s own doc comment already promises for
/// `list_tools`/`call_tool` — now `suggested_next` filtering shares that
/// guarantee too instead of running a parallel, driftable computation.
pub(crate) fn is_tool_available(
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
    tool: &str,
) -> bool {
    router.has_route(tool)
}

pub(crate) fn filter_suggested_next(
    sn: Option<SuggestedNext>,
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
) -> Option<SuggestedNext> {
    match &sn {
        Some(s) if !is_tool_available(router, &s.tool) => None,
        _ => sn,
    }
}

/// Typed `{"error": {"code","message","recoverable"}}` envelope.
pub(crate) fn error_output(code: &str, message: &str, recoverable: bool) -> ErrorOutput {
    ErrorOutput {
        error: ErrorDetail {
            code: code.into(),
            message: message.into(),
            recoverable,
        },
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorOutput {
    pub(crate) error: ErrorDetail,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorDetail {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) recoverable: bool,
}

/// Typed not-found envelope for `ResolvedOutcome::not_found`.
pub(crate) fn not_found_error(symbol: &str) -> ErrorOutput {
    error_output(
        "NOT_FOUND",
        &format!("Symbol '{symbol}' not found in index"),
        false,
    )
}

/// Typed `{"error": {"code": "DB_ERROR", ...}}` for the read-connection
/// failure every read-only tool guards against. All tools now emit this
/// shape via `ToolOutcome::error` / `ResolvedOutcome::error`.
pub(crate) fn error_detail(code: &str, message: &str, recoverable: bool) -> ErrorDetail {
    ErrorDetail {
        code: code.into(),
        message: message.into(),
        recoverable,
    }
}
pub(crate) fn db_error<T>(e: impl std::fmt::Display) -> ToolOutcome<T> {
    ToolOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Same as `db_error`, for tools whose success path can also be
/// `Ambiguous` (anything built on `resolve_symbol`).
pub(crate) fn db_error_resolved<T>(e: impl std::fmt::Display) -> ResolvedOutcome<T> {
    ResolvedOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Shared success/error envelope for tools with no ambiguous-name branch
/// (i.e. no `resolve_symbol` call).
///
/// NOT a `#[serde(untagged)]` enum: rmcp 2.2.0's `Json<T>` requires T's
/// JSON Schema to have root `"type": "object"` (`schema_for_output`
/// panics otherwise — an untagged enum's schema is a bare `oneOf`/`anyOf`
/// with no top-level `"type"`). So this is a genuine struct with optional/
/// flattened fields instead. Exactly one of `error` / the flattened `T` is
/// ever `Some` at a time — enforced by only constructing through `error`/
/// `success` below, never a struct literal — which reproduces the exact
/// same wire shape tools emitted as a bare JSON string before this type
/// existed (`{"error": {...}}` or `T`'s fields directly at the root).
#[derive(Serialize, JsonSchema)]
pub(crate) struct ToolOutcome<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
}

impl<T> ToolOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        ToolOutcome {
            error: Some(detail),
            success: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ToolOutcome {
            error: None,
            success: Some(value),
        }
    }

    /// Bridges `edit_symbol` (which resolves a name first) into the same
    /// `ResolvedOutcome` envelope as other `resolve_symbol`-based tools.
    pub(crate) fn into_resolved(self) -> ResolvedOutcome<T> {
        if let Some(detail) = self.error {
            ResolvedOutcome::error(detail)
        } else if let Some(value) = self.success {
            ResolvedOutcome::success(value)
        } else {
            ResolvedOutcome::error(error_detail("INTERNAL", "empty ToolOutcome", false))
        }
    }
}

/// Structured, machine-checkable hint attached to a tool result whose
/// literal content (empty list / not-found) could otherwise be misread as
/// proof of absence. `class` lets a safety gate branch without parsing
/// `message`; `message` is the human-readable explanation. Design mirrors
/// zzet/gortex's `ZeroEdgeCaveat` (Apache-2.0) — reimplemented against
/// CALM's own resolver shape, not a line-for-line port.
#[derive(Serialize, JsonSchema)]
pub(crate) struct Caveat {
    pub(crate) class: &'static str,
    pub(crate) message: String,
}

impl Caveat {
    /// The queried symbol did not resolve to anything in the index at all
    /// — the most common cause of an unpleasant "0 results" surprise, and
    /// almost always a typo, wrong case, or a file in an excluded path
    /// rather than the symbol genuinely not existing.
    pub(crate) fn not_found(symbol: &str) -> Self {
        Caveat {
            class: "not_found",
            message: format!(
                "no symbol named '{symbol}' is in the index — likely a typo, wrong \
                 case, or the file lives in an excluded path (target/, node_modules/, \
                 dist/, build/, __pycache__/, venv/, legacy/, dotdirs). Run \
                 search(kind=\"hybrid\") to find the exact name before concluding it \
                 doesn't exist — a not-found result here is not proof the symbol is \
                 unused or absent from the codebase."
            ),
        }
    }

    /// The symbol resolved, but the specific edge/usage query on it came
    /// back with zero rows. Distinct from `not_found`: the symbol is real,
    /// but static analysis may simply not see how it's reached (dynamic
    /// dispatch, reflection, string-based invocation, or a public API
    /// consumed outside this repo).
    pub(crate) fn no_direct_usage(symbol: &str) -> Self {
        Caveat {
            class: "no_direct_usage",
            message: format!(
                "'{symbol}' has zero direct callers in the index. This can mean \
                 genuine dead code, but it can also mean call sites use dynamic \
                 dispatch, reflection, or string-based invocation that static \
                 analysis can't resolve, or that '{symbol}' is a public API consumed \
                 outside this repo. Do not treat this as proof of no usage without \
                 also checking dependencies() and the symbol's exported visibility."
            ),
        }
    }

    /// Some, but not all, of a `symbols_batch` call's requested
    /// `qualified_names` matched nothing in the index. Names the first
    /// few missing ids so the caller doesn't have to diff the request
    /// against `results` to see which ones failed.
    pub(crate) fn batch_some_not_found(missing: &[String]) -> Self {
        let sample: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
        Caveat {
            class: "batch_some_not_found",
            message: format!(
                "{} of the requested qualified_names were not found in the index \
                 (e.g. {}). symbols_batch does no fuzzy matching — a near-miss id \
                 comes back found:false rather than silently substituting the \
                 closest name. Run search(kind=\"hybrid\") to get the exact \
                 qualified_name for each missing entry.",
                missing.len(),
                sample.join(", "),
            ),
        }
    }
}

/// Same as `ToolOutcome<T>`, plus the `ambiguous` branch every
/// `resolve_symbol`-based tool can also produce — same flatten-based,
/// root-`type:object` reasoning as `ToolOutcome<T>` above.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ResolvedOutcome<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    ambiguous: Option<AmbiguousResult>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
    /// Advisory hint on an empty/not-found result. Never set alongside a
    /// populated `success` unless a tool opts in via `with_caveat` (e.g.
    /// `callers` on zero direct callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    caveat: Option<Caveat>,
}

impl<T> ResolvedOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        ResolvedOutcome {
            error: Some(detail),
            ambiguous: None,
            success: None,
            caveat: None,
        }
    }

    /// Bridges the existing `SymbolResolution` match arms: `NotFound`/
    /// `Ambiguous` map onto their typed shape here. `Found` is left to the
    /// caller — it needs tool-specific work (`track_symbol`, health
    /// lookups, ...) that doesn't belong in a generic helper.
    pub(crate) fn not_found(symbol: &str) -> Self {
        let mut out = Self::error(not_found_error(symbol).error);
        out.caveat = Some(Caveat::not_found(symbol));
        out
    }

    pub(crate) fn ambiguous(candidates: &[CandidateRow]) -> Self {
        ResolvedOutcome {
            error: None,
            ambiguous: Some(to_ambiguous(candidates)),
            success: None,
            caveat: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ResolvedOutcome {
            error: None,
            ambiguous: None,
            success: Some(value),
            caveat: None,
        }
    }

    /// Attaches an advisory caveat to an already-built success result —
    /// e.g. `callers` on a resolved symbol with zero direct callers. Never
    /// overrides `error`/`ambiguous`; only meaningful after `success`.
    pub(crate) fn with_caveat(mut self, caveat: Caveat) -> Self {
        self.caveat = Some(caveat);
        self
    }
}

// ---------------------------------------------------------------------------
// Ambiguity Contract — shared symbol resolver
// ---------------------------------------------------------------------------
//
// `symbols.name` is not unique: the same bare name can appear in many files,
// or more than once in one file (distinct classes' methods). Tools that take
// a bare `symbol` name must not silently pick one match via `LIMIT 1` — per
// CONTRACTS.md they return `AmbiguousResult` instead when the name has
// multiple matches and no `path` was given to disambiguate.

const MAX_AMBIGUOUS_CANDIDATES: usize = 10;

#[derive(Serialize, JsonSchema)]
pub(crate) struct AmbiguousCandidate {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) class_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) caller_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AmbiguousResult {
    pub(crate) ambiguous: bool,
    /// Total candidates matched before the display cap of
    /// `MAX_AMBIGUOUS_CANDIDATES`. `truncated` is `true` when `total >
    /// candidates.len()`, telling the caller there are more matches than
    /// shown and to narrow with `path`/`line` — the list is never silently
    /// presented as the complete set.
    pub(crate) total: usize,
    pub(crate) truncated: bool,
    pub(crate) candidates: Vec<AmbiguousCandidate>,
}

/// One `symbols` row matched by a bare-name (+ optional path) lookup.
/// Carries enough columns to populate either a concrete tool output (e.g.
/// `SymbolInfoOutput`) or an `AmbiguousCandidate` when the lookup turns out
/// to match more than one row.
pub(crate) struct CandidateRow {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    pub(crate) signature: String,
    pub(crate) docstring: String,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) language: String,
    pub(crate) class_context: Option<String>,
    pub(crate) is_entry_point: bool,
    pub(crate) is_test: bool,
    pub(crate) coreness: Option<i64>, // from symbols.coreness column
    pub(crate) boundary_ambiguous: bool,
}

impl CandidateRow {
    pub(crate) fn to_symbol_info(&self) -> SymbolInfoOutput {
        SymbolInfoOutput {
            name: self.name.clone(),
            qualified_name: self.qualified_name.clone(),
            kind: self.kind.clone(),
            path: self.path.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            // Extracted verbatim from source at index time — a default
            // parameter value or doc-comment example can embed a real secret,
            // so this must be redacted the same as `source()`'s body text.
            signature: Some(calm_core::sanitize::sanitize_source_output(&self.signature))
                .filter(|s| !s.is_empty()),
            docstring: Some(calm_core::sanitize::sanitize_source_output(&self.docstring))
                .filter(|s| !s.is_empty()),
            caller_count: self.caller_count,
            is_hub: self.is_hub,
            coreness: None, // set by handler based on edges_ready
            health: None,
            suggested_next: None,
        }
    }

    pub(crate) fn to_ambiguous_candidate(&self) -> AmbiguousCandidate {
        AmbiguousCandidate {
            name: self.name.clone(),
            path: self.path.clone(),
            kind: self.kind.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            class_context: self.class_context.clone(),
            caller_count: Some(self.caller_count),
            language: Some(self.language.clone()).filter(|s| !s.is_empty()),
            signature: Some(self.signature.clone()).filter(|s| !s.is_empty()),
        }
    }
}

/// All `symbols` rows matching `name` (and `path`, when given). Unlike the
/// old per-tool `LIMIT 1` queries, this returns every match so callers can
/// detect ambiguity instead of guessing.
pub(crate) fn resolve_symbol_candidates(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
) -> rusqlite::Result<Vec<CandidateRow>> {
    let sql = if path.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 AND path = ?2 ORDER BY path, line_start"
    } else {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 ORDER BY path, line_start"
    };

    // audit F9: `?` on both statement-level failures below (a genuine DB/
    // schema problem) — a single malformed *row* still doesn't kill the
    // whole result set (see the `filter_map` further down, deliberately
    // unchanged), only a failure to even prepare/execute the query does.
    let mut stmt = conn.prepare(sql).inspect_err(|e| {
        tracing::warn!("resolve_symbol_candidates: prepare failed for {name:?}: {e}");
    })?;

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<CandidateRow> {
        Ok(CandidateRow {
            name: row.get(0)?,
            qualified_name: row.get(1)?,
            kind: row.get(2)?,
            path: row.get(3)?,
            line_start: row.get(4)?,
            line_end: row.get(5)?,
            signature: row.get(6)?,
            docstring: row.get(7)?,
            caller_count: row.get(8)?,
            is_hub: row.get::<_, i64>(9)? != 0,
            language: row.get(10)?,
            class_context: row.get(11)?,
            is_entry_point: row.get::<_, i64>(12)? != 0,
            is_test: row.get::<_, i64>(13)? != 0,
            coreness: row.get(14)?,
            boundary_ambiguous: row.get::<_, i64>(15)? != 0,
        })
    };

    let rows = if let Some(path) = path {
        stmt.query_map(rusqlite::params![name, path], map_row)
    } else {
        stmt.query_map(rusqlite::params![name], map_row)
    };

    match rows {
        Ok(iter) => Ok(iter.filter_map(|r| r.ok()).collect()),
        Err(e) => {
            tracing::warn!("resolve_symbol_candidates: query_map failed for {name:?}: {e}");
            Err(e)
        }
    }
}
pub(crate) enum SymbolResolution {
    NotFound,
    Ambiguous(Vec<CandidateRow>),
    Found(Box<CandidateRow>),
}

/// Resolve a bare symbol name (+ optional path, + optional disambiguating
/// `line`) to exactly one row. `path` narrows the candidate set (see
/// `resolve_symbol_candidates`) but does not by itself guarantee a unique
/// match — `name` + `path` is not a DB-enforced unique key (only
/// `qualified_name` is), so e.g. two same-named functions in the same file
/// (a common shape in this codebase: `#[cfg(feature = "x")]` real impl vs.
/// `#[cfg(not(feature = "x"))]` stub, both named identically) still resolve
/// as ambiguous even with `path` set. `line` breaks that tie: when given, it
/// narrows to whichever candidate's `[line_start, line_end]` contains it —
/// exactly the range every `Ambiguous` response already echoes back per
/// candidate, so a caller that got `ambiguous: true` can retry once with
/// the `line_start` of the one it meant. A `line` that matches none of the
/// candidates is ignored (falls back to the unnarrowed set) rather than
/// forcing `NotFound` — a stale/wrong hint should degrade to the old
/// behavior, not make an otherwise-resolvable symbol disappear.
pub(crate) fn resolve_symbol(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
    line: Option<i64>,
) -> rusqlite::Result<SymbolResolution> {
    let mut candidates = resolve_symbol_candidates(conn, name, path)?;
    if let Some(line) = line {
        let in_range = |c: &CandidateRow| c.line_start <= line && line <= c.line_end;
        if candidates.iter().any(in_range) {
            candidates.retain(in_range);
        }
    }
    Ok(if candidates.is_empty() {
        SymbolResolution::NotFound
    } else if candidates.len() == 1 {
        SymbolResolution::Found(Box::new(candidates.remove(0)))
    } else {
        SymbolResolution::Ambiguous(candidates)
    })
}

/// Ambient "related notes" surfaced automatically on `edit_context`/
/// `locate` (docs/superskills/specs/2026-07-11-superskills-inspired-features.md
/// #3 v2) — closes 3 audit findings against the original design:
/// (1) specificity-gating: a hub file's notes only qualify if their text
/// mentions `symbol_name`, so a stale file-level note doesn't bury every
/// symbol in a large/important file forever; a non-hub file keeps the
/// looser file-level match (low noise risk there by construction).
/// (2) fail-open: any lookup error returns an empty list, never propagates
/// — mirrors `capture_refs`'s own "best-effort" precedent in this same
/// module family, so a bug here can never break `edit_context`/`locate`
/// themselves. (3) content-safety: a note whose text trips
/// `injection_warning` is dropped from this *automatic* surface — it
/// remains fully visible via an explicit `recall()` call, where the
/// existing Stage-3 "source is untrusted" wariness already applies —
/// and (audit F7) `recall` now carries an explicit per-note
/// `content_warning` field alongside that wariness, not just the reader's
/// own judgment.
impl CalmServer {
    /// Ambient "related notes" surfaced automatically on `edit_context`/
    /// `locate` (docs/superskills/specs/2026-07-11-superskills-inspired-features.md
    /// #3 v2) — closes 3 audit findings against the original design:
    /// (1) specificity-gating: a hub file's notes only qualify if their text
    /// mentions `symbol_name`, so a stale file-level note doesn't bury every
    /// symbol in a large/important file forever; a non-hub file keeps the
    /// looser file-level match (low noise risk there by construction).
    /// (2) fail-open: any lookup error returns an empty list, never propagates
    /// — mirrors `capture_refs`'s own "best-effort" precedent in this same
    /// module family, so a bug here can never break `edit_context`/`locate`
    /// themselves. (3) content-safety: a note whose text trips
    /// `injection_warning` is dropped from this *automatic* surface — it
    /// remains fully visible via an explicit `recall()` call, where the
    /// existing Stage-3 "source is untrusted" wariness already applies —
    /// and (audit F7) `recall` now carries an explicit per-note
    /// `content_warning` field alongside that wariness, not just the
    /// reader's own judgment.
    pub(crate) fn related_notes(
        &self,
        conn: &rusqlite::Connection,
        path: &str,
        symbol_name: &str,
        is_hub: bool,
    ) -> Vec<RelatedNoteOutput> {
        const CAP: usize = 2;
        // Overfetch: hub-gating and injection-filtering below can both drop
        // candidates, so asking for exactly CAP would under-return once either
        // filter removes anything.
        const OVERFETCH: usize = 8;

        let candidates = match calm_core::memory::notes_for_path(conn, path, OVERFETCH) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("related_notes: lookup failed for {path}: {e}");
                return Vec::new();
            }
        };

        // Plan 3 §3.5(d): loaded once per call, same reasoning as `recall`'s
        // own batch load — this is the ambient/passive-injection surface
        // (an agent doesn't ask for these, they just show up in
        // edit_context/locate), so unlike `recall` (which reports
        // "mismatch" explicitly and lets the agent judge), a note that
        // fails MAC verification is dropped here rather than surfaced —
        // silently trusting a possibly-forged note into a passive channel
        // is the exact risk this feature exists to close. `None` (key
        // unreadable) degrades to treating every candidate as unverifiable
        // — NOT the same as verified, so nothing here gets dropped for that
        // reason alone; only an explicit MAC mismatch drops a note.
        let mac_key = calm_core::memory::load_or_create_mac_key(&self.project_root).ok();

        let mut out = Vec::with_capacity(CAP);
        for (topic, content, content_mac) in candidates {
            if out.len() >= CAP {
                break;
            }
            if let Some(key) = &mac_key {
                let integrity = calm_core::memory::verify_integrity(
                    key,
                    &topic,
                    &content,
                    content_mac.as_deref(),
                );
                if integrity == "mismatch" {
                    tracing::warn!(
                        "related_notes: dropping topic {topic:?} — content_mac mismatch \
                         (possible out-of-band edit)"
                    );
                    continue;
                }
            }
            let mentions_symbol = !symbol_name.is_empty() && content.contains(symbol_name);
            if is_hub && !mentions_symbol {
                continue;
            }
            if injection_warning(&content).is_some() {
                continue;
            }
            let staleness =
                match calm_core::memory::check_staleness(conn, &self.project_root, &topic) {
                    Ok(stale) if stale.is_empty() => "fresh",
                    Ok(stale) if stale.iter().any(|s| s.status == "deleted") => "gone",
                    Ok(_) => "stale",
                    Err(_) => "unknown",
                };
            let excerpt: String = content.chars().take(160).collect();
            out.push(RelatedNoteOutput {
                topic,
                excerpt,
                specificity: if mentions_symbol { "symbol" } else { "file" },
                staleness,
            });
        }
        out
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RelatedNoteOutput {
    pub(crate) topic: String,
    /// First 160 characters of the note's content — not the full note;
    /// call `recall(topic=...)` for the whole thing.
    pub(crate) excerpt: String,
    /// `"symbol"` when the note's text mentions the resolved symbol's bare
    /// name (higher trust), `"file"` when it only matched at file level
    /// (the note references this file but may be about a different symbol
    /// in it — calibrate trust accordingly).
    pub(crate) specificity: &'static str,
    /// `"fresh"` / `"stale"` / `"gone"` (same convention as `recall`'s
    /// per-note staleness) / `"unknown"` when the staleness check itself
    /// failed (fail-open: the note still surfaces, just without a
    /// confident freshness read).
    pub(crate) staleness: &'static str,
}

/// Build the typed `AmbiguousResult` payload for `ResolvedOutcome::ambiguous`.
pub(crate) fn to_ambiguous(candidates: &[CandidateRow]) -> AmbiguousResult {
    let total = candidates.len();
    let shown = candidates
        .iter()
        .take(MAX_AMBIGUOUS_CANDIDATES)
        .map(CandidateRow::to_ambiguous_candidate)
        .collect();
    AmbiguousResult {
        ambiguous: true,
        total,
        truncated: total > MAX_AMBIGUOUS_CANDIDATES,
        candidates: shown,
    }
}

// ---------------------------------------------------------------------------
// Frontier computation helper (for session_context)
// ---------------------------------------------------------------------------

/// Runs `{sql_prefix} (?, ?, ...) AND from_path IS NOT NULL` in chunks of ≤999
/// to stay within SQLite's SQLITE_LIMIT_VARIABLE_NUMBER, accumulating distinct
/// `from_path` values into `out`.
pub(crate) fn query_paths_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    params: &[String],
    out: &mut std::collections::HashSet<String>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders}) AND from_path IS NOT NULL");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.insert(r);
                    }
                });
        }
    }
}

// ---------------------------------------------------------------------------
// Personalization boost helper (for search/locate)
// ---------------------------------------------------------------------------

/// Runs `{sql_prefix} (?, ?, ...){sql_suffix}` in chunks of ≤999 to stay
/// within SQLite's SQLITE_LIMIT_VARIABLE_NUMBER, accumulating `(a, b)` row
/// pairs — the two-column counterpart to `query_paths_chunked` above, needed
/// here because `compute_proximity_boosts` must know *which* explored anchor
/// a candidate connects to (to look up that anchor's own recency), not just
/// whether one exists.
fn query_pairs_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    sql_suffix: &str,
    params: &[String],
    out: &mut Vec<(String, String)>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders}){sql_suffix}");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.push(r);
                    }
                });
        }
    }
}

/// Same as `query_pairs_chunked` but the second column (`call_edges.from_path`/
/// `to_path`) is nullable — a call edge's enclosing file isn't always known.
fn query_symbol_path_pairs_chunked(
    conn: &rusqlite::Connection,
    sql_prefix: &str,
    params: &[String],
    out: &mut Vec<(String, Option<String>)>,
) {
    const CHUNK: usize = 999;
    for chunk in params.chunks(CHUNK) {
        let placeholders = chunk
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("{sql_prefix} ({placeholders})");
        if let Ok(mut stmt) = conn.prepare(&sql) {
            let _ = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map(|rows| {
                    for r in rows.flatten() {
                        out.push(r);
                    }
                });
        }
    }
}

/// Per-path proximity boost in `(0.0, 1.0]`, derived from this session's
/// explored files/symbols: a candidate path gets the *best* (most recent)
/// connection found among —
/// - files adjacent to an explored file via `import_edges`, either direction
/// - files containing a caller of an explored symbol, via `call_edges`
///
/// Weight decays with `now - last_touch` (in tool-calls, not wall-clock) via
/// `1.0 / (1.0 + distance)`, so a file explored on the immediately preceding
/// call outweighs one from 20 calls ago. Paths with no connection at all are
/// simply absent from the result (implicit boost 0), not zero-valued
/// entries — callers should use `.get(path)` and treat a miss as no boost.
fn compute_proximity_boosts(
    conn: &rusqlite::Connection,
    explored_files: &std::collections::HashMap<String, u64>,
    explored_symbols: &std::collections::HashMap<String, u64>,
    now: u64,
) -> std::collections::HashMap<String, f64> {
    let mut boosts: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let decay = |touch: u64| 1.0 / (1.0 + now.saturating_sub(touch) as f64);
    let bump = |boosts: &mut std::collections::HashMap<String, f64>, path: String, w: f64| {
        let entry = boosts.entry(path).or_insert(0.0);
        if w > *entry {
            *entry = w;
        }
    };

    if !explored_files.is_empty() {
        let anchors: Vec<String> = explored_files.keys().cloned().collect();

        // Files that import an explored file.
        let mut importers = Vec::new();
        query_pairs_chunked(
            conn,
            "SELECT from_path, to_path FROM import_edges WHERE to_path IN",
            " AND from_path IS NOT NULL",
            &anchors,
            &mut importers,
        );
        for (from_path, to_path) in &importers {
            if let Some(&touch) = explored_files.get(to_path) {
                bump(&mut boosts, from_path.clone(), decay(touch));
            }
        }

        // Files an explored file imports.
        let mut imported = Vec::new();
        query_pairs_chunked(
            conn,
            "SELECT from_path, to_path FROM import_edges WHERE from_path IN",
            " AND to_path IS NOT NULL",
            &anchors,
            &mut imported,
        );
        for (from_path, to_path) in &imported {
            if let Some(&touch) = explored_files.get(from_path) {
                bump(&mut boosts, to_path.clone(), decay(touch));
            }
        }
    }

    if !explored_symbols.is_empty() {
        let anchors: Vec<String> = explored_symbols.keys().cloned().collect();

        // Files containing a caller of an explored symbol.
        let mut callers = Vec::new();
        query_symbol_path_pairs_chunked(
            conn,
            "SELECT to_symbol, from_path FROM call_edges WHERE to_symbol IN",
            &anchors,
            &mut callers,
        );
        for (symbol, from_path) in &callers {
            if let (Some(&touch), Some(path)) = (explored_symbols.get(symbol), from_path) {
                bump(&mut boosts, path.clone(), decay(touch));
            }
        }
    }

    boosts
}

/// The pure score math behind `CalmServer::apply_personalization_boost`
/// (Plan 3 §3.2), extracted as a free `&self`-free function so it's
/// directly unit-testable without a `CalmServer`/DB fixture. Every
/// result's `score` is min-max normalized to `[0,1]` across `results`
/// FIRST, then `weight * boost` (0 for a result with no entry in `boosts`)
/// is added — normalizing first matters because raw scores are on wildly
/// different scales across search kinds (RRF top-1 ≈ 0.05-0.17, grep/file
/// = 1.0 constant, bm25 1-30+, semantic 0-1); adding a fixed-magnitude
/// boost directly to the raw score (the pre-Plan-3 behavior) let it swamp
/// RRF results outright while doing nothing at all on bm25 — the exact
/// contradiction of `apply_personalization_boost`'s own "never overriding
/// a strong match" doc promise.
///
/// `compute_proximity_boosts` bounds every `boosts` value to `(0.0, 1.0]`,
/// which makes that promise an actual invariant here: two results whose
/// normalized scores differ by more than `weight` can never have their
/// relative order flipped by any boost, since the largest a boost can ever
/// move a score is `weight * 1.0 = weight` — see
/// `personalization_tests::normalize_then_boost_never_flips_a_large_gap`.
///
/// Returns `false` (and leaves `results` completely untouched — not even
/// re-normalized) when `boosts` is empty or none of its keys match any
/// path in `results`, preserving `apply_personalization_boost`'s "no-op
/// when nothing to boost" guarantee. Otherwise rewrites every result's
/// `score` (not just the boosted ones — `score`'s scale changes from raw
/// to normalized, a deliberate, documented trade-off: it was already
/// opaque to callers/agents, and `personalized: true` is the field that
/// reports this happened), re-sorts descending by the new score, and
/// returns `true`.
fn normalize_then_boost(
    results: &mut [calm_core::search::SearchResult],
    boosts: &std::collections::HashMap<String, f64>,
    weight: f64,
) -> bool {
    if boosts.is_empty() || !results.iter().any(|r| boosts.contains_key(&r.path)) {
        return false;
    }

    let (min, max) = results
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), r| {
            (lo.min(r.score), hi.max(r.score))
        });
    let range = max - min;
    let normalize = |s: f64| -> f64 { if range > 0.0 { (s - min) / range } else { 0.5 } };

    for r in results.iter_mut() {
        let boost = boosts.get(&r.path).copied().unwrap_or(0.0);
        r.score = normalize(r.score) + weight * boost;
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    true
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct SymbolInfoOutput {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) docstring: Option<String>,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) coreness: Option<i64>, // null when edges not yet built; 0 = isolated; >0 = k-core depth
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) health: Option<HealthOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CallerEntry {
    /// `path::name` (or `path::Class::name`) — the enclosing file path is
    /// always the substring before the first `::`, since every producer
    /// (tree-sitter extraction in `indexer::pipeline`, SQL indexer, SCIP
    /// overlay ingestion) derives both from the same `rel`/`from_path`
    /// value at the point the symbol/edge is created. A separate `path`
    /// field used to duplicate this verbatim on every entry — pure waste
    /// on a hub symbol with many callers in the same file; split on the
    /// first `::` if you need it standalone.
    pub(crate) symbol: String,
    pub(crate) edge_confidence: String,
    /// `"call"` or `"reference"` (SQL view/proc reading a table via
    /// FROM/JOIN) — see `call_edges.edge_kind`. Lets a consumer tell a real
    /// invocation apart from a mere read without misreading a JOIN as a
    /// function call.
    pub(crate) edge_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

/// Deterministic fingerprint of a caller/callee result set, for
/// `if_none_match`/`etag` conditional-fetch (same pattern as `source`'s own
/// etag — see `range_checksum`/`hash_content`). Includes `preview` (not just
/// the SQL columns) so a call site whose *line content* changed — but not
/// its confidence/path/line-number — still gets a fresh etag; two calls
/// with the same set of `(symbol, edge_confidence, edge_kind, line,
/// preview)` tuples in the same order are guaranteed to hash identically.
/// No separate `path` component: `symbol` is always `path::name` (or
/// `path::Class::name`), so any change to the path is already a change to
/// `symbol` — hashing both would be redundant, not more discriminating.
pub(crate) fn hash_caller_entries<'a>(
    entries: impl IntoIterator<Item = &'a CallerEntry>,
) -> String {
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&e.symbol);
        buf.push('\u{1}');
        buf.push_str(&e.edge_confidence);
        buf.push('\u{1}');
        buf.push_str(&e.edge_kind);
        buf.push('\u{1}');
        if let Some(l) = e.line {
            buf.push_str(&l.to_string());
        }
        buf.push('\u{1}');
        if let Some(p) = &e.preview {
            buf.push_str(p);
        }
        buf.push('\u{2}');
    }
    calm_core::indexer::pipeline::hash_content(&buf)
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct CalleeEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) edge_confidence: String,
    /// See `CallerEntry::edge_kind`.
    pub(crate) edge_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

/// `CalleeEntry` counterpart of `hash_caller_entries` — same rationale and
/// field set, just for `callees`'s direction.
pub(crate) fn hash_callee_entries<'a>(
    entries: impl IntoIterator<Item = &'a CalleeEntry>,
) -> String {
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&e.symbol);
        buf.push('\u{1}');
        buf.push_str(&e.path);
        buf.push('\u{1}');
        buf.push_str(&e.edge_confidence);
        buf.push('\u{1}');
        buf.push_str(&e.edge_kind);
        buf.push('\u{1}');
        if let Some(l) = e.line {
            buf.push_str(&l.to_string());
        }
        buf.push('\u{1}');
        if let Some(p) = &e.preview {
            buf.push_str(p);
        }
        buf.push('\u{2}');
    }
    calm_core::indexer::pipeline::hash_content(&buf)
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TransitiveEntry {
    pub(crate) symbol: String,
    pub(crate) path: String,
    pub(crate) depth: i64,
    pub(crate) edge_confidence: String,
}

#[derive(Clone, Copy)]
pub(crate) enum EdgeDirection {
    Callers,
    Callees,
}

/// BFS over `call_edges` beyond the direct neighbors, shared by `callers` and
/// `callees` when `transitive: true`. Bounded by `max_depth` and a wall-clock
/// timeout so a hub symbol can't blow up the response. Returns `(entries,
/// capped)` — `capped` is true when the BFS stopped early (depth limit hit
/// with a non-empty frontier remaining, or the timeout fired) rather than
/// because there was nothing left to explore.
pub(crate) fn transitive_bfs(
    conn: &rusqlite::Connection,
    start_qualified_name: &str,
    direction: EdgeDirection,
    max_depth: usize,
    timeout_ms: u64,
) -> (Vec<TransitiveEntry>, bool) {
    let sql = match direction {
        EdgeDirection::Callers => {
            "SELECT from_symbol, from_path, edge_confidence FROM call_edges WHERE to_symbol = ?1"
        }
        EdgeDirection::Callees => {
            "SELECT to_symbol, to_path, edge_confidence FROM call_edges WHERE from_symbol = ?1"
        }
    };
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return (vec![], false),
    };

    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);

    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    visited.insert(start_qualified_name.to_string());
    let mut frontier = vec![start_qualified_name.to_string()];
    let mut results = Vec::new();
    let mut depth = 0usize;
    let mut capped = false;

    while depth < max_depth && !frontier.is_empty() {
        if start.elapsed() > deadline {
            capped = true;
            break;
        }
        depth += 1;
        let mut next_frontier = Vec::new();
        for sym in &frontier {
            let rows = stmt.query_map(rusqlite::params![sym], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1).unwrap_or_default(),
                    row.get::<_, String>(2)?,
                ))
            });
            let Ok(rows) = rows else { continue };
            for (sym_name, sym_path, edge_confidence) in rows.filter_map(|r| r.ok()) {
                if visited.insert(sym_name.clone()) {
                    results.push(TransitiveEntry {
                        symbol: sym_name.clone(),
                        path: sym_path,
                        depth: depth as i64,
                        edge_confidence,
                    });
                    next_frontier.push(sym_name);
                }
            }
        }
        if !capped && depth >= max_depth && !next_frontier.is_empty() {
            capped = true;
        }
        frontier = next_frontier;
    }

    (results, capped)
}

/// Shared caller-count risk tiering used by `edit_context`, `diff_impact`,
/// and `edit_lines`/`edit_symbol`'s risk gate — previously three independent
/// copies of the same `>10`/`>3` thresholds had drifted apart as separate
/// inline `if`/`else` chains. Centralized here so all three read the same
/// policy and can't silently diverge again.
pub(crate) fn risk_level_from_caller_count(caller_count: i64) -> &'static str {
    if caller_count > 10 {
        "high"
    } else if caller_count > 3 {
        "medium"
    } else {
        "low"
    }
}

const CALL_SITE_PREVIEW_MAX_CHARS: usize = 160;

/// Read the trimmed source line at `line` (1-indexed) from `project_root/path`
/// for a `CallerEntry`/`CalleeEntry` preview. Best-effort: missing files, a
/// line number past EOF, or a `None` line all just yield `None` rather than
/// an error — a preview is a convenience, not load-bearing.
/// Same semantics as `line_preview`, but reads each distinct file at most
/// once regardless of how many `items` reference it (audit F11) — a hub
/// symbol's `callers`/`callees` rows routinely repeat the same file dozens
/// of times, each of which used to be its own full-file `read_to_string`.
/// Returns previews in the same order as `items`.
pub(crate) fn line_previews_batched(
    project_root: &std::path::Path,
    items: &[(String, Option<i64>)],
) -> Vec<Option<String>> {
    let mut file_cache: std::collections::HashMap<&str, Option<String>> =
        std::collections::HashMap::new();
    items
        .iter()
        .map(|(path, line)| {
            let line = (*line)?;
            if line < 1 {
                return None;
            }
            let content = file_cache
                .entry(path.as_str())
                .or_insert_with(|| std::fs::read_to_string(project_root.join(path)).ok())
                .as_ref()?;
            let raw = content.lines().nth((line - 1) as usize)?.trim();
            if raw.is_empty() {
                return None;
            }
            let sanitized = calm_core::sanitize::sanitize_source_output(raw);
            if sanitized.chars().count() > CALL_SITE_PREVIEW_MAX_CHARS {
                Some(format!(
                    "{}…",
                    sanitized
                        .chars()
                        .take(CALL_SITE_PREVIEW_MAX_CHARS)
                        .collect::<String>()
                ))
            } else {
                Some(sanitized)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tool 8: dependencies
// ---------------------------------------------------------------------------

#[cfg(test)]
mod personalization_tests {
    use super::*;
    use std::collections::HashMap;

    fn test_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        calm_core::db::schema::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn empty_explored_state_yields_no_boosts() {
        let conn = test_conn();
        let boosts = compute_proximity_boosts(&conn, &HashMap::new(), &HashMap::new(), 5);
        assert!(boosts.is_empty());
    }

    #[test]
    fn boosts_file_that_imports_an_explored_file() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('a.rs', 'b.rs', 'b')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("b.rs".to_string(), 3u64); // touched at tool-call 3

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 4);
        // now(4) - touch(3) = 1 -> decay = 1/(1+1) = 0.5
        assert_eq!(boosts.get("a.rs"), Some(&0.5));
    }

    #[test]
    fn boosts_file_an_explored_file_imports_too() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('a.rs', 'b.rs', 'b')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("a.rs".to_string(), 3u64);

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 4);
        assert_eq!(boosts.get("b.rs"), Some(&0.5));
    }

    #[test]
    fn more_recent_touch_decays_less_than_older_touch() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('recent.rs', 'anchor.rs', 'anchor')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('stale.rs', 'old_anchor.rs', 'old_anchor')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("anchor.rs".to_string(), 9u64); // touched 1 call ago
        explored_files.insert("old_anchor.rs".to_string(), 0u64); // touched 10 calls ago

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 10);
        let recent = boosts["recent.rs"];
        let stale = boosts["stale.rs"];
        assert!(
            recent > stale,
            "recently-touched anchor must decay less: recent={recent} stale={stale}"
        );
    }

    #[test]
    fn boosts_file_containing_caller_of_an_explored_symbol() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, from_path, to_path) \
             VALUES ('caller_fn', 'target_fn', 'caller_file.rs', 'target_file.rs')",
            [],
        )
        .unwrap();
        let mut explored_symbols = HashMap::new();
        explored_symbols.insert("target_fn".to_string(), 2u64);

        let boosts = compute_proximity_boosts(&conn, &HashMap::new(), &explored_symbols, 2);
        // now(2) - touch(2) = 0 -> decay = 1/(1+0) = 1.0 (just-touched anchor)
        assert_eq!(boosts.get("caller_file.rs"), Some(&1.0));
    }

    #[test]
    fn takes_the_best_boost_when_multiple_anchors_connect_to_the_same_path() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('shared.rs', 'old.rs', 'old')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('shared.rs', 'fresh.rs', 'fresh')",
            [],
        )
        .unwrap();
        let mut explored_files = HashMap::new();
        explored_files.insert("old.rs".to_string(), 0u64);
        explored_files.insert("fresh.rs".to_string(), 5u64);

        let boosts = compute_proximity_boosts(&conn, &explored_files, &HashMap::new(), 5);
        // Must take the fresher connection's weight (1.0), not the older one's (1/6).
        assert_eq!(boosts.get("shared.rs"), Some(&1.0));
    }

    // Plan 3 §3.2 — tests for the pure `normalize_then_boost` score math
    // (not `compute_proximity_boosts`, which the tests above already
    // cover and this doesn't touch).

    fn sr(path: &str, score: f64) -> calm_core::search::SearchResult {
        calm_core::search::SearchResult {
            name: path.to_string(),
            qualified_name: path.to_string(),
            path: path.to_string(),
            kind: None,
            line_start: None,
            line_end: None,
            score,
            match_type: "symbol".to_string(),
            snippet: None,
            is_test: false,
        }
    }

    #[test]
    fn normalize_then_boost_leaves_results_untouched_when_no_path_matches() {
        let mut results = vec![sr("unrelated.rs", 0.5)];
        let mut boosts = HashMap::new();
        boosts.insert("other.rs".to_string(), 1.0);
        let adjusted = normalize_then_boost(&mut results, &boosts, 0.15);
        assert!(!adjusted);
        assert_eq!(
            results[0].score, 0.5,
            "score must be left exactly as-is, not even normalized, when nothing matches"
        );
    }

    #[test]
    fn normalize_then_boost_regression_top1_survives_neighbor_boost() {
        // Reproduces the exact scenario from the original audit (F3): an RRF
        // result set where the best match (top-1, score ~0.071) sits far
        // above a "neighbor of an explored file" at rank 8 (score ~0.036) —
        // before Plan 3's normalize-first fix, raw `score += weight * boost`
        // (weight 0.15, boost up to 1.0) let the rank-8 neighbor jump
        // straight to rank 1. Fails on the pre-fix math, passes after.
        let scores = [0.071, 0.065, 0.058, 0.052, 0.047, 0.042, 0.039, 0.036];
        let mut results: Vec<_> = scores
            .iter()
            .enumerate()
            .map(|(i, &score)| sr(&format!("p{i}.rs"), score))
            .collect();

        let mut boosts = HashMap::new();
        boosts.insert("p7.rs".to_string(), 1.0); // rank-8 (0-indexed 7), max boost

        let adjusted = normalize_then_boost(&mut results, &boosts, 0.15);
        assert!(adjusted);
        assert_eq!(
            results[0].path,
            "p0.rs",
            "the strongest original match must stay rank 1 — got order: {:?}",
            results.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn normalize_then_boost_never_flips_a_large_gap() {
        // Property test: 50 random result sets spanning the 4 real score
        // scales (RRF ~0.03-0.2, bm25 1-30, grep/file constant 1.0,
        // semantic/cosine 0-1) plus random boosts in (0,1] — for every
        // pair whose *normalized* scores differ by more than `weight`,
        // boost must never flip their relative order. True by construction
        // once boost is bounded to <=1.0 (the max any boost can move a
        // score is `weight * 1.0`), but tested directly against the real
        // implementation rather than just argued algebraically.
        let weight = 0.15;
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next_u64 = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let next_f64 = |lo: f64, hi: f64, s: &mut dyn FnMut() -> u64| {
            let r = (s() >> 11) as f64 / (1u64 << 53) as f64; // [0,1)
            lo + r * (hi - lo)
        };

        for trial in 0..50 {
            let n = 6;
            let results_seed: Vec<(String, f64)> = (0..n)
                .map(|i| {
                    let score = match i % 4 {
                        0 => next_f64(0.03, 0.2, &mut next_u64),
                        1 => next_f64(1.0, 30.0, &mut next_u64),
                        2 => 1.0,
                        _ => next_f64(0.0, 1.0, &mut next_u64),
                    };
                    (format!("p{trial}_{i}.rs"), score)
                })
                .collect();
            let mut results: Vec<_> = results_seed.iter().map(|(p, s)| sr(p, *s)).collect();

            let (min, max) = results
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), r| {
                    (lo.min(r.score), hi.max(r.score))
                });
            let range = max - min;
            let norm_of = |s: f64| if range > 0.0 { (s - min) / range } else { 0.5 };
            let normalized: Vec<f64> = results.iter().map(|r| norm_of(r.score)).collect();
            let paths: Vec<String> = results.iter().map(|r| r.path.clone()).collect();

            let mut boosts = HashMap::new();
            for path in &paths {
                if next_u64() % 2 == 0 {
                    boosts.insert(path.clone(), next_f64(0.0001, 1.0, &mut next_u64));
                }
            }
            if boosts.is_empty() {
                continue; // guaranteed no-op — nothing to check
            }

            assert!(normalize_then_boost(&mut results, &boosts, weight));
            let rank_of = |path: &str| results.iter().position(|r| r.path == path).unwrap();

            for i in 0..n {
                for j in (i + 1)..n {
                    let gap = (normalized[i] - normalized[j]).abs();
                    if gap <= weight {
                        continue;
                    }
                    let (stronger, weaker) = if normalized[i] > normalized[j] {
                        (i, j)
                    } else {
                        (j, i)
                    };
                    assert!(
                        rank_of(&paths[stronger]) < rank_of(&paths[weaker]),
                        "trial {trial}: normalized gap {gap:.4} > weight {weight} but boost \
                         flipped {} (norm {:.4}) behind {} (norm {:.4})",
                        paths[stronger],
                        normalized[stronger],
                        paths[weaker],
                        normalized[weaker]
                    );
                }
            }
        }
    }
}
