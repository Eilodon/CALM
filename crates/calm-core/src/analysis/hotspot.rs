use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;

use crate::config::HotspotsConfig;

#[derive(Debug, Clone)]
pub struct ChurnInfo {
    pub commit_count: i64,
    pub authors: HashSet<String>,
    pub last_changed: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComplexityInfo {
    pub symbol_count: i64,
    pub hub_count: i64,
    pub avg_caller_count: f64,
    pub connected_coreness_count: i64,
    pub language: String,
}

#[derive(Debug, Clone)]
pub struct HotspotSymbol {
    pub name: String,
    pub kind: String,
    pub is_hub: bool,
    pub coreness: Option<i64>,
    pub caller_count: i64,
    /// Disambiguates two same-named symbols in the same file (e.g. a
    /// `#[cfg(feature)]` real impl vs. its stub) — mirrors `symbol_info`,
    /// which already carries these for the same reason.
    pub line_start: i64,
    pub line_end: i64,
}

#[derive(Debug, Clone)]
pub struct HotspotEntry {
    pub path: String,
    pub language: String,
    pub churn: ChurnInfo,
    pub complexity: ComplexityInfo,
    pub hotspot_score: f64,
    pub risk_level: String,
    pub top_symbols: Option<Vec<HotspotSymbol>>,
}

#[derive(Debug)]
pub struct HotspotsOutput {
    pub hotspots: Vec<HotspotEntry>,
    pub git_available: bool,
    pub since: String,
    pub total_files_analyzed: usize,
    pub hotspot_method: String,
    pub note: String,
}

pub fn compute_hotspots(
    project_root: &Path,
    conn: &Connection,
    config: &HotspotsConfig,
    top_n: usize,
    since: &str,
    min_churn: i64,
    include_symbols: bool,
) -> HotspotsOutput {
    // Step 1: Churn from git (optional)
    let (churn_map, git_available) = collect_git_churn(project_root, since);

    // Step 2: Complexity from index
    let complexity_map = collect_complexity(conn);

    // Step 3: Merge + normalize
    let candidates: HashMap<String, ChurnInfo> = if git_available {
        churn_map
            .into_iter()
            .filter(|(path, data)| {
                data.commit_count >= min_churn && complexity_map.contains_key(path)
            })
            .collect()
    } else {
        complexity_map
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    ChurnInfo {
                        commit_count: 0,
                        authors: HashSet::new(),
                        last_changed: None,
                    },
                )
            })
            .collect()
    };

    if candidates.is_empty() {
        let note = if git_available {
            format!(
                "No files exceeded min_churn={min_churn} commits since {since}. Try reducing min_churn."
            )
        } else {
            "Git unavailable: ranking by complexity only. min_churn parameter not applied."
                .to_string()
        };
        return HotspotsOutput {
            hotspots: Vec::new(),
            git_available,
            since: since.to_string(),
            total_files_analyzed: 0,
            hotspot_method: if git_available {
                "git+index"
            } else {
                "index_only"
            }
            .to_string(),
            note,
        };
    }

    let total_files_analyzed = candidates.len();

    let churn_scores: HashMap<&str, f64> = candidates
        .iter()
        .map(|(p, d)| (p.as_str(), d.commit_count as f64))
        .collect();
    let compl_scores: HashMap<&str, f64> = candidates
        .keys()
        .filter_map(|p| {
            complexity_map
                .get(p)
                .map(|c| (p.as_str(), complexity_score(c)))
        })
        .collect();

    let max_churn = churn_scores.values().cloned().fold(1.0_f64, f64::max);
    let max_compl = compl_scores.values().cloned().fold(1.0_f64, f64::max);

    let mut results: Vec<HotspotEntry> = candidates
        .iter()
        .filter_map(|(path, churn)| {
            let cm = complexity_map.get(path)?;
            let norm_compl = compl_scores.get(path.as_str()).copied().unwrap_or(0.0) / max_compl;
            let score = if git_available {
                let norm_churn =
                    churn_scores.get(path.as_str()).copied().unwrap_or(0.0) / max_churn;
                norm_churn * norm_compl
            } else {
                norm_compl
            };

            let risk_level = if score >= config.risk_critical_threshold {
                "critical"
            } else if score >= config.risk_high_threshold {
                "high"
            } else if score >= config.risk_medium_threshold {
                "medium"
            } else {
                "low"
            };

            Some(HotspotEntry {
                path: path.clone(),
                language: cm.language.clone(),
                churn: churn.clone(),
                complexity: cm.clone(),
                hotspot_score: (score * 10000.0).round() / 10000.0,
                risk_level: risk_level.to_string(),
                top_symbols: None,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.hotspot_score
            .partial_cmp(&a.hotspot_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_n);

    if include_symbols {
        for entry in &mut results {
            entry.top_symbols = Some(query_top_symbols(conn, &entry.path));
        }
    }

    let note = if git_available {
        if results.is_empty() {
            format!(
                "No files exceeded min_churn={min_churn} commits since {since}. Try reducing min_churn."
            )
        } else {
            format!("Analyzed commits since {since}.")
        }
    } else {
        "Git unavailable: ranking by complexity only. min_churn parameter not applied.".to_string()
    };

    HotspotsOutput {
        hotspots: results,
        git_available,
        since: since.to_string(),
        total_files_analyzed,
        hotspot_method: if git_available {
            "git+index"
        } else {
            "index_only"
        }
        .to_string(),
        note,
    }
}

fn complexity_score(c: &ComplexityInfo) -> f64 {
    c.symbol_count as f64 * 0.3
        + c.hub_count as f64 * 3.0
        + c.connected_coreness_count as f64 * 1.5
        + c.avg_caller_count * 0.5
}

/// "Already quite high in absolute terms" reference points for
/// [`compute_absolute_hotspot_risk`] — deliberately independent of any other
/// file in the repo (see that function's doc comment for why).
const ABSOLUTE_CHURN_REFERENCE: f64 = 50.0;
const ABSOLUTE_COMPLEXITY_REFERENCE: f64 = 150.0;

/// Highest single-file hotspot risk on an absolute 0..=1 scale, for use as a
/// fitness-check gate — unlike `compute_hotspots`' `hotspot_score`, which is
/// *relative*: min-max normalized against this same repo's own busiest and
/// most complex file (`norm = value / max_in_this_repo`). That's the right
/// behavior for the `hotspots` tool ("which file should I look at first, in
/// this repo") but makes the #1-ranked file's score approach 1.0 *by
/// construction* — divided by its own repo's max, it doesn't matter whether
/// that max is genuinely alarming or the repo is small and healthy, there is
/// always a "biggest" file. Comparing that relative score against a fixed
/// `max_hotspot_risk` threshold is a category error: a relative ranking
/// compared against an absolute gate will tend to fail for nearly any
/// multi-file repo where churn and complexity concentrate in the same file
/// (common — the main implementation file usually is both), regardless of
/// whether the codebase is actually healthy.
///
/// This anchors both dimensions to fixed reference points instead, so a
/// file only approaches 1.0 by being absolutely high on both axes, not
/// merely higher than its neighbors.
pub fn compute_absolute_hotspot_risk(project_root: &Path, conn: &Connection, since: &str) -> f64 {
    let (churn_map, git_available) = collect_git_churn(project_root, since);
    let complexity_map = collect_complexity(conn);

    complexity_map
        .iter()
        .map(|(path, cm)| {
            let norm_compl = (complexity_score(cm) / ABSOLUTE_COMPLEXITY_REFERENCE).min(1.0);
            if git_available {
                let commits = churn_map.get(path).map(|c| c.commit_count).unwrap_or(0);
                let norm_churn = (commits as f64 / ABSOLUTE_CHURN_REFERENCE).min(1.0);
                norm_churn * norm_compl
            } else {
                // Matches compute_hotspots' own no-git fallback: rank by
                // complexity alone rather than treating "no churn data" as
                // "zero churn" (which would silently zero out every score).
                norm_compl
            }
        })
        .fold(0.0_f64, f64::max)
}

fn collect_git_churn(project_root: &Path, since: &str) -> (HashMap<String, ChurnInfo>, bool) {
    // Git already reports paths relative to `current_dir` (project_root) using
    // forward slashes — this must match `symbols.path`'s format exactly (see
    // `pipeline::rel_path`), or the churn/complexity merge below silently drops
    // every candidate.
    let (commits, git_available) = super::git_log::commits_with_files(project_root, since);
    if !git_available {
        return (HashMap::new(), false);
    }

    let mut churn_map: HashMap<String, ChurnInfo> = HashMap::new();
    // `commits` is newest-first (git log's default order): `or_insert_with`
    // only fires on a file's first occurrence, so `last_changed` naturally
    // ends up as the date of the most recent commit that touched it.
    for commit in &commits {
        for path in &commit.files {
            let entry = churn_map.entry(path.clone()).or_insert_with(|| ChurnInfo {
                commit_count: 0,
                authors: HashSet::new(),
                // H-1 fix: last_changed is Option<String>, None instead of ""
                last_changed: commit.date.clone(),
            });
            entry.commit_count += 1;
            if let Some(ref author) = commit.author {
                entry.authors.insert(author.clone());
            }
        }
    }
    (churn_map, true)
}

fn collect_complexity(conn: &Connection) -> HashMap<String, ComplexityInfo> {
    // is_test = 0: test functions structurally call production code, so they
    // almost always end up with coreness > 0 despite never being called
    // *back* (never hubs). Left unfiltered, a thoroughly-tested file's own
    // test module inflates connected_coreness_count/symbol_count as if that
    // were production complexity — penalizing exactly the files that
    // invested most in test coverage. Same taxonomy as the dead_code_pct /
    // edge_coverage_pct denominators: only count symbols that can actually
    // carry production risk.
    let mut stmt = conn
        .prepare(
            "SELECT path, \
             COUNT(*) as symbol_count, \
             SUM(CASE WHEN is_hub = 1 THEN 1 ELSE 0 END) as hub_count, \
             AVG(COALESCE(caller_count, 0)) as avg_caller_count, \
             SUM(CASE WHEN coreness > 0 THEN 1 ELSE 0 END) as connected_coreness_count, \
             MAX(language) as language \
             FROM symbols WHERE path IS NOT NULL AND is_test = 0 GROUP BY path",
        )
        .unwrap();

    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            ComplexityInfo {
                symbol_count: row.get(1)?,
                hub_count: row.get::<_, i64>(2).unwrap_or(0),
                avg_caller_count: row.get::<_, f64>(3).unwrap_or(0.0),
                connected_coreness_count: row.get::<_, i64>(4).unwrap_or(0),
                language: row.get::<_, String>(5).unwrap_or_default(),
            },
        ))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn query_top_symbols(conn: &Connection, path: &str) -> Vec<HotspotSymbol> {
    let mut stmt = conn
        .prepare(
            "SELECT name, kind, is_hub, coreness, caller_count, line_start, line_end \
             FROM symbols WHERE path = ? \
             ORDER BY COALESCE(caller_count, 0) DESC, coreness DESC \
             LIMIT 5",
        )
        .unwrap();

    stmt.query_map([path], |row| {
        Ok(HotspotSymbol {
            name: row.get(0)?,
            kind: row.get(1)?,
            is_hub: row.get::<_, i32>(2).unwrap_or(0) != 0,
            coreness: row.get(3)?,
            caller_count: row.get::<_, i64>(4).unwrap_or(0),
            line_start: row.get(5)?,
            line_end: row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use std::process::Command;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn insert_symbol(
        conn: &Connection,
        qname: &str,
        path: &str,
        caller_count: i64,
        is_hub: bool,
        coreness: i64,
    ) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, caller_count, is_hub, coreness, indexed_at) \
             VALUES (?, ?, 'function', 'python', ?, 1, 10, ?, ?, ?, 0.0)",
            rusqlite::params![qname, qname, path, caller_count, is_hub as i32, coreness],
        )
        .unwrap();
    }

    #[test]
    fn test_empty_index_no_git() {
        let conn = setup_db();
        let config = HotspotsConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let output = compute_hotspots(dir.path(), &conn, &config, 10, "6 months ago", 2, false);
        assert!(output.hotspots.is_empty());
        assert!(!output.git_available);
    }

    #[test]
    fn test_complexity_only_ranking() {
        let conn = setup_db();
        let dir = tempfile::tempdir().unwrap();
        // File a: 1 symbol, 0 hubs
        insert_symbol(&conn, "a.func1", "/a.py", 0, false, 0);
        // File b: 2 symbols, 1 hub, high coreness
        insert_symbol(&conn, "b.func1", "/b.py", 10, true, 5);
        insert_symbol(&conn, "b.func2", "/b.py", 2, false, 1);

        let config = HotspotsConfig::default();
        let output = compute_hotspots(dir.path(), &conn, &config, 10, "6 months ago", 2, false);
        assert!(!output.git_available);
        assert_eq!(output.hotspot_method, "index_only");
        assert!(!output.hotspots.is_empty());
        // b.py should rank higher (more symbols, hub, coreness)
        assert_eq!(output.hotspots[0].path, "/b.py");
    }

    #[test]
    fn test_include_symbols() {
        let conn = setup_db();
        let dir = tempfile::tempdir().unwrap();
        insert_symbol(&conn, "m.foo", "/m.py", 5, true, 3);
        insert_symbol(&conn, "m.bar", "/m.py", 1, false, 0);

        let config = HotspotsConfig::default();
        let output = compute_hotspots(dir.path(), &conn, &config, 10, "6 months ago", 2, true);
        assert!(!output.hotspots.is_empty());
        let syms = output.hotspots[0].top_symbols.as_ref().unwrap();
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "m.foo"); // higher caller_count first
        // Regression: line_start/line_end must be carried through so two
        // same-named symbols in one file (e.g. a #[cfg(feature)] real impl
        // vs. its stub) are distinguishable, same as symbol_info already does.
        assert_eq!(syms[0].line_start, 1);
        assert_eq!(syms[0].line_end, 10);
    }

    fn insert_test_symbol(conn: &Connection, qname: &str, path: &str, coreness: i64) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, caller_count, is_hub, coreness, is_test, indexed_at) \
             VALUES (?, ?, 'function', 'python', ?, 1, 10, 0, 0, ?, 1, 0.0)",
            rusqlite::params![qname, qname, path, coreness],
        )
        .unwrap();
    }

    /// Regression: a file's own `#[test]` functions must not count toward its
    /// complexity score. Test functions almost always end up `coreness > 0`
    /// (they call production code) despite never being hubs, so a
    /// thoroughly-tested file with modest production code but many test
    /// cases used to look far more "complex" than one with the same
    /// production code and no tests at all — penalizing test coverage.
    #[test]
    fn test_complexity_ignores_test_symbols() {
        let conn = setup_db();
        insert_symbol(&conn, "prod.func1", "/prod.py", 0, false, 1);
        for i in 0..50 {
            insert_test_symbol(&conn, &format!("prod.test_func{i}"), "/prod.py", 1);
        }

        let complexity = collect_complexity(&conn);
        let info = complexity
            .get("/prod.py")
            .expect("prod.py should be present");
        assert_eq!(
            info.symbol_count, 1,
            "the 50 test-flagged symbols must not inflate symbol_count"
        );
        assert_eq!(info.connected_coreness_count, 1);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// Regression test for the path-format mismatch between `collect_git_churn`
    /// (which used to absolutize paths) and `symbols.path` (always project-root-
    /// relative): with a real git repo present, churn-ranked hotspots must
    /// actually surface indexed files, not silently merge to an empty result.
    #[test]
    fn test_git_churn_merges_with_relative_symbol_paths() {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("hot.py"), "def foo():\n    pass\n").unwrap();
        run_git(dir.path(), &["add", "hot.py"]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        // A second commit so commit_count >= min_churn=2.
        std::fs::write(dir.path().join("hot.py"), "def foo():\n    return 1\n").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "update"]);

        let conn = setup_db();
        insert_symbol(&conn, "hot.foo", "hot.py", 3, true, 1);

        let config = HotspotsConfig::default();
        let output = compute_hotspots(dir.path(), &conn, &config, 10, "1 year", 2, false);

        assert!(output.git_available);
        assert_eq!(output.hotspot_method, "git+index");
        assert_eq!(output.hotspots.len(), 1);
        assert_eq!(output.hotspots[0].path, "hot.py");
        assert_eq!(output.hotspots[0].churn.commit_count, 2);
    }

    /// Regression: `compute_absolute_hotspot_risk` must NOT saturate to
    /// ~1.0 just because one file is *relatively* the busiest in a small,
    /// healthy repo — unlike `compute_hotspots`' `hotspot_score` (min-max
    /// normalized against this same repo's own max), which mathematically
    /// approaches 1.0 for the #1 file regardless of absolute health, since
    /// it's divided by its own repo's max. Here `busy.py` has 2 commits and
    /// a single tiny symbol — the repo's relatively "busiest" file, but
    /// nowhere near the absolute reference points (50 commits, complexity
    /// 150) — and must score far below the fitness-check's
    /// `max_hotspot_risk` threshold (0.75).
    #[test]
    fn test_absolute_hotspot_risk_does_not_saturate_for_healthy_small_repo() {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("busy.py"), "def foo():\n    pass\n").unwrap();
        std::fs::write(dir.path().join("quiet.py"), "def bar():\n    pass\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        // One more commit to busy.py only, making it the repo's relatively
        // "busiest" file (2 commits vs. quiet.py's 1) — exactly the
        // situation that saturated the old relative score to ~1.0.
        std::fs::write(dir.path().join("busy.py"), "def foo():\n    return 1\n").unwrap();
        run_git(dir.path(), &["commit", "-q", "-am", "update busy"]);

        let conn = setup_db();
        insert_symbol(&conn, "busy.foo", "busy.py", 0, false, 0);
        insert_symbol(&conn, "quiet.bar", "quiet.py", 0, false, 0);

        let risk = compute_absolute_hotspot_risk(dir.path(), &conn, "1 year");
        assert!(
            risk < 0.1,
            "a 2-commit, 1-symbol file is nowhere near the absolute reference points, got {risk}"
        );
    }

    /// Companion case: a file that genuinely IS high churn and high
    /// complexity in absolute terms still scores high — the fix changes the
    /// *normalization basis*, not whether real hotspots get flagged.
    #[test]
    fn test_absolute_hotspot_risk_still_flags_genuinely_busy_file() {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test"]);
        std::fs::write(dir.path().join("giant.py"), "def f():\n    pass\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        for i in 0..60 {
            std::fs::write(
                dir.path().join("giant.py"),
                format!("def f():\n    return {i}\n"),
            )
            .unwrap();
            run_git(dir.path(), &["commit", "-q", "-am", &format!("update {i}")]);
        }

        let conn = setup_db();
        for i in 0..200 {
            insert_symbol(&conn, &format!("giant.f{i}"), "giant.py", 0, i < 30, 1);
        }

        let risk = compute_absolute_hotspot_risk(dir.path(), &conn, "1 year");
        assert!(
            risk > 0.75,
            "61 commits + 200 symbols/30 hubs is genuinely high churn+complexity, got {risk}"
        );
    }
}
