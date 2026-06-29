use rusqlite::Connection;

use crate::config::HubThresholdConfig;

/// Update `is_hub` flags for all symbols based on degree-hub and bridge-hub detection.
///
/// Degree-hub: caller_count in top N% AND >= min_callers.
/// Bridge-hub: caller_count >= min_callers_bridge AND coreness >= p75.
///
/// Bug fix vs Python (C-2): p75_coreness floor uses min_callers_bridge, not min_callers.
pub fn update_is_hub_flags(
    conn: &Connection,
    config: &HubThresholdConfig,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT qualified_name, caller_count, coreness \
         FROM symbols WHERE caller_count >= 1 OR coreness > 0",
    )?;

    let rows: Vec<(String, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2).unwrap_or(0),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    conn.execute("UPDATE symbols SET is_hub = 0", [])?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut sorted_counts: Vec<i64> = rows.iter().filter(|r| r.1 >= 1).map(|r| r.1).collect();
    sorted_counts.sort();
    let total = sorted_counts.len().max(1);

    let percentile_rank = |caller_count: i64| -> f64 {
        if sorted_counts.is_empty() {
            return 0.0;
        }
        let pos = sorted_counts.partition_point(|&c| c <= caller_count);
        pos as f64 / total as f64
    };

    let all_coreness: Vec<i64> = rows.iter().filter(|r| r.2 > 0).map(|r| r.2).collect();
    let p75_coreness: f64 = if all_coreness.is_empty() {
        f64::INFINITY
    } else {
        let mut sorted_c = all_coreness.clone();
        sorted_c.sort();
        let idx = ((sorted_c.len() as f64 * config.coreness_pct / 100.0) as usize)
            .saturating_sub(1)
            .min(sorted_c.len() - 1);
        // C-2 fix: floor uses min_callers_bridge (not min_callers)
        (sorted_c[idx] as f64).max(config.min_callers_bridge as f64)
    };

    let top_threshold = 1.0 - config.top_pct / 100.0;

    let mut update_stmt =
        conn.prepare("UPDATE symbols SET is_hub = ? WHERE qualified_name = ?")?;

    for (qname, caller_count, coreness) in &rows {
        let caller_pct = percentile_rank(*caller_count);
        let is_degree_hub =
            *caller_count >= config.min_callers && caller_pct >= top_threshold;
        let is_bridge_hub =
            *caller_count >= config.min_callers_bridge && (*coreness as f64) >= p75_coreness;
        let is_hub = is_degree_hub || is_bridge_hub;
        update_stmt.execute(rusqlite::params![is_hub as i32, qname])?;
    }

    Ok(())
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

    fn insert_symbol(conn: &Connection, qname: &str, caller_count: i64, coreness: i64) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, caller_count, coreness, indexed_at) \
             VALUES (?, ?, 'function', 'python', 'test.py', 1, 1, ?, ?, 0.0)",
            rusqlite::params![qname, qname, caller_count, coreness],
        )
        .unwrap();
    }

    #[test]
    fn test_no_symbols() {
        let conn = setup_db();
        let config = HubThresholdConfig::default();
        update_is_hub_flags(&conn, &config).unwrap();
    }

    #[test]
    fn test_degree_hub() {
        let conn = setup_db();
        for i in 0..20 {
            insert_symbol(&conn, &format!("sym_{i}"), i, 0);
        }
        let config = HubThresholdConfig::default();
        update_is_hub_flags(&conn, &config).unwrap();

        let is_hub: i32 = conn
            .query_row(
                "SELECT is_hub FROM symbols WHERE qualified_name = 'sym_19'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(is_hub, 1, "top caller should be hub");

        let is_hub: i32 = conn
            .query_row(
                "SELECT is_hub FROM symbols WHERE qualified_name = 'sym_1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(is_hub, 0, "low caller should not be hub");
    }
}
