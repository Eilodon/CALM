use super::common::*;
use super::*;

/// `(symbol, path, edge_confidence, edge_kind, line, formal_source)` --
/// D2 (2026-07-30 stack-graphs-demotion-lever) pushed this past clippy's
/// type_complexity threshold as a bare tuple; named here purely to satisfy
/// that lint, same row shape serves both the callers and callees queries
/// below.
type EdgeRow = (String, String, String, String, Option<i64>, Option<String>);

/// Lookback window for the `trend` field — chosen to match typical daily CI
/// cadence (one `calm fitness-check` snapshot/day) while staying short enough
/// to reflect recent activity rather than all-time drift.
const EDIT_CONTEXT_TREND_LOOKBACK_DAYS: i64 = 7;

#[rmcp::tool_router(router = "guardrails_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "edit_context",
        description = "ALWAYS CALL THIS before any code modification — mandatory, never skip. USE WHEN: you are about to edit, refactor, or delete a symbol. NOT FOR: read-only inspection (use symbol_info + source). NOT post-edit (use diff_impact).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) fn edit_context(
        &self,
        Parameters(p): Parameters<EditContextParams>,
    ) -> Json<ResolvedOutcome<EditContextOutput>> {
        Json(self.timed_tool("edit_context", || {
            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error_resolved(e),
            };
            // Range mode: `symbol` omitted -> review a raw [line, end_line]
            // window straight from `path`, no symbol resolution. Mirrors
            // `source`'s own range-mode dispatch (inspect.rs). Wave 8 (audit
            // follow-up, P0-A): before this, a pure whitespace/comment/
            // module-level/gap region had no way to be reviewed at all, so
            // Strict mode's edit gate had no success path for editing one.
            let symbol_name = match p.symbol.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(s) => s.to_string(),
                None => return self.edit_context_range(&conn, &p),
            };
            let resolution = match resolve_symbol(
                &conn,
                &self.project_root,
                &symbol_name,
                p.path.as_deref(),
                p.line,
                p.qualified_name.as_deref(),
            ) {
                Ok(r) => r,
                Err(e) => return db_error_resolved(e),
            };
            let (c, verified_bytes) = match resolution {
                SymbolResolution::NotFound => return ResolvedOutcome::not_found(&symbol_name),
                SymbolResolution::Ambiguous(candidates) => {
                    return ResolvedOutcome::ambiguous(&candidates);
                }
                SymbolResolution::ReadFailed(e) => return ResolvedOutcome::error(e),
                SymbolResolution::Found(c, bytes) => (*c, bytes),
            };
            self.track_symbol(&c.qualified_name);
            self.track_file(&c.path);

            let config = self.config();

            // Wave 7 (audit follow-up, P0-C): slice the checksum from the
            // EXACT bytes verify_live just read, instead of a second,
            // independent read here -- edit_context MINTS authority, so a
            // separate read reopens the same TOCTOU window Wave 6 already
            // closed for source()/understand() (see resolve_symbol/
            // verify_live in outcome.rs). Falls back to a fresh read only
            // when verify_live's own read failed (bytes is None) -- the
            // same rare "unreadable" case source() falls back for.
            // For edit_lines/edit_symbol's expected_hash — computed the same
            // way apply_hunks hashes a range, so this checksum is directly
            // usable without a separate round trip to learn it.
            let range_checksum = verified_bytes
                .or_else(|| std::fs::read_to_string(self.project_root.join(&c.path)).ok())
                .and_then(|content| {
                    calm_core::edit::range_checksum(
                        &content,
                        c.line_start as usize,
                        c.line_end as usize,
                    )
                });

            let callers: Vec<CallerEntry> = {
                let mut stmt = match conn.prepare(
                    "SELECT ce.from_symbol, ce.from_path, ce.edge_confidence, ce.call_site_line, ce.edge_kind, ce.formal_source
                     FROM call_edges ce
                     LEFT JOIN symbols s ON s.qualified_name = ce.from_symbol
                     WHERE ce.to_symbol = ?1 AND ce.ruled_out_by_scip = 0
                     ORDER BY COALESCE(s.is_test, 0) ASC, ce.from_path, ce.call_site_line",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                // Rows collected first (no preview yet) so previews can be
                // batched by unique file afterward instead of once per row
                // (audit F11) -- a hub symbol's callers routinely repeat a
                // handful of files dozens of times.
                let rows: Vec<EdgeRow> =
                    match stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1).unwrap_or_default(),
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    }) {
                        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                        Err(e) => return db_error_resolved(e),
                    };
                let preview_items: Vec<(String, Option<i64>)> = rows
                    .iter()
                    .map(|(_, path, _, _, line, _)| (path.clone(), *line))
                    .collect();
                let previews = line_previews_batched(&self.project_root, &preview_items);
                rows.into_iter()
                    .zip(previews)
                    .map(
                        |(
                            (symbol, _path, edge_confidence, edge_kind, line, formal_source),
                            preview,
                        )| CallerEntry {
                            symbol,
                            edge_confidence,
                            formal_source,
                            edge_kind,
                            line,
                            preview,
                        },
                    )
                    .collect()
            };

            let callees: Vec<CalleeEntry> = {
                let mut stmt = match conn.prepare(
                    "SELECT ce.to_symbol, ce.to_path, ce.edge_confidence, ce.call_site_line, ce.edge_kind, ce.formal_source
                     FROM call_edges ce
                     LEFT JOIN symbols s ON s.qualified_name = ce.to_symbol
                     WHERE ce.from_symbol = ?1 AND ce.ruled_out_by_scip = 0
                     ORDER BY COALESCE(s.is_test, 0) ASC, ce.to_path, ce.call_site_line",
                ) {
                    Ok(s) => s,
                    Err(e) => return db_error_resolved(e),
                };
                let rows: Vec<EdgeRow> =
                    match stmt.query_map(rusqlite::params![c.qualified_name], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1).unwrap_or_default(),
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    }) {
                        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                        Err(e) => return db_error_resolved(e),
                    };
                // The call site lives in the symbol being inspected
                // (`c.path`), not in the callee's own file (`to_path`) --
                // every row's preview key is this same constant path, so
                // line_previews_batched reads it exactly once no matter how
                // many callees there are (audit F11).
                let from_path = c.path.clone();
                let preview_items: Vec<(String, Option<i64>)> = rows
                    .iter()
                    .map(|(_, _, _, _, line, _)| (from_path.clone(), *line))
                    .collect();
                let previews = line_previews_batched(&self.project_root, &preview_items);
                rows.into_iter()
                    .zip(previews)
                    .map(
                        |(
                            (symbol, path, edge_confidence, edge_kind, line, formal_source),
                            preview,
                        )| CalleeEntry {
                            symbol,
                            path,
                            edge_confidence,
                            formal_source,
                            edge_kind,
                            line,
                            preview,
                        },
                    )
                    .collect()
            };

            let blast_radius = {
                let (entries, capped) = transitive_bfs(
                    &conn,
                    &c.qualified_name,
                    EdgeDirection::Callers,
                    config.callers.max_depth_cap,
                    config.callers.transitive_timeout_ms,
                );
                // F3: confidence-filter to match `risk_assessment` below — an
                // `ambiguous` edge is index-time name-collision fan-out, not a
                // confirmed caller of *this* symbol, so it must not pad the
                // blast radius any more than it pads `confirmed_caller_count`.
                // transitive_bfs already refuses to EXPAND through ambiguous
                // edges (ADR-0009), so only depth-1 ambiguous neighbors can
                // leak in here — dropping them keeps blast_radius and
                // risk_assessment telling the same story.
                let confirmed_paths: Vec<String> = entries
                    .iter()
                    .filter(|e| e.edge_confidence != "ambiguous")
                    .map(|e| e.path.clone())
                    .collect();
                let transitive = confirmed_paths.len() as i64;
                let mut files_affected = confirmed_paths;
                files_affected.sort();
                files_affected.dedup();
                BlastRadiusInfo {
                    transitive,
                    files_affected,
                    capped: capped.then_some(true),
                }
            };

            let co_changed_files: Vec<CoChangedFileOutput> = self
                .co_changes_cached(
                    &c.path,
                    &config.cochange.since,
                    config.cochange.min_co_changes,
                    config.cochange.top_n,
                )
                .entries
                .into_iter()
                .map(CoChangedFileOutput::from)
                .collect();

            let related_notes = self.related_notes(&conn, &c.path, &c.name, c.is_hub);
            // `callers` (shown in full above, `ambiguous` entries included so
            // the caller can judge each one) is not the same count as "risk of
            // touching this symbol" — an `ambiguous` edge is index-time fan-out
            // to every same-named candidate when a call's receiver type
            // couldn't be resolved (see `refresh_caller_counts`'s doc comment),
            // not a confirmed caller of *this* one. Counting it here inflated
            // risk to "high" purely from name-collision noise (e.g. a common
            // method name shared by several unrelated classes) even when the
            // real, confirmed caller count was low or zero — the same
            // "confirmed caller" definition `symbols.caller_count` already
            // uses elsewhere in this codebase, now applied consistently here.
            //
            // Computed from the FULL `callers` (before any truncation below)
            // — risk/dead-code must never be skewed by the display cap.
            let confirmed_caller_count = callers
                .iter()
                .filter(|c| c.edge_confidence != "ambiguous")
                .count();
            let mut risk = risk_level_from_caller_count(confirmed_caller_count as i64).to_string();

            // Raw caller-count alone can't see entry points, test-only
            // helpers, or runtime dispatch (reflection, framework callbacks)
            // that never shows up in the static call graph — exactly the
            // richer signal `compute_dead_code_confidence` already computes
            // for `symbol_info`'s `build_health`, just never wired in here
            // before. Only relevant when there are zero confirmed callers
            // (`risk_level_from_caller_count` already tiers nonzero counts
            // sensibly on its own): if the dead-code heuristic disagrees
            // that this looks safely removable — `"none"` (confirmed
            // entry-point/test) or `"low"` (runtime-covered despite no
            // static callers, or genuinely ambiguous scope) — escalate from
            // "low" to "medium" so the caller doesn't read a bare 0-caller
            // count as "safe to delete" when it isn't.
            let abs_path =
                calm_core::analysis::coverage::normalize_path(&self.project_root.join(&c.path));
            let is_private = calm_core::analysis::dead_code::is_private_symbol(
                &c.language,
                &c.name,
                &c.signature,
            );
            let scope_clear = calm_core::analysis::dead_code::scope_clear_for_language(&c.language);
            let (dead_code_confidence, dead_code_source) =
                calm_core::analysis::dead_code::compute_dead_code_confidence(
                    &abs_path,
                    c.line_start,
                    c.line_end,
                    confirmed_caller_count as i64,
                    c.is_entry_point,
                    c.is_test,
                    is_private,
                    scope_clear,
                    &self.coverage.read_ok(),
                    &c.kind,
                );
            let mut risk_reasons: Vec<String> = Vec::new();
            if confirmed_caller_count == 0
                && risk == "low"
                && zero_caller_count_is_uncertain(dead_code_confidence)
            {
                risk = "medium".to_string();
                risk_reasons.push(format!(
                    "zero confirmed callers, but dead-code confidence ({dead_code_confidence}) disagrees this is safe to remove"
                ));
            }

            // Ownership-entropy escalation (#2, 2026-07-27 martin/entropy/
            // churn plan): a single-author file is a low-bus-factor risk no
            // caller-count-based signal above can see -- confirmed_caller_count
            // and dead_code_confidence both describe how USED a symbol is,
            // not how much independent review its history has had. Gated
            // strictly on entropy == 0.0 (exactly one distinct commit
            // author, not merely "low" by some arbitrary threshold) so this
            // never fires on a fuzzy cutoff -- ownership_entropy's own doc
            // comment guarantees 0.0 is returned only for the single-author
            // case, never as a rounding artifact of a skewed-but-multi-
            // author split. Deliberately confined to this local `risk`
            // string, NOT `risk_level_from_caller_count` itself: that shared
            // function also feeds `compute_touch_risk`'s write-blocking gate
            // (edit_lines/edit_symbol), and entropy is an advisory reviewer-
            // coverage signal that must never acquire the power to refuse an
            // edit outright.
            if risk == "low"
                && let Some(entropy) = self.ownership_entropy_for(&c.path)
                && entropy == 0.0
            {
                risk = "medium".to_string();
                risk_reasons.push(
                    "single-author file (low bus factor) — no second reviewer has context here"
                        .to_string(),
                );
            }
            let risk = Some(RiskAssessmentOutput {
                level: risk,
                reasons: risk_reasons,
            });

            // Plan 3 gate_prediction (FIX2/F2b, UPGRADE_PLAN.md): reuses the
            // exact same compute_touch_risk + classify_gate the real
            // edit_lines/edit_symbol write gate uses (edit.rs), over the SAME
            // range [c.line_start, c.line_end] -- symbols_overlapping_ranges
            // scans the whole line range, so `gate_touched` naturally
            // includes an enclosing class when this symbol sits inside one
            // (closes F2b, not just F2). Single source of truth with the
            // real gate -- see classify_gate's doc comment.
            // Wave 9 (audit follow-up, finding #4 -- nested-symbol
            // WRONG_TARGET_SCOPE): `gate_touched` is also returned now so
            // mint_review_authority_for_edit_context below can bind the
            // SAME touched-symbol set (method + any enclosing class/struct
            // whose own range overlaps [c.line_start, c.line_end]) that a
            // real edit_lines/edit_symbol spend against this range will
            // compute for itself -- see that function's own doc comment.
            let (
                gate_prediction,
                gate_touched,
                gate_touches_uncovered_code,
                gate_uncertain_zero_caller_bool,
                gate_signature_touch_bool,
            ) = {
                let gate_policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
                // P0-3 (audit follow-up, 2026-08-23): without
                // `p.proposed_new_text`, no real proposed edit content
                // exists yet at this pre-edit exploration call, so a
                // synthetic full-range placeholder hunk (empty text,
                // real_hunks=false) is used exactly as before -- it still
                // lets compute_touch_risk's uncovered-code probe run for
                // real (that probe only reads hunk start/end, never text),
                // but gate_signature_touch stays None from it (see
                // TouchRiskResult's own doc comment on its 8th element).
                // When the caller DOES supply `proposed_new_text`, this is
                // a real whole-range-replace hunk with real_hunks=true, so
                // the exact same signature-change detection the real
                // edit_lines/edit_symbol write gate uses at spend time
                // also runs here at mint time -- closing the dead end
                // where a genuine signature edit could never mint an
                // authority whose signature_changed matched what spend
                // time would independently recompute from the real diff.
                let gate_hunks: Vec<(i64, i64, &str)> = vec![(
                    c.line_start,
                    c.line_end,
                    p.proposed_new_text.as_deref().unwrap_or(""),
                )];
                let gate_real_hunks = p.proposed_new_text.is_some();
                let (
                    gate_risk,
                    gate_hub_hit,
                    gate_hub_kind,
                    gate_uncertain_zero_caller,
                    gate_touched,
                    gate_risk_rule_reason,
                    gate_touches_uncovered_code,
                    gate_signature_touch,
                ) = edit::compute_touch_risk(
                    &conn,
                    &self.project_root,
                    &c.path,
                    &[(c.line_start, c.line_end)],
                    &self.coverage.read_ok(),
                    &config.risk_rules,
                    &gate_hunks,
                    &gate_policy,
                    gate_real_hunks,
                );
                // Mirrors edit_lines_impl_gated's own bridge-downgrade
                // eligibility check exactly (edit.rs) -- computed here too,
                // separately from compute_touch_risk, so `requires` doesn't
                // overstate the tier for a bridge-only hub (audit-design
                // finding, UPGRADE_PLAN.md Risk Assessment §2).
                let bridge_downgrade_eligible = gate_hub_kind.as_deref() == Some("bridge")
                    && gate_risk.as_deref() != Some("high")
                    && gate_uncertain_zero_caller.is_none()
                    && edit::all_caller_edges_confident(
                        &conn,
                        &gate_touched
                            .iter()
                            .filter(|t| t.hub_kind.is_some())
                            .map(|t| t.qualified_name.clone())
                            .collect::<Vec<_>>(),
                    );
                let classification = edit::classify_gate(
                    gate_hub_hit,
                    gate_risk.as_deref(),
                    gate_uncertain_zero_caller,
                    bridge_downgrade_eligible,
                    config.edit.always_require_edit_context_effective(),
                    gate_risk_rule_reason.as_deref(),
                );
                let blocking_symbols: Vec<String> = gate_touched
                    .iter()
                    .filter(|t| t.is_hub)
                    .map(|t| t.qualified_name.clone())
                    .collect();
                (
                    GatePredictionOutput {
                        will_block: classification.will_block_without_confirm,
                        is_hub: c.is_hub,
                        hub_kind: gate_hub_kind,
                        blocking_symbols,
                        requires: classification.requirement.as_str().to_string(),
                        reason: classification.why,
                    },
                    gate_touched,
                    gate_touches_uncovered_code,
                    gate_uncertain_zero_caller.is_some(),
                    gate_signature_touch.is_some(),
                )
            };

            // Structural half of edit_symbol/edit_lines' confirm gate (docs/
            // superskills/specs/2026-07-11-superskills-inspired-features.md
            // #5 v2): record that edit_context ran for this symbol, plus the
            // real caller identifiers a `reason` string will later need to
            // cite — the content-grounded half. Uses the full, untruncated
            // `callers` computed above, independent of the conditional-fetch
            // etag branch below (a cache-hit response must not blank this).
            let caller_qns_full: Vec<String> =
                callers.iter().map(|e| e.symbol.clone()).collect();
            // WS-2 Phase 2 (docs/plans/2026-08-02-phase2-priority-and-ws2-
            // execution-plan.md §5): digest the FULL caller set (before the
            // 5-item cap below) so `edit_lines_impl_gated` can later detect
            // when the real caller set drifted since this review, not just
            // whether the call-count freshness window expired.
            let caller_set_digest = Self::caller_set_digest(&caller_qns_full);
            // PR D (issue #65, docs/plans/2026-08-08-derived-artifact-
            // hardening-execution-plan.md): graph_generation at review time
            // -- see EditContextReview::graph_generation's doc comment for
            // why this is now load-bearing (STALE_GRAPH_AUTHORITY in
            // edit.rs), not just diagnostic metadata.
            let graph_generation: i64 = conn
                .query_row(
                    "SELECT generation FROM graph_generation_state WHERE id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            // CCK-10 (#65): mint a ReviewAuthority for this single-symbol
            // review -- fail-open (mint_review_authority_for_edit_context
            // returns None on any error), so a minting hiccup never blocks
            // this mandatory pre-edit tool's existing fields. Borrowed here
            // (not moved) so record_edit_context_review below still gets
            // its own owned copies unchanged.
            let minted = self.mint_review_authority_for_edit_context(
                &conn,
                &c,
                &caller_set_digest,
                graph_generation,
                &gate_touched,
                gate_touches_uncovered_code,
                gate_uncertain_zero_caller_bool,
                gate_signature_touch_bool,
            );
            self.record_edit_context_review(
                &c.qualified_name,
                &caller_qns_full,
                risk.as_ref().map(|r| r.level.as_str()).unwrap_or("unknown"),
                caller_set_digest,
                graph_generation,
            );
            self.note_reviewing(&c.qualified_name);
            let trend = calm_core::fitness::compute_trend(
                &conn,
                &c.qualified_name,
                EDIT_CONTEXT_TREND_LOOKBACK_DAYS,
            )
            .ok()
            .flatten()
            .map(TrendOutput::from);

            // Conditional-fetch — ONLY `callers`/`callees` are ever gated
            // behind this etag. risk_assessment/dead_code_confidence/
            // blast_radius/trend/co_changed_files/range_checksum above are
            // already fully (re)computed from live data regardless of a
            // match, and are never omitted — this is the mandatory pre-edit
            // safety tool, so none of those may go silently stale.
            let edges_etag = Some(calm_core::indexer::pipeline::hash_content(&format!(
                "{}\u{3}{}",
                hash_caller_entries(callers.iter()),
                hash_callee_entries(callees.iter()),
            )));
            let edges_not_modified = p.if_none_match.is_some() && p.if_none_match == edges_etag;

            let (callers, callees, callers_truncated, callees_truncated) = if edges_not_modified {
                (Vec::new(), Vec::new(), None, None)
            } else {
                let caller_cap = config.callers.direct_list_cap;
                let callee_cap = config.callees.direct_list_cap;
                let callers_truncated = (callers.len() > caller_cap).then_some(true);
                let callees_truncated = (callees.len() > callee_cap).then_some(true);
                let mut callers = callers;
                let mut callees = callees;
                callers.truncate(caller_cap);
                callees.truncate(callee_cap);
                (callers, callees, callers_truncated, callees_truncated)
            };

            let (change_id, authority_id, authority_expires_at) = match minted {
                Some(m) => (
                    Some(m.change_id),
                    Some(m.authority_id),
                    Some(m.authority_expires_at),
                ),
                None => (None, None, None),
            };

            ResolvedOutcome::success(EditContextOutput {
                symbol: symbol_name,
                edges_ready: self.edges_ready(),
                index_freshness: self.phase_str(),
                callers,
                callees,
                callers_truncated,
                callees_truncated,
                blast_radius,
                range_checksum,
                risk_assessment: risk,
                gate_prediction,
                dead_code_confidence: dead_code_confidence.to_string(),
                dead_code_source: dead_code_source.to_string(),
                trend,
                co_changed_files,
                related_notes,                edges_etag,
                edges_not_modified: edges_not_modified.then_some(true),
                suggested_next: self.filter_sn(suggested_gated(
                    "diff_impact",
                    "MANDATORY after changes — verify blast radius",
                )),
                change_id,
                authority_id,
                authority_expires_at,
            })
        }))
    }

    #[tool(
        name = "diff_impact",
        description = "CALL THIS after every code change, BEFORE commit or push — never skip. USE WHEN: you have uncommitted changes and want to verify blast radius. NOT FOR: pre-edit analysis (use edit_context). vs edit_context: edit_context=pre-edit; diff_impact=post-edit. Omit all three for the unstaged working-tree diff, or provide at most one of: diff, staged=true, commits=<range>.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn diff_impact(
        &self,
        Parameters(p): Parameters<DiffImpactParams>,
    ) -> Json<ToolOutcome<DiffImpactOutput>> {
        Json(self.timed_tool("diff_impact", || {
            let input_count =
                p.diff.is_some() as u8 + p.staged.is_some() as u8 + p.commits.is_some() as u8;
            if input_count > 1 {
                return ToolOutcome::error(error_detail(
                    "INVALID_INPUT",
                    "At most one of diff, staged, or commits may be provided (omit all three for the unstaged working-tree diff)",
                    false,
                ));
            }

            const DIFF_GIT_TIMEOUT_SECS: u64 = 10;
            let diff_text = if let Some(d) = p.diff {
                d
            } else {
                let staged = p.staged.unwrap_or(false);
                let (diff, err) = calm_core::analysis::diff_impact::get_git_diff(
                    &self.project_root,
                    staged,
                    p.commits.as_deref(),
                    DIFF_GIT_TIMEOUT_SECS,
                );
                match diff {
                    Some(d) => d,
                    None => {
                        return ToolOutcome::error(error_detail(
                            "FEATURE_UNAVAILABLE",
                            &err.unwrap_or_else(|| "git diff failed".into()),
                            true,
                        ));
                    }
                }
            };

            let file_diffs = calm_core::analysis::diff_impact::parse_unified_diff(&diff_text);
            let files_changed: Vec<String> = file_diffs.iter().map(|f| f.path.clone()).collect();

            let mut unindexed_files: Vec<UnindexedFileOutput> = Vec::new();
            // audit H4: built directly instead of via a HashMap<String, serde_json::Value>
            // intermediate -- avoids a serialize-then-deserialize round trip on every call,
            // and a typo'd map key silently dropping a symbol instead of failing to compile.
            let mut affected: Vec<AffectedSymbolOutput> = Vec::new();
            // Audit 5.1: a file whose `file_index` row is gone AND whose
            // extension the indexer would normally extract symbols from
            // (`language_for_extension(ext).is_some()`) reads identically to
            // "never indexed" once reindexing has already cleaned up after
            // the deletion -- `reason == "deleted"` below hits the SAME
            // `continue` as a file that never had any symbols, contributing
            // zero `affected_symbols` and never touching `pending_scan_paths`.
            // With nothing else in the diff, `compute_aggregate_risk` then
            // falls through to its `unwrap_or("low")` default -- silently
            // reporting "low" for a deletion whose actual former caller
            // count this tool has no way left to check, not because it's
            // known to be low. Tracked separately from `pending_scan_paths`
            // (which means "wait for indexing, it resolves itself" -- a
            // deletion never resolves that way, see the suggested_next
            // branch below) so aggregate_risk becomes "unknown" without
            // wrongly pointing an agent at `indexing_status`.
            let mut unverifiable_deletions: Vec<String> = Vec::new();

            // READ-only: open a dedicated read connection (SINGLE_WRITER enforcement)
            {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                for fd in &file_diffs {
                    // file_index has one row per file the indexer has ever
                    // scanned, independent of how many symbols it found — a
                    // file with 0 symbols (e.g. a Rust `mod.rs` that's just
                    // `pub mod` statements) is still fully indexed, just
                    // empty, and must not be reported as "unindexed" (the old
                    // `symbols`-only check couldn't tell the two apart). A row
                    // whose `language` is NULL is a third case: a
                    // recognized-but-unparseable extension (see
                    // `is_recognized_unparsed_extension`) the indexer tracks by
                    // path only — it can never have symbols no matter how long
                    // you wait, so it's still reported here, with its own
                    // reason rather than silently reading as a normal empty file.
                    let row_language: Option<Option<String>> = conn
                        .query_row(
                            "SELECT language FROM file_index WHERE path = ?1",
                            rusqlite::params![fd.path],
                            |r| r.get::<_, Option<String>>(0),
                        )
                        .ok();
                    let reason = match &row_language {
                        None => {
                            let path = std::path::Path::new(&fd.path);
                            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            // A recognized extension only means "pending_scan" if the
                            // indexer would ever actually reach it — a .rs file under
                            // a dotdir or IGNORE_DIRS (e.g. .claude/, target/) never
                            // gets scanned no matter how long you wait, so it must be
                            // "out_of_scope" too, not just files of an unrecognized
                            // extension. See `calm_core::walk::path_has_ignored_dir_component`.
                            if !calm_core::walk::path_has_ignored_dir_component(path)
                                && (calm_core::indexer::lang_constants::language_for_extension(ext)
                                    .is_some()
                                    || calm_core::indexer::lang_constants::is_recognized_unparsed_extension(
                                        ext,
                                    ))
                            {
                                // This branch would otherwise say "pending_scan" —
                                // but a file that no longer exists on disk will
                                // never actually get scanned no matter how long
                                // you wait, so it must not be reported as a state
                                // that "resolves itself" (audit F2: caused an
                                // agent to loop diff_impact <-> indexing_status
                                // forever waiting on a file that was deleted).
                                // Checked here, not before the ignored-dir/
                                // extension check above, so a deleted file that
                                // was ALSO never going to be scanned (dotdir, or
                                // unrecognized extension) still correctly reports
                                // "out_of_scope", not "deleted".
                                if !self.project_root.join(path).exists() {
                                    // Audit 5.1: only a REAL call-graph source
                                    // extension makes this deletion's blast
                                    // radius genuinely unverifiable -- not
                                    // `is_recognized_unparsed_extension`'s
                                    // path-only-tracked config/lockfiles
                                    // (never had symbols), and not markdown/
                                    // sql either: both are real entries in
                                    // `language_for_extension` but are its
                                    // two explicitly "standalone, not
                                    // tree-sitter" fallback arms -- markdown
                                    // symbols are ATX headings
                                    // (`extract_markdown_symbols`) that never
                                    // participate in `call_edges`, so their
                                    // `caller_count` is always 0 and there is
                                    // no blast radius to lose in the first
                                    // place. Matches `language_for_extension`'s
                                    // own two-tier structure deliberately, so
                                    // this stays in sync if a language moves
                                    // between tiers.
                                    let has_real_call_graph = calm_core::indexer::lang_constants::language_for_extension(ext)
                                        .is_some_and(|lang| lang != "markdown" && lang != "sql");
                                    if has_real_call_graph {
                                        unverifiable_deletions.push(fd.path.clone());
                                    }
                                    Some("deleted")
                                } else {
                                    Some("pending_scan")
                                }
                            } else {
                                Some("out_of_scope")
                            }
                        }
                        Some(None) => Some("recognized_unparsed"),
                        Some(Some(_)) => None,
                    };
                    if let Some(reason) = reason {
                        unindexed_files.push(UnindexedFileOutput {
                            path: fd.path.clone(),
                            reason: reason.to_string(),
                        });
                        continue;
                    }

                    let mut stmt = match conn.prepare(
                        "SELECT qualified_name, name, kind, line_start, line_end, caller_count, signature, language
                         FROM symbols WHERE path = ?1",
                    ) {
                        Ok(s) => s,
                        Err(e) => return db_error(e),
                    };
                    // (qualified_name, name, kind, line_start, line_end, caller_count, signature, language)
                    type SymbolOverlapRow = (String, String, String, i64, i64, i64, String, String);
                    let rows: Vec<SymbolOverlapRow> = match stmt
                        .query_map(rusqlite::params![fd.path], |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                            ))
                        }) {
                        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                        Err(e) => return db_error(e),
                    };

                    for (
                        qualified_name,
                        name,
                        kind,
                        line_start,
                        line_end,
                        caller_count,
                        signature,
                        language,
                    ) in rows
                    {
                        let overlaps = fd
                            .hunks
                            .iter()
                            .any(|&(hs, he)| !(he < line_start || hs > line_end));
                        if !overlaps {
                            continue;
                        }

                        // The indexer's own signature extraction (parser.rs::walk_symbols)
                        // already scans to the real body-opening `{` (or `:` for Python) —
                        // its embedded newlines tell us exactly how many lines the real
                        // signature spans, instead of guessing with a fixed line cap (which
                        // silently missed changes past line 3 of a longer signature, e.g.
                        // calm_core::analysis::cochange::compute_co_changes's 7-line one).
                        // Clamped to line_end as a defensive bound, never exceeded in
                        // practice since a signature can't outlive its own symbol.
                        let sig_end =
                            (line_start + signature.matches('\n').count() as i64).min(line_end);
                        let is_new_symbol = calm_core::analysis::diff_impact::is_new_symbol(
                            (line_start, sig_end),
                            fd.is_new_file,
                            &fd.added_lines,
                            caller_count,
                            calm_core::analysis::diff_impact::signature_range_has_removal(
                                fd,
                                (line_start, sig_end),
                            ),
                        );
                        // A symbol that didn't exist before this diff cannot have had
                        // its signature "changed" — there is no prior signature to
                        // compare against, and (by definition) no prior call sites.
                        // `is_signature_changed` (line-overlap) is a cheap pre-filter;
                        // when it's true, `is_signature_semantically_changed` still has
                        // to agree the *text* actually differs (not just a parameter
                        // rename or reformat) before this escalates to high risk.
                        let signature_changed = !is_new_symbol
                            && calm_core::analysis::diff_impact::is_signature_changed(
                                (line_start, sig_end),
                                &fd.added_lines,
                            )
                            && {
                                let (old_text, new_text) =
                                    calm_core::analysis::diff_impact::signature_text_before_after(
                                        fd,
                                        (line_start, sig_end),
                                    );
                                calm_core::analysis::diff_impact::is_signature_semantically_changed(
                                    &old_text, &new_text, &language,
                                )
                            };

                        let base_level = risk_level_from_caller_count(caller_count);
                        let mut reasons: Vec<String> = Vec::new();
                        let level = if is_new_symbol {
                            reasons.push(
                                "newly added symbol — no prior call sites to check; review its own correctness".to_string(),
                            );
                            base_level.to_string()
                        } else {
                            calm_core::analysis::diff_impact::escalate_risk_if_signature_changed(
                                signature_changed,
                                base_level,
                                &mut reasons,
                            )
                        };

                        affected.push(AffectedSymbolOutput {
                            qualified_name,
                            name,
                            path: fd.path.clone(),
                            kind,
                            signature_changed,
                            symbol_is_new: is_new_symbol,
                            blast_radius: BlastRadiusOutput {
                                direct_callers: caller_count,
                            },
                            risk_assessment: RiskAssessmentOutput { level, reasons },
                        });
                    }
                }
            }

            let pending_scan_paths: Vec<String> = unindexed_files
                .iter()
                .filter(|f| f.reason == "pending_scan")
                .map(|f| f.path.clone())
                .collect();
            // Audit 5.1: `compute_aggregate_risk` treats any non-empty second
            // argument as "cannot verify, report unknown" -- exactly the
            // posture an unverifiable deletion needs too, so it's folded
            // into the same call rather than adding a third parameter to a
            // widely-tested, otherwise-unrelated function. Kept as a
            // separate Vec upstream (not merged into `pending_scan_paths`
            // itself) purely so the `suggested_next` branch below can still
            // tell the two causes of "unknown" apart.
            let unknown_gating_paths: Vec<String> = pending_scan_paths
                .iter()
                .cloned()
                .chain(unverifiable_deletions.iter().cloned())
                .collect();
            let aggregate_risk = calm_core::analysis::diff_impact::compute_aggregate_risk(
                &affected,
                &unknown_gating_paths,
            );
            const MAX_AFFECTED_SYMBOLS: usize = 20;
            calm_core::analysis::diff_impact::sort_affected_symbols(
                &mut affected,
                MAX_AFFECTED_SYMBOLS,
            );
            let affected_symbols = affected;

            let codeowner_patterns =
                calm_core::analysis::codeowners::load_codeowners(&self.project_root);
            let mut suggested_reviewers: Vec<String> = Vec::new();
            for f in &files_changed {
                for owner in calm_core::analysis::codeowners::find_owners(&codeowner_patterns, f) {
                    if !suggested_reviewers.contains(&owner) {
                        suggested_reviewers.push(owner);
                    }
                }
            }

            let sn = if !pending_scan_paths.is_empty() {
                suggested("indexing_status", "Wait for index before treating as safe")
            } else if let Some(path) = unverifiable_deletions.first() {
                // Audit 5.1: unlike pending_scan, waiting never resolves
                // this -- the file and its old symbols are gone from the
                // index for good. Point at a concrete manual check instead
                // of the generic "unknown" fallback below.
                suggested_with_args(
                    "search",
                    "A deleted file's blast radius could not be verified from the index \
                     (its old symbols/callers are no longer in the graph) — grep for \
                     lingering references to it before merging",
                    serde_json::json!({
                        "query": std::path::Path::new(path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or(path),
                        "kind": "grep",
                    }),
                )
            } else if aggregate_risk == "critical" || aggregate_risk == "high" {
                affected_symbols.first().map(|s| SuggestedNext {
                    tool: "callers".into(),
                    reason: "Verify high-risk callers manually".into(),
                    args: Some(serde_json::json!({"symbol": s.name})),
                    gate: None,
                    required_user_input: None,
                })
            } else if aggregate_risk == "medium" {
                affected_symbols.first().map(|s| SuggestedNext {
                    tool: "callers".into(),
                    reason: "Medium-risk changes — spot-check key callers".into(),
                    args: Some(serde_json::json!({"symbol": s.name})),
                    gate: None,
                    required_user_input: None,
                })
            } else if aggregate_risk == "unknown" {
                suggested("indexing_status", "Risk unknown — check index state")
            } else {
                None
            };

            // audit F6: only a genuinely successful analysis clears the
            // pending_diff_impact gate — an INVALID_INPUT/FEATURE_UNAVAILABLE/
            // db_error return above must leave it set, since "the call was
            // attempted" proves nothing about whether the diff was actually
            // analyzed.
            self.clear_written_files();
            let note = calm_core::analysis::diff_impact::any_compile_checkable_file(&files_changed)
                .then(|| {
                    "aggregate_risk reflects call-graph blast radius and edit-time syntax \
                     checks only — it does not run a build or test suite. files_changed \
                     includes Rust/Go/TypeScript source; confirm this project's own build/tests \
                     (e.g. cargo test, go build, tsc --noEmit) still pass before committing"
                        .to_string()
                });
            ToolOutcome::success(DiffImpactOutput {
                files_changed,
                affected_symbols,
                unindexed_files,
                aggregate_risk,
                suggested_reviewers,
                note,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    /// Runs `diff_impact` and returns its output as plain `serde_json::Value`
    /// -- for a caller outside this crate (the `calm guard` CLI command,
    /// `calm-cli`) that needs the result without depending on this crate's
    /// internal (`pub(crate)`) tool param/output types (`DiffImpactParams`,
    /// `DiffImpactOutput`, ...). Same pattern this crate's own tests use
    /// (`jv`) to assert on tool output without naming those types either.
    /// `calm guard` is CALM's Git/CI-native integration point
    /// (`KNOWN_LIMITATIONS.md` "No Git/CI-native integration path"): a
    /// pre-commit hook or CI step running against a diff made outside any
    /// MCP session, reusing this exact tool instead of a second risk-
    /// analysis implementation.
    pub fn diff_impact_json(
        &self,
        diff: Option<String>,
        staged: Option<bool>,
        commits: Option<String>,
    ) -> serde_json::Value {
        let result = self.diff_impact(rmcp::handler::server::wrapper::Parameters(
            DiffImpactParams {
                diff,
                staged,
                commits,
            },
        ));
        serde_json::to_value(result.0).unwrap()
    }
}

#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
pub(crate) struct EditContextParams {
    /// Bare symbol name (not a `path::name` qualified name) — e.g. `load`,
    /// not `crates/calm-core/src/embedding.rs::Embedder::load`. Omit ONLY
    /// in range mode (see `end_line`), where a raw `[line, end_line]`
    /// window is reviewed directly with no symbol resolution — mirrors
    /// `source`'s own `symbol`-omitted range mode (Wave 8 audit follow-up,
    /// P0-A).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) symbol: Option<String>,
    /// Narrows the search to one file when `symbol` alone is ambiguous
    /// across the repo. Repo-relative, e.g. `crates/calm-core/src/embedding.rs`.
    /// Required in range mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file (e.g. a
    /// `#[cfg(feature)]` real impl vs. its stub) — any line within the
    /// intended candidate's range, as echoed in an earlier `ambiguous`
    /// response's `line_start`/`line_end`. In range mode (symbol omitted)
    /// this is the 1-indexed START line of the window to review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// 5.3 (Wave 5): exact `qualified_name` from a prior `search`/`locate`
    /// result — when set, resolves directly by identity and `path`/`line`
    /// are ignored, so this can never come back ambiguous even for a
    /// globally-common bare `symbol` name. Still flows through the same
    /// live-verification every resolution does (Wave 1's `verify_live`).
    /// Ignored in range mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) qualified_name: Option<String>,
    /// Range mode: 1-indexed, inclusive END line of a raw window to
    /// review directly from `path` with no symbol resolution — for
    /// module-level or between-symbol code no symbol range covers (pure
    /// whitespace/comment/module-constant/gap regions). Requires `path` +
    /// `line` (the start) and `symbol` omitted. Ignored in symbol mode.
    /// Wave 8 (audit follow-up, P0-A): before this, Strict mode had no
    /// success path at all for editing such a region — neither the
    /// confirm/reason gate nor the full ReviewAuthority path could ever
    /// clear, because nothing could review it first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) end_line: Option<i64>,
    /// `edges_etag` from a prior `edit_context` call on this exact symbol —
    /// if the caller/callee lists haven't changed since, the response omits
    /// `callers`/`callees` and sets `edges_not_modified: true`. Every other
    /// field (`risk_assessment`, `dead_code_confidence`, `blast_radius`,
    /// `trend`, `co_changed_files`, `range_checksum`) is always recomputed
    /// and returned in full regardless — never gated behind this etag, since
    /// this is the mandatory pre-edit safety tool and none of those may go
    /// silently stale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) if_none_match: Option<String>,
    /// P0-3 (audit follow-up, 2026-08-23): opt-in full replacement text for
    /// the reviewed range (the symbol's own `[line_start, line_end]` in
    /// symbol mode, `[line, end_line]` in range mode) -- when set, the
    /// same `compute_touch_risk` signature-change detection the real
    /// `edit_lines`/`edit_symbol` write gate uses at spend time also runs
    /// here at mint time, against this exact text, instead of the
    /// placeholder empty hunk. Without this, `edit_context` has no
    /// proposed content yet and can only ever mint an authority with
    /// `signature_changed=false` -- correct as a conservative default, but
    /// a dead end for a genuine signature edit: the minted RiskVector
    /// under-claims risk relative to what `edit_lines_impl_gated`
    /// independently recomputes from the real diff at spend time, so
    /// `AUTHORITY_STALE_RISK_VECTOR` fires deterministically for every
    /// signature change that goes through the authority flow, with no
    /// successful path (see the audit finding this closes). Assumes a
    /// WHOLE-RANGE replace, matching `edit_symbol`'s default `"replace"`
    /// position -- not meaningful for a narrower `edit_lines` hunk within
    /// the range, which should omit this and rely on the conservative
    /// default instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proposed_new_text: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BlastRadiusInfo {
    pub(crate) transitive: i64,
    pub(crate) files_affected: Vec<String>,
    /// Wave 9 (audit follow-up, finding #5): whether the transitive BFS hit
    /// `callers.max_depth_cap`/`callers.transitive_timeout_ms` before
    /// finishing -- `transitive`/`files_affected` are then a LOWER bound,
    /// not the true blast radius, same "truncated, don't trust as exact"
    /// meaning `callers_truncated`/`callees_truncated` already carry on
    /// this same output. Previously computed (`transitive_bfs`'s second
    /// return value) but silently discarded as `_capped`. `Some(true)`
    /// when capped, absent otherwise -- never `Some(false)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) capped: Option<bool>,
}

/// How much `caller_count`/`coreness`/`is_hub` moved since the oldest snapshot
/// still at least `EDIT_CONTEXT_TREND_LOOKBACK_DAYS` old — see
/// `calm_core::fitness::compute_trend`.

#[derive(Serialize, JsonSchema)]
pub(crate) struct TrendOutput {
    pub(crate) compared_to: String,
    pub(crate) caller_count_delta: i64,
    pub(crate) coreness_delta: i64,
    pub(crate) is_hub_changed: bool,
}

impl From<calm_core::fitness::TrendInfo> for TrendOutput {
    fn from(t: calm_core::fitness::TrendInfo) -> Self {
        Self {
            compared_to: t.compared_to,
            caller_count_delta: t.caller_count_delta,
            coreness_delta: t.coreness_delta,
            is_hub_changed: t.is_hub_changed,
        }
    }
}

/// A file with no import/call relationship to the symbol's file, but that
/// historically changed alongside it in the same commit — a coupling signal
/// the static graph cannot see. See `calm_core::analysis::cochange`.

#[derive(Serialize, JsonSchema)]
pub(crate) struct CoChangedFileOutput {
    pub(crate) path: String,
    pub(crate) co_change_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_co_changed: Option<String>,
}

impl From<calm_core::analysis::cochange::CoChangeEntry> for CoChangedFileOutput {
    fn from(e: calm_core::analysis::cochange::CoChangeEntry) -> Self {
        Self {
            path: e.path,
            co_change_count: e.co_change_count,
            last_co_changed: e.last_co_changed,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditContextOutput {
    pub(crate) symbol: String,
    pub(crate) edges_ready: bool,
    pub(crate) index_freshness: String,
    pub(crate) callers: Vec<CallerEntry>,
    pub(crate) callees: Vec<CalleeEntry>,
    /// `true` when `callers` was cut down to `config.callers.direct_list_cap`
    /// entries (a real hub symbol can have 50-200+) — `blast_radius`'s own
    /// counts are unaffected, only this raw per-entry dump is capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) callers_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) callees_truncated: Option<bool>,
    pub(crate) blast_radius: BlastRadiusInfo,
    /// Hash of the symbol's current `[line_start, line_end]` — pass this
    /// straight to `edit_lines`/`edit_symbol` as `expected_hash` to skip
    /// the "learn the hash" preview round trip. Absent if the file
    /// couldn't be read from disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) range_checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk_assessment: Option<RiskAssessmentOutput>,
    /// Exactly what the `edit_lines`/`edit_symbol` write gate will do for
    /// this range (FIX2/F2b) — unlike `risk_assessment` above (advisory),
    /// this is what actually determines a write block. See
    /// `GatePredictionOutput`'s own doc comment.
    pub(crate) gate_prediction: GatePredictionOutput,
    /// `"none"` (confirmed not dead — entry point, test, or has confirmed
    /// callers), `"high"`/`"medium"`/`"low"` confidence it genuinely is dead
    /// code — see `calm_core::analysis::dead_code::compute_dead_code_confidence`.
    /// Also feeds `risk_assessment`: a 0-caller symbol only keeps "low" risk
    /// when this independently agrees (`"high"`/`"medium"`).
    pub(crate) dead_code_confidence: String,
    /// `"static"` or `"static+coverage"` — whether a runtime coverage file
    /// (see `scripts/gen-coverage.sh`) was available to inform `dead_code_confidence`.
    pub(crate) dead_code_source: String,
    /// Absent when there's no snapshot yet at least `EDIT_CONTEXT_TREND_LOOKBACK_DAYS`
    /// old (e.g. `calm fitness-check` hasn't run for that long) — not an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) trend: Option<TrendOutput>,
    /// Empty when git is unavailable or no file co-changed with this
    /// symbol's file often enough to clear `config.cochange.min_co_changes`
    /// — not an error signal, most edits legitimately have none.
    pub(crate) co_changed_files: Vec<CoChangedFileOutput>,
    /// Notes saved via `remember` that reference this symbol's file —
    /// surfaced automatically so a known gotcha isn't missed just because
    /// nobody thought to call `recall` first. Empty is the common case, not
    /// an error signal. See `CalmServer::related_notes` for the specificity/
    /// fail-open/content-safety rules governing what qualifies.
    pub(crate) related_notes: Vec<RelatedNoteOutput>,
    /// Fingerprint of `callers`+`callees` only (see `if_none_match` on
    /// `EditContextParams`) — every other field above is always fresh on
    /// every call, never gated behind this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edges_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edges_not_modified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
    /// CCK-10 (#65): `ChangeIntent::intent_id` for the authority minted by
    /// this call, if minting succeeded (fail-open — absent on any minting
    /// error, never blocks the mandatory pre-edit fields above). Pass
    /// alongside `authority_id` to `edit_lines`/`edit_symbol` to use the
    /// authority-validated write path instead of `confirm`/`reason`/`cites`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) change_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) authority_id: Option<String>,
    /// Unix seconds — the minted authority is refused (single-use, TTL-
    /// bound) after this even if every bound field still matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) authority_expires_at: Option<f64>,
}

// ---------------------------------------------------------------------------
// Tool 11: session_context
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct DiffImpactParams {
    /// A raw unified diff (`git diff` output) to analyze directly, instead
    /// of having this tool run git itself. At most one of `diff`, `staged`,
    /// `commits` may be set — omitting all three analyzes the unstaged
    /// working-tree diff (plain `git diff`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) diff: Option<String>,
    /// `true` to analyze the staged diff (`git diff --cached`); `false` or
    /// omitted analyzes the unstaged working-tree diff. At most one of
    /// `diff`, `staged`, `commits` may be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) staged: Option<bool>,
    /// A commit range/rev understood by `git diff`, e.g. `HEAD~3..HEAD` or
    /// a single commit SHA. At most one of `diff`, `staged`, `commits`
    /// may be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) commits: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct BlastRadiusOutput {
    pub(crate) direct_callers: i64,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct RiskAssessmentOutput {
    pub(crate) level: String,
    pub(crate) reasons: Vec<String>,
}

/// Predicts exactly what the `edit_lines`/`edit_symbol` write gate will do
/// for this touched range — UPGRADE_PLAN.md FIX2/F2b. Built from the same
/// `compute_touch_risk` + `classify_gate` the real gate uses
/// (`edit_lines_impl_gated`), over the same `[c.line_start, c.line_end]`
/// range, so it can never drift from the gate's actual behavior. Distinct
/// from `risk_assessment` above: that field is an advisory review-risk
/// signal (entropy, dead-code confidence); THIS is what determines a write
/// block.
#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct GatePredictionOutput {
    /// `true` iff a write to this range with `confirm: false` would be
    /// rejected. Independent of the two runtime session-state checks
    /// (`edit_context` freshness, a grounded `reason`) a `confirm: true`
    /// attempt would still have to clear when `requires` is
    /// `"edit_context+confirm+grounded_reason"`.
    #[schemars(description = "true iff a write here with confirm: false would be rejected.")]
    pub(crate) will_block: bool,
    /// This exact symbol's own `is_hub` — distinct from `will_block`, which
    /// also accounts for the wider touched range (e.g. an enclosing hub
    /// class this symbol sits inside — see `blocking_symbols`).
    #[schemars(
        description = "This exact symbol's own is_hub (distinct from will_block, which also covers the wider touched range)."
    )]
    pub(crate) is_hub: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hub_kind: Option<String>,
    /// Touched symbols (qualified names) that are themselves a hub — names
    /// the enclosing class here when THAT, not this symbol, is what the
    /// gate would actually block on.
    #[schemars(
        description = "Touched symbols (qualified names) that are themselves a hub, when different from this one."
    )]
    pub(crate) blocking_symbols: Vec<String>,
    /// `"none"` | `"confirm"` | `"edit_context+confirm+grounded_reason"`.
    pub(crate) requires: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub(crate) struct AffectedSymbolOutput {
    pub(crate) qualified_name: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) signature_changed: bool,
    /// True when this symbol didn't exist before the diff (new file, or a
    /// pure-addition hunk covering its signature) — it has zero prior call
    /// sites by definition, so `signature_changed` is always false for it
    /// and risk is not escalated on "callers may need update" grounds.
    pub(crate) symbol_is_new: bool,
    pub(crate) blast_radius: BlastRadiusOutput,
    pub(crate) risk_assessment: RiskAssessmentOutput,
}

impl calm_core::analysis::diff_impact::AffectedSymbolFacts for AffectedSymbolOutput {
    fn risk_level(&self) -> &str {
        &self.risk_assessment.level
    }
    fn direct_callers(&self) -> i64 {
        self.blast_radius.direct_callers
    }
    fn signature_changed(&self) -> bool {
        self.signature_changed
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct UnindexedFileOutput {
    pub(crate) path: String,
    /// "pending_scan" — a recognized source file, in a path the indexer
    /// actually walks, that just hasn't been scanned yet; resolves itself
    /// once indexing catches up (check `indexing_status`).
    /// "out_of_scope" — will stay unindexed no matter how long you wait:
    /// either not a source extension the indexer parses at all (docs,
    /// config, etc.), or it sits under a dotdir/`IGNORE_DIRS` path (e.g.
    /// `.claude/`, `target/`) the walker categorically never descends into,
    /// regardless of extension.
    /// "recognized_unparsed" — a `file_index` row exists (the indexer has
    /// scanned it) but `language` is NULL: a recognized-but-unsupported
    /// extension (see `is_recognized_unparsed_extension`) tracked by path
    /// only. Like "out_of_scope", this never resolves on its own — there is
    /// no symbol extraction to wait for — but unlike "out_of_scope" the file
    /// genuinely is indexed (has a row), just never for symbols.
    /// "deleted" — the file appears in the diff but no longer exists on
    /// disk (deleted or renamed away); like "out_of_scope" this is a
    /// permanent state — it will never be scanned no matter how long you
    /// wait — so it must never gate `aggregate_risk` on `indexing_status`.
    pub(crate) reason: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DiffImpactOutput {
    pub(crate) files_changed: Vec<String>,
    pub(crate) affected_symbols: Vec<AffectedSymbolOutput>,
    pub(crate) unindexed_files: Vec<UnindexedFileOutput>,
    pub(crate) aggregate_risk: String,
    pub(crate) suggested_reviewers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

/// Authority minted for a single-symbol `edit_context` review — CCK-10
/// (#65, docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
/// `change_id` is `ChangeIntent::intent_id`; pass both `change_id` and
/// `authority_id` to `edit_lines`/`edit_symbol` to use the new
/// authority-validated write path instead of `confirm`/`reason`/`cites`.
pub(crate) struct MintedAuthorityOutput {
    pub(crate) change_id: String,
    pub(crate) authority_id: String,
    pub(crate) authority_expires_at: f64,
}

impl CalmServer {
    /// `edit_context`'s compat-wrapper half (CCK-10): synthesizes a
    /// single-symbol `ChangeIntent`, captures an `EvidenceSnapshot`,
    /// computes the current `.calm/policy.toml` digest, and mints a
    /// `ReviewAuthority` binding all of it plus the caller-set digest and
    /// graph_generation `edit_context` already computed. Deliberately
    /// fail-open (`None` on any error, never a `Result` the caller must
    /// handle) — `edit_context` is the MANDATORY pre-edit tool and must
    /// keep returning its existing fields even if minting hits an infra
    /// hiccup (a locked state.db, an unreadable control.key); the OLD
    /// confirm/reason/cites path stays fully usable either way, since
    /// `edit_lines_impl_gated` only takes the new authority path when a
    /// caller explicitly supplies both `change_id` and `authority_id`.
    // 9 params: each is an independently meaningful input, same rationale
    // as compute_touch_risk's own #[allow] just above its definition.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn mint_review_authority_for_edit_context(
        &self,
        conn: &rusqlite::Connection,
        c: &CandidateRow,
        caller_set_digest: &str,
        graph_generation: i64,
        gate_touched: &[edit::TouchedSymbolOutput],
        // Wave 10 (item 1): the real value, straight from gate_prediction's
        // own compute_touch_risk call (a placeholder full-range hunk over
        // [c.line_start, c.line_end] already makes this computable there --
        // see TouchRiskResult's doc comment). Was hardcoded `false` here.
        touches_uncovered_code: bool,
        // Wave 5 (audit follow-up, 2026-08-23): same pattern as
        // touches_uncovered_code above -- gate_prediction's compute_touch_risk
        // call already computes this (position 4 of TouchRiskResult), it was
        // just being discarded before reaching this function.
        uncertain_zero_caller: bool,
        // Wave 5: ALWAYS `false` at this call site by construction --
        // edit_context has no real proposed edit content yet (its
        // compute_touch_risk call passes real_hunks=false), so there is no
        // future signature to compare against. This is genuinely
        // structural, unlike uncertain_zero_caller/touches_uncovered_code
        // above (both purely properties of the symbol today, not of a
        // not-yet-written edit) -- kept as an explicit parameter rather
        // than a bare `false` in the RiskVector literal below so the two
        // axes aren't silently conflated by a future reader.
        signature_changed: bool,
    ) -> Option<MintedAuthorityOutput> {
        // WS2b (audit follow-up, gap #1): opened before `compute` so a
        // past full reconciliation recorded via
        // `EvidenceSnapshot::compute_after_reconciliation` (see
        // `WatchSupervisor::refresh`) can upgrade this call's freshness --
        // otherwise a Human-tier target could never clear the bar below,
        // since drift-derived `compute` alone never yields `Reconciled`.
        let mut state_conn = calm_core::db::conn::open_state_writer(&self.state_db_path).ok()?;
        let snapshot = calm_core::authority::EvidenceSnapshot::compute_with_recorded_freshness(
            conn,
            &self.project_root,
            &state_conn,
        )
        .ok()?;

        let policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
        let policy_digest = policy.digest();
        let principal = format!("session:{}", self.session_id);
        let authority_ttl = calm_core::authority::AuthorityTtl::from_secs(1800.0)
            .expect("30 minutes is within AuthorityTtl's valid range");

        // CCK-26 (audit follow-up): a real (if minimal -- single candidate,
        // no manifest/kind-mismatch signal available at edit_context time)
        // RiskVector, so this auto-minted authority carries an honest
        // required_approver_class rather than always claiming SelfReviewed.
        // Not itself a gate here (edit_context has no approval channel and
        // is deliberately fail-open) -- CCK-23's spend-time check is what
        // actually enforces HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW regardless
        // of what this authority claims. Computed BEFORE the freshness gate
        // below (WS2, audit follow-up) -- that gate now needs
        // `required_approver_class` to know which freshness bar applies.
        // Wave 8 (audit follow-up, P0-E): real hub_kind, straight from
        // `symbols` -- mirrors review_change's own CCK-26/WS1 fix
        // (change.rs), which this auto-mint path never received. Left as
        // `hub_kind: None` unconditionally, this authority's risk_vector
        // digest could never match the real spend-time digest
        // (edit_lines_impl_gated computes hub_kind for real via
        // compute_touch_risk) for ANY actual hub symbol -- so every
        // authority minted here for a hub target was unspendable via the
        // authority path, deterministically, not as a race: spend always
        // failed closed with AUTHORITY_STALE_RISK_VECTOR, forcing every
        // hub edit back onto the legacy confirm/reason/cites gate
        // regardless of whether the caller had a valid authority in hand.
        let hub_kind: Option<String> = if c.is_hub {
            conn.query_row(
                "SELECT hub_kind FROM symbols WHERE qualified_name = ?1",
                rusqlite::params![c.qualified_name],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
        } else {
            None
        };
        let risk_vector = calm_core::policy::RiskVector {
            caller_count_level: calm_core::policy::RiskLevel::parse(
                super::detail::risk_level_from_caller_count(c.caller_count),
            )
            .unwrap_or(calm_core::policy::RiskLevel::Low),
            is_hub: c.is_hub,
            hub_kind,
            signature_changed,
            uncertain_zero_caller,
            risk_rule_floor: calm_core::config::risk_floor_for_path(
                &self.config().risk_rules,
                &c.path,
            )
            .and_then(|(level, _glob)| calm_core::policy::RiskLevel::parse(level)),
            kind_mismatch: false,
            touches_manifest: calm_core::change::classify::is_manifest_path(&c.path),
            touches_uncovered_code,
        };
        let policy_decision = calm_core::policy::evaluate(&risk_vector, &policy);
        let policy_decision_digest = policy_decision.digest();

        // CCK-23/WS2 (audit follow-up): mirror the same tiered freshness
        // gate review_change now applies -- see FreshnessClass::meets_bar_for's
        // own doc comment. Fail-open here means `None` (no authority),
        // matching this function's existing fail-open contract on every
        // other error, not a hard tool error.
        if !snapshot
            .freshness_class
            .meets_bar_for(policy_decision.required_approver_class)
        {
            return None;
        }
        // CCK-R5 (audit follow-up): snapshot persist + intent insert +
        // authority mint (which itself does 3 more inserts) must land as
        // one atomic unit -- a partial write here (e.g. the intent row
        // persisted, then a crash before the authority row exists) would
        // leave an orphaned change_intents row with no authority ever
        // bound to it. Every `?` below rolls the whole transaction back on
        // drop (Transaction's own Drop impl) if we never reach `commit`.
        let tx = state_conn.transaction().ok()?;

        snapshot.persist(&tx).ok()?;

        // Wave 9 (audit follow-up, finding #4 -- nested-symbol
        // WRONG_TARGET_SCOPE): bind EVERY symbol this range touches, not
        // just the resolved candidate `c` -- `gate_touched` is the exact
        // same compute_touch_risk scan gate_prediction already ran over
        // [c.line_start, c.line_end], so it already includes any enclosing
        // class/struct whose own indexed range overlaps this one. A real
        // edit_lines/edit_symbol spend against this same range
        // independently recomputes the identical touched-symbol set
        // (edit_lines_impl_gated's own compute_touch_risk call) -- binding
        // only `c` here meant a spend that also (unavoidably) touched the
        // enclosing symbol always failed WRONG_TARGET_SCOPE, even though
        // nothing outside what edit_context reviewed was ever touched.
        let mut seen_qns = std::collections::HashSet::new();
        let mut targets: Vec<calm_core::change::ChangeIntentTarget> = gate_touched
            .iter()
            .filter(|t| seen_qns.insert(t.qualified_name.clone()))
            .map(|t| calm_core::change::ChangeIntentTarget {
                path: c.path.clone(),
                qualified_name: Some(t.qualified_name.clone()),
            })
            .collect();
        if seen_qns.insert(c.qualified_name.clone()) {
            targets.push(calm_core::change::ChangeIntentTarget {
                path: c.path.clone(),
                qualified_name: Some(c.qualified_name.clone()),
            });
        }
        let intent = calm_core::change::ChangeIntent::new(
            calm_core::change::ChangeIntentKind(calm_core::change::ChangeKind::Body),
            "edit_context compat wrapper: single-symbol review (CCK-10)",
            snapshot.snapshot_id.clone(),
            targets.clone(),
        );
        calm_core::change::insert_change_intent(&tx, &intent, None).ok()?;

        let authority = calm_core::authority::ReviewAuthority::mint(
            &tx,
            calm_core::authority::MintParams {
                intent_id: &intent.intent_id,
                snapshot_id: &snapshot.snapshot_id,
                graph_generation,
                caller_set_digest,
                policy_digest: &policy_digest,
                principal: &principal,
                ttl_secs: authority_ttl,
                targets: &targets,
                policy_decision_digest: &policy_decision_digest,
                risk_vector: &risk_vector,
                required_approver_class: policy_decision.required_approver_class,
            },
        )
        .ok()?;

        tx.commit().ok()?;

        Some(MintedAuthorityOutput {
            change_id: intent.intent_id,
            authority_id: authority.authority_id,
            authority_expires_at: authority.expires_at,
        })
    }

    /// Range mode for `edit_context`: review a raw `[line, end_line]`
    /// window with no symbol resolution -- mirrors `source`'s own range
    /// mode (`source_range`, inspect.rs). Wave 8 (audit follow-up, P0-A):
    /// gives a pure whitespace/comment/module-level/gap region -- exactly
    /// the content `edit_lines` touches when no indexed symbol's range
    /// covers the hunk -- a real review to point back to, on both the
    /// legacy confirm/reason gate (`record_path_context_review`) and the
    /// full ReviewAuthority mint+spend path
    /// (`mint_review_authority_for_edit_context_range`). Before this,
    /// neither existed for such a region and Strict mode had no success
    /// path at all for editing one.
    fn edit_context_range(
        &self,
        conn: &rusqlite::Connection,
        p: &EditContextParams,
    ) -> ResolvedOutcome<EditContextOutput> {
        let path = match p.path.as_deref() {
            Some(pth) if !pth.is_empty() => pth,
            _ => {
                return ResolvedOutcome::error(error_detail(
                    "INVALID_PARAMS",
                    "range mode needs `path` (plus `line` and `end_line`) when `symbol` is omitted",
                    false,
                ));
            }
        };
        let (start, end) = match (p.line, p.end_line) {
            (Some(s), Some(e)) if s >= 1 && e >= s => (s, e),
            _ => {
                return ResolvedOutcome::error(error_detail(
                    "INVALID_PARAMS",
                    "range mode needs `line` (start) and `end_line` (end), 1-indexed with end >= start",
                    false,
                ));
            }
        };
        self.track_file(path);
        let full_path = match edit::resolve_repo_path(&self.project_root, path) {
            Ok(fp) => fp,
            Err(e) => return ResolvedOutcome::error(e),
        };
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => {
                return ResolvedOutcome::error(error_detail(
                    "FILE_NOT_READABLE",
                    &format!("could not read {path}"),
                    false,
                ));
            }
        };
        let range_checksum =
            calm_core::edit::range_checksum(&content, start as usize, end as usize);

        let config = self.config();
        let policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
        // P0-3 (audit follow-up, 2026-08-23): mirrors edit_context's own
        // symbol-mode fix -- without `p.proposed_new_text`, this stays
        // the placeholder empty hunk with real_hunks=false (unchanged
        // behavior, range_signature_touch always None). When supplied,
        // it's a real whole-range-replace hunk with real_hunks=true, so
        // signature-change detection runs for real here too.
        let range_hunks: Vec<(i64, i64, &str)> =
            vec![(start, end, p.proposed_new_text.as_deref().unwrap_or(""))];
        let range_real_hunks = p.proposed_new_text.is_some();
        let (
            risk,
            hub_hit,
            hub_kind,
            uncertain_zero_caller,
            touched,
            risk_rule_reason,
            range_touches_uncovered_code,
            range_signature_touch,
        ) = edit::compute_touch_risk(
            conn,
            &self.project_root,
            path,
            &[(start, end)],
            &self.coverage.read_ok(),
            &config.risk_rules,
            &range_hunks,
            &policy,
            range_real_hunks,
        );

        let bridge_downgrade_eligible = hub_kind.as_deref() == Some("bridge")
            && risk.as_deref() != Some("high")
            && uncertain_zero_caller.is_none()
            && edit::all_caller_edges_confident(
                conn,
                &touched
                    .iter()
                    .filter(|t| t.hub_kind.is_some())
                    .map(|t| t.qualified_name.clone())
                    .collect::<Vec<_>>(),
            );
        let classification = edit::classify_gate(
            hub_hit,
            risk.as_deref(),
            uncertain_zero_caller,
            bridge_downgrade_eligible,
            config.edit.always_require_edit_context_effective(),
            risk_rule_reason.as_deref(),
        );
        let blocking_symbols: Vec<String> = touched
            .iter()
            .filter(|t| t.is_hub)
            .map(|t| t.qualified_name.clone())
            .collect();
        let gate_prediction = GatePredictionOutput {
            will_block: classification.will_block_without_confirm,
            is_hub: hub_hit,
            hub_kind: hub_kind.clone(),
            blocking_symbols,
            requires: classification.requirement.as_str().to_string(),
            reason: classification.why,
        };

        // Union of every touched symbol's live callers, same shape as
        // edit_lines_impl_gated's own union_caller_set_digest (edit.rs) --
        // empty when the range genuinely touches no indexed symbol, the
        // common case this fix exists for. Kept identical to the
        // spend-time computation (same query, same digest function) so a
        // range authority minted here matches what edit_lines_impl_gated
        // recomputes for the real hunks being written.
        let mut union_callers: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for t in &touched {
            union_callers.extend(edit::caller_symbol_set(conn, &t.qualified_name));
        }
        let caller_set_digest =
            Self::caller_set_digest(&union_callers.into_iter().collect::<Vec<_>>());

        let graph_generation: i64 = conn
            .query_row(
                "SELECT generation FROM graph_generation_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let minted = self.mint_review_authority_for_edit_context_range(
            conn,
            path,
            &touched,
            hub_hit,
            hub_kind.clone(),
            uncertain_zero_caller.is_some(),
            &caller_set_digest,
            graph_generation,
            range_touches_uncovered_code,
            range_signature_touch.is_some(),
        );
        // Structural half of the legacy confirm/reason gate
        // (edit_lines_impl_gated, edit.rs): record that this path's
        // symbol-less region was reviewed this session, mirroring
        // record_edit_context_review's per-symbol bookkeeping -- see
        // record_path_context_review's own doc comment (common.rs) for
        // why a separate path-keyed store, not a repurposed
        // qualified_name key.
        self.record_path_context_review(
            path,
            risk.as_deref().unwrap_or("unknown"),
            graph_generation,
        );

        let (change_id, authority_id, authority_expires_at) = match minted {
            Some(m) => (
                Some(m.change_id),
                Some(m.authority_id),
                Some(m.authority_expires_at),
            ),
            None => (None, None, None),
        };

        ResolvedOutcome::success(EditContextOutput {
            symbol: String::new(),
            edges_ready: self.edges_ready(),
            index_freshness: self.phase_str(),
            callers: Vec::new(),
            callees: Vec::new(),
            callers_truncated: None,
            callees_truncated: None,
            blast_radius: BlastRadiusInfo {
                transitive: 0,
                files_affected: Vec::new(),
                capped: None,
            },
            range_checksum,
            risk_assessment: risk.map(|level| RiskAssessmentOutput {
                level,
                reasons: Vec::new(),
            }),
            gate_prediction,
            // "Dead code" isn't a meaningful concept for a symbol-less
            // range (no single declaration to judge) -- these two fields
            // are placeholders, not a real analysis, unlike symbol mode.
            dead_code_confidence: "none".to_string(),
            dead_code_source: "static".to_string(),
            trend: None,
            co_changed_files: Vec::new(),
            related_notes: Vec::new(),
            edges_etag: None,
            edges_not_modified: None,
            suggested_next: self.filter_sn(suggested_gated(
                "diff_impact",
                "MANDATORY after changes — verify blast radius",
            )),
            change_id,
            authority_id,
            authority_expires_at,
        })
    }

    /// Range-mode analog of `mint_review_authority_for_edit_context` --
    /// mints a `ReviewAuthority` for a symbol-less `[line, end_line]`
    /// window instead of a resolved symbol candidate. Wave 8 (audit
    /// follow-up, P0-A): `target_scope_digest` (calm-core) already
    /// canonicalizes a `ChangeIntentTarget` with `qualified_name: None` as
    /// `(path, "")` -- no calm_core changes needed, this reuses that
    /// exact encoding. Any symbol the range DOES happen to overlap
    /// (`touched`, non-empty when the caller's range wasn't actually
    /// symbol-less) is bound as its own additional target too, the same
    /// way a real per-symbol review would -- this generalizes rather than
    /// assumes `touched` is empty. Deliberately fail-open (`None` on any
    /// error), matching the symbol-mode function's own contract.
    #[allow(clippy::too_many_arguments)]
    fn mint_review_authority_for_edit_context_range(
        &self,
        conn: &rusqlite::Connection,
        path: &str,
        touched: &[edit::TouchedSymbolOutput],
        hub_hit: bool,
        hub_kind: Option<String>,
        uncertain_zero_caller: bool,
        caller_set_digest: &str,
        graph_generation: i64,
        // Wave 10 (item 1): real value from edit_context_range's own
        // compute_touch_risk call (placeholder full-range hunk) -- was
        // hardcoded `false` here, same fix as the single-symbol mint path.
        touches_uncovered_code: bool,
        // Wave 5 (audit follow-up, 2026-08-23): same pattern -- always
        // false at this call site (no real edit content yet), kept as an
        // explicit param rather than a bare literal so it isn't silently
        // conflated with uncertain_zero_caller/touches_uncovered_code
        // above (which ARE properties of the symbol today, not of a
        // not-yet-written edit).
        signature_changed: bool,
    ) -> Option<MintedAuthorityOutput> {
        let mut state_conn = calm_core::db::conn::open_state_writer(&self.state_db_path).ok()?;
        let snapshot = calm_core::authority::EvidenceSnapshot::compute_with_recorded_freshness(
            conn,
            &self.project_root,
            &state_conn,
        )
        .ok()?;

        let policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
        let policy_digest = policy.digest();
        let principal = format!("session:{}", self.session_id);
        let authority_ttl = calm_core::authority::AuthorityTtl::from_secs(1800.0)
            .expect("30 minutes is within AuthorityTtl's valid range");

        let caller_count_level = touched
            .iter()
            .filter_map(|t| {
                calm_core::policy::RiskLevel::parse(super::detail::risk_level_from_caller_count(
                    t.caller_count,
                ))
            })
            .max()
            .unwrap_or(calm_core::policy::RiskLevel::Low);
        let risk_vector = calm_core::policy::RiskVector {
            caller_count_level,
            is_hub: hub_hit,
            hub_kind,
            signature_changed,
            uncertain_zero_caller,
            risk_rule_floor: calm_core::config::risk_floor_for_path(
                &self.config().risk_rules,
                path,
            )
            .and_then(|(level, _glob)| calm_core::policy::RiskLevel::parse(level)),
            kind_mismatch: false,
            touches_manifest: calm_core::change::classify::is_manifest_path(path),
            touches_uncovered_code,
        };
        let policy_decision = calm_core::policy::evaluate(&risk_vector, &policy);
        let policy_decision_digest = policy_decision.digest();

        if !snapshot
            .freshness_class
            .meets_bar_for(policy_decision.required_approver_class)
        {
            return None;
        }
        let tx = state_conn.transaction().ok()?;

        snapshot.persist(&tx).ok()?;

        // Every symbol the range actually overlaps, PLUS a path-only
        // target (qualified_name: None) covering the symbol-less portion
        // -- target_scope_digest treats the latter as (path, ""), and
        // edit_lines_impl_gated's current_targets construction pushes the
        // same fallback when a hunk touches nothing indexed, so the two
        // sides always have a matching target to check against.
        let mut targets: Vec<calm_core::change::ChangeIntentTarget> = touched
            .iter()
            .map(|t| calm_core::change::ChangeIntentTarget {
                path: path.to_string(),
                qualified_name: Some(t.qualified_name.clone()),
            })
            .collect();
        targets.push(calm_core::change::ChangeIntentTarget {
            path: path.to_string(),
            qualified_name: None,
        });

        let intent = calm_core::change::ChangeIntent::new(
            calm_core::change::ChangeIntentKind(calm_core::change::ChangeKind::Body),
            "edit_context compat wrapper: range review, no single symbol (Wave 8 P0-A)",
            snapshot.snapshot_id.clone(),
            targets.clone(),
        );
        calm_core::change::insert_change_intent(&tx, &intent, None).ok()?;

        let authority = calm_core::authority::ReviewAuthority::mint(
            &tx,
            calm_core::authority::MintParams {
                intent_id: &intent.intent_id,
                snapshot_id: &snapshot.snapshot_id,
                graph_generation,
                caller_set_digest,
                policy_digest: &policy_digest,
                principal: &principal,
                ttl_secs: authority_ttl,
                targets: &targets,
                policy_decision_digest: &policy_decision_digest,
                risk_vector: &risk_vector,
                required_approver_class: policy_decision.required_approver_class,
            },
        )
        .ok()?;

        tx.commit().ok()?;

        Some(MintedAuthorityOutput {
            change_id: intent.intent_id,
            authority_id: authority.authority_id,
            authority_expires_at: authority.expires_at,
        })
    }
}

// ---------------------------------------------------------------------------
// Tool 13: indexing_status
// ---------------------------------------------------------------------------
