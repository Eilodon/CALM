use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Instant;

use crate::db::queries;
use crate::types::TerminatedBy;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct PathStep {
    pub symbol: String,
    pub edge_confidence: Option<String>,
}

#[derive(Debug)]
pub struct PathResult {
    pub routes: Vec<Vec<PathStep>>,
    pub exists: Option<bool>,
    pub terminated_by: Option<TerminatedBy>,
}

/// Bidirectional BFS path finder.
/// Implements fixes [F1] batch queries, [F2] tie-break alternation,
/// [F3] meeting_nodes as HashSet, [F10] exhaustion flags.
pub fn bidirectional_bfs_path(
    conn: &Connection,
    from_sym: &str,
    to_sym: &str,
    max_hops: usize,
    max_paths: usize,
    timeout_ms: u64,
) -> rusqlite::Result<PathResult> {
    if from_sym == to_sym {
        return Ok(PathResult {
            routes: vec![vec![PathStep {
                symbol: from_sym.to_string(),
                edge_confidence: None,
            }]],
            exists: Some(true),
            terminated_by: None,
        });
    }

    let start = Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);

    // Predecessor maps: node -> (parent_node, edge_confidence)
    let mut forward_pred: HashMap<String, Option<(String, String)>> = HashMap::new();
    let mut backward_pred: HashMap<String, Option<(String, String)>> = HashMap::new();

    forward_pred.insert(from_sym.to_string(), None);
    backward_pred.insert(to_sym.to_string(), None);

    let mut forward_frontier: HashSet<String> = [from_sym.to_string()].into();
    let mut backward_frontier: HashSet<String> = [to_sym.to_string()].into();

    let mut f_depth: usize = 0;
    let mut b_depth: usize = 0;
    let mut meeting_nodes: HashSet<String> = HashSet::new(); // [F3]

    let mut forward_exhausted = false; // [F10]
    let mut backward_exhausted = false;
    let mut tie_toggle = true; // [F10]

    while !(forward_exhausted && backward_exhausted) {
        if start.elapsed() > deadline {
            return Ok(PathResult {
                routes: vec![],
                exists: None,
                terminated_by: Some(TerminatedBy::Timeout),
            });
        }
        if f_depth + b_depth >= max_hops {
            return Ok(PathResult {
                routes: vec![],
                exists: None,
                terminated_by: Some(TerminatedBy::MaxHops),
            });
        }

        // [F2+F10] Vertex-balanced expansion
        let expand_forward = if forward_exhausted {
            false
        } else if backward_exhausted || forward_frontier.len() < backward_frontier.len() {
            true
        } else if backward_frontier.len() < forward_frontier.len() {
            false
        } else {
            let v = tie_toggle;
            tie_toggle = !tie_toggle;
            v
        };

        if expand_forward {
            let frontier_refs: Vec<&str> = forward_frontier.iter().map(|s| s.as_str()).collect();
            let callee_map = queries::batch_callees(conn, &frontier_refs)?; // [F1]
            let mut new_f: HashSet<String> = HashSet::new();

            for node in &forward_frontier {
                if let Some(callees) = callee_map.get(node) {
                    for (callee, edge) in callees {
                        if !forward_pred.contains_key(callee) {
                            forward_pred.insert(callee.clone(), Some((node.clone(), edge.clone())));
                            new_f.insert(callee.clone());
                            if backward_pred.contains_key(callee) {
                                meeting_nodes.insert(callee.clone());
                            }
                        }
                    }
                }
            }

            forward_frontier = new_f;
            if forward_frontier.is_empty() {
                forward_exhausted = true;
            } else {
                f_depth += 1;
            }
        } else {
            let frontier_refs: Vec<&str> = backward_frontier.iter().map(|s| s.as_str()).collect();
            let caller_map = queries::batch_callers(conn, &frontier_refs)?; // [F1]
            let mut new_b: HashSet<String> = HashSet::new();

            for node in &backward_frontier {
                if let Some(callers) = caller_map.get(node) {
                    for (caller, edge) in callers {
                        if !backward_pred.contains_key(caller) {
                            backward_pred
                                .insert(caller.clone(), Some((node.clone(), edge.clone())));
                            new_b.insert(caller.clone());
                            if forward_pred.contains_key(caller) {
                                meeting_nodes.insert(caller.clone());
                            }
                        }
                    }
                }
            }

            backward_frontier = new_b;
            if backward_frontier.is_empty() {
                backward_exhausted = true;
            } else {
                b_depth += 1;
            }
        }

        if !meeting_nodes.is_empty() {
            break;
        }
    }

    if meeting_nodes.is_empty() {
        return Ok(PathResult {
            routes: vec![],
            exists: Some(false),
            terminated_by: None,
        });
    }

    let mut routes: Vec<Vec<PathStep>> = Vec::new();
    for meeting in &meeting_nodes {
        // Reconstruct forward path (meeting -> ... -> from_sym), reversed
        let mut fwd: Vec<PathStep> = Vec::new();
        let mut node = meeting.clone();
        loop {
            let pred = &forward_pred[&node];
            fwd.push(PathStep {
                symbol: node.clone(),
                edge_confidence: pred.as_ref().map(|(_, e)| e.clone()),
            });
            match pred {
                Some((parent, _)) => node = parent.clone(),
                None => break,
            }
        }
        fwd.reverse();

        // Reconstruct backward path (meeting -> ... -> to_sym)
        let mut bwd: Vec<PathStep> = Vec::new();
        let mut node = meeting.clone();
        while let Some(Some(pred)) = backward_pred.get(&node) {
            bwd.push(PathStep {
                symbol: pred.0.clone(),
                edge_confidence: Some(pred.1.clone()),
            });
            node = pred.0.clone();
        }

        fwd.extend(bwd);
        routes.push(fwd);

        if routes.len() >= max_paths {
            break;
        }
    }

    let terminated_by = if routes.len() >= max_paths {
        Some(TerminatedBy::PathCount)
    } else {
        None
    };

    Ok(PathResult {
        routes,
        exists: Some(true),
        terminated_by,
    })
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

    fn insert_edge(conn: &Connection, from: &str, to: &str, conf: &str) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence) \
             VALUES (?, ?, ?)",
            rusqlite::params![from, to, conf],
        )
        .unwrap();
    }

    #[test]
    fn test_self_path() {
        let conn = setup_db();
        let result = bidirectional_bfs_path(&conn, "a", "a", 8, 3, 5000).unwrap();
        assert_eq!(result.exists, Some(true));
        assert_eq!(result.routes.len(), 1);
        assert_eq!(result.routes[0].len(), 1);
    }

    #[test]
    fn test_direct_edge() {
        let conn = setup_db();
        insert_edge(&conn, "a", "b", "resolved");
        let result = bidirectional_bfs_path(&conn, "a", "b", 8, 3, 5000).unwrap();
        assert_eq!(result.exists, Some(true));
        assert_eq!(result.routes.len(), 1);
        assert_eq!(result.routes[0].len(), 2);
        assert_eq!(result.routes[0][0].symbol, "a");
        assert_eq!(result.routes[0][1].symbol, "b");
    }

    #[test]
    fn test_no_path() {
        let conn = setup_db();
        insert_edge(&conn, "a", "b", "resolved");
        let result = bidirectional_bfs_path(&conn, "b", "a", 8, 3, 5000).unwrap();
        assert_eq!(result.exists, Some(false));
        assert!(result.routes.is_empty());
    }

    #[test]
    fn test_chain_path() {
        let conn = setup_db();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "b", "c", "inferred");
        insert_edge(&conn, "c", "d", "textual");
        let result = bidirectional_bfs_path(&conn, "a", "d", 8, 3, 5000).unwrap();
        assert_eq!(result.exists, Some(true));
        assert_eq!(result.routes[0].len(), 4);
    }

    #[test]
    fn test_max_hops_exceeded() {
        let conn = setup_db();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "b", "c", "resolved");
        insert_edge(&conn, "c", "d", "resolved");
        let result = bidirectional_bfs_path(&conn, "a", "d", 2, 3, 5000).unwrap();
        assert_eq!(result.exists, None);
        assert!(matches!(result.terminated_by, Some(TerminatedBy::MaxHops)));
    }

    #[test]
    fn test_max_hops_zero() {
        let conn = setup_db();
        insert_edge(&conn, "a", "b", "resolved");
        let result = bidirectional_bfs_path(&conn, "a", "b", 0, 3, 5000).unwrap();
        assert_eq!(result.exists, None);
    }
}
