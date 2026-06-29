use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

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

fn collect_git_churn(project_root: &Path, since: &str) -> (HashMap<String, ChurnInfo>, bool) {
    let result = Command::new("git")
        .args([
            "log",
            &format!("--since={since}"),
            "--name-only",
            "--format=|||%ae|||%aI",
        ])
        .current_dir(project_root)
        .output();

    let output = match result {
        Ok(o) if o.status.success() => o,
        _ => return (HashMap::new(), false),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut churn_map: HashMap<String, ChurnInfo> = HashMap::new();
    let mut current_author: Option<String> = None;
    let mut current_date: Option<String> = None;

    for line in stdout.lines() {
        if line.starts_with("|||") {
            let parts: Vec<&str> = line.split("|||").collect();
            current_author = parts.get(1).map(|s| s.trim().to_string());
            current_date = parts.get(2).map(|s| s.trim().to_string());
        } else if !line.trim().is_empty() {
            let abs_path = project_root.join(line.trim()).to_string_lossy().to_string();
            let entry = churn_map.entry(abs_path).or_insert_with(|| ChurnInfo {
                commit_count: 0,
                authors: HashSet::new(),
                // H-1 fix: last_changed is Option<String>, None instead of ""
                last_changed: current_date.clone(),
            });
            entry.commit_count += 1;
            if let Some(ref author) = current_author {
                entry.authors.insert(author.clone());
            }
        }
    }
    (churn_map, true)
}

fn collect_complexity(conn: &Connection) -> HashMap<String, ComplexityInfo> {
    let mut stmt = conn
        .prepare(
            "SELECT path, \
             COUNT(*) as symbol_count, \
             SUM(CASE WHEN is_hub = 1 THEN 1 ELSE 0 END) as hub_count, \
             AVG(COALESCE(caller_count, 0)) as avg_caller_count, \
             SUM(CASE WHEN coreness > 0 THEN 1 ELSE 0 END) as connected_coreness_count, \
             MAX(language) as language \
             FROM symbols WHERE path IS NOT NULL GROUP BY path",
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
            "SELECT name, kind, is_hub, coreness, caller_count \
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
    }
}
