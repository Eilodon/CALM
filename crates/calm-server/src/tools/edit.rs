use super::common::*;
use super::*;

/// Serializes background embedding jobs spawned after an edit's reindex
/// commits (Plan 3 §3.1 Phase C) — a second `edit_lines`/`edit_symbol` call
/// on the same or a different file, arriving while a prior edit's
/// background embed thread is still running, would otherwise open a
/// second concurrent writer connection racing the first's `embed_pending`/
/// `embed_pending_chunks` passes. Unconditional rather than relying on
/// `embed_pending*` being provably idempotent under concurrent callers —
/// cheaper to serialize outright than to bet on that assumption holding as
/// Phase B raises how often the same file gets reindexed in quick
/// succession. Guards `()` only — poison-tolerant via `LockExt`.
static EMBED_BG: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[rmcp::tool_router(router = "edit_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "edit_lines",
        description = "The only write-capable tool in ci — line-range granularity, works on ANY tracked file (source code, Cargo.toml, docs — not just parsed symbols). NOT FOR: symbol-scoped edits with auto-resolved range (use edit_symbol). Requires expected_hash from a prior call's current_hash (or edit_context's range_checksum for a whole symbol); omit it to preview a range's hash/content without writing anything. All hunks in one call apply to the same file and must be disjoint (non-overlapping).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) fn edit_lines(
        &self,
        Parameters(p): Parameters<EditLinesParams>,
    ) -> Json<ToolOutcome<EditLinesOutput>> {
        Json(self.timed_tool("edit_lines", || {
            let hunks: Vec<calm_core::edit::HunkRequest> = p
                .edits
                .into_iter()
                .map(|h| calm_core::edit::HunkRequest {
                    start_line: h.start_line.max(0) as usize,
                    end_line: h.end_line.max(0) as usize,
                    expected_hash: h.expected_hash,
                    new_text: h.new_text,
                })
                .collect();
            self.edit_lines_impl(&p.path, hunks, p.confirm, p.reason.as_deref(), false)
        }))
    }

    #[tool(
        name = "edit_symbol",
        description = "Sugar over edit_lines: resolves symbol (+ optional path/line, same disambiguation contract as edit_context). Default position=\"replace\" swaps the symbol's whole [line_start, line_end] for new_text in one hunk (needs expected_hash). position=\"before\"/\"after\"/\"append_inside\" instead INSERTS new_text relative to the symbol, anchored on a fresh parse of the file on disk — no line numbers, no expected_hash, no preview round trip, immune to stale line offsets (append_inside = end of a class/function body; after = new sibling below it, e.g. a new test after the last existing test). USE WHEN: replacing an entire function/class/method body by name, or inserting new code relative to one. NOT FOR: editing a single line inside a symbol, or anything outside a parsed symbol (an import line, Cargo.toml) — use edit_lines directly for those.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) fn edit_symbol(
        &self,
        Parameters(p): Parameters<EditSymbolParams>,
    ) -> Json<ResolvedOutcome<EditLinesOutput>> {
        Json(self.timed_tool("edit_symbol", || {
            if matches!(
                p.position.as_deref(),
                Some("top_of_file") | Some("end_of_file")
            ) {
                // No symbol resolution at all for these two modes -- pure
                // file-relative anchors for brand-new module-level content
                // (a new `use`, a new top-level function) with no existing
                // sibling symbol to anchor on.
                let path = match p.path.as_deref() {
                    Some(p) => p,
                    None => {
                        return ResolvedOutcome::error(error_detail(
                            "PATH_REQUIRED",
                            "position=\"top_of_file\"/\"end_of_file\" needs `path` (no symbol \
                             is resolved for these modes)",
                            false,
                        ));
                    }
                };
                let full_path = match resolve_repo_path(&self.project_root, path) {
                    Ok(p) => p,
                    Err(e) => return ResolvedOutcome::error(e),
                };
                let live = match std::fs::read_to_string(&full_path) {
                    Ok(s) => s,
                    Err(e) => {
                        return ResolvedOutcome::error(error_detail(
                            "READ_FAILED",
                            &format!("could not read {path}: {e}"),
                            false,
                        ));
                    }
                };
                let total_lines = live.lines().count().max(1);
                let (line_start, line_end, insert_pos) =
                    if p.position.as_deref() == Some("top_of_file") {
                        (1, 1, calm_core::edit::InsertPosition::Before)
                    } else {
                        (1, total_lines, calm_core::edit::InsertPosition::After)
                    };
                let hunk = match calm_core::edit::insertion_hunk(
                    &live,
                    line_start,
                    line_end,
                    insert_pos,
                    &p.new_text,
                ) {
                    Some(h) => h,
                    None => {
                        return ResolvedOutcome::error(error_detail(
                            "INVALID_RANGE",
                            &format!("{path} appears to be empty or unreadable as text"),
                            false,
                        ));
                    }
                };
                return self
                    .edit_lines_impl(path, vec![hunk], p.confirm, p.reason.as_deref(), true)
                    .into_resolved();
            }
            let c = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error_resolved(e),
                };
                let resolution = match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
                    Ok(r) => r,
                    Err(e) => return db_error_resolved(e),
                };
                match resolution {
                    SymbolResolution::NotFound => return ResolvedOutcome::not_found(&p.symbol),
                    SymbolResolution::Ambiguous(candidates) => {
                        return ResolvedOutcome::ambiguous(&candidates);
                    }
                    SymbolResolution::Found(c) => *c,
                }
            };
            if c.boundary_ambiguous {
                return ResolvedOutcome::error(error_detail(
                    "BOUNDARY_AMBIGUOUS",
                    &format!(
                        "'{}' shares a physical source line with an adjacent symbol in {} \
                         (see fitness_report's boundary_ambiguous_count) — a line-range replace \
                         here could silently delete part of the neighboring symbol. Fix the \
                         shared line by hand first (insert the missing newline), then retry.",
                        p.symbol, c.path
                    ),
                    true,
                ));
            }
            // Insertion modes re-anchor via a fresh live parse (see
            // insertion_hunk_for), not raw hash matching, so the generic
            // "content also appears elsewhere" ambiguity warning
            // edit_lines_impl attaches for line-range hunks doesn't apply
            // to them — see edit_lines_impl's position_anchored parameter.
            let position_anchored = matches!(
                p.position.as_deref(),
                Some("before" | "after" | "append_inside")
            );
            let hunk = match p.position.as_deref().unwrap_or("replace") {
                "replace" => match &p.old_text {
                    None => calm_core::edit::HunkRequest {
                        start_line: c.line_start as usize,
                        end_line: c.line_end as usize,
                        expected_hash: p.expected_hash,
                        new_text: p.new_text,
                    },
                    Some(old_text) => {
                        let full_path = match resolve_repo_path(&self.project_root, &c.path) {
                            Ok(p) => p,
                            Err(e) => return ResolvedOutcome::error(e),
                        };
                        let live = match std::fs::read_to_string(&full_path) {
                            Ok(s) => s,
                            Err(e) => {
                                return ResolvedOutcome::error(error_detail(
                                    "READ_FAILED",
                                    &format!("could not read {}: {e}", c.path),
                                    false,
                                ));
                            }
                        };
                        match calm_core::edit::find_and_replace_hunk(
                            &live,
                            c.line_start as usize,
                            c.line_end as usize,
                            old_text,
                            &p.new_text,
                        ) {
                            Ok(h) => h,
                            Err(calm_core::edit::MatchOutcome::NotFound) => {
                                return ResolvedOutcome::error(error_detail(
                                    "MATCH_NOT_FOUND",
                                    &format!(
                                        "old_text {old_text:?} was not found within '{}' \
                                         ({}..{}) on disk",
                                        p.symbol, c.line_start, c.line_end
                                    ),
                                    true,
                                ));
                            }
                            Err(calm_core::edit::MatchOutcome::Ambiguous(lines)) => {
                                let where_str = lines
                                    .iter()
                                    .map(|l| format!("line {l}"))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                return ResolvedOutcome::error(error_detail(
                                    "AMBIGUOUS_MATCH",
                                    &format!(
                                        "old_text {old_text:?} occurs {} times within '{}' \
                                         ({where_str}) — narrow it with more surrounding \
                                         context so it matches exactly once",
                                        lines.len(),
                                        p.symbol
                                    ),
                                    true,
                                ));
                            }
                        }
                    }
                },
                pos @ ("before" | "after" | "append_inside") => {
                    let position = match pos {
                        "before" => calm_core::edit::InsertPosition::Before,
                        "after" => calm_core::edit::InsertPosition::After,
                        _ => calm_core::edit::InsertPosition::AppendInside,
                    };
                    match insertion_hunk_for(&self.project_root, &c, position, &p.new_text) {
                        Ok(h) => h,
                        Err(detail) => return ResolvedOutcome::error(detail),
                    }
                }
                other => {
                    return ResolvedOutcome::error(error_detail(
                        "INVALID_POSITION",
                        &format!(
                            "unknown position {other:?} — use \"replace\" (default), \
                             \"before\", \"after\", \"append_inside\", \"top_of_file\", or \
                             \"end_of_file\""
                        ),
                        false,
                    ));
                }
            };
            self.edit_lines_impl(
                &c.path,
                vec![hunk],
                p.confirm,
                p.reason.as_deref(),
                position_anchored,
            )
            .into_resolved()
        }))
    }

    /// Shared implementation for `edit_lines`/`edit_symbol`. Flow: apply
    /// hunks in-memory (all-or-nothing, see `calm_core::edit::apply_hunks`) →
    /// pre-write syntax validation → risk gate (query-only, against
    /// pre-edit symbol ranges) → atomic write → reindex (same
    /// `reindex_paths` (dirty-path only, Plan 3 §3.1 Phase A) + `embed_pending*` gate the file watcher uses, so
    /// the DB is never observably staler than a watcher-driven update) →
    /// post-edit symbol lookup for the response. Failures BEFORE the write
    /// are tool errors; failures AFTER it surface as a success with
    /// `index_stale: true` — the disk write already happened, and reporting
    /// it as an error made agents re-apply edits that had in fact landed.
    fn edit_lines_impl(
        &self,
        path: &str,
        hunks: Vec<calm_core::edit::HunkRequest>,
        confirm: bool,
        reason: Option<&str>,
        position_anchored: bool,
    ) -> ToolOutcome<EditLinesOutput> {
        // In-process guard: serializes the whole read -> hash-check -> write
        // -> reindex sequence within this one `calm serve` process. rmcp
        // dispatches tool calls concurrently, and locking only the write
        // phase left the read+hash-check racy (TOCTOU) -- two concurrent
        // calls could both read the pre-edit snapshot, both pass hash
        // validation, and the second writer's full-file replace would
        // silently discard the first writer's change even on disjoint line
        // ranges.
        let _guard = self.edit_lock.lock_ok();

        // Cross-process guard: a *different* `calm serve` process (another
        // IDE session on the same project) has its own independent
        // `edit_lock` Mutex above, so it isn't covered by it -- see
        // `calm_core::db::edit_lock`'s doc comment for the exact same TOCTOU,
        // still open across processes, this closes. Acquired after the cheap
        // in-process Mutex (so at most one thread per process ever contends
        // for it), with the same scope (held through the end of this
        // function) so the two guards never disagree about what's protected.
        // A failure here is treated as a hard error rather than silently
        // proceeding in-process-only: proceeding would just reintroduce the
        // cross-process race this guard exists to close.
        let calm_dir = self
            .db_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.project_root.clone());
        let _cross_guard = match calm_core::db::edit_lock::acquire(&calm_dir) {
            Ok(g) => g,
            Err(e) => {
                return ToolOutcome::error(error_detail(
                    "EDIT_LOCK_FAILED",
                    &format!(
                        "could not acquire cross-process edit lock in {}: {e}",
                        calm_dir.display()
                    ),
                    true,
                ));
            }
        };

        let full_path = match resolve_repo_path(&self.project_root, path) {
            Ok(p) => p,
            Err(e) => return ToolOutcome::error(e),
        };
        let original = match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(e) => {
                return ToolOutcome::error(error_detail(
                    "READ_FAILED",
                    &format!("could not read {path}: {e}"),
                    false,
                ));
            }
        };

        let outcome = match calm_core::edit::apply_hunks(&original, &hunks) {
            Ok(o) => o,
            Err(e) => {
                return ToolOutcome::error(error_detail("INVALID_HUNKS", &e.to_string(), false));
            }
        };

        let hunks_output: Vec<EditHunkResultOutput> = outcome
            .results
            .iter()
            .map(EditHunkResultOutput::from)
            .collect();

        // A hash proves WHAT is at a range, not WHERE the range is: when the
        // same content exists at other line windows of this file (a lone `}`
        // line has dozens of twins), a stale line number can still hash-match
        // and the edit lands at the wrong spot. Surface that on every
        // response that reports such a hunk — preview AND applied.
        let ambiguity_note = if position_anchored {
            None
        } else {
            let flagged: Vec<String> = outcome
                .results
                .iter()
                .filter(|r| r.content_occurrences > 1)
                .map(|r| {
                    format!(
                        "{}..{} ({} identical elsewhere)",
                        r.start_line,
                        r.end_line,
                        r.content_occurrences - 1
                    )
                })
                .collect();
            (!flagged.is_empty()).then(|| {
                format!(
                    "position warning — the content of range(s) {} also appears elsewhere in \
                     this file, so a hash match verifies content, not position; double-check \
                     the line numbers or anchor on structure with edit_symbol \
                     position=\"before\"/\"after\"/\"append_inside\"",
                    flagged.join(", ")
                )
            })
        };

        if !outcome.all_applied {
            let mut note = String::from(
                "nothing written — some hunk was a preview or had a stale hash; \
                 retry with the current_hash shown for each hunk",
            );
            if let Some(a) = &ambiguity_note {
                note.push_str(". ");
                note.push_str(a);
            }
            return ToolOutcome::success(EditLinesOutput {
                path: path.to_string(),
                applied: false,
                hunks: hunks_output,
                parse_status: None,
                touched_symbols: vec![],
                risk_assessment: None,
                index_stale: None,
                note: Some(note),
                suggested_next: None,
            });
        }
        let new_content = outcome.new_content.expect("all_applied implies Some");

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let parse_status = match calm_core::edit::validate_syntax(&new_content, ext) {
            Some(true) => "clean",
            Some(false) => {
                // Show the ORIGINAL boundary line(s) so a pre-existing
                // corrupted shared line (two symbols fused onto one
                // physical line by a missing trailing newline in an
                // earlier edit -- see apply_hunks' newline normalization)
                // is visible immediately instead of costing a multi-call
                // investigation. Purely factual (just echoes disk content),
                // no heuristic guess about fault.
                let orig_lines: Vec<&str> = original.lines().collect();
                let boundary_hint: Vec<String> = hunks
                    .iter()
                    .filter_map(|h| {
                        orig_lines
                            .get(h.end_line.saturating_sub(1))
                            .map(|line| format!("line {}: {line:?}", h.end_line))
                    })
                    .collect();
                let hint = if boundary_hint.is_empty() {
                    String::new()
                } else {
                    format!(
                        " — original boundary line(s) for reference: {}; if one visibly \
                         holds content from more than one symbol (e.g. a closing brace \
                         immediately followed by unrelated code with no newline between \
                         them), that line was already corrupted before this edit and needs \
                         a manual fix first",
                        boundary_hint.join(", ")
                    )
                };
                return ToolOutcome::error(error_detail(
                    "PARSE_ERROR",
                    &format!(
                        "this edit would introduce a syntax error in {path} — nothing written{hint}"
                    ),
                    true,
                ));
            }
            None => "skipped_unrecognized_language",
        };

        let (risk, hub_hit, hub_kind, bridge_downgrade_eligible, pre_touched) = {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let ranges: Vec<(i64, i64)> = hunks
                .iter()
                .map(|h| (h.start_line as i64, h.end_line as i64))
                .collect();
            let (risk, hub_hit, hub_kind, touched) = compute_touch_risk(&conn, path, &ranges);
            // Plan 3 §3.3 (F10): a bridge-only touch (never degree/both) at
            // risk ≤ medium MAY use the lighter CONFIRM_REQUIRED-only tier
            // below — but ONLY if every touched hub's caller edges are all
            // resolved/formal confidence (see all_caller_edges_confident's
            // doc comment for why textual/ambiguous callers disqualify it
            // regardless of hub_kind: the true blast radius can exceed the
            // counted caller_count).
            let eligible = hub_kind.as_deref() == Some("bridge")
                && risk.as_deref() != Some("high")
                && all_caller_edges_confident(
                    &conn,
                    &touched
                        .iter()
                        .filter(|t| t.hub_kind.is_some())
                        .map(|t| t.qualified_name.clone())
                        .collect::<Vec<_>>(),
                );
            (risk, hub_hit, hub_kind, eligible, touched)
        };
        if hub_hit || risk.as_deref() == Some("high") {
            let why = if hub_hit {
                "a hub symbol (is_hub=true)".to_string()
            } else {
                "a high-risk symbol (>10 callers)".to_string()
            };

            if bridge_downgrade_eligible {
                // Lighter tier: bridge-only hub, risk ≤ medium, every caller
                // edge resolved/formal confidence — skip EDIT_CONTEXT_REQUIRED
                // and REASON_NOT_GROUNDED entirely, confirm:true is enough.
                if !confirm {
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code = "CONFIRM_REQUIRED",
                        path,
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                        hub_kind = hub_kind.as_deref().unwrap_or("none"),
                    );
                    return ToolOutcome::error(error_detail(
                        "CONFIRM_REQUIRED",
                        "this edit touches a bridge hub (structurally central, but not a \
                         high-caller symbol, and every known caller is confidently \
                         resolved) — confirm:true is enough here; edit_context is still \
                         recommended, but not required",
                        true,
                    ));
                }
            } else {
                // Structural half (docs/superskills/specs/2026-07-11-superskills-
                // inspired-features.md #5 v2): edit_context must have run for
                // EVERY touched symbol this session, and not have gone stale.
                // Checked before `confirm` so the error names the real blocker
                // instead of a generic "pass confirm:true" that wouldn't help.
                const FRESHNESS_WINDOW_CALLS: u64 = 200;
                let now = self.session_tool_calls();
                let mut missing: Vec<&str> = Vec::new();
                let mut known_caller_qns: Vec<String> = Vec::new();
                let mut reviewed_risk_levels: Vec<String> = Vec::new();
                for t in &pre_touched {
                    match self.edit_context_review(&t.qualified_name) {
                        Some(r) if now.saturating_sub(r.at) <= FRESHNESS_WINDOW_CALLS => {
                            known_caller_qns.extend(r.caller_qns);
                            reviewed_risk_levels.push(r.risk_level);
                        }
                        _ => missing.push(t.qualified_name.as_str()),
                    }
                }
                if !missing.is_empty() {
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code = "EDIT_CONTEXT_REQUIRED",
                        path,
                        symbol = missing[0],
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                    );
                    return ToolOutcome::error(error_detail(
                        "EDIT_CONTEXT_REQUIRED",
                        &format!(
                            "this edit touches {why} — call edit_context(\"{}\") first THIS \
                             session before editing (a prior session's review, or one older \
                             than {FRESHNESS_WINDOW_CALLS} tool calls, doesn't count)",
                            missing[0]
                        ),
                        true,
                    ));
                }
                // Observability only — the gate itself never re-derives risk from
                // this; it just makes "what was reviewed, and at what tier"
                // greppable in server logs when investigating a disputed edit.
                tracing::debug!(
                    "edit gate: {} touched symbol(s) reviewed this session at risk level(s) {:?}",
                    pre_touched.len(),
                    reviewed_risk_levels
                );
                if !confirm {
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code = "CONFIRM_REQUIRED",
                        path,
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                    );
                    return ToolOutcome::error(error_detail(
                        "CONFIRM_REQUIRED",
                        &format!("this edit touches {why} — pass confirm:true to proceed"),
                        true,
                    ));
                }

                // Content-grounded half: `reason` must cite a real caller
                // edit_context returned, not a generic phrase — closes the gap a
                // purely structural gate leaves open (calling edit_context and
                // never reading the response is as cheap as never calling it).
                let reason = reason.unwrap_or("").trim();
                let cites_real_signal = if known_caller_qns.is_empty() {
                    !reason.is_empty()
                } else {
                    known_caller_qns.iter().any(|qn| {
                        let short = qn.rsplit("::").next().unwrap_or(qn);
                        let last_two = last_two_segments(qn);
                        (short.len() >= MIN_BARE_NAME_LEN && cites_token(reason, short))
                            || cites_token(reason, &last_two)
                            || cites_token(reason, qn)
                    })
                };
                if !cites_real_signal {
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code = "REASON_NOT_GROUNDED",
                        path,
                        reason,
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                    );
                    let examples: Vec<String> = known_caller_qns
                        .iter()
                        .map(|qn| {
                            let short = qn.rsplit("::").next().unwrap_or(qn.as_str());
                            // Show the longer Type::name form for a short bare
                            // name so the agent knows which form actually needs
                            // citing (a bare name under MIN_BARE_NAME_LEN never
                            // counts on its own — see cites_real_signal above).
                            if short.len() < MIN_BARE_NAME_LEN {
                                last_two_segments(qn)
                            } else {
                                short.to_string()
                            }
                        })
                        .take(3)
                        .collect();
                    return ToolOutcome::error(error_detail(
                        "REASON_NOT_GROUNDED",
                        &format!(
                            "reason must reference at least one real caller edit_context \
                             returned ({}), or explicitly state why none apply",
                            if examples.is_empty() {
                                "this symbol has no confirmed callers".to_string()
                            } else {
                                examples.join(", ")
                            }
                        ),
                        true,
                    ));
                }
            }
        }
        if let Err(e) = calm_core::edit::atomic_write(&full_path, &new_content) {
            drop(_cross_guard);
            drop(_guard);
            return ToolOutcome::error(error_detail(
                "WRITE_FAILED",
                &format!("failed to write {path}: {e}"),
                false,
            ));
        }
        {
            // One audit event per successful write, unconditional (not just
            // hub/high-risk touches) — the "who/when/confirmed-or-refused/
            // hash-before-after" trail; see AUDIT_TARGET's doc comment.
            let hash_of = |c: &str| {
                let n = c.lines().count().max(1);
                calm_core::edit::range_checksum(c, 1, n).unwrap_or_else(|| "empty".to_string())
            };
            tracing::info!(
                target: crate::telemetry::AUDIT_TARGET,
                session_id = self.session_id,
                decision = "applied",
                path,
                hunks = hunks_output.len() as u64,
                risk = risk.as_deref().unwrap_or("none"),
                hub_hit,
                confirmed = confirm,
                old_hash = hash_of(&original),
                new_hash = hash_of(&new_content),
            );
        }

        // From here on the file on disk already holds the new content, so an
        // index-refresh failure must NOT surface as a tool error: the error
        // envelope is indistinguishable from the pre-write failures above
        // ("nothing was written"), and agents receiving the old
        // REINDEX_FAILED error re-verified or re-applied edits that had in
        // fact succeeded. Collect the failure and report it as a stale-index
        // warning on a success response instead.
        let mut index_stale: Option<String> = None;
        let mut should_embed_bg = false;
        match calm_core::db::conn::open_writer(&self.db_path) {
            Err(e) => index_stale = Some(format!("could not open DB to reindex: {e}")),
            Ok(mut write_conn) => {
                match calm_core::indexer::pipeline::reindex_paths(
                    &mut write_conn,
                    &self.project_root,
                    &[path.to_string()],
                ) {
                    Ok(summary) if !summary.is_noop() => {
                        // Embedding moved out of this lock-held section (Plan 3
                        // §3.1 Phase C) — the reindex above already committed the
                        // DB write, so correctness doesn't depend on embedding
                        // finishing before the response returns; only semantic-
                        // search freshness does, and that's an eventual-
                        // consistency concern, not worth holding _guard/
                        // _cross_guard (and therefore every OTHER edit_lines/
                        // edit_symbol call in this and other processes) for.
                        // Spawned after both guards drop below.
                        should_embed_bg = self.embedder().is_some();
                        // This reindex just ran rebuild_graph, which DELETEs every
                        // call_edges row — including all `formal` upgrades from the
                        // SCIP/LSP overlays — and re-resolves syntactically. The
                        // watcher can't restore them either: by the time its file
                        // event fires, this reindex already updated the hashes, so
                        // its own reindex_changed is a no-op and its overlay hook
                        // never runs. Root cause of the formal tier silently dying
                        // after every CALM-tool edit (observed live 2026-07-10:
                        // 0 formal edges in a DB whose sidecar recorded 2863
                        // upgrades 30 minutes earlier). Fire-and-forget on a
                        // background thread — same posture as the watcher's own
                        // post-reindex hook — so the edit response isn't held for a
                        // ~20s rust-analyzer batch run; `run_all_coalesced` keeps
                        // rapid successive edits from stacking concurrent passes.
                        #[cfg(feature = "scip-overlay")]
                        {
                            let root = self.project_root.clone();
                            let db = self.db_path.clone();
                            std::thread::spawn(move || {
                                crate::scip_overlay::run_all_coalesced(&root, &db);
                            });
                        }
                    }
                    Ok(_) => {}
                    Err(e) => index_stale = Some(format!("reindex failed: {e}")),
                }
            }
        }
        drop(_cross_guard);
        drop(_guard);

        // Plan 3 §3.1 Phase C: background embed, now outside both guards. Own
        // writer connection (this thread doesn't hold write_conn, which is
        // already out of scope) — busy_timeout in open_writer handles any
        // contention with a concurrent edit's reindex. EMBED_BG (module-level
        // static above) serializes concurrent background embed jobs against
        // each other, not against reindex_paths itself.
        if should_embed_bg && let Some(model) = self.embedder() {
            let db_path = self.db_path.clone();
            std::thread::spawn(move || {
                let _bg_guard = EMBED_BG.lock_ok();
                match calm_core::db::conn::open_writer(&db_path) {
                    Ok(bg_conn) => {
                        if let Err(e) =
                            calm_core::embedding::embed_pending(&bg_conn, model.as_ref())
                        {
                            tracing::error!("edit_lines: background embedding failed: {e}");
                        }
                        if let Err(e) =
                            calm_core::embedding::embed_pending_chunks(&bg_conn, model.as_ref())
                        {
                            tracing::error!("edit_lines: background chunk embedding failed: {e}");
                        }
                    }
                    Err(e) => tracing::error!(
                        "edit_lines: could not open DB for background embedding: {e}"
                    ),
                }
            });
        }

        // Session tracking must reflect what hit the disk even when the
        // index refresh didn't: skipping these on the stale path exempted
        // the write from the diff_impact pre-commit gate.
        self.track_file(path);
        self.mark_written(path);

        if let Some(detail) = index_stale {
            let mut note = format!(
                "edit APPLIED — {path} on disk is correct, but the index could not be \
                 refreshed ({detail}); do NOT re-apply or rewrite. Symbol line numbers may \
                 be stale until the index recovers"
            );
            if let Some(a) = &ambiguity_note {
                note.push_str(". ");
                note.push_str(a);
            }
            return ToolOutcome::success(EditLinesOutput {
                path: path.to_string(),
                applied: true,
                hunks: hunks_output,
                parse_status: Some(parse_status.to_string()),
                touched_symbols: vec![],
                risk_assessment: risk,
                index_stale: Some(true),
                note: Some(note),
                suggested_next: self.filter_sn(suggested(
                    "indexing_status",
                    "Index is stale after a successful write — check and recover",
                )),
            });
        }

        let touched_symbols = {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(_) => {
                    return ToolOutcome::success(EditLinesOutput {
                        path: path.to_string(),
                        applied: true,
                        hunks: hunks_output,
                        parse_status: Some(parse_status.to_string()),
                        touched_symbols: vec![],
                        risk_assessment: risk,
                        index_stale: None,
                        note: Some("edit applied but could not re-query touched symbols".into()),
                        suggested_next: None,
                    });
                }
            };
            let new_ranges: Vec<(i64, i64)> = outcome
                .results
                .iter()
                .map(|r| (r.start_line as i64, r.new_end_line as i64))
                .collect();
            let (_, _, _, touched) = compute_touch_risk(&conn, path, &new_ranges);
            touched
        };

        ToolOutcome::success(EditLinesOutput {
            path: path.to_string(),
            applied: true,
            hunks: hunks_output,
            parse_status: Some(parse_status.to_string()),
            touched_symbols,
            risk_assessment: risk,
            index_stale: None,
            note: ambiguity_note,
            suggested_next: self.filter_sn(suggested_gated(
                "diff_impact",
                "Verify wider blast radius, especially if this touched a hub/high-risk symbol",
            )),
        })
    }
}

/// Symbols in `path` whose `[line_start, line_end]` overlaps any of `ranges`
/// — shared by the pre-write risk gate (against original ranges) and the
/// post-write response (against the edited ranges' new positions).
fn symbols_overlapping_ranges(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
) -> Vec<(String, i64, bool, Option<String>)> {
    let mut stmt = match conn.prepare(
        "SELECT qualified_name, caller_count, is_hub, hub_kind, line_start, line_end FROM symbols WHERE path = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![path], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)? != 0,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .filter(|(_, _, _, _, line_start, line_end)| {
                ranges
                    .iter()
                    .any(|&(rs, re)| !(*line_end < rs || *line_start > re))
            })
            .map(|(qn, callers, is_hub, hub_kind, _, _)| (qn, callers, is_hub, hub_kind))
            .collect()
    })
    .unwrap_or_default()
}

/// `(risk_level, hub_hit, touched_symbols)` for whatever symbols in `path`
/// overlap `ranges`. `risk_level` is `None` when nothing overlaps (editing
/// dead space between symbols, or a file with no parsed symbols at all —
/// Cargo.toml, docs) — that's not an error, just nothing to gate on.
/// Strength ordering for picking the single strongest `hub_kind` among
/// several touched symbols: a `degree`/`both` touch always outranks a
/// `bridge`-only one, since Plan 3 §3.3 (F10) only ever downgrades the
/// gate when EVERY touched hub is bridge-only.
fn hub_kind_strength(kind: &str) -> u8 {
    match kind {
        "degree" | "both" => 2,
        "bridge" => 1,
        _ => 0,
    }
}

/// `(risk_level, hub_hit, strongest_hub_kind, touched_symbols)` for
/// whatever symbols in `path` overlap `ranges`. `risk_level` is `None` when
/// nothing overlaps (editing dead space between symbols, or a file with no
/// parsed symbols at all — Cargo.toml, docs) — that's not an error, just
/// nothing to gate on. `strongest_hub_kind` is `None` when nothing touched
/// is a hub, `Some("bridge")` only when every touched hub is bridge-only,
/// and `Some("degree")`/`Some("both")` if any touched hub is stronger than
/// bridge-only (see `hub_kind_strength`).
fn compute_touch_risk(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
) -> (
    Option<String>,
    bool,
    Option<String>,
    Vec<TouchedSymbolOutput>,
) {
    let rows = symbols_overlapping_ranges(conn, path, ranges);
    let mut max_callers = 0i64;
    let mut hub_hit = false;
    let mut strongest_hub_kind: Option<String> = None;
    let mut touched = Vec::with_capacity(rows.len());
    for (qualified_name, caller_count, is_hub, hub_kind) in rows {
        max_callers = max_callers.max(caller_count);
        hub_hit |= is_hub;
        if let Some(k) = &hub_kind {
            let stronger = strongest_hub_kind
                .as_deref()
                .is_none_or(|cur| hub_kind_strength(k) > hub_kind_strength(cur));
            if stronger {
                strongest_hub_kind = Some(k.clone());
            }
        }
        touched.push(TouchedSymbolOutput {
            qualified_name,
            caller_count,
            is_hub,
            hub_kind,
        });
    }
    let risk = (!touched.is_empty()).then(|| risk_level_from_caller_count(max_callers).to_string());
    (risk, hub_hit, strongest_hub_kind, touched)
}

/// Plan 3 §3.3 (F10): true iff every caller edge (`call_edges.to_symbol`)
/// pointing at any of `qualified_names` has `edge_confidence` in
/// `{'resolved', 'formal'}` — gates whether a bridge-only hub touch may use
/// the lighter (`CONFIRM_REQUIRED`-only) tier. A symbol's TRUE blast radius
/// can exceed its counted `caller_count` when some incoming edges are only
/// `'textual'`/`'ambiguous'` (dynamic dispatch, reflection, a resolver gap)
/// — those callers were found by name/heuristic, not proven, so undercounting
/// is possible and the full 3-layer gate must still apply regardless of
/// `hub_kind`. A symbol with zero caller edges is treated as NOT confident
/// (conservative — falls through to the full gate) rather than vacuously
/// true; `qualified_names` empty also returns `false` for the same reason.
fn all_caller_edges_confident(conn: &rusqlite::Connection, qualified_names: &[String]) -> bool {
    if qualified_names.is_empty() {
        return false;
    }
    let placeholders = qualified_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT COUNT(*), SUM(CASE WHEN edge_confidence IN ('resolved','formal') THEN 1 ELSE 0 END) \
         FROM call_edges WHERE to_symbol IN ({placeholders})"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let params: Vec<&dyn rusqlite::ToSql> = qualified_names
        .iter()
        .map(|s| s as &dyn rusqlite::ToSql)
        .collect();
    match stmt.query_row(params.as_slice(), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
    }) {
        Ok((total, confident)) if total > 0 => confident.unwrap_or(0) == total,
        _ => false,
    }
}

/// Builds the insertion hunk for `edit_symbol`'s `position` modes. The
/// indexed `[line_start, line_end]` of `c` is only a hint here: the range
/// is re-resolved from a fresh parse of the file on disk, so an index left
/// stale by an earlier failed reindex can't steer the insertion to a wrong
/// offset — the exact failure mode of trusting remembered line numbers.
/// Languages without a parse tree (docs, configs, shallow-tier grammars)
/// fall back to the indexed range; the anchor-line hash pre-filled by
/// `insertion_hunk` still conflict-checks the write either way.
/// Resolves `path` (repo-relative, caller-supplied) against `project_root`
/// and verifies the *real* location — after any `..` traversal or symlink
/// is followed — stays inside it. Both callers require the target to
/// already exist (`edit_lines_impl` only edits existing files;
/// `insertion_hunk_for` reads one to compute an insertion point), so
/// canonicalizing the full path directly, rather than just its parent, is
/// enough to catch an escape via any path component, including the leaf
/// itself being a symlink.
///
/// Independently discovered via code review while cross-checking CALM
/// against Wiz's "GhostApproval" report (2026-07-08,
/// wiz.io/blog/ghostapproval-a-trust-boundary-gap-in-ai-coding-assistants),
/// which documented the same *class* of bug (CWE-61 symlink following +
/// UI misrepresentation, not a TOCTOU race as sometimes summarized) in
/// several AI coding assistants' own file-write paths. CALM never renders
/// a confirmation dialog itself, but a host MCP client's dialog shows
/// `path` exactly as supplied here — an unvalidated traversal/symlink
/// escape at this layer is still an informed-consent bypass one level
/// down, regardless of what the host displays.
fn resolve_repo_path(
    project_root: &std::path::Path,
    path: &str,
) -> Result<std::path::PathBuf, ErrorDetail> {
    let candidate = project_root.join(path);
    let real = std::fs::canonicalize(&candidate)
        .map_err(|e| error_detail("READ_FAILED", &format!("could not read {path}: {e}"), false))?;
    // `project_root` isn't guaranteed canonical by every caller (tests in
    // particular construct `CalmServer` directly from an uncanonicalized
    // temp dir) — canonicalize both sides rather than assume the
    // constructor already did, so this check can't be defeated simply by
    // an un-canonicalized root.
    let root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    if !real.starts_with(&root) {
        return Err(error_detail(
            "PATH_ESCAPES_PROJECT_ROOT",
            &format!(
                "{path} resolves outside the project root (via `..` traversal or a symlink) \
                 — refusing to read or write it"
            ),
            false,
        ));
    }
    Ok(real)
}

fn insertion_hunk_for(
    project_root: &std::path::Path,
    c: &CandidateRow,
    position: calm_core::edit::InsertPosition,
    new_text: &str,
) -> Result<calm_core::edit::HunkRequest, ErrorDetail> {
    let full_path = resolve_repo_path(project_root, &c.path)?;
    let live = std::fs::read_to_string(&full_path).map_err(|e| {
        error_detail(
            "READ_FAILED",
            &format!("could not read {}: {e}", c.path),
            false,
        )
    })?;
    let (line_start, line_end) =
        match calm_core::indexer::parser::extract_symbols(&live, &c.language, &c.path) {
            Ok(symbols) => match best_live_range(&symbols, &c.name, c.line_start) {
                Some(range) => range,
                None => {
                    return Err(error_detail(
                        "STALE_SYMBOL",
                        &format!(
                            "'{}' was not found in a fresh parse of {} — the index entry is \
                             stale; call indexing_status, then re-resolve the symbol",
                            c.name, c.path
                        ),
                        true,
                    ));
                }
            },
            Err(_) => (c.line_start as usize, c.line_end as usize),
        };
    calm_core::edit::insertion_hunk(&live, line_start, line_end, position, new_text).ok_or_else(
        || {
            error_detail(
                "INVALID_RANGE",
                &format!(
                    "resolved range {line_start}..{line_end} is out of bounds for {} on disk",
                    c.path
                ),
                true,
            )
        },
    )
}

/// Picks the live-parse occurrence of `name` whose start is nearest the
/// indexed one — same-named symbols (overloads, `#[cfg]` twins) tie-break
/// to the least-shifted candidate.
/// audit F14: true when `reason` contains `needle` as a whole token — the
/// byte immediately before/after each match is not `[A-Za-z0-9_]` (or the
/// match sits at the start/end of the string). Checks every occurrence,
/// not just the first, since a needle can appear once mid-word (no match)
/// and again as a real standalone token later in the same reason. `needle`
/// is always an identifier segment (ASCII-only qualified-name piece), so
/// byte indexing is safe here: none of its bytes can ever land mid-way
/// through a multi-byte UTF-8 character in `reason` (continuation bytes
/// are always >= 0x80, never equal to an ASCII needle byte).
fn cites_token(reason: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = reason.as_bytes();
    let mut start = 0;
    while let Some(rel) = reason[start..].find(needle) {
        let idx = start + rel;
        let before_ok = idx == 0 || !is_word_byte(bytes[idx - 1]);
        let after = idx + needle.len();
        let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
        if start >= reason.len() {
            break;
        }
    }
    false
}

const MIN_BARE_NAME_LEN: usize = 4;

/// Joins the last two `::`-separated segments of `qn` ("Type::name") when
/// there are at least two, otherwise returns the whole thing unchanged —
/// gives a short bare name (e.g. "new") a longer, still-real form to cite
/// that can't collide with an unrelated word in `reason`.
fn last_two_segments(qn: &str) -> String {
    let mut rev = qn.rsplit("::");
    let last = rev.next().unwrap_or(qn);
    match rev.next() {
        Some(second) => format!("{second}::{last}"),
        None => last.to_string(),
    }
}

fn best_live_range(
    symbols: &[calm_core::indexer::parser::ParsedSymbol],
    name: &str,
    indexed_start: i64,
) -> Option<(usize, usize)> {
    symbols
        .iter()
        .filter(|s| s.name == name)
        .min_by_key(|s| (s.line_start as i64 - indexed_start).abs())
        .map(|s| (s.line_start, s.line_end))
}

// ---------------------------------------------------------------------------
// Params / Output
// ---------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditHunkParam {
    /// 1-indexed, inclusive.
    pub(crate) start_line: i64,
    /// 1-indexed, inclusive.
    pub(crate) end_line: i64,
    /// Hash of this range's current content — from a prior call's
    /// `current_hash`, or `edit_context`'s `range_checksum` when the range
    /// is exactly a whole symbol. Omit to preview instead of writing: the
    /// response still reports `current_hash`/`old_text` for this range, so
    /// a first call can learn the hash before a real edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_hash: Option<String>,
    /// Replacement text for the range, used exactly as given (no implicit
    /// newline handling) — include your own `\n` between lines and after
    /// the last one if the following line should stay on its own line.
    pub(crate) new_text: String,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditLinesParams {
    /// Repo-relative path. All hunks in one call apply to this one file.
    pub(crate) path: String,
    /// Must be disjoint (non-overlapping) ranges; applied bottom-up so
    /// earlier (lower-numbered) hunks are never affected by line-count
    /// changes from later (higher-numbered) ones.
    pub(crate) edits: Vec<EditHunkParam>,
    /// Required `true` to write when any touched range falls inside a
    /// `risk_assessment: "high"` symbol or one with `is_hub: true` (see
    /// `edit_context`). Omitted/`false` rejects such an edit with an
    /// explanation instead of proceeding. Two further requirements gate on
    /// top of `confirm` for a DEGREE-hub/both-hub/high-risk touch
    /// (docs/superskills/specs/2026-07-11-superskills-inspired-features.md
    /// #5 v2): `edit_context` must have been called for the touched
    /// symbol(s) THIS session (`EDIT_CONTEXT_REQUIRED` otherwise — merely
    /// having called it in a prior session, or a stale review past the
    /// freshness window, doesn't count), and `reason` must cite a real
    /// caller name from that exact `edit_context` response
    /// (`REASON_NOT_GROUNDED` otherwise) — `confirm: true` alone is never
    /// sufficient for those. Plan 3 §3.3 (F10): a BRIDGE-only hub touch
    /// (structurally central via coreness, not a high-caller symbol) at
    /// risk ≤ medium, where every known caller edge is `resolved`/`formal`
    /// confidence (no `textual`/`ambiguous` undercounting risk), skips
    /// both of those extra requirements — `confirm: true` alone is enough.
    /// A single low-confidence caller on that same symbol still forces the
    /// full requirements regardless of `hub_kind`.
    #[serde(default)]
    pub(crate) confirm: bool,
    /// Required (non-empty, and referencing a real caller — see `confirm`)
    /// when touching a hub/high-risk symbol. Ignored otherwise. State which
    /// caller(s) you checked and why this change is safe for them — not a
    /// free-form justification a generic phrase could satisfy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditSymbolParams {
    /// Bare symbol name (not a `path::name` qualified name).
    pub(crate) symbol: String,
    /// Narrows the search to one file when `symbol` alone is ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// Disambiguates same-named symbols in the same file — any line within
    /// the intended candidate's range, as echoed in an earlier `ambiguous`
    /// response's `line_start`/`line_end`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    /// Same contract as `edit_lines`' hunk `expected_hash` — omit to
    /// preview the symbol's current hash/content instead of writing.
    /// Ignored by the insertion `position` modes, which anchor and hash
    /// themselves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_hash: Option<String>,
    /// With the default `position` ("replace"): full replacement text for
    /// the symbol's `[line_start, line_end]`. With an insertion `position`:
    /// the new code to insert — the symbol itself is left untouched.
    pub(crate) new_text: String,
    /// One of `"replace"` (default), `"before"`, `"after"`,
    /// `"append_inside"`, `"top_of_file"`, `"end_of_file"`. `"replace"`
    /// swaps the symbol's whole range for `new_text`. `"before"`/`"after"`/
    /// `"append_inside"` INSERT `new_text` relative to the symbol:
    /// `"before"` = directly above it, `"after"` = directly below it (a
    /// new sibling — e.g. add a test after the last test in a module),
    /// `"append_inside"` = at the end of its body (above the closing
    /// delimiter when one exists). Insertion modes re-resolve the symbol's
    /// range from a fresh parse of the file on disk and pre-fill the
    /// anchor hash themselves, so no `expected_hash`, preview round trip,
    /// or line arithmetic is needed — they cannot land at a stale line
    /// offset. `"top_of_file"`/`"end_of_file"` insert relative to the
    /// WHOLE FILE (`path` required, `symbol` ignored) — for brand-new
    /// module-level content (a new `use`, a new top-level function) with
    /// no existing sibling symbol to anchor on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) position: Option<String>,
    /// Same gate as `edit_lines`' `confirm` — including the `edit_context`-
    /// this-session and `reason`-must-cite-a-real-caller requirements on
    /// top of it for a hub/high-risk touch. See `EditLinesParams::confirm`.
    #[serde(default)]
    pub(crate) confirm: bool,
    /// See `EditLinesParams::reason`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    /// Small-text-match mode: when set, `new_text` replaces the FIRST
    /// (and required-to-be-only) occurrence of `old_text` found within the
    /// resolved symbol's current range, instead of replacing the whole
    /// symbol. No line numbers, no `expected_hash` needed — the server
    /// reads the symbol's live content to find the match, so staleness is
    /// impossible by construction. Refused with `BOUNDARY_AMBIGUOUS` if
    /// the target symbol carries that flag (its own range can't be
    /// trusted as a search scope — see fitness_report). Ignored when
    /// `position` is not `"replace"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) old_text: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditHunkResultOutput {
    pub(crate) start_line: i64,
    pub(crate) end_line: i64,
    /// "applied" | "preview" | "conflict"
    pub(crate) status: String,
    /// Hash of the range's content before this call — pass this as
    /// `expected_hash` on retry.
    pub(crate) current_hash: String,
    /// Content of the range before this call — undo material when
    /// `status == "applied"`, or what to inspect otherwise.
    pub(crate) old_text: String,
    /// Only present when `status == "applied"`: the line the replacement
    /// now ends at (`start_line` is unchanged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) new_end_line: Option<i64>,
    /// Present when the range's pre-edit content is byte-identical to N
    /// OTHER line windows of this file (a lone `}` line, say): the hash
    /// proves content, not position — verify the line numbers point where
    /// intended, or anchor structurally via edit_symbol's `position`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) other_matches: Option<i64>,
}

impl From<&calm_core::edit::HunkResult> for EditHunkResultOutput {
    fn from(r: &calm_core::edit::HunkResult) -> Self {
        let applied = r.status == calm_core::edit::HunkStatus::Applied;
        Self {
            start_line: r.start_line as i64,
            end_line: r.end_line as i64,
            status: match r.status {
                calm_core::edit::HunkStatus::Applied => "applied",
                calm_core::edit::HunkStatus::Preview => "preview",
                calm_core::edit::HunkStatus::Conflict => "conflict",
            }
            .to_string(),
            current_hash: r.current_hash.clone(),
            old_text: r.old_text.clone(),
            new_end_line: applied.then_some(r.new_end_line as i64),
            other_matches: (r.content_occurrences > 1).then_some(r.content_occurrences as i64 - 1),
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct TouchedSymbolOutput {
    pub(crate) qualified_name: String,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    /// Plan 3 §3.3 (F10): `'degree' | 'bridge' | 'both'`, or `None` when
    /// `is_hub` is `false` — see `graph::hub::update_is_hub_flags`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hub_kind: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditLinesOutput {
    pub(crate) path: String,
    pub(crate) applied: bool,
    pub(crate) hunks: Vec<EditHunkResultOutput>,
    /// "clean" | "skipped_unrecognized_language" — absent when nothing was
    /// written (preview/conflict/risk-blocked/parse error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) parse_status: Option<String>,
    /// Symbols overlapping the touched ranges (post-edit positions once
    /// `applied`) — the same callers/is_hub signal `edit_context`/
    /// `diff_impact` report, bundled here so a caller doesn't need a
    /// separate round trip just to see what it just changed.
    pub(crate) touched_symbols: Vec<TouchedSymbolOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) risk_assessment: Option<String>,
    /// `true` only when `applied` is `true` but the post-write index
    /// refresh failed: the file on disk holds the new content — do NOT
    /// re-apply — while symbol line numbers served from the index may lag
    /// until it recovers (see `note`, and call `indexing_status`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) index_stale: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
