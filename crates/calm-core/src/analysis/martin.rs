use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

/// Languages where `symbols.kind` distinguishes an abstract type
/// declaration (interface/trait/protocol) from a concrete one
/// (class/struct/enum) — confirmed by reading `node_kind_to_symbol_kind`
/// and the per-language `detect_*` fallbacks in
/// `crates/calm-core/src/indexer/parser.rs`, not assumed from language
/// name alone:
///
/// - Rust `trait_item`, PHP/Scala/Groovy `trait`, Haskell `class`
///   (typeclass) → `SymbolKind::Trait`
/// - Java/TypeScript/Kotlin/C#/Groovy/PHP `interface`, Swift `protocol`
///   → `SymbolKind::Interface`
///
/// Go is deliberately **excluded** despite having a real interface
/// construct: its `type_spec`/`type_alias` node kind maps to the generic
/// `SymbolKind::Type` for every `type X = ...` declaration alike (see
/// `node_kind_to_symbol_kind` line ~114) — a Go `interface{}` type and an
/// ordinary struct-backed type alias are indistinguishable at the `kind`
/// column today, so `A` cannot be computed honestly for Go. Ruby is also
/// excluded: its `module` keyword (the nearest thing Ruby has to an
/// interface) maps to `SymbolKind::Class`, same as `class`
/// (`test_ruby_real_grammar_class_module_kinds`), for the same reason.
///
/// Every other language (Python, JavaScript, C, C++, Lua, Elixir, Dart,
/// Zig, OCaml, PowerShell, R, shell, SQL, …) has no abstract/concrete
/// type distinction in `symbols.kind` at all and is excluded by omission,
/// not by an explicit "not applicable" list — the allowlist below is the
/// single source of truth for which languages can report `A`.
const ABSTRACTNESS_SUPPORTED_LANGUAGES: &[&str] = &[
    "rust",
    "java",
    "typescript",
    "kotlin",
    "csharp",
    "swift",
    "php",
    "scala",
    "groovy",
    "haskell",
];

/// Concrete-or-abstract type-level symbol kinds that make up the
/// denominator of `A` (abstractness) — every symbol kind that declares a
/// named type. `class`/`struct`/`enum` are concrete; `interface`/`trait`
/// are abstract. Function/method/variable/etc. are not type declarations
/// at all and don't belong in either side of the ratio.
const TYPE_KINDS: &[&str] = &["class", "struct", "enum", "interface", "trait"];
const ABSTRACT_KINDS: &[&str] = &["interface", "trait"];

/// Per-file Martin/OOD metrics. `instability` is always defined once a
/// file clears the `Ca + Ce > 0` population gate (see
/// `compute_martin_metrics`'s doc comment); `abstractness`/`distance` are
/// `None` for a file in a language outside
/// `ABSTRACTNESS_SUPPORTED_LANGUAGES`, or one with zero type-level
/// symbols of its own (e.g. a Rust file that's all free functions) —
/// reporting `A` from an empty denominator would be a fabricated 0, not a
/// conservative one.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMartinMetrics {
    pub path: String,
    /// Afferent coupling: distinct files that import this one.
    pub ca: i64,
    /// Efferent coupling: distinct files this one imports.
    pub ce: i64,
    /// I = Ce / (Ce + Ca).
    pub instability: f64,
    /// A = abstract type symbols / total type symbols, language-gated.
    pub abstractness: Option<f64>,
    /// D = |A + I - 1|, only when `abstractness` is `Some`.
    pub distance: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MartinSummary {
    pub avg_instability: f64,
    pub avg_distance: Option<f64>,
    /// Files with `Ca + Ce > 0` — the only files for which `I` is a
    /// well-defined ratio rather than an arbitrary zero-guard value.
    pub files_measured: i64,
    /// Total indexed source files considered for measurement (excludes
    /// markdown, which is indexed for search but has no import graph and
    /// would only ever pad the denominator with structural zeros).
    pub files_total: i64,
    pub files: Vec<FileMartinMetrics>,
}

/// Computes Ca (afferent coupling) and Ce (efferent coupling) per file
/// from `import_edges`, then derives `I`/`A`/`D` per Robert Martin's
/// OOD metrics. Population is scoped to files with `Ca + Ce > 0`
/// (T1.2): measured on this repo, 118 of 214 indexed files (55%) have no
/// import edge at all — averaging `I`/`D` over the full population would
/// be dominated by structurally-undefined zeros, not a genuine measurement
/// diluted by a few edge cases. `files_measured`/`files_total` are
/// reported alongside every aggregate so that denominator is never
/// invisible to a caller.
///
/// `DISTINCT` in both queries is load-bearing: `import_edges` has no
/// unique constraint on `(from_path, to_path)` and this repo's own table
/// has 13 duplicate pairs today (re-parsing the same import on an
/// incremental reindex before a stale row is pruned) — without it, a
/// re-parsed file would double-count its own coupling.
pub fn compute_martin_metrics(conn: &Connection) -> rusqlite::Result<MartinSummary> {
    let mut ca_map: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT to_path, COUNT(DISTINCT from_path) FROM import_edges \
             WHERE to_path IS NOT NULL GROUP BY to_path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (path, ca) = row?;
            ca_map.insert(path, ca);
        }
    }

    let mut ce_map: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT from_path, COUNT(DISTINCT to_path) FROM import_edges \
             WHERE to_path IS NOT NULL GROUP BY from_path",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (path, ce) = row?;
            ce_map.insert(path, ce);
        }
    }

    let files_total: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT path) FROM symbols WHERE kind != 'heading'",
        [],
        |r| r.get(0),
    )?;

    // Per-file (language, kind) type-symbol tallies — only fetched for
    // languages where `A` is meaningful, so a large repo in mostly
    // unsupported languages doesn't pay for a query whose result it will
    // discard for every row.
    let mut type_counts: HashMap<String, HashMap<&'static str, i64>> = HashMap::new();
    {
        let placeholders = TYPE_KINDS.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let lang_placeholders = ABSTRACTNESS_SUPPORTED_LANGUAGES
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT path, kind, COUNT(*) FROM symbols \
             WHERE kind IN ({placeholders}) AND language IN ({lang_placeholders}) \
             GROUP BY path, kind"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = TYPE_KINDS
            .iter()
            .map(|k| k as &dyn rusqlite::ToSql)
            .chain(
                ABSTRACTNESS_SUPPORTED_LANGUAGES
                    .iter()
                    .map(|l| l as &dyn rusqlite::ToSql),
            )
            .collect();
        let rows = stmt.query_map(params.as_slice(), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (path, kind, count) = row?;
            let kind_static = TYPE_KINDS.iter().find(|k| **k == kind).copied();
            if let Some(kind_static) = kind_static {
                *type_counts
                    .entry(path)
                    .or_default()
                    .entry(kind_static)
                    .or_insert(0) += count;
            }
        }
    }

    let all_paths: HashSet<String> = ca_map.keys().chain(ce_map.keys()).cloned().collect();

    let mut files: Vec<FileMartinMetrics> = all_paths
        .into_iter()
        .filter_map(|path| {
            let ca = ca_map.get(&path).copied().unwrap_or(0);
            let ce = ce_map.get(&path).copied().unwrap_or(0);
            if ca + ce == 0 {
                return None;
            }
            let instability = ce as f64 / (ce + ca) as f64;

            let abstractness = type_counts.get(&path).and_then(|kinds| {
                let total: i64 = TYPE_KINDS
                    .iter()
                    .map(|k| kinds.get(k).copied().unwrap_or(0))
                    .sum();
                if total == 0 {
                    return None;
                }
                let abstract_count: i64 = ABSTRACT_KINDS
                    .iter()
                    .map(|k| kinds.get(k).copied().unwrap_or(0))
                    .sum();
                Some(abstract_count as f64 / total as f64)
            });
            let distance = abstractness.map(|a| (a + instability - 1.0).abs());

            Some(FileMartinMetrics {
                path,
                ca,
                ce,
                instability,
                abstractness,
                distance,
            })
        })
        .collect();

    files.sort_by(|a, b| a.path.cmp(&b.path));

    let files_measured = files.len() as i64;
    let avg_instability = if files_measured > 0 {
        files.iter().map(|f| f.instability).sum::<f64>() / files_measured as f64
    } else {
        0.0
    };
    let distances: Vec<f64> = files.iter().filter_map(|f| f.distance).collect();
    let avg_distance = if distances.is_empty() {
        None
    } else {
        Some(distances.iter().sum::<f64>() / distances.len() as f64)
    };

    Ok(MartinSummary {
        avg_instability,
        avg_distance,
        files_measured,
        files_total,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn insert_symbol(conn: &Connection, path: &str, kind: &str, language: &str, name: &str) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 1)",
            rusqlite::params![format!("{path}::{name}"), name, kind, language, path],
        )
        .unwrap();
    }

    fn insert_import(conn: &Connection, from_path: &str, to_path: &str) {
        conn.execute(
            // OR IGNORE mirrors production (indexer::edges): the UNIQUE index
            // on import_edges makes a duplicate insert a silent no-op.
            "INSERT OR IGNORE INTO import_edges (from_path, to_path, module_name) VALUES (?1, ?2, 'x')",
            rusqlite::params![from_path, to_path],
        )
        .unwrap();
    }

    #[test]
    fn test_empty_db_reports_zero_measured() {
        let conn = test_conn();
        let summary = compute_martin_metrics(&conn).unwrap();
        assert_eq!(summary.files_measured, 0);
        assert_eq!(summary.files_total, 0);
        assert_eq!(summary.avg_instability, 0.0);
        assert_eq!(summary.avg_distance, None);
    }

    #[test]
    fn test_isolated_file_excluded_from_measured_population() {
        let conn = test_conn();
        insert_symbol(&conn, "isolated.rs", "function", "rust", "f");
        insert_symbol(&conn, "a.rs", "function", "rust", "g");
        insert_symbol(&conn, "b.rs", "function", "rust", "h");
        insert_import(&conn, "a.rs", "b.rs");

        let summary = compute_martin_metrics(&conn).unwrap();
        assert_eq!(
            summary.files_total, 3,
            "isolated.rs, a.rs, b.rs all indexed"
        );
        assert_eq!(
            summary.files_measured, 2,
            "isolated.rs has Ca=Ce=0 and must not enter the measured population"
        );
        assert!(!summary.files.iter().any(|f| f.path == "isolated.rs"));
    }

    #[test]
    fn test_pure_efferent_file_is_fully_unstable() {
        let conn = test_conn();
        insert_symbol(&conn, "leaf.rs", "function", "rust", "f");
        insert_symbol(&conn, "root.rs", "function", "rust", "g");
        insert_import(&conn, "root.rs", "leaf.rs");

        let summary = compute_martin_metrics(&conn).unwrap();
        let root = summary.files.iter().find(|f| f.path == "root.rs").unwrap();
        assert_eq!(root.ca, 0);
        assert_eq!(root.ce, 1);
        assert_eq!(
            root.instability, 1.0,
            "pure importer, no dependents -> I=1.0"
        );

        let leaf = summary.files.iter().find(|f| f.path == "leaf.rs").unwrap();
        assert_eq!(leaf.ca, 1);
        assert_eq!(leaf.ce, 0);
        assert_eq!(
            leaf.instability, 0.0,
            "pure dependency, imports nothing -> I=0.0"
        );
    }

    #[test]
    fn test_duplicate_import_edges_do_not_inflate_coupling() {
        let conn = test_conn();
        insert_symbol(&conn, "a.rs", "function", "rust", "f");
        insert_symbol(&conn, "b.rs", "function", "rust", "g");
        insert_import(&conn, "a.rs", "b.rs");
        insert_import(&conn, "a.rs", "b.rs"); // duplicate attempt — dropped by the UNIQUE index

        let summary = compute_martin_metrics(&conn).unwrap();
        let a = summary.files.iter().find(|f| f.path == "a.rs").unwrap();
        // The UNIQUE index on import_edges (2026-07-28) prevents the duplicate
        // from ever landing; compute_martin_metrics' own COUNT(DISTINCT) is
        // retained as defense-in-depth. Either way, coupling stays at 1.
        assert_eq!(a.ce, 1, "duplicate import edge must not inflate coupling");
    }

    #[test]
    fn test_abstractness_computed_for_supported_language_with_type_symbols() {
        let conn = test_conn();
        insert_symbol(&conn, "shape.rs", "trait", "rust", "Shape");
        insert_symbol(&conn, "shape.rs", "struct", "rust", "Circle");
        insert_symbol(&conn, "shape.rs", "function", "rust", "area");
        insert_symbol(&conn, "user.rs", "function", "rust", "main");
        insert_import(&conn, "user.rs", "shape.rs");

        let summary = compute_martin_metrics(&conn).unwrap();
        let shape = summary.files.iter().find(|f| f.path == "shape.rs").unwrap();
        assert_eq!(
            shape.abstractness,
            Some(0.5),
            "1 trait + 1 struct = 2 type symbols, 1 abstract -> A=0.5"
        );
        assert!(shape.distance.is_some());
    }

    #[test]
    fn test_abstractness_none_for_unsupported_language() {
        let conn = test_conn();
        insert_symbol(&conn, "main.go", "type", "go", "Shape");
        insert_symbol(&conn, "user.go", "function", "go", "main");
        insert_import(&conn, "user.go", "main.go");

        let summary = compute_martin_metrics(&conn).unwrap();
        let main_go = summary.files.iter().find(|f| f.path == "main.go").unwrap();
        assert_eq!(
            main_go.abstractness, None,
            "Go's type_spec conflates interface and concrete types in `kind` -- must not fabricate A"
        );
        assert_eq!(main_go.distance, None);
    }

    #[test]
    fn test_abstractness_none_when_supported_language_file_has_no_type_symbols() {
        let conn = test_conn();
        insert_symbol(&conn, "util.rs", "function", "rust", "helper");
        insert_symbol(&conn, "user.rs", "function", "rust", "main");
        insert_import(&conn, "user.rs", "util.rs");

        let summary = compute_martin_metrics(&conn).unwrap();
        let util = summary.files.iter().find(|f| f.path == "util.rs").unwrap();
        assert_eq!(
            util.abstractness, None,
            "zero type-level symbols -> empty denominator must not become a fabricated 0.0"
        );
    }

    #[test]
    fn test_avg_distance_only_averages_over_files_with_abstractness() {
        let conn = test_conn();
        // Rust file with a resolvable A.
        insert_symbol(&conn, "shape.rs", "trait", "rust", "Shape");
        insert_symbol(&conn, "user.rs", "function", "rust", "main");
        insert_import(&conn, "user.rs", "shape.rs");
        // Go file with no resolvable A, but still enters files_measured.
        insert_symbol(&conn, "main.go", "type", "go", "Widget");
        insert_symbol(&conn, "caller.go", "function", "go", "main");
        insert_import(&conn, "caller.go", "main.go");

        let summary = compute_martin_metrics(&conn).unwrap();
        assert_eq!(summary.files_measured, 4);
        assert!(
            summary.avg_distance.is_some(),
            "at least one file (shape.rs) has a resolvable A"
        );
    }
}
