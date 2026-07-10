use std::fs::{File, OpenOptions};
use std::path::Path;

use fs4::fs_std::FileExt;

/// Holds an OS advisory lock (`flock`/`LockFileEx`) on `<calm_dir>/indexer.lock`
/// for the lifetime of this handle. Dropping it (including on process exit,
/// even a crash — the OS releases `flock`s automatically when the holding
/// process dies) releases the lock.
///
/// Exists because every `calm serve` process independently spawned its own
/// background indexer + file watcher against the same shared `index.db` —
/// harmless with a single process, but running several concurrently (e.g.
/// multiple editor/MCP-client sessions on the same project_root) meant N
/// redundant reindex loops racing each other, mitigated only by
/// `open_writer`'s `busy_timeout`, not prevented.
pub struct IndexerLock(#[allow(dead_code)] File);

/// Tries to become this project's sole indexer/watcher owner.
///
/// `Some(lock)` — this process acquired the lock and must run the
/// background indexer + watcher; keep the returned guard alive for as long
/// as that's true.
/// `None` — another live process already holds it. The caller should skip
/// spawning its own indexer/watcher entirely and just serve tool calls
/// read-only against the DB the owning process keeps fresh — still fully
/// functional, just not the one doing the (re)indexing.
pub fn try_acquire(calm_dir: &Path) -> Option<IndexerLock> {
    let path = calm_dir.join("indexer.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    file.try_lock_exclusive().ok()?;
    Some(IndexerLock(file))
}

/// Blocks until this process becomes the project's sole indexer/watcher
/// owner. Used by a process that initially lost the `try_acquire` race to
/// promote itself automatically if the current owner ever exits later in
/// the session — gracefully (clean shutdown drops the lock) or not (a
/// crash/OOM/SIGKILL still releases the OS `flock` the moment the process
/// dies, same guarantee `try_acquire`'s own doc comment relies on). Without
/// this, a lock-loser process that started before the owner died would stay
/// read-only for its entire remaining lifetime — nothing else in this
/// codebase ever retries the initial `try_acquire`.
///
/// Unlike `try_acquire`, this call can block indefinitely (as long as some
/// other process holds the lock). Callers must run it on a dedicated
/// blocking thread (e.g. inside `tokio::task::spawn_blocking`), never on an
/// async reactor thread — blocking a reactor thread here would stall every
/// other task sharing that thread for as long as some other process keeps
/// the lock.
/// Blocks until this process becomes the project's sole indexer/watcher
/// owner. Used by a process that initially lost the `try_acquire` race to
/// promote itself automatically if the current owner ever exits later in
/// the session — gracefully (clean shutdown drops the lock) or not (a
/// crash/OOM/SIGKILL still releases the OS `flock` the moment the process
/// dies, same guarantee `try_acquire`'s own doc comment relies on). Without
/// this, a lock-loser process that started before the owner died would stay
/// read-only for its entire remaining lifetime — nothing else in this
/// codebase ever retries the initial `try_acquire`.
///
/// Unlike `try_acquire`, this call can block indefinitely (as long as some
/// other process holds the lock). Callers must run it on a dedicated
/// blocking thread (e.g. inside `tokio::task::spawn_blocking`), never on an
/// async reactor thread — blocking a reactor thread here would stall every
/// other task sharing that thread for as long as some other process keeps
/// the lock. Never cancellable — see `acquire_blocking_cancellable` for a
/// shutdown-aware variant; this thin wrapper exists only so the one existing
/// test and any future non-shutdown caller keep an unconditional-wait API.
pub fn acquire_blocking(calm_dir: &Path) -> std::io::Result<IndexerLock> {
    acquire_blocking_cancellable(calm_dir, &|| false)
        .map(|opt| opt.expect("cancel closure always returns false, so this is always Some"))
}

/// How often a losing process re-checks `cancel` while waiting to be
/// promoted. `flock`'s blocking wait has no interruption mechanism (not even
/// SIGTERM reliably interrupts it — see the SIGTERM-hang investigation this
/// fixes), so cancellation here is necessarily poll-based, not a genuine
/// wakeup. Small enough that shutdown stays snappy, large enough not to spin.
const CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

/// Same as `acquire_blocking`, but polls instead of making a single
/// indefinite blocking `lock_exclusive` call, checking `cancel` between
/// attempts — `Ok(None)` means `cancel` fired before the lock was acquired
/// (the caller should give up and exit without becoming the owner), `Ok(Some(_))`
/// means promotion succeeded normally.
///
/// This trades away the old unconditional version's "OS wakes this thread
/// the instant the lock is released, no interval to tune" property for a
/// bounded `CANCEL_POLL_INTERVAL` latency on promotion — worth it because the
/// old property came with an unbounded latency on *shutdown*: a process stuck
/// here during a SIGTERM-triggered shutdown had no way to ever notice the
/// cancellation, and Tokio's runtime-drop blocks process exit on this
/// `spawn_blocking` task until it returns, so the process hung until whatever
/// process held the lock happened to exit on its own.
///
/// A `try_lock_exclusive` error while polling is treated the same as
/// `try_acquire` treats it (any error, not just a would-block one) — still
/// contended, keep waiting — rather than special-cased on `ErrorKind`,
/// because `fs4`'s Windows backend doesn't reliably surface contention as
/// `ErrorKind::WouldBlock` the way Unix `flock`'s `EWOULDBLOCK` does; `cancel`
/// is the escape hatch either way.
pub fn acquire_blocking_cancellable(
    calm_dir: &Path,
    cancel: &dyn Fn() -> bool,
) -> std::io::Result<Option<IndexerLock>> {
    let path = calm_dir.join("indexer.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    loop {
        if cancel() {
            return Ok(None);
        }
        if file.try_lock_exclusive().is_ok() {
            return Ok(Some(IndexerLock(file)));
        }
        std::thread::sleep(CANCEL_POLL_INTERVAL);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_acquirer_succeeds_second_fails_until_first_drops() {
        let dir = tempfile::tempdir().unwrap();
        let first = try_acquire(dir.path());
        assert!(first.is_some(), "first acquirer must succeed");

        let second = try_acquire(dir.path());
        assert!(
            second.is_none(),
            "second acquirer must fail while first still holds the lock"
        );

        drop(first);
        let third = try_acquire(dir.path());
        assert!(third.is_some(), "lock must be acquirable after release");
    }

    #[test]
    fn acquire_blocking_waits_for_release_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let first = try_acquire(dir.path()).unwrap();

        let dir_path = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _second = acquire_blocking(&dir_path).unwrap();
            tx.send(()).unwrap();
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "acquire_blocking must not return while the first lock is held"
        );

        drop(first);
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
            "acquire_blocking must succeed once the first lock is released"
        );
        handle.join().unwrap();
    }

    #[test]
    fn acquire_blocking_cancellable_returns_none_promptly_when_cancelled() {
        // Regression test for the SIGTERM-hang investigation: a losing
        // process waiting to be promoted must notice cancellation quickly
        // even while the winner never releases the lock at all.
        let dir = tempfile::tempdir().unwrap();
        let _first = try_acquire(dir.path()).unwrap(); // never dropped — lock held for the whole test

        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_reader = cancelled.clone();
        let dir_path = dir.path().to_path_buf();

        let start = std::time::Instant::now();
        let handle = std::thread::spawn(move || {
            acquire_blocking_cancellable(&dir_path, &|| {
                cancelled_reader.load(std::sync::atomic::Ordering::Relaxed)
            })
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);

        let result = handle.join().unwrap();
        assert!(
            result.unwrap().is_none(),
            "cancelled wait must return Ok(None), not acquire the still-held lock"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "cancellation must be noticed within a couple of poll intervals, not hang \
             until the lock holder exits (elapsed: {:?})",
            start.elapsed()
        );
    }
}
