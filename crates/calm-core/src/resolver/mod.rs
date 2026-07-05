pub mod conservative;
pub mod formal;
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
