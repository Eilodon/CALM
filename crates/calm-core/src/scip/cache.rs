//! Stable cache key for a SCIP overlay pass, so we skip re-running rust-analyzer
//! when nothing that affects the index changed.

/// FNV-1a over (RA version, Cargo.lock hash, sorted dirty files). Reuses the
/// same stable hash as the indexer's file hashing (pipeline::hash_content).
pub fn overlay_cache_key(ra_version: &str, lockfile_hash: &str, dirty: &[String]) -> String {
    let mut sorted: Vec<&str> = dirty.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let material = format!("{ra_version}|{lockfile_hash}|{}", sorted.join(","));
    crate::indexer::pipeline::hash_content(&material)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_lockfile() {
        let a = overlay_cache_key("1.96.0", "hashAAA", &["src/x.rs".into()]);
        let b = overlay_cache_key("1.96.0", "hashBBB", &["src/x.rs".into()]);
        assert_ne!(a, b);
    }

    /// Regression: before `dirty` was threaded into `run_overlay`, editing a
    /// Rust file's body with an unchanged `Cargo.lock`/toolchain produced the
    /// exact same key every time, so the overlay silently never re-ran for
    /// that file's new content. `dirty` (per-file content fingerprints) must
    /// change the key on its own, independent of lockfile/version.
    #[test]
    fn cache_key_changes_with_dirty_content_alone() {
        let a = overlay_cache_key("1.96.0", "hashAAA", &["src/x.rs@hash1".into()]);
        let b = overlay_cache_key("1.96.0", "hashAAA", &["src/x.rs@hash2".into()]);
        assert_ne!(
            a, b,
            "same version + lockfile but different file content must not collide"
        );
    }

    #[test]
    fn cache_key_is_order_independent_over_dirty_set() {
        let a = overlay_cache_key(
            "1.96.0",
            "hashAAA",
            &["src/a.rs@h1".into(), "src/b.rs@h2".into()],
        );
        let b = overlay_cache_key(
            "1.96.0",
            "hashAAA",
            &["src/b.rs@h2".into(), "src/a.rs@h1".into()],
        );
        assert_eq!(
            a, b,
            "dirty set is sorted before hashing — order must not matter"
        );
    }
}
