use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexingPhase {
    Scanning,
    Parsing,
    BuildingEdges,
    Ready,
}

impl IndexingPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scanning => "scanning",
            Self::Parsing => "parsing",
            Self::BuildingEdges => "building_edges",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeConfidence {
    Formal,
    Resolved,
    Inferred,
    Textual,
    /// A bare-name match with >1 same-named candidate and no in-file
    /// preference to break the tie (the `rebuild_graph` `MAX_CALLEE_CANDIDATES`
    /// fallback) — one edge is emitted per candidate, so this call site is
    /// double/triple/etc.-counted across unrelated symbols. Distinct from
    /// `Textual` (which still names exactly one real candidate) precisely so
    /// consumers can tell "low-confidence but singular" apart from "spread
    /// across N locations, most likely wrong for N-1 of them".
    Ambiguous,
}

impl EdgeConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Formal => "formal",
            Self::Resolved => "resolved",
            Self::Inferred => "inferred",
            Self::Textual => "textual",
            Self::Ambiguous => "ambiguous",
        }
    }

    pub fn rank(&self) -> u8 {
        match self {
            Self::Formal => 4,
            Self::Resolved => 3,
            Self::Inferred => 2,
            Self::Textual => 1,
            Self::Ambiguous => 0,
        }
    }

    /// Inverse of `as_str` — parses a DB-stored `edge_confidence` value back
    /// into the typed enum. `None` on an unrecognized string (defensive;
    /// every writer goes through `as_str`, so this should never happen on
    /// data this codebase produced itself).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "formal" => Some(Self::Formal),
            "resolved" => Some(Self::Resolved),
            "inferred" => Some(Self::Inferred),
            "textual" => Some(Self::Textual),
            "ambiguous" => Some(Self::Ambiguous),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Symbol,
    Text,
    File,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadCodeConfidence {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadCodeSource {
    #[serde(rename = "static")]
    Static,
    #[serde(rename = "static+coverage")]
    StaticPlusCoverage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbedStatus {
    Disabled,
    Downloading,
    Embedding,
    Ready,
    Failed,
    /// The vendored default-model asset is unusable (e.g. an unresolved Git
    /// LFS pointer) and `semantic_search.allow_network_fallback` is `false`,
    /// so semantic search stays off rather than reaching the network — a
    /// deliberate policy outcome, distinct from `Failed` (an unexpected
    /// error). `indexing_status(retry_embeddings: true)` re-checks this the
    /// same way it reclaims `Failed`, so flipping the config and retrying
    /// recovers without a restart.
    OfflineUnavailable,
}

impl EmbedStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Downloading => "downloading",
            Self::Embedding => "embedding",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::OfflineUnavailable => "offline_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminatedBy {
    Timeout,
    MaxHops,
    PathCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Interface,
    Type,
    Variable,
    Enum,
    Constructor,
    Struct,
    Trait,
    Impl,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Method => "method",
            Self::Interface => "interface",
            Self::Type => "type",
            Self::Variable => "variable",
            Self::Enum => "enum",
            Self::Constructor => "constructor",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Impl => "impl",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_confidence_parse_roundtrips_with_as_str() {
        for ec in [
            EdgeConfidence::Formal,
            EdgeConfidence::Resolved,
            EdgeConfidence::Inferred,
            EdgeConfidence::Textual,
        ] {
            assert_eq!(EdgeConfidence::parse(ec.as_str()), Some(ec));
        }
    }

    #[test]
    fn test_edge_confidence_parse_rejects_unknown() {
        assert_eq!(EdgeConfidence::parse("bogus"), None);
        assert_eq!(EdgeConfidence::parse(""), None);
    }

    #[test]
    fn test_edge_confidence_rank_orders_formal_highest() {
        assert!(EdgeConfidence::Formal.rank() > EdgeConfidence::Resolved.rank());
        assert!(EdgeConfidence::Resolved.rank() > EdgeConfidence::Inferred.rank());
        assert!(EdgeConfidence::Inferred.rank() > EdgeConfidence::Textual.rank());
    }
}
