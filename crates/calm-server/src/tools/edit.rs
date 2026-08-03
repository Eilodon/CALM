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
        description = "The only write-capable tool in calm — line-range granularity, works on ANY tracked file (source code, Cargo.toml, docs — not just parsed symbols). NOT FOR: symbol-scoped edits with auto-resolved range (use edit_symbol). Requires expected_hash from a prior call's current_hash (or edit_context's range_checksum for a whole symbol); omit it to preview a range's hash/content without writing anything. Alternative to expected_hash: set old_text on a hunk instead — replaces its one occurrence within [start_line, end_line] with no hash needed and no preview round trip (fixes the common 'read a wide range for context, then edit one narrow line inside it' case: keep [start_line, end_line] as the wide range you already read, old_text pins the exact spot). All hunks in one call apply to the same file and must be disjoint (non-overlapping).",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) async fn edit_lines_tool(
        &self,
        Parameters(p): Parameters<EditLinesParams>,
        ctx: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Json<ToolOutcome<EditLinesOutput>> {
        let elicit_timeout = self.elicit_setup(&ctx.peer);
        let gate = if elicit_timeout.is_some() {
            ElicitGate::Ask
        } else {
            ElicitGate::Off
        };
        let mut ask: Option<HubAskContext> = None;
        let first = self.edit_lines_flow(&p, gate, &mut ask);
        let (Some(timeout), Some(ask_ctx)) = (elicit_timeout, ask) else {
            return Json(first);
        };
        // `first` has fully returned above — neither the in-process
        // edit_lock nor the cross-process lock (both scoped inside
        // edit_lines_impl_gated) is held across this await (audit FM1).
        let fingerprint = fingerprint_edit_lines(&p);
        Json(
            match self
                .hub_elicit_roundtrip(
                    &ctx.peer,
                    "edit_lines",
                    &p.path,
                    &fingerprint,
                    &ask_ctx,
                    p.reason.as_deref(),
                    timeout,
                )
                .await
            {
                Ok(()) => self.edit_lines_flow(&p, ElicitGate::Approved, &mut None),
                Err(detail) => ToolOutcome::error(detail),
            },
        )
    }

    /// Legacy sync surface — same behavior as `edit_lines_tool` with the
    /// elicitation gate off; kept so the existing (sync) test suite and any
    /// in-crate caller keep working unchanged.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn edit_lines(
        &self,
        Parameters(p): Parameters<EditLinesParams>,
    ) -> Json<ToolOutcome<EditLinesOutput>> {
        Json(self.edit_lines_flow(&p, ElicitGate::Off, &mut None))
    }

    /// Sync body of `edit_lines` — extracted so the async tool wrapper can
    /// run it twice (Ask, then Approved) around the elicitation await.
    /// pub(crate) so tools.rs's test mod can drive the Ask/Approved gate
    /// states directly (the async wrapper needs a live rmcp peer).
    pub(crate) fn edit_lines_flow(
        &self,
        p: &EditLinesParams,
        gate: ElicitGate,
        ask_out: &mut Option<HubAskContext>,
    ) -> ToolOutcome<EditLinesOutput> {
        self.timed_tool("edit_lines", || {
            // old_text-mode hunks (see EditHunkParam::old_text) need one
            // live read of the file to resolve against — done once up
            // front, shared by every such hunk in this call, not once per
            // hunk. Hash-mode hunks (the common case) never touch this and
            // pay nothing extra.
            let live: Option<String> = if p.edits.iter().any(|h| h.old_text.is_some()) {
                let full_path = match resolve_repo_path(&self.project_root, &p.path) {
                    Ok(fp) => fp,
                    Err(e) => return ToolOutcome::error(e),
                };
                match std::fs::read_to_string(&full_path) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        return ToolOutcome::error(error_detail(
                            "READ_FAILED",
                            &format!("could not read {}: {e}", p.path),
                            false,
                        ));
                    }
                }
            } else {
                None
            };

            let mut hunks: Vec<calm_core::edit::HunkRequest> = Vec::with_capacity(p.edits.len());
            for h in &p.edits {
                let start = h.start_line.max(0) as usize;
                let end = h.end_line.max(0) as usize;
                match &h.old_text {
                    None => hunks.push(calm_core::edit::HunkRequest {
                        start_line: start,
                        end_line: end,
                        expected_hash: h.expected_hash.clone(),
                        new_text: h.new_text.clone(),
                    }),
                    Some(old_text) => {
                        // `live` is always Some here: the check above sets it
                        // whenever any hunk in `p.edits` has `old_text` set.
                        let live_ref = live.as_deref().expect("live read done above");
                        match calm_core::edit::find_and_replace_hunk(
                            live_ref,
                            start,
                            end,
                            old_text,
                            &h.new_text,
                        ) {
                            Ok(hunk) => hunks.push(hunk),
                            Err(calm_core::edit::MatchOutcome::NotFound) => {
                                return ToolOutcome::error(error_detail(
                                    "MATCH_NOT_FOUND",
                                    &format!(
                                        "old_text {old_text:?} was not found within \
                                         {start}..{end} of '{}' on disk",
                                        p.path
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
                                return ToolOutcome::error(error_detail(
                                    "AMBIGUOUS_MATCH",
                                    &format!(
                                        "old_text {old_text:?} occurs {} times within \
                                         '{}' ({where_str}) — narrow it with more \
                                         surrounding context so it matches exactly once",
                                        lines.len(),
                                        p.path
                                    ),
                                    true,
                                ));
                            }
                        }
                    }
                }
            }
            self.edit_lines_impl_gated(
                &p.path,
                hunks,
                p.confirm,
                p.reason.as_deref(),
                false,
                None,
                gate,
                ask_out,
            )
        })
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
    pub(crate) async fn edit_symbol_tool(
        &self,
        Parameters(p): Parameters<EditSymbolParams>,
        ctx: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Json<ResolvedOutcome<EditLinesOutput>> {
        let elicit_timeout = self.elicit_setup(&ctx.peer);
        let gate = if elicit_timeout.is_some() {
            ElicitGate::Ask
        } else {
            ElicitGate::Off
        };
        let mut ask: Option<HubAskContext> = None;
        let first = self.edit_symbol_flow(&p, gate, &mut ask);
        let (Some(timeout), Some(ask_ctx)) = (elicit_timeout, ask) else {
            return Json(first);
        };
        // `first` has fully returned above — no edit/DB lock is held across
        // this await (all scoped inside edit_lines_impl_gated); audit FM1.
        let fingerprint = fingerprint_edit_symbol(&p);
        let cache_key_path = p.path.clone().unwrap_or_else(|| p.symbol.clone());
        Json(
            match self
                .hub_elicit_roundtrip(
                    &ctx.peer,
                    "edit_symbol",
                    &cache_key_path,
                    &fingerprint,
                    &ask_ctx,
                    p.reason.as_deref(),
                    timeout,
                )
                .await
            {
                Ok(()) => self.edit_symbol_flow(&p, ElicitGate::Approved, &mut None),
                Err(detail) => ResolvedOutcome::error(detail),
            },
        )
    }

    /// Legacy sync surface — same behavior as `edit_symbol_tool` with the
    /// elicitation gate off; kept so the existing (sync) test suite and any
    /// in-crate caller keep working unchanged.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn edit_symbol(
        &self,
        Parameters(p): Parameters<EditSymbolParams>,
    ) -> Json<ResolvedOutcome<EditLinesOutput>> {
        Json(self.edit_symbol_flow(&p, ElicitGate::Off, &mut None))
    }

    /// Sync body of `edit_symbol` — extracted so the async tool wrapper can
    /// run it twice (Ask, then Approved) around the elicitation await.
    fn edit_symbol_flow(
        &self,
        p: &EditSymbolParams,
        gate: ElicitGate,
        ask_out: &mut Option<HubAskContext>,
    ) -> ResolvedOutcome<EditLinesOutput> {
        self.timed_tool("edit_symbol", || {
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
                    .edit_lines_impl_gated(
                        path,
                        vec![hunk],
                        p.confirm,
                        p.reason.as_deref(),
                        true,
                        None,
                        gate,
                        ask_out,
                    )
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
            let mut insertion_note: Option<String> = None;
            let hunk = match p.position.as_deref().unwrap_or("replace") {
                "replace" => match &p.old_text {
                    None => calm_core::edit::HunkRequest {
                        start_line: c.line_start as usize,
                        end_line: c.line_end as usize,
                        expected_hash: p.expected_hash.clone(),
                        new_text: p.new_text.clone(),
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
                        Ok((h, note)) => {
                            insertion_note = note;
                            h
                        }
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
            self.edit_lines_impl_gated(
                &c.path,
                vec![hunk],
                p.confirm,
                p.reason.as_deref(),
                position_anchored,
                insertion_note,
                gate,
                ask_out,
            )
            .into_resolved()
        })
    }

    #[tool(
        name = "format_files",
        description = "Formats Rust source files via rustfmt — the safe replacement for shelling out to `rustfmt`/`cargo fmt` directly. Only `.rs` files are supported (rustfmt is Rust-specific); a non-Rust path is reported as skipped, not an error. Reindexes any file it actually changes, same as edit_lines/edit_symbol.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn format_files(
        &self,
        Parameters(p): Parameters<FormatFilesParams>,
    ) -> Json<ToolOutcome<FormatFilesOutput>> {
        Json(self.timed_tool("format_files", || self.format_files_impl(p.paths)))
    }

    /// Shared implementation for `format_files`. Formats each path in
    /// isolation (a syntax error in one file never blocks the rest), writes
    /// only the files that actually changed via the same `atomic_write` +
    /// `reindex_paths` path `edit_lines_impl` uses, and reindexes all of
    /// them together in one batched call rather than once per file.
    ///
    /// Deliberately does NOT run the hub/high-risk `CONFIRM_REQUIRED`/
    /// `edit_context`-required gate `edit_lines_impl` enforces: that gate
    /// exists because an arbitrary text edit can change program semantics
    /// in ways blast-radius analysis needs to catch. `rustfmt` cannot —
    /// by construction it only ever changes whitespace/line-breaks/
    /// trailing commas, never identifiers, expressions, or control flow —
    /// so gating a formatting-only write behind the same machinery
    /// designed for semantic risk would be safety theater, not safety.
    /// Still marks written files for the Stage 7 `diff_impact` gate below
    /// (same as every other write path) for consistency, even though a
    /// `diff_impact` run on a pure-formatting change will correctly report
    /// no symbol-level changes.
    fn format_files_impl(&self, paths: Vec<String>) -> ToolOutcome<FormatFilesOutput> {
        let _guard = self.edit_lock.lock_ok();
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

        let mut results = Vec::with_capacity(paths.len());
        let mut changed_paths: Vec<String> = Vec::new();
        // Shadow-mode WS-1 (docs/plans/2026-08-02-phase1-p0-execution-plan.md
        // §4.4): same non-blocking posture as edit_lines_impl_gated -- every
        // txn:: call is best-effort, a failure never changes this
        // function's outcome. One shadow tx per formatted file
        // (action_class = semantics_preserving_transform per adopt-plan §5
        // P0-3), all advanced to IndexCommitted/Done together after the
        // single batched reindex below, since format_files reindexes every
        // changed path in one call rather than per-file.
        let mut shadow_tx_ids: Vec<String> = Vec::new();
        let project_id = self.project_root.to_string_lossy().into_owned();

        for path in &paths {
            let full_path = match resolve_repo_path(&self.project_root, path) {
                Ok(p) => p,
                Err(e) => {
                    results.push(FormatFileResult {
                        path: path.clone(),
                        status: "error".to_string(),
                        detail: Some(e.message),
                    });
                    continue;
                }
            };
            let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "rs" {
                results.push(FormatFileResult {
                    path: path.clone(),
                    status: "skipped_unsupported_extension".to_string(),
                    detail: Some("format_files only supports .rs files today".to_string()),
                });
                continue;
            }
            let original = match std::fs::read_to_string(&full_path) {
                Ok(s) => s,
                Err(e) => {
                    results.push(FormatFileResult {
                        path: path.clone(),
                        status: "error".to_string(),
                        detail: Some(format!("could not read {path}: {e}")),
                    });
                    continue;
                }
            };
            let edition = calm_core::format::detect_rust_edition(&full_path, &self.project_root);
            let formatted = match calm_core::format::format_rust_source(&original, &edition) {
                Ok(f) => f,
                Err(e) => {
                    results.push(FormatFileResult {
                        path: path.clone(),
                        status: "error".to_string(),
                        detail: Some(e),
                    });
                    continue;
                }
            };
            if formatted == original {
                results.push(FormatFileResult {
                    path: path.clone(),
                    status: "already_formatted".to_string(),
                    detail: None,
                });
                continue;
            }
            // WS-1 enforce transition (docs/plans/2026-08-02-ws1-enforce-and-
            // critical-risk-execution-plan.md §2): same fail-closed posture
            // as edit_lines_impl_gated -- a file whose transaction journal
            // couldn't even BEGIN is skipped (as an "error" result for that
            // file only, not a batch-wide abort) rather than formatted with
            // no journal at all. Other files in this same paths list are
            // unaffected, consistent with every other per-file failure this
            // loop already handles independently.
            //
            // Tier-1 perf fix (docs/plans/2026-08-02-shadow-txn-connection-
            // consolidation-plan.md §3): ONE writer connection per file for
            // begin + the FileCommitted/Failed advance below, instead of a
            // fresh open_writer() at each -- same pattern applied to
            // edit_lines_impl_gated. _guard/_cross_guard are already held
            // for this whole function, so no other CALM writer contends.
            let file_conn = match calm_core::db::conn::open_writer(&self.db_path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        "txn::begin failed, skipping format for {path} (enforce mode): \
                         could not open DB for transaction init: {e}"
                    );
                    results.push(FormatFileResult {
                        path: path.clone(),
                        status: "error".to_string(),
                        detail: Some(format!(
                            "could not initialize the durable edit-transaction journal, \
                             refusing to format this file (could not open DB for \
                             transaction init: {e})"
                        )),
                    });
                    continue;
                }
            };
            let base_digest = calm_core::digest::evidence_digest(original.as_bytes());
            let proposed_digest = calm_core::digest::evidence_digest(formatted.as_bytes());
            let shadow_tx_id = match calm_core::txn::begin(
                &file_conn,
                &project_id,
                path,
                &base_digest,
                &proposed_digest,
            ) {
                Ok(tx) => tx.tx_id,
                Err(e) => {
                    tracing::warn!(
                        "txn::begin failed, skipping format for {path} (enforce mode): {e}"
                    );
                    results.push(FormatFileResult {
                        path: path.clone(),
                        status: "error".to_string(),
                        detail: Some(format!(
                            "could not initialize the durable edit-transaction journal, \
                             refusing to format this file ({e})"
                        )),
                    });
                    continue;
                }
            };
            if let Err(e) = calm_core::edit::atomic_write(&full_path, &formatted) {
                let _ = calm_core::txn::advance(
                    &file_conn,
                    &shadow_tx_id,
                    calm_core::txn::TxState::Failed,
                    "system",
                    &e.to_string(),
                );
                results.push(FormatFileResult {
                    path: path.clone(),
                    status: "error".to_string(),
                    detail: Some(format!("failed to write {path}: {e}")),
                });
                continue;
            }
            let _ = calm_core::txn::advance(
                &file_conn,
                &shadow_tx_id,
                calm_core::txn::TxState::FileCommitted,
                "system",
                "atomic_write succeeded (format)",
            );
            shadow_tx_ids.push(shadow_tx_id);
            self.track_file(path);
            self.mark_written(path);
            changed_paths.push(path.clone());
            results.push(FormatFileResult {
                path: path.clone(),
                status: "formatted".to_string(),
                detail: None,
            });
        }

        let mut index_stale: Option<String> = None;
        // Tier-1 perf fix (docs/plans/2026-08-02-shadow-txn-connection-
        // consolidation-plan.md §3): reindex and the shadow-tx advance loop
        // below always run together (`changed_paths`/`shadow_tx_ids` are
        // pushed in lockstep in the loop above, so one is empty iff the
        // other is) -- reuse reindex's own connection for the advance loop
        // instead of a separate open_writer call, falling back to an
        // independent open only if reindex's own open itself failed (same
        // redundancy the original code had for that one case).
        let mut reindex_conn: Option<rusqlite::Connection> = None;
        if !changed_paths.is_empty() {
            match calm_core::db::conn::open_writer(&self.db_path) {
                Err(e) => index_stale = Some(format!("could not open DB to reindex: {e}")),
                Ok(mut write_conn) => {
                    if let Err(e) = calm_core::indexer::pipeline::reindex_paths(
                        &mut write_conn,
                        &self.project_root,
                        &changed_paths,
                    ) {
                        index_stale = Some(format!("reindex failed: {e}"));
                    }
                    reindex_conn = Some(write_conn);
                }
            }
        }
        if !shadow_tx_ids.is_empty() {
            let conn =
                reindex_conn.or_else(|| calm_core::db::conn::open_writer(&self.db_path).ok());
            if let Some(conn) = conn {
                let (to, reason): (calm_core::txn::TxState, String) = match &index_stale {
                    None => (
                        calm_core::txn::TxState::IndexCommitted,
                        "base index refreshed (batched format)".to_string(),
                    ),
                    Some(detail) => (calm_core::txn::TxState::Failed, detail.clone()),
                };
                // Tier 2 Option B (docs/plans/2026-08-02-shadow-txn-connection-
                // consolidation-plan.md §5.2): batch the SAME state transition
                // across every independent tx_id into ONE transaction instead of
                // one advance() call each -- safe because each tx_id is a fully
                // independent business object, so batching them changes nothing
                // about any single tx_id's own crash-recovery story (unlike
                // batching DIFFERENT states for the SAME tx_id, which §5.0 found
                // breaks the crash-injection suite's guarantee -- not done here).
                let first_pass: Vec<(&str, calm_core::txn::TxState, &str, &str)> = shadow_tx_ids
                    .iter()
                    .map(|tx_id| (tx_id.as_str(), to, "system", reason.as_str()))
                    .collect();
                let first_results = calm_core::txn::advance_many(&conn, &first_pass);
                for (tx_id, result) in shadow_tx_ids.iter().zip(first_results.iter()) {
                    if let Err(e) = result {
                        tracing::warn!(
                            "shadow txn::advance to {to:?} failed (non-blocking) for {tx_id}: {e}"
                        );
                    }
                }
                if to == calm_core::txn::TxState::IndexCommitted {
                    let done_tx_ids: Vec<&str> = shadow_tx_ids
                        .iter()
                        .zip(first_results.iter())
                        .filter(|(_, r)| r.is_ok())
                        .map(|(tx_id, _)| tx_id.as_str())
                        .collect();
                    if !done_tx_ids.is_empty() {
                        let done_pass: Vec<(&str, calm_core::txn::TxState, &str, &str)> =
                            done_tx_ids
                                .iter()
                                .map(|tx_id| {
                                    (
                                        *tx_id,
                                        calm_core::txn::TxState::Done,
                                        "system",
                                        "base index committed, disk+index consistent",
                                    )
                                })
                                .collect();
                        let done_results = calm_core::txn::advance_many(&conn, &done_pass);
                        for (tx_id, result) in done_tx_ids.iter().zip(done_results.iter()) {
                            if let Err(e) = result {
                                tracing::warn!(
                                    "shadow txn::advance to Done failed (non-blocking) for \
                                     {tx_id}: {e}"
                                );
                            }
                        }
                    }
                }
            }
        }
        drop(_cross_guard);
        drop(_guard);

        let suggested_next = if changed_paths.is_empty() {
            None
        } else {
            self.filter_sn(suggested_gated(
                "diff_impact",
                "Formatting wrote to disk — diff_impact should report no symbol-level changes, only style",
            ))
        };

        ToolOutcome::success(FormatFilesOutput {
            results,
            index_stale,
            suggested_next,
        })
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
    #[allow(clippy::too_many_arguments)]
    fn edit_lines_impl_gated(
        &self,
        path: &str,
        hunks: Vec<calm_core::edit::HunkRequest>,
        confirm: bool,
        reason: Option<&str>,
        position_anchored: bool,
        extra_note: Option<String>,
        gate: ElicitGate,
        ask_out: &mut Option<HubAskContext>,
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
            // 2026-07-14 backlog B1: insertion modes can carry their own
            // warning computed by the caller (e.g. insertion_hunk_for's
            // doc-comment-sandwich note) -- distinct from the hash-ambiguity
            // note below, which only applies to line-range replace hunks.
            extra_note
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
                tx_id: None,
                note: Some(note),
                suggested_next: None,
            });
        }
        let new_content = outcome.new_content.expect("all_applied implies Some");
        let dogfood_note =
            calm_core::is_own_running_binary_source(&self.project_root, path).then(|| {
                "this edit touched crates/ Rust source that IS the binary currently serving this \
             MCP session — the running daemon will not reflect it until it's rebuilt and \
             reconnected (the file on disk is correct now, this session's live tool behavior \
             just won't show the change yet)"
                    .to_string()
            });

        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let parse_status = match calm_core::edit::validate_syntax_diff(&original, &new_content, ext)
        {
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

        let (
            risk,
            hub_hit,
            hub_kind,
            bridge_downgrade_eligible,
            uncertain_zero_caller,
            pre_touched,
            fresh_caller_digests,
        ) = {
            let conn = match self.make_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let ranges: Vec<(i64, i64)> = hunks
                .iter()
                .map(|h| (h.start_line as i64, h.end_line as i64))
                .collect();
            let coverage = self.coverage.read_ok();
            let (risk, hub_hit, hub_kind, uncertain_zero_caller, touched) =
                compute_touch_risk(&conn, path, &ranges, &coverage);
            // Plan 3 §3.3 (F10): a bridge-only touch (never degree/both) at
            // risk ≤ medium MAY use the lighter CONFIRM_REQUIRED-only tier
            // below — but ONLY if every touched hub's caller edges are all
            // resolved/formal confidence (see all_caller_edges_confident's
            // doc comment for why textual/ambiguous callers disqualify it
            // regardless of hub_kind: the true blast radius can exceed the
            // counted caller_count). Never eligible when
            // `uncertain_zero_caller` is set -- that signal means the real
            // caller is invisible to the graph entirely (or the coverage/
            // dead-code heuristic disagrees it's safe), not just under-
            // confident about a caller edge that does exist.
            let eligible = hub_kind.as_deref() == Some("bridge")
                && risk.as_deref() != Some("high")
                && uncertain_zero_caller.is_none()
                && all_caller_edges_confident(
                    &conn,
                    &touched
                        .iter()
                        .filter(|t| t.hub_kind.is_some())
                        .map(|t| t.qualified_name.clone())
                        .collect::<Vec<_>>(),
                );
            // WS-2 Phase 2 (docs/plans/2026-08-02-phase2-priority-and-ws2-
            // execution-plan.md §5): fresh caller-set digest per touched
            // symbol, computed now while `conn` is already open (same DB
            // snapshot the risk classification above used) so the
            // freshness-check loop below can detect TOCTOU drift without a
            // second connection. Only, and always, from `call_edges` —
            // never derived from `touched`'s own risk fields, so it can
            // never accidentally agree with a stale review just because
            // both happened to read the same risk classification.
            let fresh_caller_digests: std::collections::HashMap<String, String> = touched
                .iter()
                .map(|t| {
                    let live_callers = caller_symbol_set(&conn, &t.qualified_name);
                    (
                        t.qualified_name.clone(),
                        Self::caller_set_digest(&live_callers),
                    )
                })
                .collect();
            (
                risk,
                hub_hit,
                hub_kind,
                eligible,
                uncertain_zero_caller,
                touched,
                fresh_caller_digests,
            )
        };
        // `always_require_edit_context` (Config.edit) widens this gate to
        // every touched symbol regardless of risk -- see OrientationConfig/
        // EditConfig's own doc comments (config.rs) for why this exists as a
        // protocol-level, client-agnostic alternative to a Claude-Code-only
        // hook.
        let force_gate_always = self.config().edit.always_require_edit_context;
        let gate_classification = classify_gate(
            hub_hit,
            risk.as_deref(),
            uncertain_zero_caller,
            bridge_downgrade_eligible,
            force_gate_always,
        );
        if gate_classification.will_block_without_confirm {
            let why = gate_classification.why.unwrap_or_default();

            if matches!(
                gate_classification.requirement,
                GateRequirement::ConfirmOnly
            ) {
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
                let mut stale_caller_set: Vec<&str> = Vec::new();
                let mut known_caller_qns: Vec<String> = Vec::new();
                let mut reviewed_risk_levels: Vec<String> = Vec::new();
                for t in &pre_touched {
                    match self.edit_context_review(&t.qualified_name) {
                        Some(r) if now.saturating_sub(r.at) <= FRESHNESS_WINDOW_CALLS => {
                            // WS-2 Phase 2 (docs/plans/2026-08-02-phase2-
                            // priority-and-ws2-execution-plan.md §5): the
                            // call-count window alone can't see an
                            // unrelated incremental edit that changed this
                            // symbol's real caller set since review --
                            // compare against the fresh digest computed
                            // above from live `call_edges` before trusting
                            // the stored review at all.
                            let fresh = fresh_caller_digests
                                .get(t.qualified_name.as_str())
                                .map(String::as_str)
                                .unwrap_or_default();
                            if fresh == r.caller_set_digest {
                                known_caller_qns.extend(r.caller_qns);
                                reviewed_risk_levels.push(r.risk_level);
                            } else {
                                stale_caller_set.push(t.qualified_name.as_str());
                            }
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
                // WS-2 Phase 2 (docs/plans/2026-08-02-phase2-priority-and-
                // ws2-execution-plan.md §5): distinguished from
                // EDIT_CONTEXT_REQUIRED above -- edit_context WAS called
                // for this symbol and is still within the call-count
                // freshness window, but the caller set it saw has since
                // drifted (an unrelated incremental edit added/removed a
                // caller). Fails closed the same shape as "never
                // reviewed" -- a stale answer is not a current one -- but
                // with an accurate message instead of a misleading
                // "call edit_context first" when it already was called.
                if !stale_caller_set.is_empty() {
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code = "STALE_CALLER_SET",
                        path,
                        symbol = stale_caller_set[0],
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                    );
                    return ToolOutcome::error(error_detail(
                        "STALE_CALLER_SET",
                        &format!(
                            "the caller set for \"{}\" changed since edit_context reviewed it \
                             this session (e.g. an unrelated incremental edit added or removed \
                             a caller) — still inside the {FRESHNESS_WINDOW_CALLS}-tool-call \
                             freshness window, but the reviewed caller list is no longer \
                             accurate. Call edit_context(\"{}\") again to get a fresh review \
                             before editing",
                            stale_caller_set[0], stale_caller_set[0]
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
                // WS-2 Phase 1 (docs/plans/2026-08-02-ws2-review-token-execution-plan.md
                // §3.1, closing the confirmed live bypass): when a touched
                // symbol's `known_caller_qns` is empty, the OLD unconditional
                // `!reason.is_empty()` bypass here let a low-effort but
                // non-empty string like "ok" satisfy the gate regardless of
                // WHY there are no callers. `uncertain_zero_caller` (already
                // computed above, same signal edit_context's own risk
                // escalation and the bridge-downgrade eligibility check
                // already trust) distinguishes two very different
                // situations:
                //   - EntryPoint/TestOnly: the system already has a
                //     STRUCTURAL, independently-derived explanation for zero
                //     callers (is_entry_point/is_test are indexer facts, not
                //     the agent's claim) -- still just requires a non-blank
                //     reason, same bar as the confirmed-safe
                //     (uncertain_zero_caller=None) case, deliberately
                //     unchanged. This is exactly this session's own common
                //     dogfooding pattern: editing a zero-caller
                //     `#[tool(...)]` MCP handler after creating it --
                //     is_entry_point=true, permanently 0 static callers by
                //     construction.
                //   - LowConfidence: no structural explanation at all -- the
                //     dead-code heuristic just disagrees this looks safe,
                //     full stop. THIS is the case a free-text reason cannot
                //     manufacture confidence for. Deliberately NOT keyword-
                //     matched against `reason`'s content either (an agent
                //     could learn "always mention entry_point" exactly the
                //     way the old unconditional bypass was trivially
                //     learnable) -- passing now additionally requires an
                //     elicitation round-trip to actually run (Ask -> human
                //     approves -> Approved); when elicitation isn't
                //     configured at all (`ElicitGate::Off`, the default),
                //     there is no such second check available, so this
                //     fails closed with no override.
                let uncertain_empty_caller_needs_review = known_caller_qns.is_empty()
                    && matches!(
                        uncertain_zero_caller,
                        Some(UncertainZeroCallerReason::LowConfidence)
                    );
                // Gate criterion 4 of the "Write-Safety Beta" milestone
                // (docs/plans/2026-08-02-phase1-p0-execution-plan.md §6; design
                // verified in docs/plans/2026-08-02-ws1-enforce-and-critical-risk-
                // execution-plan.md §1): a >10-caller ("high") touch is exactly
                // the tier the master plan calls "critical" and wants an
                // independent approver for. `bridge_downgrade_eligible`
                // (`eligible` above) is structurally false whenever
                // `risk == "high"` (its own `risk.as_deref() != Some("high")`
                // condition, line ~943) -- so this can never conflict with the
                // bridge-only-hub downgrade path. A cited real caller is not,
                // by itself, independent review at this risk tier -- only an
                // actual elicitation round-trip (Ask -> Approved) is, same
                // fail-closed shape as LowConfidence above.
                let high_risk_needs_independent_review = risk.as_deref() == Some("high")
                    && !matches!(gate, ElicitGate::Ask | ElicitGate::Approved);
                let cites_real_signal = if known_caller_qns.is_empty() {
                    if uncertain_empty_caller_needs_review || high_risk_needs_independent_review {
                        !reason.is_empty() && matches!(gate, ElicitGate::Ask | ElicitGate::Approved)
                    } else {
                        !reason.is_empty()
                    }
                } else if high_risk_needs_independent_review {
                    false
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
                    let reason_code = if uncertain_empty_caller_needs_review {
                        "UNCERTAIN_ZERO_CALLER"
                    } else if high_risk_needs_independent_review {
                        "HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW"
                    } else {
                        "REASON_NOT_GROUNDED"
                    };
                    tracing::info!(
                        target: crate::telemetry::AUDIT_TARGET,
                        session_id = self.session_id,
                        decision = "denied",
                        reason_code,
                        path,
                        reason,
                        risk = risk.as_deref().unwrap_or("none"),
                        hub_hit,
                    );
                    if uncertain_empty_caller_needs_review {
                        return ToolOutcome::error(error_detail(
                            "UNCERTAIN_ZERO_CALLER",
                            "this symbol has zero confirmed callers AND the dead-code \
                             heuristic disagrees it looks safely removable — a written \
                             reason cannot substitute for that missing confidence. \
                             Enable [edit] elicit_hub_confirm and get human approval via \
                             the elicitation round-trip, or investigate further \
                             (callers/understand) before treating this as safe to edit",
                            true,
                        ));
                    }
                    if high_risk_needs_independent_review {
                        return ToolOutcome::error(error_detail(
                            "HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW",
                            "this symbol has >10 confirmed callers (\"high\" risk) — a \
                             cited reason alone is not independent review at this risk \
                             tier. Enable [edit] elicit_hub_confirm and get human \
                             approval via the elicitation round-trip before treating \
                             this as safe to edit",
                            true,
                        ));
                    }
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

            // Human veto (elicitation — docs/superskills/specs/
            // 2026-07-20-calm-elicitation-hub-edit-confirm.md): every machine
            // check above passed, so this write WOULD proceed. In Ask mode,
            // hand the question context back to the async wrapper (which
            // holds no locks) instead of writing; Approved means the human
            // already said yes to this exact call. Placement inside this
            // hub/high-risk block is what makes non-hub edits never elicit.
            if matches!(gate, ElicitGate::Ask) {
                *ask_out = Some(HubAskContext {
                    why: why.clone(),
                    risk: risk.clone(),
                    hub_kind: hub_kind.clone(),
                    touched: pre_touched
                        .iter()
                        .map(|t| (t.qualified_name.clone(), t.caller_count))
                        .collect(),
                });
                return ToolOutcome::error(error_detail(
                    "ELICITATION_PENDING",
                    "hub edit pending human approval — internal sentinel, never \
                     surfaced to the client (the elicitation round-trip resolves \
                     it)",
                    true,
                ));
            }
        }
        // Shadow-mode WS-1 (docs/plans/2026-08-02-phase1-p0-execution-plan.md
        // §4.4/§4.6 task 4.4): observes the same write this function already
        // performs, never changes its outcome. Every txn:: call below is
        // best-effort -- a failure just logs and shadow_tx_id becomes None,
        // the real edit proceeds exactly as it did before this block
        // existed. `atomic_write` itself is UNCHANGED (still Fast mode) --
        // switching to `atomic_write_with(.., HighAssurance)` is a
        // deliberate later enforce-stage change, not bundled into shadow
        // wiring.
        let project_id = self.project_root.to_string_lossy().into_owned();
        // WS-1 enforce transition (docs/plans/2026-08-02-ws1-enforce-and-
        // critical-risk-execution-plan.md §2): unlike every other txn::/
        // maintenance:: call below (still shadow, non-blocking), failing to
        // even BEGIN the durable transaction journal is fail-closed --
        // refuse the write rather than proceed with no journal at all. This
        // is the one point before any real content has changed on disk;
        // every later step stays non-blocking because a "rollback" once disk
        // has changed would be a materially riskier operation than the
        // failure it would be reacting to.
        //
        // Tier-1 perf fix (docs/plans/2026-08-02-shadow-txn-connection-
        // consolidation-plan.md §3): ONE writer connection for this whole
        // guarded critical section (through the IndexCommitted/Done advance
        // below) instead of a fresh open_writer() at every step -- same
        // "one connection, sequential explicit transactions" pattern
        // txn_crash_harness.rs already uses (and the crash-injection suite
        // already exercises), just applied here too. _guard/_cross_guard are
        // already held for this whole section, so no other CALM writer can
        // be contending with it anyway. Still fail-closed exactly as before:
        // this one open must succeed before anything proceeds.
        let txn_init_failed = |detail: String| -> ToolOutcome<EditLinesOutput> {
            tracing::warn!("txn::begin failed, refusing write (enforce mode): {detail}");
            ToolOutcome::error(error_detail(
                "TRANSACTION_INIT_FAILED",
                &format!(
                    "could not initialize the durable edit-transaction journal -- \
                     refusing to write rather than proceed with no journal at all \
                     ({detail}). This usually indicates a database-level problem (disk \
                     full, permissions, lock contention) that would likely also \
                     affect the write itself"
                ),
                true,
            ))
        };
        let mut shared_conn = match calm_core::db::conn::open_writer(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                return txn_init_failed(format!("could not open DB for transaction init: {e}"));
            }
        };
        let base_digest = calm_core::digest::evidence_digest(original.as_bytes());
        let proposed_digest = calm_core::digest::evidence_digest(new_content.as_bytes());
        let shadow_tx_id: Option<String> = match calm_core::txn::begin(
            &shared_conn,
            &project_id,
            path,
            &base_digest,
            &proposed_digest,
        ) {
            Ok(tx) => Some(tx.tx_id),
            Err(e) => return txn_init_failed(e.to_string()),
        };

        if let Err(e) = calm_core::edit::atomic_write(&full_path, &new_content) {
            if let Some(tx_id) = &shadow_tx_id {
                let _ = calm_core::txn::advance(
                    &shared_conn,
                    tx_id,
                    calm_core::txn::TxState::Failed,
                    "system",
                    &e.to_string(),
                );
            }
            drop(_cross_guard);
            drop(_guard);
            return ToolOutcome::error(error_detail(
                "WRITE_FAILED",
                &format!("failed to write {path}: {e}"),
                false,
            ));
        }
        if let Some(tx_id) = &shadow_tx_id {
            let _ = calm_core::txn::advance(
                &shared_conn,
                tx_id,
                calm_core::txn::TxState::FileCommitted,
                "system",
                "atomic_write succeeded",
            );
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
                human_approved = matches!(gate, ElicitGate::Approved),
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
        {
            let reindex_start = std::time::Instant::now();
            let reindex_result = calm_core::indexer::pipeline::reindex_paths(
                &mut shared_conn,
                &self.project_root,
                &[path.to_string()],
            );
            match reindex_result {
                Ok(summary) if !summary.is_noop() => {
                    // Phase B T6.5: record which rebuild path this edit's
                    // reindex took (surfaced by indexing_status.graph_mode)
                    // and log the reindex+graph duration on its own — the
                    // acceptance number the plan tracks ("reindex+graph <
                    // 150ms"), isolated here from the surrounding
                    // write/lock/serialize cost that timed_tool's overall
                    // duration_ms folds in.
                    let mode = summary.graph_mode.label();
                    *self.last_graph_mode.write_ok() = Some(mode.clone());
                    tracing::info!(
                        reindex_ms = reindex_start.elapsed().as_millis(),
                        graph_mode = %mode,
                        path = %path,
                        "edit_reindex_completed"
                    );
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
                        // WS-1 durable outbox (plan
                        // §4.1b/§4.3/§4.6 task 4.5): records the
                        // trigger before spawning so a crash between
                        // here and the thread completing still leaves
                        // a 'queued'/'running' row for startup
                        // recovery to find -- run_all_coalesced itself
                        // is UNCHANGED, still fire-and-forget, still
                        // self-coalescing via its own in-memory flags.
                        // mark_completed after it returns is an honest
                        // "the call returned", not "every language's
                        // pass is proven fresh" -- a concurrent
                        // caller's own OVERLAY_RERUN loop can still be
                        // running a rerun that covers this trigger
                        // when this returns; that's inherent to
                        // run_all_coalesced's existing design, not
                        // something this wrapper changes or hides.
                        // Tier-1 perf fix (docs/plans/2026-08-02-shadow-txn-
                        // connection-consolidation-plan.md §3): reuses
                        // `shared_conn` (already open for this whole critical
                        // section) instead of a separate open_writer call here.
                        let _ = calm_core::maintenance::enqueue(
                            &shared_conn,
                            calm_core::maintenance::MaintenanceKind::ScipRefresh,
                            shadow_tx_id.as_deref(),
                        );
                        std::thread::spawn(move || {
                            if let Ok(conn) = calm_core::db::conn::open_writer(&db) {
                                let _ = calm_core::maintenance::mark_running(
                                    &conn,
                                    calm_core::maintenance::MaintenanceKind::ScipRefresh,
                                );
                            }
                            crate::scip_overlay::run_all_coalesced(&root, &db);
                            if let Ok(conn) = calm_core::db::conn::open_writer(&db) {
                                let _ = calm_core::maintenance::mark_completed(
                                    &conn,
                                    calm_core::maintenance::MaintenanceKind::ScipRefresh,
                                    Ok(()),
                                );
                            }
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => index_stale = Some(format!("reindex failed: {e}")),
            }
        }
        if let Some(tx_id) = &shadow_tx_id {
            let (to, reason): (calm_core::txn::TxState, String) = match &index_stale {
                None => (
                    calm_core::txn::TxState::IndexCommitted,
                    "base index refreshed".to_string(),
                ),
                Some(detail) => (calm_core::txn::TxState::Failed, detail.clone()),
            };
            match calm_core::txn::advance(&shared_conn, tx_id, to, "system", &reason) {
                Ok(()) if to == calm_core::txn::TxState::IndexCommitted => {
                    // WS-6 first slice (docs/plans/2026-08-03-ws6-verification-
                    // pipeline-execution-plan.md): opt-in (config default
                    // false, so this is a no-op for anyone who hasn't turned
                    // it on) and Rust-only for now. When applicable, land at
                    // VerifyPending instead of Done -- the first real
                    // producer of that transition, which existed as a legal
                    // `allowed_next` target since WS-1 but nothing ever
                    // emitted it. `verify_change(tx_id)` is the tool that
                    // picks it up from here and advances it to Done/Failed.
                    let should_verify = self.config().verification.rust_check_on_write
                        && calm_core::verify::is_verifiable_rust_file(&full_path);
                    let next = if should_verify {
                        calm_core::txn::TxState::VerifyPending
                    } else {
                        calm_core::txn::TxState::Done
                    };
                    let next_reason = if should_verify {
                        "base index committed, disk+index consistent, awaiting verify_change"
                    } else {
                        "base index committed, disk+index consistent"
                    };
                    let _ =
                        calm_core::txn::advance(&shared_conn, tx_id, next, "system", next_reason);
                }
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!("shadow txn::advance to {to:?} failed (non-blocking): {e}")
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
            // WS-1 durable outbox (plan §4.1b/§4.3/§4.6 task 4.5) -- same
            // rationale as the SCIP wrapper above: enqueue before
            // spawning, mark_completed after; embed_pending/
            // embed_pending_chunks themselves are UNCHANGED and already
            // idempotent/resumable.
            if let Ok(conn) = calm_core::db::conn::open_writer(&self.db_path) {
                let _ = calm_core::maintenance::enqueue(
                    &conn,
                    calm_core::maintenance::MaintenanceKind::EmbedRefresh,
                    shadow_tx_id.as_deref(),
                );
            }
            std::thread::spawn(move || {
                let _bg_guard = EMBED_BG.lock_ok();
                if let Ok(conn) = calm_core::db::conn::open_writer(&db_path) {
                    let _ = calm_core::maintenance::mark_running(
                        &conn,
                        calm_core::maintenance::MaintenanceKind::EmbedRefresh,
                    );
                }
                let mut error_detail: Option<String> = None;
                match calm_core::db::conn::open_writer(&db_path) {
                    Ok(bg_conn) => {
                        if let Err(e) =
                            calm_core::embedding::embed_pending(&bg_conn, model.as_ref())
                        {
                            tracing::error!("edit_lines: background embedding failed: {e}");
                            error_detail = Some(e.to_string());
                        }
                        if let Err(e) =
                            calm_core::embedding::embed_pending_chunks(&bg_conn, model.as_ref())
                        {
                            tracing::error!("edit_lines: background chunk embedding failed: {e}");
                            error_detail.get_or_insert_with(|| e.to_string());
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "edit_lines: could not open DB for background embedding: {e}"
                        );
                        error_detail = Some(e.to_string());
                    }
                }
                if let Ok(conn) = calm_core::db::conn::open_writer(&db_path) {
                    let result = match &error_detail {
                        None => Ok(()),
                        Some(detail) => Err(detail.as_str()),
                    };
                    let _ = calm_core::maintenance::mark_completed(
                        &conn,
                        calm_core::maintenance::MaintenanceKind::EmbedRefresh,
                        result,
                    );
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
            if let Some(d) = &dogfood_note {
                note.push_str(". ");
                note.push_str(d);
            }
            return ToolOutcome::success(EditLinesOutput {
                path: path.to_string(),
                applied: true,
                hunks: hunks_output,
                parse_status: Some(parse_status.to_string()),
                touched_symbols: vec![],
                risk_assessment: risk,
                index_stale: Some(true),
                tx_id: shadow_tx_id.clone(),
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
                        tx_id: shadow_tx_id.clone(),
                        note: match &dogfood_note {
                            Some(d) => Some(format!(
                                "edit applied but could not re-query touched symbols. {d}"
                            )),
                            None => {
                                Some("edit applied but could not re-query touched symbols".into())
                            }
                        },
                        suggested_next: None,
                    });
                }
            };
            let new_ranges: Vec<(i64, i64)> = outcome
                .results
                .iter()
                .map(|r| (r.start_line as i64, r.new_end_line as i64))
                .collect();
            let coverage = self.coverage.read_ok();
            let (_, _, _, _, touched) = compute_touch_risk(&conn, path, &new_ranges, &coverage);
            touched
        };

        let note = match (&ambiguity_note, &dogfood_note) {
            (Some(a), Some(d)) => Some(format!("{a}. {d}")),
            (Some(a), None) => Some(a.clone()),
            (None, Some(d)) => Some(d.clone()),
            (None, None) => None,
        };
        ToolOutcome::success(EditLinesOutput {
            path: path.to_string(),
            applied: true,
            hunks: hunks_output,
            parse_status: Some(parse_status.to_string()),
            touched_symbols,
            risk_assessment: risk,
            index_stale: None,
            tx_id: shadow_tx_id.clone(),
            note,
            suggested_next: self.filter_sn(suggested_gated(
                "diff_impact",
                "Verify wider blast radius, especially if this touched a hub/high-risk symbol",
            )),
        })
    }
}

/// How the hub/high-risk gate interacts with the human-elicitation veto
/// (docs/superskills/specs/2026-07-20-calm-elicitation-hub-edit-confirm.md).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ElicitGate {
    /// Elicitation inactive (config off, or the client never declared the
    /// capability): the machine gate alone decides, exactly as before.
    Off,
    /// Elicitation active: a write that passes the machine gate on a
    /// hub/high-risk touch returns the ELICITATION_PENDING sentinel instead
    /// of writing, so the async wrapper can ask the human first.
    Ask,
    /// The human approved this exact call — write, and audit-log it.
    Approved,
}

/// Question context the gated impl hands back at the sentinel point —
/// everything the human needs for the veto to be decision-relevant
/// (audit-design Ab2).
pub(crate) struct HubAskContext {
    why: String,
    risk: Option<String>,
    hub_kind: Option<String>,
    /// `(qualified_name, caller_count)` of every touched symbol.
    touched: Vec<(String, i64)>,
}

/// Typed answer the human's client returns for the hub-edit veto question.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct HubEditApproval {
    /// true = allow this edit to be written; false = refuse it.
    approve: bool,
}
rmcp::elicit_safe!(HubEditApproval);

impl CalmServer {
    /// `Some(timeout)` when the human-veto flow is active for this
    /// connection: `[edit] elicit_hub_confirm` opted in AND the client
    /// declared form-mode elicitation (MCP 2025-06-18 requires clients to
    /// declare it at initialize). `None` = `ElicitGate::Off`, byte-identical
    /// legacy behavior — by construction the veto can only ADD a refusal on
    /// top of the machine gate, never remove one (spec Option A).
    fn elicit_setup(&self, peer: &rmcp::Peer<rmcp::RoleServer>) -> Option<std::time::Duration> {
        let cfg = self.config().edit;
        if !cfg.elicit_hub_confirm {
            return None;
        }
        if !peer
            .supported_elicitation_modes()
            .contains(&rmcp::service::ElicitationMode::Form)
        {
            return None;
        }
        Some(std::time::Duration::from_secs(cfg.elicit_timeout_secs))
    }

    /// One human-veto round-trip: declined-cache short-circuit, sanitized
    /// question, `elicit_with_timeout`, decision mapping, audit logging.
    /// `Ok(())` = approved; every other outcome is a fail-closed refusal.
    #[allow(clippy::too_many_arguments)]
    async fn hub_elicit_roundtrip(
        &self,
        peer: &rmcp::Peer<rmcp::RoleServer>,
        tool: &str,
        cache_path: &str,
        fingerprint: &str,
        ask: &HubAskContext,
        reason: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<(), ErrorDetail> {
        if self.elicit_declined_contains(cache_path, fingerprint) {
            return Err(error_detail(
                "USER_DECLINED",
                "a human already declined this exact edit this session — do not \
                 retry it; surface their veto and let them decide the next step",
                false,
            ));
        }
        let message = build_hub_elicit_message(tool, cache_path, ask, reason);
        tracing::info!(
            target: crate::telemetry::AUDIT_TARGET,
            session_id = self.session_id,
            decision = "elicit_asked",
            tool,
            path = cache_path,
        );
        let started = std::time::Instant::now();
        let result = peer
            .elicit_with_timeout::<HubEditApproval>(message, Some(timeout))
            .await;
        let (verdict, mapped) = map_elicit_outcome(result);
        tracing::info!(
            target: crate::telemetry::AUDIT_TARGET,
            session_id = self.session_id,
            decision = verdict,
            tool,
            path = cache_path,
            elapsed_ms = started.elapsed().as_millis() as u64,
        );
        if verdict == "elicit_declined" {
            self.elicit_declined_insert(cache_path, fingerprint);
        }
        mapped
    }
}

/// Pure decision-table mapping — unit-testable without a live peer. Returns
/// the audit verdict label plus the tool-facing result. Fail-closed: only an
/// explicit accept carrying `approve: true` lets the write proceed.
fn map_elicit_outcome(
    result: Result<Option<HubEditApproval>, rmcp::service::ElicitationError>,
) -> (&'static str, Result<(), ErrorDetail>) {
    use rmcp::service::ElicitationError as E;
    let declined = || {
        error_detail(
            "USER_DECLINED",
            "the human reviewing this session refused this hub edit — do not \
             retry; surface their veto and let them decide the next step",
            false,
        )
    };
    match result {
        Ok(Some(HubEditApproval { approve: true })) => ("elicit_approved", Ok(())),
        Ok(Some(HubEditApproval { approve: false })) | Ok(None) => {
            ("elicit_declined", Err(declined()))
        }
        Err(E::UserDeclined) | Err(E::UserCancelled) => ("elicit_declined", Err(declined())),
        Err(E::Service(rmcp::ServiceError::Timeout { .. })) => (
            "elicit_timeout",
            Err(error_detail(
                "ELICITATION_TIMEOUT",
                "no human answered the hub-edit approval question in time — \
                 nothing was written (fail-closed). If this session is headless \
                 (CI, batch agents), turn off `elicit_hub_confirm` under `edit` \
                 in .calm/config.json instead of retrying",
                false,
            )),
        ),
        Err(_) => (
            "elicit_failed",
            Err(error_detail(
                "ELICITATION_FAILED",
                "the elicitation round-trip to the client failed — nothing was \
                 written (fail-closed). The client declared elicitation support \
                 but could not complete it; check the client, or turn off \
                 `elicit_hub_confirm` under `edit` in .calm/config.json",
                false,
            )),
        ),
    }
}

/// Builds the human-facing question. `reason` is agent-authored text about
/// to cross into a human approval UI — run through the same redaction layer
/// as source output and hard-capped, per audit FM3 (the reason field must
/// not become an injection surface against the approver).
fn build_hub_elicit_message(
    tool: &str,
    path: &str,
    ask: &HubAskContext,
    reason: Option<&str>,
) -> String {
    let mut msg = format!(
        "CALM hub-edit approval — the agent wants {tool} to modify {path}: it \
         touches {}",
        ask.why
    );
    if let Some(k) = &ask.hub_kind {
        msg.push_str(&format!(", hub_kind={k}"));
    }
    if let Some(r) = &ask.risk {
        msg.push_str(&format!(", risk={r}"));
    }
    let mut touched = ask.touched.clone();
    touched.sort_by_key(|t| std::cmp::Reverse(t.1));
    for (qn, callers) in touched.iter().take(3) {
        msg.push_str(&format!("\n- {qn} ({callers} callers)"));
    }
    let sanitized = calm_core::sanitize::sanitize_source_output(reason.unwrap_or("(none given)"));
    let capped: String = sanitized.chars().take(400).collect();
    msg.push_str(&format!("\nAgent's stated reason: {capped}"));
    msg.push_str(
        "\nApprove this write? (approve=false, declining, or ignoring this \
         refuses the edit)",
    );
    msg
}

/// Content fingerprint for the per-session declined-cache — keyed by what
/// would actually be written, never by path alone (audit L7: changed content
/// is a NEW question and must re-ask; the identical retry must not re-harass
/// the human).
fn fingerprint_edit_lines(p: &EditLinesParams) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.path.hash(&mut h);
    for e in &p.edits {
        e.start_line.hash(&mut h);
        e.end_line.hash(&mut h);
        e.expected_hash.hash(&mut h);
        e.old_text.hash(&mut h);
        e.new_text.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// See `fingerprint_edit_lines` — same contract for `edit_symbol` params.
fn fingerprint_edit_symbol(p: &EditSymbolParams) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.symbol.hash(&mut h);
    p.path.hash(&mut h);
    p.line.hash(&mut h);
    p.position.hash(&mut h);
    p.expected_hash.hash(&mut h);
    p.old_text.hash(&mut h);
    p.new_text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// One `symbols` row overlapping an edit's touched ranges — enough fields
/// to compute both the raw caller_count/hub risk tier and (when
/// `caller_count == 0`) the same `is_entry_point`-aware dead-code signal
/// `edit_context`'s advisory risk assessment already uses, so
/// `compute_touch_risk`'s hard write-gate can see it too.
struct OverlappingSymbolRow {
    qualified_name: String,
    caller_count: i64,
    is_hub: bool,
    hub_kind: Option<String>,
    line_start: i64,
    line_end: i64,
    is_entry_point: bool,
    is_test: bool,
    language: String,
    name: String,
    signature: String,
    kind: String,
}

/// Symbols in `path` whose `[line_start, line_end]` overlaps any of `ranges`
/// — shared by the pre-write risk gate (against original ranges) and the
/// post-write response (against the edited ranges' new positions).
fn symbols_overlapping_ranges(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
) -> Vec<OverlappingSymbolRow> {
    let mut stmt = match conn.prepare(
        "SELECT qualified_name, caller_count, is_hub, hub_kind, line_start, line_end, \
         is_entry_point, is_test, language, name, signature, kind \
         FROM symbols WHERE path = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(rusqlite::params![path], |row| {
        Ok(OverlappingSymbolRow {
            qualified_name: row.get(0)?,
            caller_count: row.get(1)?,
            is_hub: row.get::<_, i64>(2)? != 0,
            hub_kind: row.get(3)?,
            line_start: row.get(4)?,
            line_end: row.get(5)?,
            is_entry_point: row.get::<_, i64>(6)? != 0,
            is_test: row.get::<_, i64>(7)? != 0,
            language: row.get(8)?,
            name: row.get(9)?,
            signature: row.get(10)?,
            kind: row.get(11)?,
        })
    })
    .map(|it| {
        it.filter_map(|r| r.ok())
            .filter(|r| {
                ranges
                    .iter()
                    .any(|&(rs, re)| !(r.line_end < rs || r.line_start > re))
            })
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
/// `(risk_level, hub_hit, strongest_hub_kind, entry_point_uncertain,
/// touched_symbols)` for whatever symbols in `path` overlap `ranges`.
/// `risk_level` is `None` when nothing overlaps (editing dead space
/// between symbols, or a file with no parsed symbols at all — Cargo.toml,
/// docs) — that's not an error, just nothing to gate on.
/// `strongest_hub_kind` is `None` when nothing touched is a hub,
/// `Some("bridge")` only when every touched hub is bridge-only, and
/// `Some("degree")`/`Some("both")` if any touched hub is stronger than
/// bridge-only (see `hub_kind_strength`). `entry_point_uncertain` is `true`
/// when a touched symbol has `caller_count == 0` AND the same dead-code
/// heuristic `edit_context` uses disagrees it looks safely removable —
/// `is_entry_point` (a framework/macro-registered handler, e.g. an rmcp
/// `#[tool(name = "...")]` MCP method) is the primary trigger, since its
/// real caller is invisible to the static call graph by construction, so
/// `caller_count == 0` can't be read as "safe" the way it can for an
/// ordinary non-entry-point symbol.
/// `(risk_level, hub_hit, strongest_hub_kind, uncertain_zero_caller,
/// touched_symbols)` for whatever symbols in `path` overlap `ranges`.
/// `risk_level` is `None` when nothing overlaps (editing dead space
/// between symbols, or a file with no parsed symbols at all — Cargo.toml,
/// docs) — that's not an error, just nothing to gate on.
/// `strongest_hub_kind` is `None` when nothing touched is a hub,
/// `Some("bridge")` only when every touched hub is bridge-only, and
/// `Some("degree")`/`Some("both")` if any touched hub is stronger than
/// bridge-only (see `hub_kind_strength`). `uncertain_zero_caller` is
/// `Some(reason)` when a touched **function or method** has
/// `caller_count == 0` AND the same dead-code heuristic `edit_context`
/// uses disagrees it looks safely removable — see
/// `classify_uncertain_zero_caller` for what `reason` distinguishes.
/// Deliberately gated on `kind` being `"function"`/`"method"`:
/// `compute_dead_code_confidence` returns `"none"` for every other kind
/// (the dead-code question isn't well-formed for a struct/enum/etc. — see
/// its own doc comment: "confirmed: 100% of this repo's own struct
/// symbols have caller_count=0") — that `"none"` is a vacuous "not
/// applicable", not a "confirmed safe" signal, so counting it here would
/// force the full write gate on nearly every struct/enum edit in this
/// codebase for no real reason.
pub(crate) fn compute_touch_risk(
    conn: &rusqlite::Connection,
    path: &str,
    ranges: &[(i64, i64)],
    coverage: &calm_core::analysis::coverage::CoverageData,
) -> (
    Option<String>,
    bool,
    Option<String>,
    Option<UncertainZeroCallerReason>,
    Vec<TouchedSymbolOutput>,
) {
    let rows = symbols_overlapping_ranges(conn, path, ranges);
    let mut max_callers = 0i64;
    let mut hub_hit = false;
    let mut strongest_hub_kind: Option<String> = None;
    let mut uncertain_zero_caller: Option<UncertainZeroCallerReason> = None;
    let mut touched = Vec::with_capacity(rows.len());
    for row in rows {
        max_callers = max_callers.max(row.caller_count);
        hub_hit |= row.is_hub;
        if let Some(k) = &row.hub_kind {
            let stronger = strongest_hub_kind
                .as_deref()
                .is_none_or(|cur| hub_kind_strength(k) > hub_kind_strength(cur));
            if stronger {
                strongest_hub_kind = Some(k.clone());
            }
        }
        if row.caller_count == 0 && matches!(row.kind.as_str(), "function" | "method") {
            let is_private = calm_core::analysis::dead_code::is_private_symbol(
                &row.language,
                &row.name,
                &row.signature,
            );
            let scope_clear =
                calm_core::analysis::dead_code::scope_clear_for_language(&row.language);
            let (dead_code_confidence, _) =
                calm_core::analysis::dead_code::compute_dead_code_confidence(
                    path,
                    row.line_start,
                    row.line_end,
                    row.caller_count,
                    row.is_entry_point,
                    row.is_test,
                    is_private,
                    scope_clear,
                    coverage,
                    &row.kind,
                );
            if let Some(reason) = classify_uncertain_zero_caller(
                row.is_entry_point,
                row.is_test,
                dead_code_confidence,
            ) {
                let stronger = uncertain_zero_caller.is_none_or(|cur| {
                    uncertain_zero_caller_strength(reason) > uncertain_zero_caller_strength(cur)
                });
                if stronger {
                    uncertain_zero_caller = Some(reason);
                }
            }
        }
        touched.push(TouchedSymbolOutput {
            qualified_name: row.qualified_name,
            caller_count: row.caller_count,
            is_hub: row.is_hub,
            hub_kind: row.hub_kind,
        });
    }
    let risk = (!touched.is_empty()).then(|| risk_level_from_caller_count(max_callers).to_string());
    (
        risk,
        hub_hit,
        strongest_hub_kind,
        uncertain_zero_caller,
        touched,
    )
}

/// Which tier of the `edit_lines`/`edit_symbol` write gate a touched range
/// needs, and why — the single source of truth shared by the gate itself
/// (`edit_lines_impl_gated`) and `edit_context`'s `gate_prediction` field, so
/// the two can never drift (UPGRADE_PLAN.md FIX2/F2b). Deliberately excludes
/// session-state (whether `edit_context` already ran this session for the
/// touched symbols, whether a `reason` string cites a real caller) — those
/// two checks stay runtime-only, decided in `edit_lines_impl_gated` itself;
/// this only decides WHICH structural tier would apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GateRequirement {
    /// No gate — the touch is low-risk enough to write freely.
    None,
    /// Bridge-only hub, risk ≤ medium, every known caller edge
    /// resolved/formal confidence (see `all_caller_edges_confident`):
    /// `confirm: true` alone is enough, no `edit_context`/grounded `reason`.
    ConfirmOnly,
    /// The full 3-layer gate: `edit_context` must have run THIS session for
    /// every touched symbol, `confirm: true`, and `reason` must cite a real
    /// caller name from that review.
    EditContextConfirmGroundedReason,
}

impl GateRequirement {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            GateRequirement::None => "none",
            GateRequirement::ConfirmOnly => "confirm",
            GateRequirement::EditContextConfirmGroundedReason => {
                "edit_context+confirm+grounded_reason"
            }
        }
    }
}

pub(crate) struct GateClassification {
    /// `true` iff a write with `confirm: false` would be rejected — i.e.
    /// `requirement != GateRequirement::None`. Independent of whether the
    /// runtime session-state checks (`edit_context` freshness, grounded
    /// `reason`) would additionally block a `confirm: true` attempt.
    pub(crate) will_block_without_confirm: bool,
    pub(crate) requirement: GateRequirement,
    /// Human-readable cause (`"a hub symbol (is_hub=true)"`, etc.) — `None`
    /// only when `requirement == GateRequirement::None`.
    pub(crate) why: Option<String>,
}

/// Pure classification — see [`GateRequirement`]'s doc comment. Mirrors
/// `edit_lines_impl_gated`'s gate condition exactly; any change to that
/// gate's structural logic (not its session-state checks) must be made here
/// so both call sites stay in sync.
pub(crate) fn classify_gate(
    hub_hit: bool,
    risk: Option<&str>,
    uncertain_zero_caller: Option<UncertainZeroCallerReason>,
    bridge_downgrade_eligible: bool,
    force_gate_always: bool,
) -> GateClassification {
    if !(hub_hit || risk == Some("high") || uncertain_zero_caller.is_some() || force_gate_always) {
        return GateClassification {
            will_block_without_confirm: false,
            requirement: GateRequirement::None,
            why: None,
        };
    }
    let why = if hub_hit {
        "a hub symbol (is_hub=true)".to_string()
    } else if let Some(reason) = uncertain_zero_caller {
        match reason {
            UncertainZeroCallerReason::EntryPoint => {
                "a zero-confirmed-caller entry point (e.g. an rmcp #[tool(name = \"...\")] MCP handler, main, a trait-dispatch protocol method, a bodyless trait method declaration, or similar framework/macro/language dispatch -- the real invocation isn't visible to the static call graph, so a low caller_count can't be trusted as low blast radius)".to_string()
            }
            UncertainZeroCallerReason::TestOnly => {
                "a zero-confirmed-caller test-only symbol (only the test harness discovers and runs it by convention/reflection, not a literal call site -- editing it risks silently breaking test coverage the static call graph can't see)".to_string()
            }
            UncertainZeroCallerReason::LowConfidence => {
                "a zero-confirmed-caller symbol the dead-code heuristic isn't confident is safe to treat as unused (e.g. runtime coverage shows it executing despite no static callers) -- treat the zero caller_count as inconclusive, not proof of low blast radius".to_string()
            }
        }
    } else if risk == Some("high") {
        "a high-risk symbol (>10 callers)".to_string()
    } else {
        "this project's `edit.always_require_edit_context` config (every edit requires edit_context first, regardless of risk)".to_string()
    };
    let requirement = if bridge_downgrade_eligible {
        GateRequirement::ConfirmOnly
    } else {
        GateRequirement::EditContextConfirmGroundedReason
    };
    GateClassification {
        will_block_without_confirm: true,
        requirement,
        why: Some(why),
    }
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
pub(crate) fn all_caller_edges_confident(
    conn: &rusqlite::Connection,
    qualified_names: &[String],
) -> bool {
    if qualified_names.is_empty() {
        return false;
    }
    let placeholders = qualified_names
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    // PATTERN-DEBT call-edges-missing-ruled-out-filter: without
    // `ruled_out_by_scip = 0`, `total` counted edges SCIP has already
    // disproven — they're never real callers, but they inflated the
    // denominator without ever counting toward `confident` (their own
    // edge_confidence was never 'resolved'/'formal' to begin with, since
    // ruled_out_by_scip only ever fires on the fan-out SIBLINGS of a
    // confirmed candidate). Net effect was a false-negative direction (this
    // function under-reports confidence, forcing the heavier 3-layer gate
    // even when every REAL caller edge is confident) rather than an unsafe
    // permissive one, but it's still wrong: a symbol whose only caller
    // edges were N confident ones plus M since-disproven ones reported
    // `false` here instead of `true`.
    let sql = format!(
        "SELECT COUNT(*), SUM(CASE WHEN edge_confidence IN ('resolved','formal') THEN 1 ELSE 0 END) \
         FROM call_edges WHERE to_symbol IN ({placeholders}) AND ruled_out_by_scip = 0"
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

/// WS-2 Phase 2 (docs/plans/2026-08-02-phase2-priority-and-ws2-execution-
/// plan.md §5): the distinct set of caller symbols for `qualified_name`,
/// read fresh from `call_edges` — same `to_symbol`/`ruled_out_by_scip`
/// filter `edit_context` itself uses (guardrails.rs) to build the caller
/// list it digests at review time, so a caller set digested from this
/// function's output is directly comparable to that stored digest.
/// `DISTINCT`/`ORDER BY` here are for cheap determinism only, since
/// `CalmServer::caller_set_digest` dedupes and sorts again regardless —
/// this query shape just avoids handing it a redundant list for nothing.
/// A query failure returns an empty set: the fail-closed direction, same
/// convention as `all_caller_edges_confident` above — a digest mismatch
/// against whatever was stored at review time is the safe outcome when we
/// can't confirm freshness, not a silent pass.
pub(crate) fn caller_symbol_set(conn: &rusqlite::Connection, qualified_name: &str) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT DISTINCT from_symbol FROM call_edges \
         WHERE to_symbol = ?1 AND ruled_out_by_scip = 0 \
         ORDER BY from_symbol",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    match stmt.query_map(rusqlite::params![qualified_name], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
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
    // Delegates to calm_core::path_policy (WS-3 task 3.3/3.4) under
    // FollowInternalSymlinks -- the exact containment check this function
    // always performed inline before that module existed. Error codes and
    // messages below are reproduced byte-for-byte so all 6 callers
    // (edit_lines_flow, edit_symbol_flow x2, format_files_impl,
    // edit_lines_impl_gated, insertion_hunk_for) see zero behavior change.
    calm_core::path_policy::resolve_within_root(
        project_root,
        path,
        calm_core::path_policy::SymlinkPolicy::FollowInternalSymlinks,
    )
    .map_err(|e| match e {
        calm_core::path_policy::PathPolicyError::ReadFailed { path, detail } => error_detail(
            "READ_FAILED",
            &format!("could not read {path}: {detail}"),
            false,
        ),
        calm_core::path_policy::PathPolicyError::EscapesRoot { path } => error_detail(
            "PATH_ESCAPES_PROJECT_ROOT",
            &format!(
                "{path} resolves outside the project root (via `..` traversal or a symlink) \
                 — refusing to read or write it"
            ),
            false,
        ),
        // FollowInternalSymlinks never produces these two -- they're only
        // reachable under RejectSymlinks/AllowExternalSymlinksWithApproval,
        // neither of which is wired into this call site yet.
        calm_core::path_policy::PathPolicyError::SymlinkRejected { path, .. }
        | calm_core::path_policy::PathPolicyError::NeedsApproval { path } => error_detail(
            "PATH_ESCAPES_PROJECT_ROOT",
            &format!("{path}: unexpected path policy result under FollowInternalSymlinks"),
            false,
        ),
    })
}

fn insertion_hunk_for(
    project_root: &std::path::Path,
    c: &CandidateRow,
    position: calm_core::edit::InsertPosition,
    new_text: &str,
) -> Result<(calm_core::edit::HunkRequest, Option<String>), ErrorDetail> {
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
    // Root-cause fix (2026-07-14, replaces the former backlog-B1 warning-only
    // mitigation): `Before` used to always anchor at the symbol's own
    // line_start, which never includes a leading doc comment (a separate
    // tree-sitter sibling node -- see walk_symbols, crates/calm-core/src/
    // indexer/parser.rs:587) -- sandwiching new_text BETWEEN the comment and
    // the symbol, silently leaving the comment describing whatever was just
    // inserted instead of its original target. `leading_doc_comment_start`
    // scans the already-read live file text (no schema change, no DB column
    // -- the "doc_start_line field" previously deferred as the only real fix
    // turns out unnecessary since this function already re-reads the file)
    // for a contiguous doc-comment block directly above with no blank-line
    // gap, and moves the actual insertion anchor above it. A residual
    // warning remains only for what this can't cover: an attribute/
    // annotation (`#[derive(...)]`, a decorator, ...) sitting between the
    // comment and the symbol as its own preceding sibling node in a grammar
    // that doesn't fold it into the item's span the way tree-sitter-rust
    // does for `#[...]`.
    let live_lines: Vec<&str> = live.lines().collect();
    let anchor_line_start = if matches!(position, calm_core::edit::InsertPosition::Before) {
        leading_doc_comment_start(&live_lines, &c.language, line_start)
    } else {
        line_start
    };
    let sandwich_warning = if matches!(position, calm_core::edit::InsertPosition::Before)
        && !c.docstring.trim().is_empty()
        && anchor_line_start == line_start
    {
        Some(format!(
            "heads up — '{}' has a leading doc comment this anchor could not locate directly \
             above it (e.g. an attribute/annotation line sits between them) — position=\"before\" \
             inserted between the comment and '{}', not above the comment, so the comment may \
             now describe the newly-inserted code instead. If the comment should stay with \
             '{}', include your own comment in new_text, or use edit_lines to insert above the \
             comment's own line.",
            c.name, c.name, c.name
        ))
    } else {
        None
    };
    let hunk =
        calm_core::edit::insertion_hunk(&live, anchor_line_start, line_end, position, new_text)
            .ok_or_else(|| {
                error_detail(
                    "INVALID_RANGE",
                    &format!(
                        "resolved range {anchor_line_start}..{line_end} is out of bounds for {} \
                         on disk",
                        c.path
                    ),
                    true,
                )
            })?;
    Ok((hunk, sandwich_warning))
}

/// Scans upward from a symbol's own first line (1-indexed, as returned by a
/// fresh parse) to find where an immediately-preceding doc-comment block
/// begins, so `Before` insertion can anchor above the comment instead of
/// between it and the symbol. Two forms recognized: (a) a contiguous run of
/// single-line markers (Rust `///`/`//!`, `#` for Python/Ruby, `//` for the
/// C-family/JS/TS/Java/C#/Go/Kotlin/Swift/Scala) with no blank line breaking
/// the run; (b) a `/* ... */`/`/** ... */` block comment on the line(s)
/// directly above, found by scanning upward for its opening `/*` (assumes
/// non-nested block comments — true for every grammar this workspace
/// indexes). Returns `symbol_start` unchanged if neither form sits
/// immediately above — a comment separated by a blank line isn't "leading"
/// in the sense that matters for sandwiching, and this deliberately doesn't
/// guess through an attribute/annotation line (see `insertion_hunk_for`'s
/// doc comment on that residual gap).
fn leading_doc_comment_start(lines: &[&str], language: &str, symbol_start: usize) -> usize {
    if symbol_start < 2 || lines.is_empty() {
        return symbol_start;
    }
    let above_idx = symbol_start - 2;

    if lines[above_idx].trim().ends_with("*/") {
        let mut i = above_idx;
        loop {
            if lines[i].trim_start().contains("/*") {
                return i + 1;
            }
            if i == 0 {
                return symbol_start;
            }
            i -= 1;
        }
    }

    let markers: &[&str] = match language {
        "rust" => &["///", "//!"],
        "python" | "ruby" => &["#"],
        _ => &["//"],
    };
    let is_marker_line = |s: &str| markers.iter().any(|m| s.trim().starts_with(m));

    let mut top = above_idx;
    loop {
        if !is_marker_line(lines[top]) {
            return top + 2;
        }
        if top == 0 {
            return 1;
        }
        top -= 1;
    }
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
    /// a first call can learn the hash before a real edit. Ignored when
    /// `old_text` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected_hash: Option<String>,
    /// Small-text-match mode: when set, `new_text` replaces the FIRST (and
    /// required-to-be-only) occurrence of `old_text` found within
    /// `[start_line, end_line]`, instead of requiring `expected_hash` for
    /// that exact sub-range. No hash needed and no preview round trip —
    /// the server reads the file's live content to find the match, so
    /// staleness is impossible by construction, same guarantee
    /// `edit_symbol`'s own `old_text` mode already provides for a resolved
    /// symbol range. The intended fix for the common "read a wide range for
    /// context, then edit one narrow line inside it" case: `[start_line,
    /// end_line]` can stay exactly the wide range just read (no new hash
    /// needed for it either — this mode doesn't check one), while
    /// `old_text` pins down the one exact spot to change. Refused with
    /// `MATCH_NOT_FOUND`/`AMBIGUOUS_MATCH` if the text isn't found exactly
    /// once in that window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) old_text: Option<String>,
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
    /// Durable edit-transaction id (WS-1). `txn::begin` is fail-closed as of
    /// v0.5.0 (docs/plans/2026-08-02-ws1-enforce-and-critical-risk-
    /// execution-plan.md §2) -- absent only when nothing was written at all,
    /// never because the journal itself silently failed to start. Later
    /// transitions (FileCommitted -> IndexCommitted -> Done) stay
    /// best-effort by design (see tools/txn.rs's module comment for why).
    /// Look it up with `edit_transaction_status`/`repair_consistency` if
    /// something about this edit looks wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct FormatFilesParams {
    /// Repo-relative paths to format. Only `.rs` files are supported today
    /// (rustfmt is Rust-specific) — a non-Rust path comes back as
    /// `skipped_unsupported_extension` in the corresponding result, not a
    /// tool error, so it's safe to pass a mixed-language batch.
    pub(crate) paths: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FormatFileResult {
    pub(crate) path: String,
    /// "formatted" | "already_formatted" | "skipped_unsupported_extension" | "error".
    pub(crate) status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct FormatFilesOutput {
    pub(crate) results: Vec<FormatFileResult>,
    /// Set only if at least one file was reformatted but the post-write
    /// index refresh failed — same meaning as `EditLinesOutput::index_stale`,
    /// carrying the failure detail directly instead of a separate bool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) index_stale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[cfg(test)]
mod elicit_tests {
    use super::*;

    fn approval(approve: bool) -> Result<Option<HubEditApproval>, rmcp::service::ElicitationError> {
        Ok(Some(HubEditApproval { approve }))
    }

    #[test]
    fn map_elicit_outcome_approve_true_is_the_only_ok() {
        let (verdict, mapped) = map_elicit_outcome(approval(true));
        assert_eq!(verdict, "elicit_approved");
        assert!(mapped.is_ok());
    }

    #[test]
    fn map_elicit_outcome_approve_false_and_empty_accept_decline() {
        for res in [approval(false), Ok(None)] {
            let (verdict, mapped) = map_elicit_outcome(res);
            assert_eq!(verdict, "elicit_declined");
            assert_eq!(mapped.unwrap_err().code, "USER_DECLINED");
        }
    }

    #[test]
    fn map_elicit_outcome_decline_and_cancel_are_user_declined() {
        use rmcp::service::ElicitationError as E;
        for e in [E::UserDeclined, E::UserCancelled] {
            let (verdict, mapped) = map_elicit_outcome(Err(e));
            assert_eq!(verdict, "elicit_declined");
            assert_eq!(mapped.unwrap_err().code, "USER_DECLINED");
        }
    }

    #[test]
    fn map_elicit_outcome_timeout_names_the_off_switch() {
        let err = rmcp::service::ElicitationError::Service(rmcp::ServiceError::Timeout {
            timeout: std::time::Duration::from_secs(1),
        });
        let (verdict, mapped) = map_elicit_outcome(Err(err));
        assert_eq!(verdict, "elicit_timeout");
        let detail = mapped.unwrap_err();
        assert_eq!(detail.code, "ELICITATION_TIMEOUT");
        // Audit FM2/Ab1: the refusal must point at the config off-switch so
        // a headless session's operator can fix the setup instead of
        // retry-looping into repeated 120s hangs.
        assert!(
            detail.message.contains("elicit_hub_confirm"),
            "{}",
            detail.message
        );
    }

    #[test]
    fn map_elicit_outcome_other_errors_fail_closed() {
        let err = rmcp::service::ElicitationError::CapabilityNotSupported;
        let (verdict, mapped) = map_elicit_outcome(Err(err));
        assert_eq!(verdict, "elicit_failed");
        assert_eq!(mapped.unwrap_err().code, "ELICITATION_FAILED");
    }

    fn lines_params(new_text: &str) -> EditLinesParams {
        EditLinesParams {
            path: "a.py".into(),
            edits: vec![EditHunkParam {
                start_line: 2,
                end_line: 2,
                expected_hash: Some("abc".into()),
                old_text: None,
                new_text: new_text.into(),
            }],
            confirm: true,
            reason: Some("r".into()),
        }
    }

    #[test]
    fn fingerprint_tracks_content_not_just_path() {
        // Audit L7: identical params must dedup, changed content must NOT
        // (a new question deserves a fresh ask — identity-reuse ≠ safe-to-
        // dedup).
        let a = fingerprint_edit_lines(&lines_params("    return 2\n"));
        let b = fingerprint_edit_lines(&lines_params("    return 2\n"));
        let c = fingerprint_edit_lines(&lines_params("    return 3\n"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn elicit_message_caps_the_reason_and_keeps_context() {
        let ask = HubAskContext {
            why: "a hub symbol (is_hub=true)".into(),
            risk: Some("high".into()),
            hub_kind: Some("degree".into()),
            touched: vec![("a.py::helper".into(), 12), ("a.py::other".into(), 3)],
        };
        let long_reason = "x".repeat(5000);
        let msg = build_hub_elicit_message("edit_lines", "a.py", &ask, Some(&long_reason));
        // Audit FM3: hard cap — the 5000-char reason must not reach the
        // human's approval UI at full length.
        assert!(
            msg.len() < 1200,
            "message unexpectedly long: {} chars",
            msg.len()
        );
        assert!(msg.contains("a.py::helper (12 callers)"));
        assert!(msg.contains("hub_kind=degree"));
        assert!(msg.contains("risk=high"));
    }
}
