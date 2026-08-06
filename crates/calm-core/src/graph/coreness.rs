use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

/// K-core decomposition via bucket-by-degree peeling. O(V+E).
///
/// Computes two metrics from one pass over `call_edges`:
/// - `coreness` ("confirmed"): only edges whose `edge_confidence` ranks
///   above `Ambiguous`/`Unresolved` (see `EdgeConfidence::rank`) count
///   toward degeneracy. This is what `is_hub`/bridge-hub detection
///   (`graph::hub::update_is_hub_flags`) and the `confirm:true` edit gate
///   consume — an unresolved multi-candidate fan-out must not be able to
///   promote a symbol into "architectural core" and gate writes on it.
/// - `possible_coreness`: the same peeling but over EVERY edge regardless
///   of confidence (still excluding `ruled_out_by_scip`) — the old,
///   undifferentiated behavior, kept as an explicit uncertainty signal
///   rather than silently folded into the gate-facing metric. Always
///   `>= coreness` for the same node, since its graph is a superset.
///
/// Self-loops are excluded from both (recursive functions don't inflate
/// degree). Also updates the `symbols` table: every symbol gets both
/// columns set (0 baseline for isolated/absent nodes).
pub fn compute_coreness(conn: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    let mut confirmed_adj: HashMap<String, HashSet<String>> = HashMap::new();
    let mut possible_adj: HashMap<String, HashSet<String>> = HashMap::new();

    // `ruled_out_by_scip = 0`: an edge SCIP proved does not exist must not
    // inflate degeneracy in either metric — coreness feeds `is_hub`, and
    // `is_hub` is what gates the `confirm:true` requirement on edits, so a
    // disproven edge could otherwise promote a symbol into that gate (or
    // mask a real hub).
    let mut stmt = conn.prepare(
        "SELECT from_symbol, to_symbol, edge_confidence FROM call_edges WHERE ruled_out_by_scip = 0",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (from_sym, to_sym, confidence) = row?;
        if from_sym == to_sym {
            continue;
        }
        // Unrecognized confidence strings (should never happen — every
        // writer goes through `EdgeConfidence::as_str`) are treated as
        // confirmed rather than silently dropped from the stricter graph.
        let is_confirmed = crate::types::EdgeConfidence::parse(&confidence)
            .map(|c| c.rank() > 0)
            .unwrap_or(true);

        possible_adj
            .entry(from_sym.clone())
            .or_default()
            .insert(to_sym.clone());
        possible_adj
            .entry(to_sym.clone())
            .or_default()
            .insert(from_sym.clone());

        if is_confirmed {
            confirmed_adj
                .entry(from_sym.clone())
                .or_default()
                .insert(to_sym.clone());
            confirmed_adj.entry(to_sym).or_default().insert(from_sym);
        }
    }

    let confirmed = peel_kcore(&confirmed_adj);
    let possible = peel_kcore(&possible_adj);

    conn.execute("UPDATE symbols SET coreness = 0, possible_coreness = 0", [])?;
    if !possible.is_empty() {
        let mut update_stmt = conn.prepare(
            "UPDATE symbols SET coreness = ?, possible_coreness = ? WHERE qualified_name = ?",
        )?;
        // `possible.keys()` is always a superset of `confirmed.keys()` —
        // the confirmed graph's edges are a subset of the possible graph's.
        for (sym, possible_val) in &possible {
            let confirmed_val = confirmed.get(sym).copied().unwrap_or(0);
            update_stmt.execute(rusqlite::params![confirmed_val, possible_val, sym])?;
        }
    }

    Ok(confirmed)
}

/// Bucket-by-degree k-core peeling over an already-built undirected
/// adjacency map. Shared by `compute_coreness`'s confirmed and possible
/// passes so the peeling logic itself has one implementation.
fn peel_kcore(adj: &HashMap<String, HashSet<String>>) -> HashMap<String, i64> {
    if adj.is_empty() {
        return HashMap::new();
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

    coreness
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

    fn insert_ruled_out_edge(conn: &Connection, from: &str, to: &str) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, \
             ruled_out_by_scip) VALUES (?, ?, 'resolved', 1)",
            rusqlite::params![from, to],
        )
        .unwrap();
    }

    fn insert_ambiguous_edge(conn: &Connection, from: &str, to: &str) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence) \
             VALUES (?, ?, 'ambiguous')",
            rusqlite::params![from, to],
        )
        .unwrap();
    }

    /// `ruled_out_by_scip` marks an edge SCIP has *proven* does not exist. Letting
    /// it into the degeneracy graph inflates coreness, which feeds `is_hub` — and
    /// `is_hub` is what gates the `confirm:true` requirement on edits. A disproven
    /// edge must not be able to promote a symbol into that gate.
    #[test]
    fn scip_ruled_out_edges_do_not_inflate_coreness() {
        let conn = setup_db();
        for s in ["a", "b", "c"] {
            insert_symbol(&conn, s);
        }
        // A real path a-b only; the triangle is closed solely by disproven edges.
        insert_edge(&conn, "a", "b");
        insert_ruled_out_edge(&conn, "b", "c");
        insert_ruled_out_edge(&conn, "c", "a");

        let result = compute_coreness(&conn).unwrap();
        assert_eq!(
            result.get("a"),
            Some(&1),
            "disproven edges closed a phantom triangle: {result:?}"
        );
        assert_eq!(result.get("b"), Some(&1), "{result:?}");
        assert_eq!(
            result.get("c"),
            None,
            "symbol reachable only via disproven edges must not enter the graph: {result:?}"
        );
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

    /// Audit 3.9: an ambiguous-fan-out closed triangle must not inflate the
    /// gate-facing `coreness` column the way a confirmed one does, but the
    /// same triangle SHOULD show up in `possible_coreness` as an explicit
    /// uncertainty signal — the two metrics must actually diverge, not just
    /// exist as two identical columns.
    #[test]
    fn ambiguous_fanout_does_not_inflate_confirmed_coreness_but_shows_in_possible() {
        let conn = setup_db();
        for s in ["a", "b", "c"] {
            insert_symbol(&conn, s);
        }
        // Same shape as `scip_ruled_out_edges_do_not_inflate_coreness`, but
        // closed by low-confidence *ambiguous* fan-out instead of a
        // SCIP-disproven edge — a different, previously-unfiltered path to
        // the same phantom-triangle problem.
        insert_edge(&conn, "a", "b");
        insert_ambiguous_edge(&conn, "b", "c");
        insert_ambiguous_edge(&conn, "c", "a");

        let confirmed = compute_coreness(&conn).unwrap();
        assert_eq!(
            confirmed.get("a"),
            Some(&1),
            "ambiguous edges closed a phantom triangle in the CONFIRMED metric: {confirmed:?}"
        );
        assert_eq!(confirmed.get("b"), Some(&1), "{confirmed:?}");
        assert_eq!(
            confirmed.get("c"),
            None,
            "symbol reachable only via ambiguous edges must not enter the confirmed graph: {confirmed:?}"
        );

        // possible_coreness sees the full triangle (degeneracy 2 for all three).
        let mut stmt = conn
            .prepare("SELECT qualified_name, coreness, possible_coreness FROM symbols ORDER BY qualified_name")
            .unwrap();
        let rows: Vec<(String, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let by_name = |name: &str| rows.iter().find(|(n, _, _)| n == name).unwrap().clone();
        assert_eq!(by_name("a"), ("a".into(), 1, 2), "a: (confirmed, possible)");
        assert_eq!(by_name("b"), ("b".into(), 1, 2), "b: (confirmed, possible)");
        assert_eq!(
            by_name("c"),
            ("c".into(), 0, 2),
            "c: confirmed=0 (ambiguous-only) but possible=2 (full triangle)"
        );
    }
}
