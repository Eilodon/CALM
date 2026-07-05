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
}
