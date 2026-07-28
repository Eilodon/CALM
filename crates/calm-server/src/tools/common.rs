use super::*;

// `LockExt`/`RwLockExt` now live in `crate::sync_ext` (zero coupling to
// `CalmServer`/tool-handler state, and `watcher.rs` needs `RwLockExt` too
// without depending on this tool-handler layer — see that module's doc
// comment). Re-exported here so every existing `use super::common::*;` glob
// import across `tools/*.rs` keeps resolving both traits unchanged.
pub(crate) use crate::sync_ext::{LockExt, RwLockExt};
// Tool-preset / composable-toolset resolution now lives in `toolset.rs`
// (2026-07-28 hotspot split); re-exported so `common::resolve_preset(...)`
// and every `use super::common::*;` glob keep resolving unchanged.
pub(crate) use crate::tools::toolset::*;
// Response envelopes / resolution / suggested helpers (`outcome`) and internal
// query / graph-traversal / boost helpers (`detail`) likewise split out
// (2026-07-28 hotspot split), re-exported so `common::…` refs and every
// `use super::common::*;` glob keep resolving unchanged.
pub(crate) use crate::tools::{detail::*, outcome::*};

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
            oriented: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            enabled_toolsets: Arc::new(RwLock::new(None)),
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
            // Must be a fresh Arc here, NOT inherited via `..self.clone()` —
            // see `CalmServer::oriented`'s own doc comment for why leaving
            // this out would silently share one gate flag across every
            // connection on a shared daemon.
            oriented: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // Fresh per connection — see field doc. NOT inherited via `..self.clone()`.
            enabled_toolsets: Arc::new(RwLock::new(None)),
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

    /// Ownership-entropy for `path` (Phase 0/#2, 2026-07-27 martin/entropy/
    /// churn plan): thin wrapper composing `calm_core::git::commits_with_files_cached`,
    /// `file_signals`, and `ownership_entropy`. Deliberately NOT a second
    /// server-layer cache on top of `commits_with_files_cached`'s own
    /// process-wide cache, since that inner cache is what actually bounds
    /// cost: a cache hit is ~microseconds, and `file_signals`'s per-call
    /// fold over the cached commit list costs only single-digit
    /// milliseconds even against a large repo (measured against a
    /// 60k-commit synthetic history in the same plan's Abductive
    /// Hypothesis 2 gate). Adding a second cache here would only guard
    /// against that already-cheap fold, at the cost of a second staleness
    /// surface to reason about.
    ///
    /// `None` when git is unavailable/timed out, or the file has fewer than
    /// `hotspots.default_min_churn` commits in the window -- same "not
    /// enough signal yet" semantics `ownership_entropy` documents, not an
    /// error.
    pub(crate) fn ownership_entropy_for(&self, path: &str) -> Option<f64> {
        let config = self.config();
        let (commits, git_available) = calm_core::git::commits_with_files_cached(
            &self.project_root,
            &config.hotspots.default_since,
        );
        if !git_available {
            return None;
        }
        let signals = calm_core::git::file_signals(&commits);
        let file_signals = signals.get(path)?;
        calm_core::git::ownership_entropy(file_signals, config.hotspots.default_min_churn as u32)
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

    /// See `SessionLog::elicit_declined` — true when a human already vetoed
    /// this exact `(path, hunk-content fingerprint)` pair this session.
    pub(crate) fn elicit_declined_contains(&self, path: &str, fingerprint: &str) -> bool {
        self.session_log.lock().is_ok_and(|log| {
            log.elicit_declined
                .contains(&(path.to_string(), fingerprint.to_string()))
        })
    }

    /// Records a human veto for this exact `(path, fingerprint)` pair — the
    /// identical retry short-circuits to USER_DECLINED without re-asking.
    pub(crate) fn elicit_declined_insert(&self, path: &str, fingerprint: &str) {
        if let Ok(mut log) = self.session_log.lock() {
            log.elicit_declined
                .insert((path.to_string(), fingerprint.to_string()));
        }
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

    /// Tool names that already satisfy the session-start orientation gate
    /// (`CalmServer::call_tool`, `Config.orientation`) on their own — calling
    /// any of these IS the orientation, so the gate never needs to inject
    /// into, or block around, a call to one of them.
    const ORIENTATION_ADJACENT_TOOLS: &'static [&'static str] =
        &["repo_overview", "indexing_status", "session_context"];

    /// Whether `name` is one of `ORIENTATION_ADJACENT_TOOLS`.
    pub(crate) fn is_orientation_adjacent(name: &str) -> bool {
        Self::ORIENTATION_ADJACENT_TOOLS.contains(&name)
    }

    /// Whether the active (preset-scoped) `tool_router` registers at least
    /// one orientation-adjacent tool. `"block"` mode's only escape hatch is
    /// calling one of them — a preset that excludes the whole `orient`
    /// toolset entirely (e.g. `--preset "security"` alone, or any composed
    /// spec that subtracts it, like `"full,-orient"`) registers none of
    /// them, so a literal block there would refuse every call for the rest
    /// of the connection with no way out at all.
    pub(crate) fn orientation_escape_hatch_available(&self) -> bool {
        self.tool_router
            .list_all()
            .iter()
            .any(|t| Self::is_orientation_adjacent(t.name.as_ref()))
    }

    /// `Config.orientation.mode`, downgraded from `Block` to `Inject` when
    /// `orientation_escape_hatch_available()` is false — see
    /// `calm_core::config::OrientationMode::Block`'s own doc comment for why
    /// a literal block with no escape hatch would deadlock the connection.
    pub(crate) fn effective_orientation_mode(&self) -> calm_core::config::OrientationMode {
        let mode = self.config().orientation.mode;
        if mode == calm_core::config::OrientationMode::Block
            && !self.orientation_escape_hatch_available()
        {
            calm_core::config::OrientationMode::Inject
        } else {
            mode
        }
    }

    /// Content merged into the first non-orientation-adjacent tool response
    /// of a session under `"inject"` mode — deliberately compact (mirrors
    /// `repo_overview`'s `compact:true` shape), since the full
    /// `repo_overview` response remains one real tool call away for an
    /// agent that wants more.
    pub(crate) fn orientation_injection_text(&self) -> String {
        serde_json::json!({
            "_calm_orientation": {
                "note": "Auto-attached: this session hasn't called repo_overview yet. \
                         Call it (or the calm_workflow prompt) for the full 8-stage workflow.",
                "indexing_phase": self.phase_str(),
                "embeddings_status": self.embed_status_str(),
            }
        })
        .to_string()
    }

    /// `{"error": {code, message, recoverable}}`-shaped `ORIENTATION_REQUIRED`
    /// refusal for `"block"` mode — same envelope every other tool-level
    /// error in this server uses (see `error_detail`), so existing
    /// client-side error handling doesn't need a special case for this one.
    pub(crate) fn orientation_required_message(&self) -> String {
        serde_json::to_string(&ErrorOutput {
            error: error_detail(
                "ORIENTATION_REQUIRED",
                "call repo_overview first this session (session-start orientation gate, \
                 [orientation] mode=\"block\" in .calm/config.json) — every other tool is \
                 refused until then",
                true,
            ),
        })
        .unwrap_or_default()
    }

    /// Content merged into a tool response when this connection has written
    /// files (`written_files_snapshot`) that haven't had `diff_impact` run
    /// on them since — surfaced on every response while pending, not just
    /// when `session_context` is asked. Always advisory: `diff_impact`-
    /// before-commit can never be a hard server-side gate at all (an MCP
    /// server has no visibility into a client's own native Bash/Edit tool
    /// calls, e.g. `git commit`), so this only makes an already-real signal
    /// (`session_context.pending_diff_impact`) harder to miss, on every
    /// client uniformly, instead of adding new enforcement.
    pub(crate) fn pending_diff_impact_reminder_text(&self) -> Option<String> {
        let files = self.written_files_snapshot();
        if files.is_empty() {
            return None;
        }
        Some(
            serde_json::json!({
                "_calm_pending_diff_impact": {
                    "note": "Files written this session have not had diff_impact run on \
                             them yet — call diff_impact before commit/push.",
                    "files": files,
                }
            })
            .to_string(),
        )
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
