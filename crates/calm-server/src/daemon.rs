//! Shared daemon on a Unix domain socket (ADR-0005 revival, v1/M2).
//!
//! Runs one long-lived process that serves many concurrent `calm connect`
//! forwarders against one shared `CalmServer` + one background indexer/
//! watcher/embedder, instead of today's default (one full `calm serve`
//! process per MCP client connection). Unix-only — the accept loop uses
//! `tokio::net::UnixListener`, which doesn't exist on non-Unix targets;
//! callers (`calm-cli`) gate `--listen`/`calm connect` behind `cfg(unix)`
//! and fall back to plain `calm serve` (stdio) everywhere else.
//!
//! v1 scope: no idle-timeout yet (a future milestone), no version-handshake
//! *enforcement* yet (`daemon.meta` is written here so `calm connect` can
//! read it once that lands, but nothing here checks it). Opt-in only —
//! `calm serve`'s default stdio behavior is completely unchanged by this
//! module's existence.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{bootstrap, current_git_head_short, shutdown_and_checkpoint, Bootstrapped};

/// Runs CALM as a daemon listening on `socket_path`. Returns once shut down
/// cleanly (SIGINT/SIGTERM via the same `CancellationToken` `bootstrap`
/// already wires up); propagates an error if this process couldn't become
/// the daemon (e.g. bind failed for a reason other than another daemon
/// already owning the socket).
pub async fn serve_unix_daemon(
    project_root: PathBuf,
    db_path: PathBuf,
    preset: String,
    socket_path: PathBuf,
) -> Result<()> {
    let calm_dir = socket_path
        .parent()
        .map(Path::to_path_buf)
        .context("--listen socket path has no parent directory")?;
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

    write_daemon_meta(&calm_dir, &project_root)?;

    tracing::info!("Daemon listening on {}", socket_path.display());

    let Bootstrapped { server, ct } = bootstrap(project_root, db_path.clone(), preset).await?;

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
                tokio::spawn(async move {
                    match rmcp::service::serve_server_with_ct(conn_server, stream, conn_ct).await {
                        Ok(service) => {
                            if let Err(e) = service.waiting().await {
                                tracing::warn!("daemon connection ended with error: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("daemon connection init failed: {e}"),
                    }
                });
            }
        }
    }

    drop(listener);
    std::fs::remove_file(&socket_path).ok();
    remove_daemon_meta(&calm_dir);
    shutdown_and_checkpoint(&db_path);
    tracing::info!("Daemon shut down cleanly");
    Ok(())
}

#[cfg(unix)]
fn create_calm_dir(calm_dir: &Path) -> Result<()> {
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
        calm_core::db::instance_lock::acquire_blocking_named(&calm_dir_for_lock, "daemon-spawn.lock")
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
fn write_daemon_meta(calm_dir: &Path, project_root: &Path) -> Result<()> {
    let meta = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": current_git_head_short(project_root),
    });
    std::fs::write(calm_dir.join("daemon.meta"), serde_json::to_string(&meta)?)
        .context("writing daemon.meta")
}

fn remove_daemon_meta(calm_dir: &Path) {
    std::fs::remove_file(calm_dir.join("daemon.meta")).ok();
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
}
