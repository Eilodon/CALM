use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

/// K-core decomposition via bucket-by-degree peeling. O(V+E).
///
/// Returns `{qualified_name -> coreness_value}`.
/// Self-loops are excluded (recursive functions don't inflate degree).
///
/// Also updates the `symbols` table:
/// - All symbols get `coreness = 0` (baseline for isolated nodes)
/// - Symbols in the call graph get their computed coreness value
pub fn compute_coreness(conn: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();

    let mut stmt = conn.prepare("SELECT from_symbol, to_symbol FROM call_edges")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    for row in rows {
        let (from_sym, to_sym) = row?;
        if from_sym == to_sym {
            continue;
        }
        adj.entry(from_sym.clone())
            .or_default()
            .insert(to_sym.clone());
        adj.entry(to_sym).or_default().insert(from_sym);
    }

    if adj.is_empty() {
        conn.execute("UPDATE symbols SET coreness = 0", [])?;
        return Ok(HashMap::new());
    }

    let mut degree: HashMap<String, usize> = adj
        .iter()
        .map(|(node, neighbors)| (node.clone(), neighbors.len()))
        .collect();

    let max_deg = *degree.values().max().unwrap_or(&0);

    let mut buckets: Vec<HashSet<String>> = (0..=max_deg).map(|_| HashSet::new()).collect();
    for (node, &d) in &degree {
        buckets[d].insert(node.clone());
    }

    let mut coreness: HashMap<String, i64> = HashMap::new();
    let mut remaining = degree.len();
    let mut k_ptr: usize = 0;

    while remaining > 0 {
        while k_ptr <= max_deg && buckets[k_ptr].is_empty() {
            k_ptr += 1;
        }
        if k_ptr > max_deg {
            break;
        }

        while let Some(v) = buckets[k_ptr].iter().next().cloned() {
            buckets[k_ptr].remove(&v);
            coreness.insert(v.clone(), k_ptr as i64);
            remaining -= 1;

            if let Some(neighbors) = adj.get(&v) {
                for u in neighbors {
                    if coreness.contains_key(u) {
                        continue;
                    }
                    let du = degree[u];
                    if du <= k_ptr {
                        continue;
                    }
                    buckets[du].remove(u);
                    *degree.get_mut(u).unwrap() = du - 1;
                    let new_du = du - 1;
                    if new_du <= k_ptr {
                        buckets[k_ptr].insert(u.clone());
                    } else {
                        buckets[new_du].insert(u.clone());
                    }
                }
            }
        }
    }

    conn.execute("UPDATE symbols SET coreness = 0", [])?;
    {
        let mut update_stmt =
            conn.prepare("UPDATE symbols SET coreness = ? WHERE qualified_name = ?")?;
        for (sym, val) in &coreness {
            update_stmt.execute(rusqlite::params![val, sym])?;
        }
    }

    Ok(coreness)
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

    fn insert_symbol(conn: &Connection, qname: &str) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, \
             line_start, line_end, indexed_at) VALUES (?, ?, 'function', 'python', \
             'test.py', 1, 1, 0.0)",
            rusqlite::params![qname, qname],
        )
        .unwrap();
    }

    fn insert_edge(conn: &Connection, from: &str, to: &str) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence) \
             VALUES (?, ?, 'resolved')",
            rusqlite::params![from, to],
        )
        .unwrap();
    }

    #[test]
    fn test_empty_graph() {
        let conn = setup_db();
        insert_symbol(&conn, "a");
        let result = compute_coreness(&conn).unwrap();
        assert!(result.is_empty());

        let coreness: Option<i64> = conn
            .query_row(
                "SELECT coreness FROM symbols WHERE qualified_name = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(coreness, Some(0));
    }

    #[test]
    fn test_self_loop_excluded() {
        let conn = setup_db();
        insert_symbol(&conn, "a");
        insert_edge(&conn, "a", "a");
        let result = compute_coreness(&conn).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_triangle_coreness_2() {
        let conn = setup_db();
        for s in ["a", "b", "c"] {
            insert_symbol(&conn, s);
        }
        insert_edge(&conn, "a", "b");
        insert_edge(&conn, "b", "c");
        insert_edge(&conn, "c", "a");

        let result = compute_coreness(&conn).unwrap();
        assert_eq!(result.get("a"), Some(&2));
        assert_eq!(result.get("b"), Some(&2));
        assert_eq!(result.get("c"), Some(&2));
    }

    #[test]
    fn test_c1_regression_coreness_recomputed_after_edge_change() {
        let conn = setup_db();
        for s in ["a", "b", "c"] {
            insert_symbol(&conn, s);
        }
        // Triangle: coreness = 2 for all
        insert_edge(&conn, "a", "b");
        insert_edge(&conn, "b", "c");
        insert_edge(&conn, "c", "a");

        let result1 = compute_coreness(&conn).unwrap();
        assert_eq!(result1.get("a"), Some(&2));

        // Remove one edge → breaks triangle → coreness drops to 1
        conn.execute(
            "DELETE FROM call_edges WHERE from_symbol = 'c' AND to_symbol = 'a'",
            [],
        )
        .unwrap();

        let result2 = compute_coreness(&conn).unwrap();
        assert_eq!(
            result2.get("a"),
            Some(&1),
            "C-1: coreness must update after edge removal"
        );
        assert_eq!(result2.get("b"), Some(&1));
        assert_eq!(result2.get("c"), Some(&1));

        // Verify DB was updated too
        let db_coreness: i64 = conn
            .query_row(
                "SELECT coreness FROM symbols WHERE qualified_name = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            db_coreness, 1,
            "C-1: DB coreness must reflect recomputed value"
        );
    }

    #[test]
    fn test_star_graph() {
        let conn = setup_db();
        for s in ["hub", "a", "b", "c", "d"] {
            insert_symbol(&conn, s);
        }
        for leaf in ["a", "b", "c", "d"] {
            insert_edge(&conn, leaf, "hub");
        }

        let result = compute_coreness(&conn).unwrap();
        assert_eq!(result.get("hub"), Some(&1));
        for leaf in ["a", "b", "c", "d"] {
            assert_eq!(result.get(leaf), Some(&1));
        }
    }
}
