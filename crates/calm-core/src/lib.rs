pub mod analysis;
pub mod config;
pub mod db;
pub mod edit;
pub mod embedding;
pub mod fitness;
pub mod gitignore;
pub mod graph;
pub mod indexer;
pub mod memory;
pub mod resolver;
pub mod sanitize;
#[cfg(feature = "scip-overlay")]
pub mod scip;
pub mod search;
pub mod types;
pub mod walk;

/// Git commit this binary was built from (short SHA, `-dirty` suffix if the
/// working tree had uncommitted changes at build time), or `"unknown"` if
/// git wasn't available at build time. See `build.rs`.
pub const BUILD_INFO: &str = env!("CI_BUILD_INFO");

#[cfg(test)]
mod tests {
    #[test]
    fn build_info_is_never_empty() {
        // build.rs always sets CI_BUILD_INFO to something — a short SHA
        // (optionally -dirty) or the literal "unknown" fallback — never "".
        assert!(!super::BUILD_INFO.is_empty());
    }
}
