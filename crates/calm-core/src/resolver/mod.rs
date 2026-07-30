pub mod conservative;
#[cfg(feature = "stack-graphs-formal")]
pub mod formal;
/// Stub when built without `stack-graphs-formal` (D1, 2026-07-30
/// stack-graphs-demotion-lever): preserves `resolver::formal`'s exact public
/// shape (`FormalResolver`/`FormalEdge` + every method `indexer::pipeline`
/// calls, including its own tests' direct `FormalEdge` construction) so
/// nothing downstream needs its own cfg-branch -- `FormalResolver` simply
/// reports no supported languages and resolves nothing. Deliberately a stub
/// with the same API rather than cfg-branching every call site the way
/// `scip-overlay` does elsewhere in this crate: `FormalResolver` is a
/// shared resource threaded through 3 separate pipeline entry points
/// (`run_indexing_pipeline_cancellable`/`reindex_changed_cancellable`/
/// `reindex_paths`) into `extract_file_data`, not a leaf-level integration
/// point -- forking that signature 3x would risk the two paths silently
/// diverging, which a single stub can't.
#[cfg(not(feature = "stack-graphs-formal"))]
pub mod formal {
    #[derive(Default)]
    pub struct FormalResolver;

    pub struct FormalEdge {
        pub reference_symbol: String,
        pub definition_symbol: String,
    }

    impl FormalResolver {
        pub fn new() -> Self {
            Self
        }
        pub fn load_python(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        pub fn load_typescript(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        pub fn load_javascript(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        pub fn load_java(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        pub fn has_language(&self, _language: &str) -> bool {
            false
        }
        pub fn supported_languages(&self) -> Vec<&str> {
            Vec::new()
        }
        pub fn resolve_file(
            &self,
            _language: &str,
            _file_path: &str,
            _source: &str,
        ) -> anyhow::Result<Vec<FormalEdge>> {
            Ok(Vec::new())
        }
    }
}
pub mod lang_constants;

use std::collections::{HashMap, HashSet};

use crate::types::EdgeConfidence;

#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub confidence: EdgeConfidence,
    pub resolved_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileContext {
    pub file_symbols: HashSet<String>,
    pub import_map: HashMap<String, String>,
    pub type_map: HashMap<String, String>,
}
