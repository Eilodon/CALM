use super::common::*;
use super::*;

/// Candidate pool size fetched from `symbols` before per-row dead-code/test
/// analysis and filtering down to `top_n` — bounded independent of `top_n`
/// so a large `top_n` request can't turn into an unbounded table scan.
const POOL_LIMIT: i64 = 300;
const DEFAULT_TOP_N: u32 = 20;

#[rmcp::tool_router(router = "testgap_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "test_gap_hotspots",
        description = "Rank symbols by coreness (structural centrality in the call graph) crossed with dead-code/test-coverage confidence — the highest-leverage places to add a test, not just the highest-churn/complexity ones (that's `hotspots`) or the riskiest single edit (that's `edit_context`). USE WHEN: deciding where limited test-writing effort pays off most, or auditing which structurally-important code has no test calling it directly. Read-only — composes existing `coreness`/`dead_code_confidence`/`test_files` signals, computes nothing new.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn test_gap_hotspots(
        &self,
        Parameters(p): Parameters<TestGapHotspotsParams>,
    ) -> Json<ToolOutcome<TestGapHotspotsOutput>> {
        Json(self.timed_tool("test_gap_hotspots", || {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let edges_ready = self.edges_ready();
            let top_n = p.top_n.unwrap_or(DEFAULT_TOP_N).max(1) as usize;

            // coreness is only meaningful once the call graph is built —
            // same honesty convention as repo_overview's core_symbols: an
            // empty list here, not a stale/partial ranking.
            if !edges_ready {
                return ToolOutcome::success(TestGapHotspotsOutput {
                    gaps: Vec::new(),
                    count: 0,
                    edges_ready,
                    suggested_next: self.filter_sn(suggested(
                        "indexing_status",
                        "Graph not ready yet — coreness is unavailable until edges finish building",
                    )),
                });
            }

            let mut stmt = match conn.prepare(
                "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous \
                 FROM symbols \
                 WHERE kind IN ('function','method') AND is_test = 0 AND coreness IS NOT NULL AND coreness > 0 \
                 ORDER BY coreness DESC, caller_count DESC, qualified_name ASC \
                 LIMIT ?1",
            ) {
                Ok(s) => s,
                Err(e) => return db_error(e),
            };
            let candidates: Vec<CandidateRow> = stmt
                .query_map(rusqlite::params![POOL_LIMIT], |r| {
                    Ok(CandidateRow {
                        name: r.get(0)?,
                        qualified_name: r.get(1)?,
                        kind: r.get(2)?,
                        path: r.get(3)?,
                        line_start: r.get(4)?,
                        line_end: r.get(5)?,
                        signature: r.get(6)?,
                        docstring: r.get(7)?,
                        caller_count: r.get(8)?,
                        is_hub: r.get::<_, i64>(9)? != 0,
                        language: r.get(10)?,
                        class_context: r.get(11)?,
                        is_entry_point: r.get::<_, i64>(12)? != 0,
                        is_test: r.get::<_, i64>(13)? != 0,
                        coreness: r.get(14)?,
                        boundary_ambiguous: r.get::<_, i64>(15)? != 0,
                    })
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();

            let coverage = self.coverage.read_ok();
            let mut gaps: Vec<TestGapItem> = Vec::new();
            for c in &candidates {
                let health =
                    super::inspect::build_health(&conn, &coverage, &self.project_root, c, edges_ready);
                let has_direct_test = !health.test_files.is_empty();
                let is_gap = matches!(health.dead_code_confidence.as_str(), "medium" | "high")
                    || !has_direct_test;
                if !is_gap {
                    continue;
                }
                gaps.push(TestGapItem {
                    qualified_name: c.qualified_name.clone(),
                    name: c.name.clone(),
                    path: c.path.clone(),
                    coreness: c.coreness.unwrap_or(0),
                    caller_count: c.caller_count,
                    dead_code_confidence: health.dead_code_confidence,
                    test_files: health.test_files,
                });
                if gaps.len() >= top_n {
                    break;
                }
            }

            let count = gaps.len();
            ToolOutcome::success(TestGapHotspotsOutput {
                gaps,
                count,
                edges_ready,
                suggested_next: self.filter_sn(suggested(
                    "source",
                    "Read the top-ranked gap before deciding whether it actually needs a test",
                )),
            })
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct TestGapHotspotsParams {
    /// Max results to return. Default 20.
    #[serde(default)]
    pub(crate) top_n: Option<u32>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TestGapItem {
    pub(crate) qualified_name: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) coreness: i64,
    pub(crate) caller_count: i64,
    /// From `calm_core::analysis::dead_code::compute_dead_code_confidence`
    /// — "none"/"low"/"medium"/"high". Elevated here means: high structural
    /// centrality (coreness) but the static+coverage evidence for this
    /// symbol being actually exercised is weak.
    pub(crate) dead_code_confidence: String,
    /// Test files with a direct call edge to this symbol, if any — may be
    /// empty even when `dead_code_confidence` is "none" (e.g. an
    /// entry-point symbol correctly reports no dead-code risk without ever
    /// being called from a test) and may be non-empty even when a symbol
    /// still shows up here (e.g. exercised only indirectly through an
    /// interface, which this direct-call check can't see) — read both
    /// fields together, neither alone is the full picture.
    pub(crate) test_files: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TestGapHotspotsOutput {
    pub(crate) gaps: Vec<TestGapItem>,
    pub(crate) count: usize,
    pub(crate) edges_ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters as P;

    fn jv<T: Serialize>(json: Json<T>) -> serde_json::Value {
        serde_json::to_value(json.0).unwrap()
    }

    fn seed_symbol(
        conn: &rusqlite::Connection,
        qualified_name: &str,
        name: &str,
        path: &str,
        caller_count: i64,
        coreness: i64,
        is_entry_point: i64,
    ) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point, is_test, coreness) \
             VALUES (?1, ?2, 'function', 'rust', ?3, 1, 5, 'fn foo()', '', ?2, ?4, 0, ?5, 0, ?6)",
            rusqlite::params![qualified_name, name, path, caller_count, is_entry_point, coreness],
        )
        .unwrap();
    }

    #[test]
    fn test_gap_hotspots_flags_high_coreness_zero_caller_symbol() {
        let dir = std::env::temp_dir().join(format!("ci_testgap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CalmServer::new(dir.clone(), dir.join("index.db")).unwrap();
        {
            let conn = server.db();
            // High coreness, zero callers, not an entry point — a real gap.
            seed_symbol(
                &conn,
                "a.rs::core_no_tests",
                "core_no_tests",
                "a.rs",
                0,
                9,
                0,
            );
            // High coreness, well-called, not an entry point — also flagged
            // as a "gap" only if it truly has no direct test caller (which
            // it won't, since no call_edges rows exist in this fixture at
            // all) — this test only asserts the zero-caller case is caught,
            // the next test asserts a symbol WITH a test caller is excluded.
        }
        *server.phase_handle().write().unwrap() = IndexingPhase::Ready;

        let v = jv(server.test_gap_hotspots(P(TestGapHotspotsParams { top_n: None })));
        let gaps = v["gaps"].as_array().unwrap();
        assert!(
            gaps.iter()
                .any(|g| g["qualified_name"] == "a.rs::core_no_tests"),
            "expected core_no_tests in gaps, got: {v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_gap_hotspots_excludes_symbol_with_a_direct_test_caller() {
        let dir = std::env::temp_dir().join(format!("ci_testgap_covered_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CalmServer::new(dir.clone(), dir.join("index.db")).unwrap();
        {
            let conn = server.db();
            seed_symbol(&conn, "a.rs::well_tested", "well_tested", "a.rs", 3, 9, 0);
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, from_path, edge_confidence) \
                 VALUES ('a_test.rs::it_works', 'a.rs::well_tested', 'a_test.rs', 'resolved')",
                [],
            )
            .unwrap();
        }
        *server.phase_handle().write().unwrap() = IndexingPhase::Ready;

        let v = jv(server.test_gap_hotspots(P(TestGapHotspotsParams { top_n: None })));
        let gaps = v["gaps"].as_array().unwrap();
        assert!(
            !gaps
                .iter()
                .any(|g| g["qualified_name"] == "a.rs::well_tested"),
            "well_tested has caller_count>0 (not dead) and a direct test caller — must not be a gap, got: {v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_gap_hotspots_recognizes_test_caller_in_a_non_test_named_file() {
        // Regression test for a real bug found 2026-07-14: `build_health`
        // used to detect "has a direct test" purely from the CALLING file's
        // NAME (`is_test_file`: must contain "test"/"spec" or start with
        // "tests/"). CALM's own `#[cfg(test)] mod tests` lives centralized
        // in `tools.rs` (no test-ish name), so every one of its 171 tests
        // was invisible to this heuristic even though `call_edges` and the
        // `symbols.is_test` flag both correctly recorded the relationship —
        // verified live against this project's own index.db before fixing.
        // This seeds the exact shape: a caller symbol with `is_test = 1`
        // living in a file named `lib.rs` (no test-ish substring at all).
        let dir = std::env::temp_dir().join(format!("ci_testgap_nontest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CalmServer::new(dir.clone(), dir.join("index.db")).unwrap();
        {
            let conn = server.db();
            seed_symbol(&conn, "a.rs::well_tested2", "well_tested2", "a.rs", 3, 9, 0);
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point, is_test, coreness) \
                 VALUES ('lib.rs::mod_tests::it_works2', 'it_works2', 'function', 'rust', 'lib.rs', 1, 5, 'fn it_works2()', '', 'it_works2', 0, 0, 0, 1, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, from_path, edge_confidence) \
                 VALUES ('lib.rs::mod_tests::it_works2', 'a.rs::well_tested2', 'lib.rs', 'resolved')",
                [],
            )
            .unwrap();
        }
        *server.phase_handle().write().unwrap() = IndexingPhase::Ready;

        let v = jv(server.test_gap_hotspots(P(TestGapHotspotsParams { top_n: None })));
        let gaps = v["gaps"].as_array().unwrap();
        assert!(
            !gaps
                .iter()
                .any(|g| g["qualified_name"] == "a.rs::well_tested2"),
            "well_tested2 has a caller whose SYMBOL is_test=1, even though the caller's \
             FILE name (lib.rs) doesn't look test-ish — must not be a gap, got: {v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_gap_hotspots_reports_honest_empty_when_edges_not_ready() {
        let dir = std::env::temp_dir().join(format!("ci_testgap_noedges_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CalmServer::new(dir.clone(), dir.join("index.db")).unwrap();

        let v = jv(server.test_gap_hotspots(P(TestGapHotspotsParams { top_n: None })));
        assert_eq!(v["edges_ready"], false);
        assert_eq!(v["gaps"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
