//! Shared daemon on a Unix domain socket (ADR-0005 revival, v1/M2).
//!
//! Runs one long-lived process that serves many concurrent `calm connect`
//! forwarders against one shared `CalmServer` + one background indexer/
//! watcher/embedder, instead of one full `calm serve` process per MCP
//! client connection. Unix-only — the accept loop uses
//! `tokio::net::UnixListener`, which doesn't exist on non-Unix targets;
//! callers (`calm-cli`) gate `--listen`/`calm connect` behind `cfg(unix)`
//! and fall back to plain `calm serve` (stdio) everywhere else.
//!
//! v1 (shipped 2026-07-10, milestones M2-M5): idle-timeout after ~30min genuinely idle
//! (`IDLE_CHECK_INTERVAL`/`IDLE_CHECKS_BEFORE_SHUTDOWN` below, gated on indexing/embed status too,
//! not just connection count) and version-handshake *enforcement* (`DaemonMeta::is_current`,
//! `try_connect_current` below — a stale build gets SIGTERMed and respawned, not just detected).
//! `calm serve`'s own default stdio behavior (invoked directly, no `--listen`) is unchanged by
//! this module's existence. But as of 2026-07-11 this daemon path IS the default entry point for
//! the npm/plugin distribution: `scripts/mcp-launcher.sh` defaults to `calm connect` whenever both
//! hold — Unix, and zero extra args were passed to the launcher — falling back to plain
//! `calm serve` otherwise, or when `CI_MCP_LAUNCHER_NO_DAEMON=1` opts out explicitly. See
//! `docs/adr/0005-daemon-forwarder-shared-process.md`'s "Update 2026-07-11" section.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::tools::CalmServer;
use crate::tools::common::RwLockExt;
use crate::{Bootstrapped, bootstrap, shutdown_and_checkpoint};

/// Runs CALM as a daemon listening on `socket_path`. Returns once shut down
/// cleanly (SIGINT/SIGTERM via the same `CancellationToken` `bootstrap`
/// already wires up); propagates an error if this process couldn't become
/// the daemon (e.g. bind failed for a reason other than another daemon
/// already owning the socket).
/// Check every 60s; after 30 consecutive idle checks (~30 minutes) with no
/// active connections and a stable (not actively indexing/embedding)
/// server, shut the daemon down. Hardcoded for v1 — revisit if dogfooding
/// (M6) shows this is wrong in either direction.
const IDLE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
const IDLE_CHECKS_BEFORE_SHUTDOWN: u32 = 30;

pub async fn serve_unix_daemon(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
    socket_path: PathBuf,
) -> Result<()> {
    // Deliberately `project_root.join(".calm")`, NOT `socket_path.parent()`:
    // `connect_or_spawn`'s `resolve_socket_path` can fall back to a short
    // path outside the project (e.g. `/tmp/calm-<hash>.sock`) when the
    // natural `.calm/daemon.sock` would exceed a unix socket's path-length
    // limit. Deriving `calm_dir` from the socket's parent in that case would
    // (a) write `daemon.meta`/`daemon-spawn.lock` into a directory shared by
    // every project using the fallback instead of this project's own
    // `.calm/`, breaking the version handshake (`calm connect` looks for
    // `daemon.meta` under the *project's* `.calm/`, per `connect_or_spawn`)
    // and conflating spawn-arbitration across unrelated projects, and (b)
    // for a bare `/tmp` fallback specifically, hand `create_calm_dir` a
    // `mode(0o700)` `create()` call against `/tmp` itself — harmless only
    // because `/tmp` already exists so the `AlreadyExists` branch short-
    // circuits before any chmod, but one directory-layout accident away from
    // trying to lock down a directory every other process on the system
    // depends on being world-writable. Caught via manual smoke-testing this
    // exact fallback path, not by inspection — both classes of bug only
    // showed up once the socket path actually diverged from `.calm/`.
    let calm_dir = project_root.join(".calm");
    create_calm_dir(&calm_dir)?;

    let listener = match bind_or_yield(&calm_dir, &socket_path).await? {
        Some(listener) => listener,
        None => {
            // Another daemon already owns this socket and is live — cheap,
            // expected outcome when several `calm connect` forwarders race
            // to spawn a daemon at once (see `bind_or_yield`'s doc comment).
            return Ok(());
        }
    };
    set_socket_perms(&socket_path)?;

    write_daemon_meta(&calm_dir)?;

    tracing::info!("Daemon listening on {}", socket_path.display());

    let Bootstrapped { server, ct } = bootstrap(project_root, db_path.clone(), preset).await?;

    run_accept_loop(
        listener,
        server,
        ct,
        IDLE_CHECK_INTERVAL,
        IDLE_CHECKS_BEFORE_SHUTDOWN,
    )
    .await;

    std::fs::remove_file(&socket_path).ok();
    remove_daemon_meta(&calm_dir);
    shutdown_and_checkpoint(&db_path);
    tracing::info!("Daemon shut down cleanly");
    Ok(())
}

/// RAII guard ensuring a connection's `active_connections` decrement and
/// `active_sessions` entry removal both run even if the connection-handling
/// future panics mid-flight — `Drop` still executes during a panic unwind,
/// unlike code sequenced after an `.await` inside the same async block,
/// which a panic skips entirely (the task dies before reaching it). Closes
/// a gap found during TEMPORAL specialist review of the session-awareness
/// work: without this, a panicking connection task leaked its
/// `active_connections` count forever — silently defeating idle-shutdown,
/// since `is_idle` gates on `active_connections == 0` — and would now also
/// leak a phantom `SessionSummary` in `active_sessions`, visible to every
/// other connection's `session_context.other_active_sessions` forever.
/// Pre-existing bug for the counter (this diff only adds the session half),
/// fixed here rather than left half-fixed.
struct ConnectionGuard {
    active_connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    session_registry: std::sync::Arc<
        std::sync::Mutex<std::collections::HashMap<u64, crate::tools::SessionSummary>>,
    >,
    session_id: u64,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.active_connections
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut sessions) = self.session_registry.lock() {
            sessions.remove(&self.session_id);
        }
    }
}

/// The daemon's whole service lifetime: accept connections, spawn one
/// `serve_server_with_ct` task per connection, and watch for either
/// SIGTERM/SIGINT (`ct` cancelled — reuses `bootstrap`'s existing handlers)
/// or sustained idleness. Extracted from `serve_unix_daemon` (rather than
/// left inline) so `idle_check_interval`/`idle_checks_before_shutdown` can
/// be tiny in a test instead of the real 30-minute production values —
/// counting discrete idle *checks* rather than tracking elapsed wall-clock
/// time via `Instant` is what makes that swap trivial, no fake-time needed.
async fn run_accept_loop(
    listener: tokio::net::UnixListener,
    server: CalmServer,
    ct: CancellationToken,
    idle_check_interval: std::time::Duration,
    idle_checks_before_shutdown: u32,
) {
    let active_connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let phase = server.phase_handle();
    let embed_status = server.embed_status_handle();
    let mut idle_ticks: u32 = 0;
    let mut ticker = tokio::time::interval(idle_check_interval);
    ticker.tick().await; // first tick fires immediately — discard it so the next one is a full interval away

    loop {
        tokio::select! {
            _ = ct.cancelled() => {
                tracing::info!("Daemon shutdown requested");
                break;
            }
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _addr)) => stream,
                    Err(e) => {
                        tracing::warn!("daemon accept() failed: {e}");
                        continue;
                    }
                };
                // `conn_ct` is a *child* of the master `ct` — cancelling this
                // one connection (peer disconnect, per-connection error) must
                // never cancel `ct` itself and take every other session down
                // with it; only the reverse (daemon-wide shutdown cancelling
                // every connection) is correct.
                let conn_ct = ct.child_token();
                let conn_server = server.for_connection();
                // Captured *before* `conn_server` moves into
                // `serve_server_with_ct` below — `session_registry_handle`
                // returns cheap clones (an `Arc` and a `Copy` u64), not a
                // borrow, so this doesn't fight the move.
                let (session_registry, session_id) = conn_server.session_registry_handle();
                let active = active_connections.clone();
                active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    // Constructed *before* the connection future runs, held
                    // for the task's entire lifetime — its `Drop` is what
                    // actually guarantees cleanup on every exit path
                    // (normal return, error return, *and* panic), not the
                    // code that used to run sequentially after the `match`.
                    let _cleanup = ConnectionGuard {
                        active_connections: active,
                        session_registry,
                        session_id,
                    };
                    match rmcp::service::serve_server_with_ct(conn_server, stream, conn_ct).await {
                        Ok(service) => {
                            if let Err(e) = service.waiting().await {
                                tracing::warn!("daemon connection ended with error: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("daemon connection init failed: {e}"),
                    }
                    // `_cleanup` drops here — decrements `active_connections`
                    // and removes this session's `active_sessions` entry.
                });
            }
            _ = ticker.tick() => {
                if is_idle(&active_connections, &phase, &embed_status) {
                    idle_ticks += 1;
                    if idle_ticks >= idle_checks_before_shutdown {
                        tracing::info!(
                            "Daemon idle for {idle_ticks} consecutive checks (~{:?}) — shutting down",
                            idle_check_interval * idle_ticks
                        );
                        break;
                    }
                } else {
                    idle_ticks = 0;
                }
            }
        }
    }
}

/// Idle means: zero active connections, AND the background indexer isn't
/// actively parsing/building edges, AND embeddings aren't actively
/// downloading/computing. Gating on `phase` alone (an earlier draft's
/// mistake, caught before writing this) would let the idle-timeout evict a
/// daemon mid-embed — `phase` can already read `Ready` while
/// `bootstrap_embeddings` is still running on a separate track, since
/// embeddings start only *after* the graph is built.
fn is_idle(
    active_connections: &std::sync::atomic::AtomicUsize,
    phase: &std::sync::Arc<std::sync::RwLock<calm_core::types::IndexingPhase>>,
    embed_status: &std::sync::Arc<std::sync::RwLock<calm_core::types::EmbedStatus>>,
) -> bool {
    use calm_core::types::{EmbedStatus, IndexingPhase};

    if active_connections.load(std::sync::atomic::Ordering::SeqCst) != 0 {
        return false;
    }
    let phase_stable = matches!(
        *phase.read_ok(),
        IndexingPhase::Ready | IndexingPhase::Failed
    );
    let embed_stable = !matches!(
        *embed_status.read_ok(),
        EmbedStatus::Downloading | EmbedStatus::Embedding
    );
    phase_stable && embed_stable
}

#[cfg(unix)]
/// Creates `calm_dir` at exactly `0700` if it doesn't already exist. Public
/// so `calm-cli`'s `init_daemon_tracing` (which must create `.calm/` to
/// write `daemon.log` into, and runs *before* `serve_unix_daemon` — tracing
/// has to be initialized before anything can log) can reuse this instead of
/// a plain `create_dir_all`, which found this exact bug: two independent
/// call sites both trying to create `.calm/`, one atomic-0700 and one
/// umask-default, racing to be first — whichever lost hit this function's
/// own "already exists → treat as success" branch and silently left `.calm/`
/// at the loose winner's permissions instead of `0700`. Caught by
/// `daemon_calm_dir_and_socket_have_restrictive_permissions`
/// (`crates/calm-cli/tests/daemon_integration.rs`), not by inspection.
///
/// Deliberately does NOT retroactively `chmod` an already-existing `.calm/`
/// (e.g. one left behind at default permissions by `calm index`/`calm serve`
/// non-daemon mode, before any daemon ever ran on this project) — that's a
/// pre-existing exposure this feature doesn't make worse, and silently
/// tightening a directory a user may have intentionally left shared would
/// be a surprising side effect of starting a daemon, not an obvious win.
/// Out of scope for now; revisit only if real usage shows it's wrong.
pub fn create_calm_dir(calm_dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // Atomic-at-creation `0700`, not create-then-`chmod` — a create-then-
    // chmod window would briefly leave `.calm/` at the process umask's
    // (commonly world-readable) default. This socket exposes the full MCP
    // surface, including `edit_lines`/`edit_symbol` writing straight into
    // the repo, so a shared multi-user machine must never see a window
    // where another user could read (or worse, race to write into) this
    // directory. `create` (not `create_all`) errors on an already-existing
    // dir, which is the common case (`.calm/` from a prior `calm index`) —
    // treat that as success rather than propagating the error.
    match std::fs::DirBuilder::new().mode(0o700).create(calm_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e).context("creating .calm/ with 0700 permissions"),
    }
}

/// Serializes the whole bind-arbitration sequence (bind → on `AddrInUse`,
/// check liveness → unlink-if-stale → rebind) behind a dedicated,
/// never-removed `daemon-spawn.lock`, distinct from `indexer.lock` (which
/// means something else — "I am this project's writer" — and must not be
/// overloaded for spawn arbitration, per ADR-0005 §3's correction over its
/// first draft).
///
/// Naively doing this *without* a lock has a real split-brain race: daemon
/// candidate A completes connect-check→unlink→bind and is now live: B, mid-
/// flight on its own independently-valid staleness check from a moment
/// earlier, then calls `remove_file` on the same path — `unlink` has no
/// liveness check, so this deletes **A's live socket's directory entry**
/// (A's fd stays valid but the path is gone), and B's subsequent `bind`
/// then succeeds cleanly. Two live daemons, silently. The lock here closes
/// that window by making the entire sequence atomic across processes.
///
/// Returns `Ok(Some(listener))` if this process is now the daemon,
/// `Ok(None)` if another daemon already owns the socket and is live (the
/// caller should exit 0 immediately — cheap, expected once per cold-start
/// race), or `Err` if arbitration itself failed unexpectedly.
async fn bind_or_yield(
    calm_dir: &Path,
    socket_path: &Path,
) -> Result<Option<tokio::net::UnixListener>> {
    let calm_dir_for_lock = calm_dir.to_path_buf();
    let _spawn_lock = tokio::task::spawn_blocking(move || {
        calm_core::db::instance_lock::acquire_blocking_named(
            &calm_dir_for_lock,
            "daemon-spawn.lock",
        )
    })
    .await
    .context("daemon-spawn.lock acquisition task panicked")??;

    let result = match tokio::net::UnixListener::bind(socket_path) {
        Ok(listener) => Ok(Some(listener)),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match tokio::net::UnixStream::connect(socket_path).await {
                Ok(_probe) => {
                    tracing::info!(
                        "another daemon already owns {} — yielding",
                        socket_path.display()
                    );
                    Ok(None)
                }
                Err(_) => {
                    tracing::info!(
                        "stale socket at {} (no live daemon) — removing and rebinding",
                        socket_path.display()
                    );
                    std::fs::remove_file(socket_path).ok();
                    tokio::net::UnixListener::bind(socket_path)
                        .map(Some)
                        .context("rebind after removing stale socket")
                }
            }
        }
        Err(e) => Err(e).context("binding daemon socket"),
    };

    // Release promptly, win or lose — holding this through the winner's
    // entire daemon lifetime would make every *other* racing candidate's
    // `acquire_blocking_named` above block for that whole lifetime instead
    // of noticing "already taken" and exiting within milliseconds, which is
    // the whole point of arbitrating the bind step specifically rather than
    // the daemon's full run.
    drop(_spawn_lock);
    result
}

#[cfg(unix)]
fn set_socket_perms(socket_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .context("setting daemon socket permissions to 0600")
}

/// Sidecar written next to the socket: this daemon's version + git SHA, so
/// a future `calm connect` can detect it's talking to a stale daemon (a
/// binary rebuilt after the daemon was spawned) and respawn instead of
/// silently running old code for a whole session — ADR-0005 §9's
/// version-skew risk. Written eagerly here (before the read side exists)
/// so that milestone needs no daemon-side change.
#[derive(serde::Serialize, serde::Deserialize)]
struct DaemonMeta {
    /// `CARGO_PKG_VERSION` — a cheap first-pass signal, but not sufficient
    /// alone: this string doesn't change between commits that don't bump
    /// it, which is most of them.
    version: String,
    /// `calm_core::BUILD_INFO` — the CALM *source repo's* git SHA (+
    /// `-dirty` if built with uncommitted changes) baked in at compile time
    /// by `calm-core/build.rs`. Deliberately NOT `current_git_head_short`
    /// of the *target* `project_root` being served — that reflects the
    /// project being indexed, not which `calm` build produced this daemon,
    /// and would be nonsensical to compare against a forwarder's own build
    /// identity (an early version of this function made exactly that
    /// mistake). This is the same mechanism `ci doctor`/`--version` already
    /// use to answer "what commit is this binary actually running".
    build_info: String,
    /// This daemon process's own PID, so a forwarder that detects a stale
    /// build can ask it to shut down (best-effort SIGTERM) before spawning
    /// a replacement — see `connect_or_spawn`'s version-handshake logic.
    pid: u32,
}

impl DaemonMeta {
    fn is_current(&self) -> bool {
        self.version == env!("CARGO_PKG_VERSION") && self.build_info == calm_core::BUILD_INFO
    }
}

/// Sidecar written next to the socket: this daemon's build identity, so a
/// future `calm connect` can detect it's talking to a stale daemon (a
/// binary rebuilt after the daemon was spawned) and respawn instead of
/// silently running old code for a whole session — ADR-0005 §9's
/// version-skew risk.
fn write_daemon_meta(calm_dir: &Path) -> Result<()> {
    let meta = DaemonMeta {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_info: calm_core::BUILD_INFO.to_string(),
        pid: std::process::id(),
    };
    std::fs::write(calm_dir.join("daemon.meta"), serde_json::to_string(&meta)?)
        .context("writing daemon.meta")
}

/// `None` covers every case a caller should treat as "can't verify this
/// daemon, respawn": file missing, unreadable, or malformed — deliberately
/// not distinguished further, since a forwarder's response is identical
/// either way (fall through to spawning a fresh daemon).
fn read_daemon_meta(calm_dir: &Path) -> Option<DaemonMeta> {
    let text = std::fs::read_to_string(calm_dir.join("daemon.meta")).ok()?;
    serde_json::from_str(&text).ok()
}

fn remove_daemon_meta(calm_dir: &Path) {
    std::fs::remove_file(calm_dir.join("daemon.meta")).ok();
}

/// `calm connect`'s entire job: connect to (or spawn, if none is live or
/// the live one is a stale build) the daemon for `project_root`, then relay
/// stdin<->socket verbatim — no JSON-RPC/MCP parsing here at all (ADR-0005
/// §2: keep the forwarder protocol-version-agnostic, so it never needs to
/// track the MCP protocol's own version).
pub async fn connect_or_spawn(
    project_root: PathBuf,
    preset: Option<String>,
    db_path: Option<PathBuf>,
) -> Result<()> {
    let root = std::fs::canonicalize(&project_root).context("resolving --project-root")?;
    let calm_dir = root.join(".calm");
    // Deliberately NOT `create_dir_all(&calm_dir)` here: `.calm/` must be
    // created exactly once, atomically at `0700`, by `create_calm_dir` on
    // the daemon side — creating it here first (a plain `create_dir_all`
    // has no mode control, so it lands at whatever the umask gives, often
    // world-readable) would let this forwarder's directory win the race
    // against the daemon's own locked-down creation, and `create_calm_dir`'s
    // "already exists → treat as success" branch would then silently leave
    // it at the wrong permissions forever. Found via this exact assertion
    // failing in `daemon_calm_dir_and_socket_have_restrictive_permissions`
    // (`crates/calm-cli/tests/daemon_integration.rs`) — nothing downstream
    // of this line actually needs `.calm/` to exist yet: `resolve_socket_path`
    // only builds a `PathBuf`, and `read_daemon_meta`/`connect()` already
    // handle a missing directory/file gracefully.
    let socket_path = resolve_socket_path(&calm_dir);

    let stream = connect_live_and_current(
        &root,
        &calm_dir,
        &socket_path,
        preset.as_deref(),
        db_path.as_deref(),
    )
    .await?;
    relay(stream).await
}

/// `<calm_dir>/daemon.sock` when it fits, else a short deterministic
/// fallback under the OS temp dir. Unix domain socket paths are capped at
/// `sizeof(sockaddr_un.sun_path)` — 108 bytes on Linux, 104 on macOS/BSD,
/// both including the null terminator — a limit `.calm/daemon.sock` can
/// blow past for a deeply nested project root (hit directly during manual
/// smoke-testing this feature, not a hypothetical: `bind()` fails with
/// "path must be shorter than SUN_LEN", and since the spawned daemon's
/// stderr is `Stdio::null()`'d, that error was otherwise silently swallowed
/// — the forwarder just polls for 5s and reports a generic timeout).
///
/// The fallback path is a hash of the *canonical* `calm_dir`, not the
/// natural path itself — deterministic (`DefaultHasher::new()` uses a fixed
/// seed, unlike `HashMap`'s randomized `RandomState`) so every `calm
/// connect` invocation for the same project recomputes the same fallback
/// path without needing to read or write any extra shared state to agree
/// on it. Only `calm connect` needs this: the daemon it spawns is always
/// told the exact resolved path via an explicit `--listen unix:<path>`
/// argument, so `serve_unix_daemon` itself never has to re-derive it.
fn resolve_socket_path(calm_dir: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};

    let natural = calm_dir.join("daemon.sock");
    const SUN_PATH_SAFE_LIMIT: usize = 100; // conservative margin under the 104-108 byte ceiling
    if natural.as_os_str().len() <= SUN_PATH_SAFE_LIMIT {
        return natural;
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    calm_dir.hash(&mut hasher);
    let short = std::env::temp_dir().join(format!("calm-{:016x}.sock", hasher.finish()));
    tracing::warn!(
        "{} is too long for a unix socket path — using {} instead",
        natural.display(),
        short.display()
    );
    short
}

/// Returns a connection to a live, version-matched daemon — spawning one
/// (detached) if none exists, or asking a stale one to shut down and
/// respawning if it's serving an old build. Bounded-poll retry, same
/// ~150ms-interval pattern `instance_lock` already uses for "no wakeup to
/// wait on, so poll" waits.
async fn connect_live_and_current(
    project_root: &Path,
    calm_dir: &Path,
    socket_path: &Path,
    preset: Option<&str>,
    db_path: Option<&Path>,
) -> Result<tokio::net::UnixStream> {
    if let Some(stream) = try_connect_current(calm_dir, socket_path).await {
        return Ok(stream);
    }

    spawn_detached_daemon(project_root, socket_path, preset, db_path)?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    // Re-spawn on this cadence, not just once up front: `bind_or_yield`'s
    // split-brain protection (ADR-0005 §3 — "bind IS the natural spawn
    // mutex") means a losing candidate is *designed* to be cheap and yield
    // immediately, so retrying the spawn here is free of new invariants, not
    // a workaround. Needed because a candidate can lose the bind race for a
    // reason other than "a live current-build daemon already owns the
    // socket": if the previous holder was mid-shutdown (stale-build
    // SIGTERM'd by `try_connect_current` below), its socket file can still
    // be bound at the exact moment this candidate calls `bind_or_yield`,
    // which then sees it as "still live" and yields — permanently, since
    // nothing else in this loop ever attempts another bind. Without a
    // repeat spawn, that single lost race means this whole call times out
    // at the deadline below even though the stale daemon finishes exiting
    // (and its socket becomes free) well within the 5s budget.
    let mut last_spawn_attempt = std::time::Instant::now();
    const RESPAWN_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        if let Some(stream) = try_connect_current(calm_dir, socket_path).await {
            return Ok(stream);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon at {} did not become reachable within 5s of spawning it",
                socket_path.display()
            );
        }
        if last_spawn_attempt.elapsed() >= RESPAWN_RETRY_INTERVAL {
            spawn_detached_daemon(project_root, socket_path, preset, db_path)?;
            last_spawn_attempt = std::time::Instant::now();
        }
    }
}

/// `Some(stream)` only if connect succeeds *and* `daemon.meta` reports the
/// same build as this binary. `None` covers everything that should trigger
/// a (re)spawn: no socket, a dead/stale socket, or a live daemon on a
/// different build — deliberately not distinguished further to the caller,
/// since the response (spawn/respawn and retry) is identical either way. A
/// version mismatch additionally best-effort-signals the stale daemon to
/// shut down (see `signal_shutdown`) — without this, `bind_or_yield`'s own
/// split-brain protection would correctly refuse to let a fresh daemon
/// candidate replace a *live* one, and this loop would spin forever finding
/// the same stale daemon on every retry.
async fn try_connect_current(
    calm_dir: &Path,
    socket_path: &Path,
) -> Option<tokio::net::UnixStream> {
    let stream = tokio::net::UnixStream::connect(socket_path).await.ok()?;

    match read_daemon_meta(calm_dir) {
        Some(meta) if meta.is_current() => Some(stream),
        Some(meta) => {
            tracing::info!(
                "daemon at {} (pid {}) is a stale build — signaling it to shut down and respawning",
                socket_path.display(),
                meta.pid
            );
            signal_shutdown(meta.pid);
            drop(stream);
            None
        }
        None => {
            tracing::warn!(
                "connected to {} but its daemon.meta is missing/unreadable — treating as stale",
                socket_path.display()
            );
            drop(stream);
            None
        }
    }
}

/// Best-effort: the daemon might already be gone, or (very unlikely, given
/// the short window) `pid` reused by an unrelated process. Either way
/// there's nothing actionable to do with a failed signal here — the
/// retry loop in `connect_live_and_current` is what actually verifies the
/// outcome (does a fresh connect eventually succeed against a *current*
/// build), not this call's return value.
fn signal_shutdown(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

/// Spawns `calm serve --listen unix:<socket_path>` fully detached from this
/// forwarder process.
fn spawn_detached_daemon(
    project_root: &Path,
    socket_path: &Path,
    preset: Option<&str>,
    db_path: Option<&Path>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("resolving current calm binary path")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve")
        .arg("--project-root")
        .arg(project_root)
        .arg("--listen")
        .arg(format!("unix:{}", socket_path.display()));
    // Forwarded only when the `calm connect` invocation itself set them —
    // omitting a flag here means the spawned `calm serve` resolves its own
    // default exactly as a direct `calm serve` invocation would (preset
    // falls back to config.json, db_path falls back to
    // `calm_server::default_db_path`), so there's no duplicated
    // default-resolution logic to keep in sync between the two commands.
    if let Some(preset) = preset {
        cmd.arg("--preset").arg(preset);
    }
    if let Some(db_path) = db_path {
        cmd.arg("--db-path").arg(db_path);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // New process group: the daemon must survive *this* forwarder's own
        // process group being SIGTERM'd (e.g. an MCP client tearing down
        // just this one session) — ADR-0005's own self-flagged Risk #1, the
        // single highest-severity risk in the whole design. Get this wrong
        // and a client closing session A kills the daemon and silently
        // takes every other session sharing it down too — reproducing the
        // exact bug this whole feature exists to fix, under a new name.
        .process_group(0);
    cmd.spawn().context("spawning detached daemon")?;
    Ok(())
}

/// Pure byte relay between this process's stdio and the daemon socket — no
/// MCP/JSON-RPC awareness. Distinguishes *which side* closed first so the
/// exit code means something: the client (our stdin) closing is a normal
/// end of session; the daemon closing on us while the client was still
/// talking is an unexpected mid-session failure (crash, or an idle-timeout
/// evicting a session that was still in use) and must surface as an error
/// rather than silently exiting 0 — consistent with this codebase's
/// existing "never fake ready" honesty (`embed_status`/`indexing_phase`
/// never pretend a failure didn't happen).
async fn relay(stream: tokio::net::UnixStream) -> Result<()> {
    let (mut sock_read, mut sock_write) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let client_to_daemon = tokio::io::copy(&mut stdin, &mut sock_write);
    let daemon_to_client = tokio::io::copy(&mut sock_read, &mut stdout);
    tokio::pin!(client_to_daemon);
    tokio::pin!(daemon_to_client);

    tokio::select! {
        result = &mut client_to_daemon => {
            if let Err(e) = result {
                tracing::warn!("client->daemon relay ended with an error: {e}");
            }
            Ok(())
        }
        result = &mut daemon_to_client => {
            match result {
                Ok(_) => anyhow::bail!("daemon closed the connection unexpectedly"),
                Err(e) => Err(anyhow::anyhow!("daemon connection error: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dirs(name: &str) -> (PathBuf, PathBuf) {
        let calm_dir = std::env::temp_dir().join(format!(
            "ci_daemon_{name}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&calm_dir);
        std::fs::create_dir_all(&calm_dir).unwrap();
        let socket_path = calm_dir.join("daemon.sock");
        (calm_dir, socket_path)
    }

    #[tokio::test]
    async fn bind_or_yield_first_caller_wins() {
        let (calm_dir, socket_path) = test_dirs("first_wins");

        let listener = bind_or_yield(&calm_dir, &socket_path)
            .await
            .unwrap()
            .expect("first caller against a fresh socket path must win");

        assert!(socket_path.exists());
        drop(listener);
        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[tokio::test]
    async fn bind_or_yield_second_caller_yields_to_live_daemon() {
        let (calm_dir, socket_path) = test_dirs("second_yields");

        let _first = bind_or_yield(&calm_dir, &socket_path)
            .await
            .unwrap()
            .expect("first caller must win");

        // Second caller, same still-live socket: must detect the live
        // listener via the connect-check and yield rather than stealing it
        // — the split-brain race this function exists to close.
        let second = bind_or_yield(&calm_dir, &socket_path).await.unwrap();
        assert!(
            second.is_none(),
            "second caller must yield while the first listener is still live"
        );

        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[tokio::test]
    async fn bind_or_yield_recovers_a_stale_socket() {
        let (calm_dir, socket_path) = test_dirs("stale_recovery");

        let first = bind_or_yield(&calm_dir, &socket_path)
            .await
            .unwrap()
            .expect("first caller must win");
        // Simulate a crashed daemon: the listener (and its fd) goes away,
        // but the socket *file* is left behind on disk, same as a real
        // process dying without reaching its own cleanup code.
        drop(first);
        assert!(
            socket_path.exists(),
            "dropping the listener must not itself remove the socket file — \
             that's the exact staleness this test simulates"
        );

        let second = bind_or_yield(&calm_dir, &socket_path)
            .await
            .unwrap()
            .expect("a caller against a stale (dead) socket must detect and recover it");
        drop(second);

        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[tokio::test]
    async fn try_connect_current_accepts_current_build_rejects_mismatch_and_signals_it() {
        let (calm_dir, socket_path) = test_dirs("version_handshake");

        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let accept_task = tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    break;
                }
            }
        });

        // No daemon.meta yet: connect succeeds, but a missing/unreadable
        // meta must be treated as stale, not silently trusted.
        assert!(
            try_connect_current(&calm_dir, &socket_path).await.is_none(),
            "a connect with no daemon.meta at all must be treated as stale"
        );

        // Current-build meta: connect must be accepted.
        write_daemon_meta(&calm_dir).unwrap();
        assert!(
            try_connect_current(&calm_dir, &socket_path).await.is_some(),
            "a daemon.meta matching this binary's own build must be accepted"
        );

        // A disposable child process stands in for "the stale daemon's real
        // pid" — real enough to prove `signal_shutdown` actually delivers
        // SIGTERM (not just \"doesn't panic\"), safe to signal since it's
        // ours and does nothing but sleep.
        let mut stale_daemon = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawning a disposable sleep process for this test");
        let stale_pid = stale_daemon.id();

        std::fs::write(
            calm_dir.join("daemon.meta"),
            serde_json::to_string(&DaemonMeta {
                version: "0.0.0-not-real".to_string(),
                build_info: "not-a-real-build".to_string(),
                pid: stale_pid,
            })
            .unwrap(),
        )
        .unwrap();

        assert!(
            try_connect_current(&calm_dir, &socket_path).await.is_none(),
            "a version/build mismatch must be rejected"
        );

        let status = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::task::spawn_blocking(move || stale_daemon.wait()),
        )
        .await
        .expect("stale-pid process must exit promptly once SIGTERM'd")
        .unwrap()
        .unwrap();
        assert!(
            !status.success(),
            "the disposable process must have been terminated by SIGTERM, not exited on its own"
        );

        accept_task.abort();
        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    fn idle_test_server(calm_dir: &std::path::Path) -> CalmServer {
        CalmServer::new(calm_dir.to_path_buf(), calm_dir.join("index.db")).unwrap()
    }

    #[tokio::test]
    async fn run_accept_loop_shuts_down_after_sustained_idle() {
        let (calm_dir, socket_path) = test_dirs("idle_shutdown");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = idle_test_server(&calm_dir);
        // `CalmServer::new` starts life in `IndexingPhase::Scanning` (matches
        // real startup, before any indexing has run) — explicitly advance to
        // `Ready` so this test exercises the "genuinely idle" case, not the
        // "never got a chance to be idle" case `embed_status` defaults
        // (`Disabled`) already cover correctly on their own.
        *server.phase_handle().write().unwrap() = calm_core::types::IndexingPhase::Ready;
        let ct = CancellationToken::new();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run_accept_loop(
                listener,
                server,
                ct,
                std::time::Duration::from_millis(20),
                2,
            ),
        )
        .await;

        assert!(
            result.is_ok(),
            "run_accept_loop must exit on its own once idle for \
             idle_checks_before_shutdown consecutive checks"
        );

        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[tokio::test]
    async fn run_accept_loop_does_not_shut_down_while_indexing_is_active() {
        let (calm_dir, socket_path) = test_dirs("idle_indexing_guard");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = idle_test_server(&calm_dir);
        *server.phase_handle().write().unwrap() = calm_core::types::IndexingPhase::Scanning;
        let ct = CancellationToken::new();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            run_accept_loop(
                listener,
                server,
                ct,
                std::time::Duration::from_millis(20),
                2,
            ),
        )
        .await;

        assert!(
            result.is_err(),
            "an actively-Scanning phase must never be treated as idle, no \
             matter how many idle checks pass"
        );

        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[tokio::test]
    async fn run_accept_loop_does_not_shut_down_with_an_active_connection() {
        let (calm_dir, socket_path) = test_dirs("idle_connection_guard");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = idle_test_server(&calm_dir);
        let ct = CancellationToken::new();

        // Held open for the whole test, never speaks MCP — the accept
        // itself must still count as "active" and block the idle-timeout,
        // since the resulting `serve_server_with_ct` task just sits waiting
        // to read a message that never comes, exactly like a real client
        // that connected but hasn't sent its next request yet.
        let _held = tokio::net::UnixStream::connect(&socket_path).await.unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            run_accept_loop(
                listener,
                server,
                ct,
                std::time::Duration::from_millis(20),
                2,
            ),
        )
        .await;

        assert!(
            result.is_err(),
            "a live (even MCP-silent) connection must block the idle-timeout \
             from ever firing"
        );

        let _ = std::fs::remove_dir_all(&calm_dir);
    }

    #[test]
    fn connection_guard_cleans_up_even_when_the_task_panics() {
        // Direct test of the exact mechanism `run_accept_loop` relies on:
        // `ConnectionGuard::drop` must run during a panic unwind, not just
        // on a normal return — that's the whole reason it exists instead of
        // plain code sequenced after the connection future. Found via
        // TEMPORAL specialist review: without this guard, a panicking
        // connection task silently leaked its `active_connections` count
        // (defeating idle-shutdown) and its `active_sessions` entry
        // (a phantom peer that never disappears).
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let registry: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<u64, crate::tools::SessionSummary>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        registry.lock().unwrap().insert(
            7,
            crate::tools::SessionSummary {
                session_id: 7,
                last_touched_file: None,
                last_touched_at: "2026-07-11T00:00:00Z".into(),
                tool_calls: 0,
                reviewing_symbol: None,
            },
        );

        let active_for_closure = active.clone();
        let registry_for_closure = registry.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = ConnectionGuard {
                active_connections: active_for_closure,
                session_registry: registry_for_closure,
                session_id: 7,
            };
            panic!("simulated connection-handler panic");
        }));

        assert!(
            result.is_err(),
            "sanity check: the panic must have actually propagated out of catch_unwind \
             (this test proves Drop runs *during* unwind, not that panics are swallowed)"
        );
        assert_eq!(
            active.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "active_connections must be decremented even though the closure panicked"
        );
        assert!(
            !registry.lock().unwrap().contains_key(&7),
            "the session_id 7 entry must be removed even though the closure panicked"
        );
    }
}
