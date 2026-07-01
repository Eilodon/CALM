use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::analysis::coverage::{CoverageData, normalize_path};
use crate::analysis::dead_code::{
    compute_dead_code_confidence, is_private_symbol, scope_clear_for_language,
};
use crate::analysis::hotspot::compute_hotspots;
use crate::config::HotspotsConfig;

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FitnessThresholds {
    pub max_hub_count: i64,
    pub max_avg_coreness: f64,
    pub max_dead_code_pct: f64,
    pub max_hotspot_risk: f64,
    pub min_edge_coverage_pct: f64,
}

impl Default for FitnessThresholds {
    fn default() -> Self {
        Self {
            max_hub_count: 50,
            max_avg_coreness: 15.0,
            max_dead_code_pct: 10.0,
            max_hotspot_risk: 0.75,
            min_edge_coverage_pct: 60.0,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct TomlFile {
    #[serde(default)]
    thresholds: FitnessThresholds,
}

pub fn load_thresholds(config_path: Option<&Path>) -> anyhow::Result<FitnessThresholds> {
    if let Some(path) = config_path
        && path.exists()
    {
        let text = std::fs::read_to_string(path)?;
        let parsed: TomlFile = toml::from_str(&text)?;
        return Ok(parsed.thresholds);
    }
    Ok(FitnessThresholds::default())
}

// ---------------------------------------------------------------------------
// Metrics collection
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct FitnessMetrics {
    pub hub_count: i64,
    pub avg_coreness: f64,
    pub dead_code_pct: f64,
    pub hotspot_risk: f64,
    pub edge_coverage_pct: f64,
}

/// Row shape needed to re-run `compute_dead_code_confidence` per symbol:
/// (path, line_start, line_end, caller_count, is_entry_point, language, name, signature).
type DeadCodeRow = (String, i64, i64, i64, bool, String, String, String);

pub fn collect_metrics(
    conn: &Connection,
    project_root: &Path,
    coverage: &CoverageData,
) -> rusqlite::Result<FitnessMetrics> {
    let hub_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE is_hub = 1", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    let avg_coreness: f64 = conn
        .query_row(
            "SELECT COALESCE(AVG(CAST(coreness AS REAL)), 0.0) FROM symbols WHERE coreness > 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0.0);

    let total_symbols: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap_or(0);

    let edge_coverage_pct = if total_symbols > 0 {
        let covered: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT from_symbol) FROM call_edges",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (covered as f64 / total_symbols as f64) * 100.0
    } else {
        100.0
    };

    let dead_code_pct = if total_symbols > 0 {
        let mut stmt = conn.prepare(
            "SELECT path, line_start, line_end, COALESCE(caller_count, 0), is_entry_point, \
             language, name, signature FROM symbols",
        )?;
        let rows: Vec<DeadCodeRow> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)? != 0,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let high_confidence_dead = rows
            .iter()
            .filter(
                |(path, line_start, line_end, caller_count, is_entry, lang, name, sig)| {
                    let abs_path = normalize_path(&project_root.join(path));
                    let is_private = is_private_symbol(lang, name, sig);
                    let scope_clear = scope_clear_for_language(lang);
                    let (confidence, _) = compute_dead_code_confidence(
                        &abs_path,
                        *line_start,
                        *line_end,
                        *caller_count,
                        *is_entry,
                        is_private,
                        scope_clear,
                        coverage,
                    );
                    confidence == "high"
                },
            )
            .count();

        100.0 * high_confidence_dead as f64 / rows.len().max(1) as f64
    } else {
        0.0
    };

    let hotspot_risk = compute_hotspots(
        project_root,
        conn,
        &HotspotsConfig::default(),
        1,
        &HotspotsConfig::default().default_since,
        0,
        false,
    )
    .hotspots
    .first()
    .map(|h| h.hotspot_score)
    .unwrap_or(0.0);

    Ok(FitnessMetrics {
        hub_count,
        avg_coreness,
        dead_code_pct,
        hotspot_risk,
        edge_coverage_pct,
    })
}

// ---------------------------------------------------------------------------
// Fitness check
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct FitnessCheckItem {
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct FitnessCheckResult {
    pub passed: bool,
    pub checks: Vec<FitnessCheckItem>,
    pub metrics: FitnessMetrics,
}

pub fn run_fitness_check(
    conn: &Connection,
    thresholds: &FitnessThresholds,
    project_root: &Path,
    coverage: &CoverageData,
) -> rusqlite::Result<FitnessCheckResult> {
    let metrics = collect_metrics(conn, project_root, coverage)?;
    let mut checks = Vec::new();

    checks.push(FitnessCheckItem {
        metric: "hub_count".into(),
        value: metrics.hub_count as f64,
        threshold: thresholds.max_hub_count as f64,
        passed: metrics.hub_count <= thresholds.max_hub_count,
        message: format!(
            "Hub count {} (max {})",
            metrics.hub_count, thresholds.max_hub_count
        ),
    });

    checks.push(FitnessCheckItem {
        metric: "avg_coreness".into(),
        value: metrics.avg_coreness,
        threshold: thresholds.max_avg_coreness,
        passed: metrics.avg_coreness <= thresholds.max_avg_coreness,
        message: format!(
            "Avg coreness {:.2} (max {:.2})",
            metrics.avg_coreness, thresholds.max_avg_coreness
        ),
    });

    checks.push(FitnessCheckItem {
        metric: "dead_code_pct".into(),
        value: metrics.dead_code_pct,
        threshold: thresholds.max_dead_code_pct,
        passed: metrics.dead_code_pct <= thresholds.max_dead_code_pct,
        message: format!(
            "Dead code {:.1}% (max {:.1}%)",
            metrics.dead_code_pct, thresholds.max_dead_code_pct
        ),
    });

    checks.push(FitnessCheckItem {
        metric: "hotspot_risk".into(),
        value: metrics.hotspot_risk,
        threshold: thresholds.max_hotspot_risk,
        passed: metrics.hotspot_risk <= thresholds.max_hotspot_risk,
        message: format!(
            "Max hotspot risk {:.2} (max {:.2})",
            metrics.hotspot_risk, thresholds.max_hotspot_risk
        ),
    });

    checks.push(FitnessCheckItem {
        metric: "edge_coverage_pct".into(),
        value: metrics.edge_coverage_pct,
        threshold: thresholds.min_edge_coverage_pct,
        passed: metrics.edge_coverage_pct >= thresholds.min_edge_coverage_pct,
        message: format!(
            "Edge coverage {:.1}% (min {:.1}%)",
            metrics.edge_coverage_pct, thresholds.min_edge_coverage_pct
        ),
    });

    let passed = checks.iter().all(|c| c.passed);
    Ok(FitnessCheckResult {
        passed,
        checks,
        metrics,
    })
}

// ---------------------------------------------------------------------------
// Snapshot writer
// ---------------------------------------------------------------------------

pub fn snapshot_metrics(conn: &Connection, timestamp: &str) -> anyhow::Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT qualified_name, caller_count, COALESCE(coreness, 0), is_hub FROM symbols",
    )?;

    let rows: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    let count = rows.len();
    for (name, caller_count, coreness, is_hub) in &rows {
        conn.execute(
            "INSERT OR IGNORE INTO symbol_metrics_history \
             (qualified_name, snapshot_at, caller_count, coreness, is_hub) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, timestamp, caller_count, coreness, is_hub],
        )?;
    }

    tracing::info!(
        snapshot_at = timestamp,
        symbols_snapshotted = count,
        "metrics_snapshot_complete"
    );

    Ok(count)
}

// ---------------------------------------------------------------------------
// Date helpers — no chrono dependency. `snapshot_at` is a plain UTC
// "YYYY-MM-DD" string, so lexical and chronological ordering coincide and it
// can be compared directly with `<`/`<=` in SQL.
// ---------------------------------------------------------------------------

fn epoch_days_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86400) as i64)
        .unwrap_or(0)
}

/// Howard Hinnant's `civil_from_days`: days-since-epoch (1970-01-01 = 0) to a
/// proleptic-Gregorian (year, month, day) triple.
/// See <http://howardhinnant.github.io/date_algorithms.html>.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn date_string(epoch_days: i64) -> String {
    let (y, m, d) = civil_from_days(epoch_days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's UTC date, rounded to the day. Used as `snapshot_at` so repeated
/// same-day CI runs collapse onto one row via the (qualified_name,
/// snapshot_at) UNIQUE constraint instead of growing the table every run.
pub fn today_utc_date() -> String {
    date_string(epoch_days_now())
}

/// UTC date `days_ago` days before today, in the same "YYYY-MM-DD" format as
/// `today_utc_date` — directly usable as a SQL bound against `snapshot_at`.
fn date_days_ago(days_ago: i64) -> String {
    date_string(epoch_days_now() - days_ago)
}

// ---------------------------------------------------------------------------
// Trend
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
pub struct TrendInfo {
    /// `snapshot_at` of the historical row this trend was computed against.
    pub compared_to: String,
    pub caller_count_delta: i64,
    pub coreness_delta: i64,
    pub is_hub_changed: bool,
}

/// Compares a symbol's *live* metrics (current `symbols` row) against its
/// most recent snapshot that is still at least `lookback_days` old — i.e. the
/// snapshot closest to (without being more recent than) the
/// `lookback_days`-ago mark. This gives the tightest valid "at least N days"
/// comparison, rather than always diffing against the oldest snapshot ever
/// recorded.
///
/// Returns `Ok(None)` when the symbol isn't in `symbols`, or has no snapshot
/// old enough yet (not tracked for `lookback_days` days).
pub fn compute_trend(
    conn: &Connection,
    qualified_name: &str,
    lookback_days: i64,
) -> anyhow::Result<Option<TrendInfo>> {
    let live: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT caller_count, COALESCE(coreness, 0), is_hub FROM symbols \
             WHERE qualified_name = ?1",
            rusqlite::params![qualified_name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((live_callers, live_coreness, live_is_hub)) = live else {
        return Ok(None);
    };

    let cutoff = date_days_ago(lookback_days);
    let baseline: Option<(String, i64, i64, i64)> = conn
        .query_row(
            "SELECT snapshot_at, caller_count, coreness, is_hub \
             FROM symbol_metrics_history \
             WHERE qualified_name = ?1 AND snapshot_at <= ?2 \
             ORDER BY snapshot_at DESC LIMIT 1",
            rusqlite::params![qualified_name, cutoff],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((compared_to, base_callers, base_coreness, base_is_hub)) = baseline else {
        return Ok(None);
    };

    Ok(Some(TrendInfo {
        compared_to,
        caller_count_delta: live_callers - base_callers,
        coreness_delta: live_coreness - base_coreness,
        is_hub_changed: (live_is_hub != 0) != (base_is_hub != 0),
    }))
}

// ---------------------------------------------------------------------------
// Prune
// ---------------------------------------------------------------------------

/// Snapshots older than this are pruned by `prune_old_snapshots` — keeps
/// `symbol_metrics_history` from growing unbounded on long-lived projects.
pub const METRICS_RETENTION_DAYS: i64 = 180;

/// Deletes `symbol_metrics_history` rows older than `METRICS_RETENTION_DAYS`.
/// Called from `ci doctor` — not on the `fitness-check` hot path, since
/// pruning isn't needed on every CI run.
pub fn prune_old_snapshots(conn: &Connection) -> anyhow::Result<usize> {
    let cutoff = date_days_ago(METRICS_RETENTION_DAYS);
    let deleted = conn.execute(
        "DELETE FROM symbol_metrics_history WHERE snapshot_at < ?1",
        rusqlite::params![cutoff],
    )?;
    if deleted > 0 {
        tracing::info!(deleted, cutoff = %cutoff, "metrics_history_pruned");
    }
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_default_thresholds() {
        let t = FitnessThresholds::default();
        assert_eq!(t.max_hub_count, 50);
        assert_eq!(t.max_avg_coreness, 15.0);
        assert_eq!(t.max_dead_code_pct, 10.0);
        assert_eq!(t.max_hotspot_risk, 0.75);
        assert_eq!(t.min_edge_coverage_pct, 60.0);
    }

    #[test]
    fn test_fitness_check_empty_db_passes() {
        let conn = test_conn();
        let thresholds = FitnessThresholds::default();
        let result = run_fitness_check(
            &conn,
            &thresholds,
            &std::env::temp_dir(),
            &CoverageData::none(),
        )
        .unwrap();
        assert!(result.passed, "Empty DB should pass all checks");
        assert_eq!(result.checks.len(), 5);
    }

    #[test]
    fn test_hub_count_fail() {
        let conn = test_conn();
        let thresholds = FitnessThresholds {
            max_hub_count: 0,
            ..Default::default()
        };

        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, is_hub, indexed_at) \
             VALUES ('mod.foo', 'foo', 'function', 'python', 'mod.py', 1, 5, 1, 0.0)",
            [],
        )
        .unwrap();

        let result = run_fitness_check(
            &conn,
            &thresholds,
            &std::env::temp_dir(),
            &CoverageData::none(),
        )
        .unwrap();
        assert!(!result.passed);
        let check = result
            .checks
            .iter()
            .find(|c| c.metric == "hub_count")
            .unwrap();
        assert!(!check.passed);
        assert_eq!(check.value, 1.0);
    }

    #[test]
    fn test_edge_coverage_fail() {
        let conn = test_conn();
        let thresholds = FitnessThresholds {
            min_edge_coverage_pct: 80.0,
            ..Default::default()
        };

        // Insert symbols but no call edges
        for (qname, name) in [("mod.foo", "foo"), ("mod.bar", "bar")] {
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, \
                 line_start, line_end, indexed_at) \
                 VALUES (?1, ?2, 'function', 'python', 'mod.py', 1, 5, 0.0)",
                rusqlite::params![qname, name],
            )
            .unwrap();
        }

        let result = run_fitness_check(
            &conn,
            &thresholds,
            &std::env::temp_dir(),
            &CoverageData::none(),
        )
        .unwrap();
        assert!(!result.passed);
        let check = result
            .checks
            .iter()
            .find(|c| c.metric == "edge_coverage_pct")
            .unwrap();
        assert!(!check.passed);
        assert_eq!(check.value, 0.0);
    }

    #[test]
    fn test_dead_code_pct_counts_high_confidence_dead_symbols() {
        let conn = test_conn();
        // private (no leading-underscore check fails for python — use a name
        // that signals private + no callers + not an entry point: "high".
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, signature, indexed_at) \
             VALUES ('mod._helper', '_helper', 'function', 'python', 'mod.py', 1, 5, 'def _helper():', 0.0)",
            [],
        )
        .unwrap();
        // Has a caller — never dead regardless of privacy.
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, signature, caller_count, indexed_at) \
             VALUES ('mod.used', 'used', 'function', 'python', 'mod.py', 10, 15, 'def used():', 1, 0.0)",
            [],
        )
        .unwrap();

        let metrics = collect_metrics(&conn, &std::env::temp_dir(), &CoverageData::none()).unwrap();
        assert_eq!(
            metrics.dead_code_pct, 50.0,
            "1 of 2 symbols (the private, callerless one) should be high-confidence dead"
        );
    }

    #[test]
    fn test_hotspot_risk_nonzero_with_complexity_signal() {
        let conn = test_conn();
        // Two files; only `busy.py` has a hub symbol, so index-only complexity
        // ranking (no git repo at temp_dir) should normalize it to score 1.0.
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, is_hub, indexed_at) \
             VALUES ('busy.foo', 'foo', 'function', 'python', 'busy.py', 1, 5, 1, 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, indexed_at) \
             VALUES ('quiet.bar', 'bar', 'function', 'python', 'quiet.py', 1, 5, 0.0)",
            [],
        )
        .unwrap();

        let metrics = collect_metrics(&conn, &std::env::temp_dir(), &CoverageData::none()).unwrap();
        assert!(
            metrics.hotspot_risk > 0.0,
            "hub-heavy file should produce a nonzero hotspot risk, got {}",
            metrics.hotspot_risk
        );
    }

    #[test]
    fn test_edge_coverage_pass_with_edges() {
        let conn = test_conn();
        let thresholds = FitnessThresholds {
            min_edge_coverage_pct: 50.0,
            ..Default::default()
        };

        for (qname, name) in [("mod.foo", "foo"), ("mod.bar", "bar")] {
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, \
                 line_start, line_end, indexed_at) \
                 VALUES (?1, ?2, 'function', 'python', 'mod.py', 1, 5, 0.0)",
                rusqlite::params![qname, name],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol) VALUES ('mod.foo', 'mod.bar')",
            [],
        )
        .unwrap();

        let metrics = collect_metrics(&conn, &std::env::temp_dir(), &CoverageData::none()).unwrap();
        assert_eq!(metrics.edge_coverage_pct, 50.0);

        let result = run_fitness_check(
            &conn,
            &thresholds,
            &std::env::temp_dir(),
            &CoverageData::none(),
        )
        .unwrap();
        let check = result
            .checks
            .iter()
            .find(|c| c.metric == "edge_coverage_pct")
            .unwrap();
        assert!(check.passed);
    }

    #[test]
    fn test_snapshot_metrics() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, indexed_at) \
             VALUES ('mod.foo', 'foo', 'function', 'python', 'mod.py', 1, 5, 0.0)",
            [],
        )
        .unwrap();

        let count = snapshot_metrics(&conn, "2026-01-01T00:00:00Z").unwrap();
        assert_eq!(count, 1);

        let (qname, caller_count): (String, i64) = conn
            .query_row(
                "SELECT qualified_name, caller_count FROM symbol_metrics_history \
                 WHERE snapshot_at = '2026-01-01T00:00:00Z'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(qname, "mod.foo");
        assert_eq!(caller_count, 0);
    }

    #[test]
    fn test_snapshot_idempotent() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, indexed_at) \
             VALUES ('mod.foo', 'foo', 'function', 'python', 'mod.py', 1, 5, 0.0)",
            [],
        )
        .unwrap();

        snapshot_metrics(&conn, "2026-01-01T00:00:00Z").unwrap();
        // Second call with same timestamp: INSERT OR IGNORE, no error
        snapshot_metrics(&conn, "2026-01-01T00:00:00Z").unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_metrics_history", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_toml_parsing() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            "[thresholds]\nmax_hub_count = 5\nmin_edge_coverage_pct = 90.0\n"
        )
        .unwrap();

        let thresholds = load_thresholds(Some(f.path())).unwrap();
        assert_eq!(thresholds.max_hub_count, 5);
        assert_eq!(thresholds.min_edge_coverage_pct, 90.0);
        assert_eq!(thresholds.max_avg_coreness, 15.0);
    }

    #[test]
    fn test_load_thresholds_missing_file() {
        let thresholds = load_thresholds(Some(Path::new("/nonexistent/path.toml"))).unwrap();
        assert_eq!(thresholds.max_hub_count, 50);
    }

    #[test]
    fn test_load_thresholds_none() {
        let thresholds = load_thresholds(None).unwrap();
        assert_eq!(thresholds.max_hub_count, 50);
    }

    #[test]
    fn test_civil_from_days_known_dates() {
        // Reference epoch-day values cross-checked against Python's datetime.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        // 2024 is a leap year — Feb 29 must exist and Mar 1 must follow it.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        assert_eq!(civil_from_days(19783), (2024, 3, 1));
        assert_eq!(date_string(20635), "2026-07-01");
    }

    #[test]
    fn test_today_utc_date_format() {
        let d = today_utc_date();
        assert_eq!(d.len(), 10, "expected YYYY-MM-DD, got {d}");
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[7], b'-');
    }

    #[test]
    fn test_date_days_ago_is_before_today() {
        let today = today_utc_date();
        let week_ago = date_days_ago(7);
        assert!(week_ago < today, "{week_ago} should sort before {today}");
    }

    fn insert_symbol_with_metrics(
        conn: &Connection,
        qname: &str,
        caller_count: i64,
        coreness: i64,
        is_hub: i64,
    ) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, caller_count, coreness, is_hub, indexed_at) \
             VALUES (?1, ?1, 'function', 'python', 'mod.py', 1, 5, ?2, ?3, ?4, 0.0)",
            rusqlite::params![qname, caller_count, coreness, is_hub],
        )
        .unwrap();
    }

    #[test]
    fn test_compute_trend_no_symbol_returns_none() {
        let conn = test_conn();
        let trend = compute_trend(&conn, "mod.missing", 7).unwrap();
        assert!(trend.is_none());
    }

    #[test]
    fn test_compute_trend_no_baseline_snapshot_returns_none() {
        let conn = test_conn();
        insert_symbol_with_metrics(&conn, "mod.foo", 5, 3, 0);
        // No snapshot rows at all yet.
        let trend = compute_trend(&conn, "mod.foo", 7).unwrap();
        assert!(trend.is_none());
    }

    #[test]
    fn test_compute_trend_computes_deltas_against_oldest_eligible_snapshot() {
        let conn = test_conn();
        insert_symbol_with_metrics(&conn, "mod.foo", 10, 5, 1);

        let old_cutoff = date_days_ago(30);
        let recent = date_days_ago(1);
        conn.execute(
            "INSERT INTO symbol_metrics_history \
             (qualified_name, snapshot_at, caller_count, coreness, is_hub) \
             VALUES ('mod.foo', ?1, 4, 2, 0)",
            rusqlite::params![old_cutoff],
        )
        .unwrap();
        // A snapshot too recent to satisfy a 7-day lookback — must be ignored
        // in favor of the older row above.
        conn.execute(
            "INSERT INTO symbol_metrics_history \
             (qualified_name, snapshot_at, caller_count, coreness, is_hub) \
             VALUES ('mod.foo', ?1, 9, 5, 1)",
            rusqlite::params![recent],
        )
        .unwrap();

        let trend = compute_trend(&conn, "mod.foo", 7).unwrap().unwrap();
        assert_eq!(trend.compared_to, old_cutoff);
        assert_eq!(trend.caller_count_delta, 6); // 10 - 4
        assert_eq!(trend.coreness_delta, 3); // 5 - 2
        assert!(trend.is_hub_changed); // false -> true
    }

    #[test]
    fn test_prune_old_snapshots_deletes_only_stale_rows() {
        let conn = test_conn();
        let stale = date_days_ago(METRICS_RETENTION_DAYS + 10);
        let fresh = date_days_ago(1);
        for (qname, ts) in [("mod.a", stale.as_str()), ("mod.b", fresh.as_str())] {
            conn.execute(
                "INSERT INTO symbol_metrics_history (qualified_name, snapshot_at, caller_count) \
                 VALUES (?1, ?2, 0)",
                rusqlite::params![qname, ts],
            )
            .unwrap();
        }

        let deleted = prune_old_snapshots(&conn).unwrap();
        assert_eq!(deleted, 1);

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_metrics_history", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 1);

        let remaining_name: String = conn
            .query_row(
                "SELECT qualified_name FROM symbol_metrics_history",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining_name, "mod.b");
    }
}
