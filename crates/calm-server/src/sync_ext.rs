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
///
/// Lives in its own module, not `tools::common`, deliberately: both traits
/// are pure `std::sync` wrappers with zero coupling to `CalmServer` or any
/// tool-handler state, but `watcher.rs` (the background reindex/watch loop)
/// needs `RwLockExt` too and must not depend on the MCP tool-handler layer
/// it runs independently of (declared boundary rule in `thresholds.toml`;
/// `tools/common.rs` previously being the only home for this forced exactly
/// that violation).
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
