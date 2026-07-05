//! Decode a SCIP `Index` into flat occurrences the ingester can match against
//! `ci`'s call sites and symbols. We keep only file/line/symbol/role — SCIP's
//! rich moniker string is preserved verbatim as the identity key.

/// One SCIP occurrence, normalized to 1-based line and `ci`'s conventions.
#[derive(Debug, Clone)]
pub struct ScipOccurrence {
    pub file: String,
    /// 1-based line of the occurrence start.
    pub line: usize,
    /// SCIP symbol moniker (opaque identity string).
    pub symbol: String,
    pub is_def: bool,
    /// True for `local N` monikers (function-scoped, not cross-file useful).
    pub is_local: bool,
}

pub fn parse_index(index: &scip::types::Index) -> Vec<ScipOccurrence> {
    let mut out = Vec::new();
    for doc in &index.documents {
        for occ in &doc.occurrences {
            // SCIP range is [startLine, startChar, endLine, endChar] (0-based) or
            // [startLine, startChar, endChar] when single-line.
            let Some(&start_line) = occ.range.first() else {
                continue;
            };
            let is_def = occ.symbol_roles & (scip::types::SymbolRole::Definition as i32) != 0;
            out.push(ScipOccurrence {
                file: doc.relative_path.clone(),
                line: (start_line as usize) + 1,
                symbol: occ.symbol.clone(),
                is_def,
                is_local: occ.symbol.starts_with("local "),
            });
        }
    }
    out
}

pub fn parse_scip_file(path: &std::path::Path) -> anyhow::Result<Vec<ScipOccurrence>> {
    let bytes = std::fs::read(path)?;
    use protobuf::Message;
    let index = scip::types::Index::parse_from_bytes(&bytes)?;
    Ok(parse_index(&index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_definition_and_reference_occurrences() {
        // Minimal hand-built SCIP index: one doc, one def + one ref.
        let mut index = scip::types::Index::new();
        let mut doc = scip::types::Document::new();
        doc.relative_path = "core/src/engine.rs".into();
        let mut def = scip::types::Occurrence::new();
        def.range = vec![2, 4, 2, 7]; // line 2 (0-based), cols
        def.symbol = "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start().".into();
        def.symbol_roles = scip::types::SymbolRole::Definition as i32;
        let mut rf = scip::types::Occurrence::new();
        rf.range = vec![5, 8, 5, 13];
        rf.symbol = def.symbol.clone();
        doc.occurrences = vec![def, rf];
        index.documents = vec![doc];

        let occ = parse_index(&index);
        assert_eq!(occ.len(), 2);
        let def = occ.iter().find(|o| o.is_def).unwrap();
        assert_eq!(def.file, "core/src/engine.rs");
        assert_eq!(def.line, 3); // 1-based
        assert_eq!(
            def.symbol,
            "rust-analyzer cargo demo-core 0.1.0 engine/impl#[Engine]start()."
        );
    }
}
