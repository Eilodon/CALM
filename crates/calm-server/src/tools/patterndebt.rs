use super::common::*;
use super::*;

/// `search(kind="similar")` overfetch limit for a pattern-debt check — small
/// on purpose: this tool reports *count of remaining duplicates*, not a
/// browsable result list, so there's no value in asking for more than a
/// screenful even if the true count is higher (`truncated` on the
/// underlying `SearchOutput` would tell us, but we only surface the count).
const PATTERN_DEBT_SIMILAR_LIMIT: usize = 20;
/// `search(kind="similar")` returns its top-K nearest chunks unconditionally
/// — no distance/similarity cutoff — so with fewer than
/// `PATTERN_DEBT_SIMILAR_LIMIT` other embedded chunks in the whole index, a
/// completely unrelated chunk still comes back as a "result" simply for
/// lacking anything closer to rank it out. Left unfiltered, `current_count`
/// would almost never reach 0 in a real repo (there's almost always *some*
/// nearest neighbor), making `status: "resolved"` practically unreachable —
/// caught by this tool's own round-trip test. Only a result whose cosine
/// score clears this bar counts as a still-live duplicate. 0.75 is a
/// deliberately high bar (near-duplicate code, not merely related code) —
/// tune from real usage, not derived analytically.
const PATTERN_DEBT_SIMILARITY_THRESHOLD: f64 = 0.75;
/// Cap on how many entries a topic-less `pattern_debt_status` call re-checks
/// in one round trip — each entry costs one `search(kind="similar")` KNN
/// scan, so an unbounded "check everything" call could turn into an
/// O(entries × KNN) cliff on a repo with many registered anchors (flagged in
/// docs/superskills/specs/2026-07-11-superskills-inspired-features.md #1's
/// audit). Callers with more entries than this should pass `topic` to check
/// one at a time, or call repeatedly.
const PATTERN_DEBT_STATUS_MAX: usize = 30;

#[rmcp::tool_router(router = "patterndebt_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "pattern_debt_register",
        description = "Register a duplicate-code-pattern anchor for later re-checking via pattern_debt_status. Resolves `symbol` the same way edit_context does (same path/line disambiguation contract), then baselines with search(kind=\"similar\") against the resolved symbol's current location. Anchored by the symbol's qualified_name, NOT path+line — survives line-shifting edits elsewhere in the file; if the symbol itself is later renamed/removed, pattern_debt_status reports anchor_lost instead of a false \"resolved\". USE WHEN: you just found/fixed one instance of a duplicated bug pattern and want to track how many other instances remain. Re-registering the same symbol replaces its note and re-baselines. Needs the embeddings feature ready — returns EMBEDDINGS_NOT_READY otherwise, same degradation as search(kind=\"similar\")/kind=\"semantic\".",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn pattern_debt_register(
        &self,
        Parameters(p): Parameters<PatternDebtRegisterParams>,
    ) -> Json<ResolvedOutcome<PatternDebtRegisterOutput>> {
        Json(self.timed_tool("pattern_debt_register", || {
            let read_conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let resolution = resolve_symbol(&read_conn, &p.symbol, p.path.as_deref(), p.line);
            let c = match resolution {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.symbol),
                SymbolResolution::Ambiguous(candidates) => {
                    return ResolvedOutcome::ambiguous(&candidates);
                }
                SymbolResolution::Found(c) => *c,
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let baseline = match calm_core::search::search_similar(
                &read_conn,
                &c.path,
                c.line_start,
                PATTERN_DEBT_SIMILAR_LIMIT,
            ) {
                Ok(o) => o,
                Err(e) => {
                    return ResolvedOutcome::error(error_detail(
                        "QUERY_FAILED",
                        &format!("similarity query failed: {e}"),
                        true,
                    ));
                }
            };
            if baseline.degraded {
                return ResolvedOutcome::error(error_detail(
                    "EMBEDDINGS_NOT_READY",
                    baseline.note.as_deref().unwrap_or(
                        "no embedded chunk at this symbol's location — the embeddings feature \
                         may be off, or the index hasn't embedded it yet",
                    ),
                    true,
                ));
            }

            let write_conn = match self.memory_write_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            let now = utc_now_iso8601();
            let baseline_count = baseline
                .results
                .iter()
                .filter(|r| r.score >= PATTERN_DEBT_SIMILARITY_THRESHOLD)
                .count() as i64;
            let topic = c.qualified_name.clone();
            if let Err(e) = write_conn.execute(
                "INSERT INTO pattern_debt \
                     (topic, anchor_qualified_name, note, baseline_count, status, created_at, last_checked_at, last_checked_count) \
                 VALUES (?1, ?1, ?2, ?3, 'open', ?4, ?4, ?3) \
                 ON CONFLICT(topic) DO UPDATE SET \
                     note = excluded.note, \
                     baseline_count = excluded.baseline_count, \
                     status = 'open', \
                     last_checked_at = excluded.last_checked_at, \
                     last_checked_count = excluded.last_checked_count",
                rusqlite::params![topic, p.note, baseline_count, now],
            ) {
                return ResolvedOutcome::error(error_detail(
                    "WRITE_FAILED",
                    &format!("write failed: {e}"),
                    false,
                ));
            }

            ResolvedOutcome::success(PatternDebtRegisterOutput {
                anchor_qualified_name: topic.clone(),
                baseline_count,
                suggested_next: self.filter_sn(suggested_with_args(
                    "pattern_debt_status",
                    "Re-check this anchor later to see if similar instances were fixed",
                    serde_json::json!({ "topic": topic }),
                )),
                topic,
            })
        }))
    }

    #[tool(
        name = "pattern_debt_status",
        description = "Re-check registered pattern-debt anchor(s): re-resolves each anchor's current location by qualified_name (never a stale line number) and re-runs search(kind=\"similar\") to report how many similar instances remain. Pass `topic` for one anchor (the value pattern_debt_register returned), or omit to check every OPEN anchor (capped — see truncated). status is one of: open (similar instances still found), resolved (none found this check — persisted), anchor_lost (the symbol was renamed/removed/split since registration — never silently reported as resolved). USE WHEN: verifying whether a duplicated bug pattern you registered earlier has actually been cleaned up elsewhere, or auditing outstanding pattern debt. Read-only towards your code; does update this anchor's own tracked status/last-checked fields.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn pattern_debt_status(
        &self,
        Parameters(p): Parameters<PatternDebtStatusParams>,
    ) -> Json<ToolOutcome<PatternDebtStatusOutput>> {
        Json(self.timed_tool("pattern_debt_status", || {
            let read_conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };

            let rows: Vec<PatternDebtRow> = if let Some(topic) = p.topic.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                match read_conn.prepare(
                    "SELECT topic, anchor_qualified_name, note, baseline_count, status FROM pattern_debt WHERE topic = ?1",
                ).and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![topic], pattern_debt_row)?
                        .collect::<Result<Vec<_>, _>>()
                }) {
                    Ok(r) => r,
                    Err(e) => return ToolOutcome::error(error_detail("QUERY_FAILED", &format!("query failed: {e}"), true)),
                }
            } else {
                match read_conn.prepare(
                    "SELECT topic, anchor_qualified_name, note, baseline_count, status FROM pattern_debt \
                     WHERE status = 'open' ORDER BY created_at ASC LIMIT ?1",
                ).and_then(|mut stmt| {
                    stmt.query_map(rusqlite::params![(PATTERN_DEBT_STATUS_MAX + 1) as i64], pattern_debt_row)?
                        .collect::<Result<Vec<_>, _>>()
                }) {
                    Ok(r) => r,
                    Err(e) => return ToolOutcome::error(error_detail("QUERY_FAILED", &format!("query failed: {e}"), true)),
                }
            };

            let truncated = rows.len() > PATTERN_DEBT_STATUS_MAX;
            let rows: Vec<PatternDebtRow> = rows.into_iter().take(PATTERN_DEBT_STATUS_MAX).collect();

            if rows.is_empty() {
                return ToolOutcome::success(PatternDebtStatusOutput {
                    entries: vec![],
                    truncated: false,
                    suggested_next: self.filter_sn(suggested(
                        "pattern_debt_register",
                        "No matching pattern-debt entries — register one after fixing a duplicated bug instance",
                    )),
                });
            }

            let write_conn = match self.memory_write_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let now = utc_now_iso8601();

            let mut entries = Vec::with_capacity(rows.len());
            for row in rows {
                entries.push(self.check_one_pattern_debt(&read_conn, &write_conn, row, &now));
            }

            ToolOutcome::success(PatternDebtStatusOutput {
                entries,
                truncated,
                suggested_next: None,
            })
        }))
    }
}

impl CalmServer {
    /// Re-resolves one anchor by `anchor_qualified_name` (a fresh lookup —
    /// never the line number captured at registration time, which may have
    /// drifted or no longer even belong to this symbol after unrelated
    /// edits) and re-runs the similarity check, persisting the result.
    fn check_one_pattern_debt(
        &self,
        read_conn: &rusqlite::Connection,
        write_conn: &rusqlite::Connection,
        row: PatternDebtRow,
        now: &str,
    ) -> PatternDebtEntryOutput {
        let live: Option<(String, i64)> = read_conn
            .query_row(
                "SELECT path, line_start FROM symbols WHERE qualified_name = ?1 LIMIT 1",
                rusqlite::params![row.anchor_qualified_name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();

        let Some((path, line_start)) = live else {
            let _ = write_conn.execute(
                "UPDATE pattern_debt SET status = 'anchor_lost', last_checked_at = ?2 WHERE topic = ?1",
                rusqlite::params![row.topic, now],
            );
            return PatternDebtEntryOutput {
                topic: row.topic,
                anchor_qualified_name: row.anchor_qualified_name,
                note: row.note,
                baseline_count: row.baseline_count,
                status: "anchor_lost".to_string(),
                current_count: None,
                remaining_locations: vec![],
                checked_at: now.to_string(),
            };
        };

        let check = calm_core::search::search_similar(
            read_conn,
            &path,
            line_start,
            PATTERN_DEBT_SIMILAR_LIMIT,
        );
        match check {
            Ok(o) if !o.degraded => {
                let remaining: Vec<_> = o
                    .results
                    .into_iter()
                    .filter(|r| r.score >= PATTERN_DEBT_SIMILARITY_THRESHOLD)
                    .collect();
                let current_count = remaining.len() as i64;
                let status = if current_count == 0 {
                    "resolved"
                } else {
                    "open"
                };
                let _ = write_conn.execute(
                    "UPDATE pattern_debt SET status = ?3, last_checked_at = ?2, last_checked_count = ?4 WHERE topic = ?1",
                    rusqlite::params![row.topic, now, status, current_count],
                );
                PatternDebtEntryOutput {
                    topic: row.topic,
                    anchor_qualified_name: row.anchor_qualified_name,
                    note: row.note,
                    baseline_count: row.baseline_count,
                    status: status.to_string(),
                    current_count: Some(current_count),
                    remaining_locations: remaining
                        .into_iter()
                        .map(|r| PatternDebtLocationOutput {
                            qualified_name: r.qualified_name,
                            path: r.path,
                            line_start: r.line_start,
                            score: (r.score * 1000.0).round() / 1000.0,
                        })
                        .collect(),
                    checked_at: now.to_string(),
                }
            }
            // Degraded (embeddings unavailable this run) or a query error:
            // leave the persisted status untouched rather than guessing —
            // an unchecked anchor must never be silently reported as
            // "resolved" just because this particular check couldn't run.
            _ => PatternDebtEntryOutput {
                topic: row.topic,
                anchor_qualified_name: row.anchor_qualified_name,
                note: row.note,
                baseline_count: row.baseline_count,
                status: format!("{} (check_unavailable_this_run)", row.status),
                current_count: None,
                remaining_locations: vec![],
                checked_at: now.to_string(),
            },
        }
    }
}

fn pattern_debt_row(row: &rusqlite::Row) -> rusqlite::Result<PatternDebtRow> {
    Ok(PatternDebtRow {
        topic: row.get(0)?,
        anchor_qualified_name: row.get(1)?,
        note: row.get(2)?,
        baseline_count: row.get(3)?,
        status: row.get(4)?,
    })
}

struct PatternDebtRow {
    topic: String,
    anchor_qualified_name: String,
    note: String,
    baseline_count: i64,
    status: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct PatternDebtRegisterParams {
    /// Bare symbol name (not a `path::name` qualified name) — same
    /// resolution contract as `edit_context`.
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// Free-text description of the bug/duplication pattern this anchor
    /// tracks — shown back on every `pattern_debt_status` check.
    pub(crate) note: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PatternDebtRegisterOutput {
    /// Stable id for this anchor — pass to `pattern_debt_status(topic=...)`.
    /// Always equal to `anchor_qualified_name` at registration time.
    pub(crate) topic: String,
    pub(crate) anchor_qualified_name: String,
    /// Similar-instance count at registration time (excludes the anchor
    /// itself), from `search(kind="similar")`.
    pub(crate) baseline_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct PatternDebtStatusParams {
    /// Check one anchor by its topic (from `pattern_debt_register`'s
    /// output). Omit to check every currently-`open` anchor instead
    /// (capped — see the output's `truncated`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) topic: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PatternDebtLocationOutput {
    pub(crate) qualified_name: String,
    pub(crate) path: String,
    pub(crate) line_start: Option<i64>,
    pub(crate) score: f64,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PatternDebtEntryOutput {
    pub(crate) topic: String,
    pub(crate) anchor_qualified_name: String,
    pub(crate) note: String,
    pub(crate) baseline_count: i64,
    /// One of `open` / `resolved` / `anchor_lost`, or a `"<status>
    /// (check_unavailable_this_run)"` suffix when this check couldn't run
    /// (e.g. embeddings not ready) — the persisted status shown is what it
    /// was *before* this call, never guessed.
    pub(crate) status: String,
    /// Similar-instance count from this check (excludes the anchor itself).
    /// `None` when the anchor is lost or this check was unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) current_count: Option<i64>,
    pub(crate) remaining_locations: Vec<PatternDebtLocationOutput>,
    pub(crate) checked_at: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PatternDebtStatusOutput {
    pub(crate) entries: Vec<PatternDebtEntryOutput>,
    /// `true` when more than `PATTERN_DEBT_STATUS_MAX` open entries exist —
    /// re-run with an explicit `topic` to check the rest.
    pub(crate) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
