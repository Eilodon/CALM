use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// One declared architecture rule: files under `from` must not import files
/// under `to`. `from`/`to` are project-relative, forward-slash paths (the
/// same format `import_edges.from_path`/`to_path` already use). A pattern
/// containing a glob metacharacter (`*`, `?`, `[`) is matched with `globset`
/// against the whole path (e.g. `"crates/*/src/indexer/**"`); otherwise it's
/// treated as a plain directory prefix via `starts_with` (e.g.
/// `"crates/calm-core/src/indexer/"`) — existing prefix-style rules keep
/// working unchanged.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BoundaryRule {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundaryViolation {
    pub from_path: String,
    pub to_path: String,
    pub rule_from: String,
    pub rule_to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Checks every `import_edges` row against every declared rule. A violation
/// is reported per (edge, rule) pair — the same edge can violate more than
/// one rule if rules overlap, which is surfaced rather than deduplicated so
/// each rule's own `reason` is visible.
enum PathMatcher {
    Prefix(String),
    Glob(globset::GlobMatcher),
}

impl PathMatcher {
    /// A pattern containing a glob metacharacter is compiled with `globset`;
    /// otherwise (including invalid glob syntax) it falls back to a plain
    /// `starts_with` prefix check, so existing prefix-style rules are
    /// unaffected and a typo'd glob degrades to its literal prefix rather
    /// than silently matching nothing.
    fn new(pattern: &str) -> Self {
        if pattern.contains(['*', '?', '[']) {
            if let Ok(glob) = globset::Glob::new(pattern) {
                return PathMatcher::Glob(glob.compile_matcher());
            }
        }
        PathMatcher::Prefix(pattern.to_string())
    }

    fn matches(&self, path: &str) -> bool {
        match self {
            PathMatcher::Prefix(prefix) => path.starts_with(prefix.as_str()),
            PathMatcher::Glob(matcher) => matcher.is_match(path),
        }
    }
}

pub fn check_boundaries(
    conn: &Connection,
    rules: &[BoundaryRule],
) -> rusqlite::Result<Vec<BoundaryViolation>> {
    if rules.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT DISTINCT from_path, to_path FROM import_edges WHERE to_path IS NOT NULL",
    )?;
    let edges: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let matchers: Vec<(PathMatcher, PathMatcher)> = rules
        .iter()
        .map(|rule| (PathMatcher::new(&rule.from), PathMatcher::new(&rule.to)))
        .collect();

    let mut violations = Vec::new();
    for (from_path, to_path) in &edges {
        for (rule, (from_matcher, to_matcher)) in rules.iter().zip(&matchers) {
            if from_matcher.matches(from_path) && to_matcher.matches(to_path) {
                violations.push(BoundaryViolation {
                    from_path: from_path.clone(),
                    to_path: to_path.clone(),
                    rule_from: rule.from.clone(),
                    rule_to: rule.to.clone(),
                    reason: rule.reason.clone(),
                });
            }
        }
    }

    violations.sort_by(|a, b| {
        a.from_path
            .cmp(&b.from_path)
            .then_with(|| a.to_path.cmp(&b.to_path))
    });
    Ok(violations)
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

    fn insert_import(conn: &Connection, from_path: &str, to_path: &str) {
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES (?1, ?2, 'x')",
            rusqlite::params![from_path, to_path],
        )
        .unwrap();
    }

    #[test]
    fn test_no_rules_no_violations() {
        let conn = test_conn();
        insert_import(
            &conn,
            "crates/calm-core/src/indexer/parser.rs",
            "crates/calm-server/src/tools.rs",
        );
        let violations = check_boundaries(&conn, &[]).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn test_detects_forbidden_import() {
        let conn = test_conn();
        insert_import(
            &conn,
            "crates/calm-core/src/indexer/parser.rs",
            "crates/calm-server/src/tools.rs",
        );
        let rules = vec![BoundaryRule {
            from: "crates/calm-core/".into(),
            to: "crates/calm-server/".into(),
            reason: "core must not depend on the server layer".into(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].from_path,
            "crates/calm-core/src/indexer/parser.rs"
        );
        assert_eq!(violations[0].to_path, "crates/calm-server/src/tools.rs");
        assert_eq!(
            violations[0].reason,
            "core must not depend on the server layer"
        );
    }

    #[test]
    fn test_allowed_import_is_not_flagged() {
        let conn = test_conn();
        insert_import(
            &conn,
            "crates/calm-server/src/tools.rs",
            "crates/calm-core/src/search.rs",
        );
        let rules = vec![BoundaryRule {
            from: "crates/calm-core/".into(),
            to: "crates/calm-server/".into(),
            reason: String::new(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert!(
            violations.is_empty(),
            "server -> core is the allowed direction, only core -> server is forbidden"
        );
    }

    #[test]
    fn test_null_to_path_is_skipped() {
        let conn = test_conn();
        // Unresolved external import — to_path is NULL.
        conn.execute(
            "INSERT INTO import_edges (from_path, to_path, module_name) VALUES ('crates/calm-core/src/indexer/parser.rs', NULL, 'external_crate')",
            [],
        )
        .unwrap();
        let rules = vec![BoundaryRule {
            from: "crates/calm-core/".into(),
            to: "crates/calm-server/".into(),
            reason: String::new(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn test_edge_matching_multiple_rules_reports_each() {
        let conn = test_conn();
        insert_import(&conn, "a/x.py", "b/y.py");
        let rules = vec![
            BoundaryRule {
                from: "a/".into(),
                to: "b/".into(),
                reason: "rule 1".into(),
            },
            BoundaryRule {
                from: "a/".into(),
                to: "b/".into(),
                reason: "rule 2 (overlaps rule 1 on purpose)".into(),
            },
        ];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert_eq!(violations.len(), 2);
    }

    #[test]
    fn test_glob_rule_matches_nested_paths() {
        let conn = test_conn();
        insert_import(&conn, "crates/calm-core/src/indexer/foo.rs", "crates/calm-server/src/tools/orient.rs");
        let rules = vec![BoundaryRule {
            from: "crates/*/src/indexer/**".into(),
            to: "crates/*/src/tools/**".into(),
            reason: "indexer must not import server tools".into(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn test_glob_rule_does_not_match_unrelated_path() {
        let conn = test_conn();
        insert_import(&conn, "crates/calm-core/src/other/foo.rs", "crates/calm-server/src/tools/orient.rs");
        let rules = vec![BoundaryRule {
            from: "crates/*/src/indexer/**".into(),
            to: "crates/*/src/tools/**".into(),
            reason: "indexer must not import server tools".into(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn test_invalid_glob_falls_back_to_literal_prefix() {
        let conn = test_conn();
        insert_import(&conn, "a/[unclosed/x.py", "b/y.py");
        let rules = vec![BoundaryRule {
            from: "a/[unclosed".into(),
            to: "b/".into(),
            reason: "invalid glob syntax degrades to prefix match".into(),
        }];
        let violations = check_boundaries(&conn, &rules).unwrap();
        assert_eq!(violations.len(), 1);
    }
}
