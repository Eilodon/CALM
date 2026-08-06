use super::common::*;
use super::*;

// ---------------------------------------------------------------------------
// WS-1 durable edit-transaction / maintenance-outbox admin tools
// (docs/plans/2026-08-02-phase1-p0-execution-plan.md §4.7). All 4 are
// read/diagnostic (plus retry_maintenance's explicit force-retry) on top of
// the durable journal edit_lines/edit_symbol/format_files write through
// (tools/edit.rs) -- none of these 4 gate anything themselves.
//
// As of 2026-08-03 (v0.5.0, docs/plans/2026-08-02-ws1-enforce-and-critical-
// risk-execution-plan.md §2): starting the journal (`txn::begin`) is now
// fail-closed -- a write is refused rather than proceeding with no journal
// at all, so "no write path can bypass EditTransaction" now holds. Later
// transitions (FileCommitted -> IndexCommitted -> Done) remain deliberately
// best-effort/non-blocking BY DESIGN, not as a leftover gap: once disk has
// actually changed, refusing to finish recording that is a materially
// riskier "rollback" than tolerating a journal/disk disagreement that
// repair_consistency can detect and report afterward. Separately (a
// different gate, not this journal), a >10-caller ("critical") edit without
// an independent approver is blocked outright -- see classify_gate/
// GateRequirement in tools/edit.rs.
//
// Split out of `recover_tool_router` into their own toolset (2026-08-02,
// docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md#part-1):
// unlike indexing_status/session_context -- the real stuck-session escape
// hatch `recover`'s SAFETY_FLOOR_TOOLSETS membership exists for -- none of
// these 4 tools are themselves a gate mechanism a write path depends on
// being reachable, so they remain deliberately excluded from the
// non-disableable floor. See toolset.rs's `TOOLSET_NAMES`/
// `SAFETY_FLOOR_TOOLSETS` doc comments for the floor's actual membership
// test.
// ---------------------------------------------------------------------------

#[rmcp::tool_router(router = "txn_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "edit_transaction_status",
        description = "USE WHEN: you have a tx_id (from maintenance_status/repair_consistency, or the tx_id an edit_lines/edit_symbol/format_files response already returns) and want to see what state that specific edit transaction reached. Read-only -- does not repair anything, see repair_consistency for that.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn edit_transaction_status(
        &self,
        Parameters(p): Parameters<EditTransactionStatusParams>,
    ) -> Json<ToolOutcome<EditTransactionStatusOutput>> {
        Json(self.timed_tool("edit_transaction_status", || {
            let conn = match self.make_state_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let tx = match calm_core::txn::get(&conn, &p.tx_id) {
                Ok(Some(tx)) => tx,
                Ok(None) => {
                    return ToolOutcome::error(error_detail(
                        "TX_NOT_FOUND",
                        &format!("no edit transaction with tx_id {}", p.tx_id),
                        false,
                    ));
                }
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "TX_JOURNAL_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };
            let replay_state = calm_core::txn::replay_state(&conn, &p.tx_id)
                .ok()
                .map(|s| s.as_str().to_string());
            let sn = if tx.state == calm_core::txn::TxState::Failed {
                suggested(
                    "repair_consistency",
                    "Transaction failed -- repair_consistency can check disk/index drift",
                )
            } else {
                None
            };
            ToolOutcome::success(EditTransactionStatusOutput {
                tx_id: tx.tx_id,
                path: tx.path,
                state: tx.state.as_str().to_string(),
                replay_state,
                base_digest: tx.base_digest,
                proposed_digest: tx.proposed_digest,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    #[tool(
        name = "batch_status",
        description = "USE WHEN: you made several edit_lines/edit_symbol/format_files calls as one multi-file change (each already returned its own tx_id) and want ONE aggregate view of how the whole change-set landed, instead of calling edit_transaction_status once per tx_id. Read-only, observability only -- this does not group or track a change-set server-side (see KNOWN_LIMITATIONS.md \"No multi-file change-set\"); pass the tx_id list you already collected from each write's own response.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn batch_status(
        &self,
        Parameters(p): Parameters<BatchStatusParams>,
    ) -> Json<ToolOutcome<BatchStatusOutput>> {
        Json(self.timed_tool("batch_status", || {
            let conn = match self.make_state_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let mut by_state: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            let mut not_found = Vec::new();
            let mut transactions = Vec::new();
            let mut any_failed = false;
            for tx_id in &p.tx_ids {
                match calm_core::txn::get(&conn, tx_id) {
                    Ok(Some(tx)) => {
                        if tx.state == calm_core::txn::TxState::Failed {
                            any_failed = true;
                        }
                        *by_state.entry(tx.state.as_str().to_string()).or_insert(0) += 1;
                        transactions.push(BatchStatusEntry {
                            tx_id: tx.tx_id,
                            path: tx.path,
                            state: tx.state.as_str().to_string(),
                        });
                    }
                    Ok(None) => not_found.push(tx_id.clone()),
                    Err(e) => {
                        return ToolOutcome::error(error_detail(
                            "TX_JOURNAL_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                }
            }
            let all_done = not_found.is_empty()
                && !transactions.is_empty()
                && by_state.get(calm_core::txn::TxState::Done.as_str()).copied().unwrap_or(0)
                    == transactions.len();
            let sn = if any_failed {
                suggested(
                    "repair_consistency",
                    "At least one transaction in this batch failed -- repair_consistency can check disk/index drift",
                )
            } else {
                None
            };
            ToolOutcome::success(BatchStatusOutput {
                total: p.tx_ids.len(),
                by_state,
                not_found,
                all_done,
                any_failed,
                transactions,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    #[tool(
        name = "maintenance_status",
        description = "USE WHEN: you want to check whether the background SCIP-overlay/embedding refresh outbox (WS-1, plan §4.1b) has a stuck or failed job. Read-only, global status -- each of scip_refresh/embed_refresh has at most one current row, not one per transaction.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn maintenance_status(&self) -> Json<ToolOutcome<MaintenanceStatusOutput>> {
        Json(self.timed_tool("maintenance_status", || {
            let conn = match self.make_state_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let jobs = match calm_core::maintenance::all_jobs(&conn) {
                Ok(j) => j,
                Err(e) => return ToolOutcome::error(error_detail("DB_ERROR", &e.to_string(), true)),
            };
            let jobs_out: Vec<MaintenanceJobOutput> =
                jobs.into_iter().map(MaintenanceJobOutput::from).collect();
            let sn = if jobs_out.iter().any(|j| j.state == "failed") {
                suggested(
                    "retry_maintenance",
                    "A maintenance job is in failed state -- retry_maintenance(job_kind) re-queues it",
                )
            } else {
                None
            };
            ToolOutcome::success(MaintenanceStatusOutput {
                jobs: jobs_out,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    #[tool(
        name = "retry_maintenance",
        description = "USE WHEN: maintenance_status shows a job_kind (\"scip_refresh\" or \"embed_refresh\") stuck at running/failed and you want to force a fresh pass right now, bypassing the normal edit-triggered enqueue. Runs the real refresh inline (not backgrounded) -- can block for a while, same cost as scip_refresh/a full embedding pass, so this is for explicit recovery, not routine use.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub(crate) fn retry_maintenance(
        &self,
        Parameters(p): Parameters<RetryMaintenanceParams>,
    ) -> Json<ToolOutcome<RetryMaintenanceOutput>> {
        Json(self.timed_tool("retry_maintenance", || {
            let Some(kind) = calm_core::maintenance::MaintenanceKind::parse(&p.job_kind) else {
                return ToolOutcome::error(error_detail(
                    "UNKNOWN_JOB_KIND",
                    &format!(
                        "job_kind must be \"scip_refresh\" or \"embed_refresh\", got {:?}",
                        p.job_kind
                    ),
                    false,
                ));
            };
            match calm_core::db::conn::open_state_writer(&self.state_db_path) {
                Ok(conn) => {
                    if let Err(e) = calm_core::maintenance::force_requeue(&conn, kind) {
                        return ToolOutcome::error(error_detail("DB_ERROR", &e.to_string(), true));
                    }
                    let _ = calm_core::maintenance::mark_running(&conn, kind);
                }
                Err(e) => return db_error(e),
            }
            // `conn` dropped here -- the real refresh below runs inline (a
            // rare, explicit, user-initiated action, same posture as
            // scip_refresh) without holding a writer connection through it;
            // a fresh connection is opened again just for mark_completed.
            let result: Result<(), String> = match kind {
                calm_core::maintenance::MaintenanceKind::ScipRefresh => {
                    #[cfg(feature = "scip-overlay")]
                    {
                        // Audit 3.3: same reasoning as the edit.rs SCIP spawn
                        // -- if this explicit retry call merely deferred to
                        // an already-in-flight leader elsewhere (another
                        // process's edit triggered one concurrently), it did
                        // no real work of its own and must not report success
                        // here; the caller sees this as a request that's
                        // "covered by the in-flight pass", not a failure.
                        let led = crate::scip_overlay::run_all_coalesced(
                            &self.project_root,
                            &self.db_path,
                        );
                        if led {
                            Ok(())
                        } else {
                            Err(
                                "deferred to an already-in-flight scip_refresh pass elsewhere \
                                 (likely another process/session) -- that pass covers this \
                                 request too; check maintenance_status shortly instead of \
                                 retrying immediately"
                                    .to_string(),
                            )
                        }
                    }
                    #[cfg(not(feature = "scip-overlay"))]
                    {
                        Err("this build wasn't compiled with the scip-overlay feature".to_string())
                    }
                }
                calm_core::maintenance::MaintenanceKind::EmbedRefresh => match self.embedder() {
                    Some(model) => match calm_core::db::conn::open_writer(&self.db_path) {
                        Ok(embed_conn) => {
                            let r1 =
                                calm_core::embedding::embed_pending(&embed_conn, model.as_ref());
                            let r2 = calm_core::embedding::embed_pending_chunks(
                                &embed_conn,
                                model.as_ref(),
                            );
                            match (r1, r2) {
                                (Ok(_), Ok(_)) => Ok(()),
                                (Err(e), _) => Err(e.to_string()),
                                (_, Err(e)) => Err(e.to_string()),
                            }
                        }
                        Err(e) => Err(e.to_string()),
                    },
                    None => Err(
                        "no embedding model loaded (semantic_search disabled or not ready)"
                            .to_string(),
                    ),
                },
            };
            let outcome_for_db: Result<(), &str> = match &result {
                Ok(()) => Ok(()),
                Err(e) => Err(e.as_str()),
            };
            if let Ok(conn) = calm_core::db::conn::open_state_writer(&self.state_db_path) {
                let _ = calm_core::maintenance::mark_completed(&conn, kind, outcome_for_db);
            }
            match result {
                Ok(()) => ToolOutcome::success(RetryMaintenanceOutput {
                    job_kind: kind.as_str().to_string(),
                    ok: true,
                    detail: None,
                    suggested_next: self.filter_sn(suggested(
                        "maintenance_status",
                        "Verify the job now shows done",
                    )),
                }),
                Err(detail) => {
                    ToolOutcome::error(error_detail("MAINTENANCE_RETRY_FAILED", &detail, true))
                }
            }
        }))
    }

    #[tool(
        name = "repair_consistency",
        description = "USE WHEN: edit_transaction_status/maintenance_status show something suspicious and you want to check whether a transaction's replayed state agrees with its cached state, and whether disk content still matches what the transaction proposed. Accepts tx_id or path (path resolves to that path's most recent transaction). Read-only diagnostic -- flags drift, does not silently auto-fix it.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn repair_consistency(
        &self,
        Parameters(p): Parameters<RepairConsistencyParams>,
    ) -> Json<ToolOutcome<RepairConsistencyOutput>> {
        Json(self.timed_tool("repair_consistency", || {
            if p.tx_id.is_none() && p.path.is_none() {
                return ToolOutcome::error(error_detail(
                    "MISSING_TARGET",
                    "at least one of tx_id or path is required",
                    false,
                ));
            }
            let conn = match self.make_state_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let tx_lookup = if let Some(tx_id) = &p.tx_id {
                calm_core::txn::get(&conn, tx_id)
            } else {
                calm_core::txn::latest_for_path(&conn, p.path.as_deref().unwrap_or_default())
            };
            let tx = match tx_lookup {
                Ok(Some(tx)) => tx,
                Ok(None) => {
                    return ToolOutcome::error(error_detail(
                        "TX_NOT_FOUND",
                        "no matching edit transaction found for the given tx_id/path",
                        false,
                    ));
                }
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "TX_JOURNAL_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };
            let replay_state = match calm_core::txn::replay_state(&conn, &tx.tx_id) {
                Ok(s) => s,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "TX_JOURNAL_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };
            let cache_matches_replay = replay_state == tx.state;
            let full_path = self.project_root.join(&tx.path);
            let disk_digest = std::fs::read(&full_path)
                .ok()
                .map(|bytes| calm_core::digest::evidence_digest(&bytes));
            let disk_matches_proposed = disk_digest.as_deref() == Some(tx.proposed_digest.as_str());
            let needs_rescan = !cache_matches_replay
                || (tx.state == calm_core::txn::TxState::Done && !disk_matches_proposed);
            let sn = if needs_rescan {
                suggested(
                    "indexing_status",
                    "Consistency drift detected -- consider a manual reindex",
                )
            } else {
                None
            };
            ToolOutcome::success(RepairConsistencyOutput {
                tx_id: tx.tx_id,
                path: tx.path,
                cached_state: tx.state.as_str().to_string(),
                replayed_state: replay_state.as_str().to_string(),
                cache_matches_replay,
                disk_digest,
                proposed_digest: tx.proposed_digest,
                disk_matches_proposed,
                needs_rescan,
                suggested_next: self.filter_sn(sn),
            })
        }))
    }

    #[tool(
        name = "verify_change",
        description = "USE WHEN: you have a tx_id (from an edit_lines/edit_symbol response) and want to actually run cargo check on that change instead of just trusting applied:true. Only does anything if [verification] rust_check_on_write is enabled in .calm/config.json AND the file is .rs -- WS-6 first slice (docs/plans/2026-08-03-ws6-verification-pipeline-execution-plan.md), other languages/tiers not implemented yet. Runs cargo check inline, not backgrounded (same posture as retry_maintenance) -- can take a while on a large package. A tx_id that was never routed through verification (feature off, non-Rust file, or already resolved) returns a clear \"nothing to verify\" result, not an error. A failing check does NOT roll back the file already on disk.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub(crate) fn verify_change(
        &self,
        Parameters(p): Parameters<VerifyChangeParams>,
    ) -> Json<ToolOutcome<VerifyChangeOutput>> {
        Json(self.timed_tool("verify_change", || {
            let conn = match self.make_state_read_conn() {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            let tx = match calm_core::txn::get(&conn, &p.tx_id) {
                Ok(Some(tx)) => tx,
                Ok(None) => {
                    return ToolOutcome::error(error_detail(
                        "TX_NOT_FOUND",
                        &format!("no edit transaction with tx_id {}", p.tx_id),
                        false,
                    ));
                }
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "TX_JOURNAL_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };
            let state = match calm_core::txn::replay_state(&conn, &p.tx_id) {
                Ok(s) => s,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "TX_JOURNAL_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };

            if state != calm_core::txn::TxState::VerifyPending {
                return ToolOutcome::success(VerifyChangeOutput {
                    tx_id: tx.tx_id,
                    path: tx.path,
                    tier: "none".to_string(),
                    verified: None,
                    state: state.as_str().to_string(),
                    diagnostics: Vec::new(),
                    command: None,
                    note: format!(
                        "nothing to verify -- this transaction is at {} (verification either \
                         wasn't enabled for it, its file isn't a supported language, or it was \
                         already resolved)",
                        state.as_str()
                    ),
                    suggested_next: None,
                });
            }

            let full_path = match calm_core::path_policy::resolve_within_root(
                &self.project_root,
                &tx.path,
                calm_core::path_policy::SymlinkPolicy::FollowInternalSymlinks,
            ) {
                Ok(p) => p,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "PATH_RESOLUTION_FAILED",
                        &format!("{e:?}"),
                        true,
                    ));
                }
            };

            if !calm_core::verify::is_verifiable_rust_file(&full_path) {
                return ToolOutcome::success(VerifyChangeOutput {
                    tx_id: tx.tx_id,
                    path: tx.path,
                    tier: "unsupported".to_string(),
                    verified: None,
                    state: state.as_str().to_string(),
                    diagnostics: Vec::new(),
                    command: None,
                    note: "only Rust (.rs) files are supported today (WS-6 first slice)"
                        .to_string(),
                    suggested_next: None,
                });
            }

            let Some(manifest_path) =
                calm_core::verify::find_nearest_cargo_toml(&full_path, &self.project_root)
            else {
                return ToolOutcome::error(error_detail(
                    "NO_CARGO_MANIFEST",
                    &format!("no Cargo.toml found above {}", tx.path),
                    true,
                ));
            };

            // Bind verification to the exact content this tx_id proposed --
            // without this, `verify_change` would run `cargo check` on
            // whatever happens to be on disk right now and bind a PASS/FAIL
            // receipt to `tx_id` regardless of who wrote that content or
            // when (a native editor, another agent, `git checkout`, ...).
            // Checked both before AND after the check itself: `cargo check`
            // on a large package can run for seconds to minutes, plenty of
            // time for a concurrent write to land mid-run and make the
            // result describe content nobody asked to verify under this
            // tx_id.
            let pre_check_digest = match std::fs::read(&full_path) {
                Ok(bytes) => calm_core::digest::evidence_digest(&bytes),
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "VERIFICATION_SNAPSHOT_UNREADABLE",
                        &format!("failed to read {} before verification: {e}", tx.path),
                        true,
                    ));
                }
            };
            if pre_check_digest != tx.proposed_digest {
                return ToolOutcome::error(error_detail(
                    "VERIFICATION_SNAPSHOT_CHANGED",
                    &format!(
                        "disk content at {} no longer matches this transaction's \
                         proposed_digest -- something wrote to it after edit_lines/edit_symbol \
                         produced tx_id {}. Refusing to bind a verification receipt to content \
                         this transaction never proposed. Use repair_consistency to inspect \
                         the drift, or start a fresh edit_context/edit_lines cycle for the \
                         content that's actually on disk now.",
                        tx.path, tx.tx_id
                    ),
                    true,
                ));
            }

            let timeout =
                std::time::Duration::from_secs(self.config().verification.timeout_secs);
            let result = match calm_core::verify::run_cargo_check(&manifest_path, timeout) {
                Ok(r) => r,
                Err(e) => {
                    return ToolOutcome::error(error_detail("CARGO_SPAWN_FAILED", &e, true));
                }
            };

            let post_check_digest = match std::fs::read(&full_path) {
                Ok(bytes) => calm_core::digest::evidence_digest(&bytes),
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "VERIFICATION_SNAPSHOT_UNREADABLE",
                        &format!("failed to read {} after verification: {e}", tx.path),
                        true,
                    ));
                }
            };
            if post_check_digest != pre_check_digest {
                return ToolOutcome::error(error_detail(
                    "VERIFICATION_SNAPSHOT_CHANGED",
                    &format!(
                        "disk content at {} changed while cargo check was still running -- \
                         the {} result does not describe a stable snapshot, so it was \
                         discarded rather than recorded against tx_id {}. Re-run verify_change \
                         once the concurrent write has settled.",
                        tx.path,
                        if result.passed { "passing" } else { "failing" },
                        tx.tx_id
                    ),
                    true,
                ));
            }

            let (to, advance_reason) = if result.passed {
                (
                    calm_core::txn::TxState::Done,
                    "cargo check passed".to_string(),
                )
            } else {
                (
                    calm_core::txn::TxState::Failed,
                    format!(
                        "cargo check failed: {} diagnostic(s)",
                        result.diagnostics.len()
                    ),
                )
            };
            let writer = match calm_core::db::conn::open_state_writer(&self.state_db_path) {
                Ok(c) => c,
                Err(e) => return db_error(e),
            };
            if let Err(e) =
                calm_core::txn::advance(&writer, &p.tx_id, to, "system", &advance_reason)
            {
                return ToolOutcome::error(error_detail("TX_JOURNAL_ERROR", &e.to_string(), true));
            }

            let sn = if result.passed {
                None
            } else {
                suggested(
                    "repair_consistency",
                    "Verification failed -- the file on disk was NOT rolled back; fix the code or use repair_consistency to inspect the transaction",
                )
            };
            ToolOutcome::success(VerifyChangeOutput {
                tx_id: tx.tx_id,
                path: tx.path,
                tier: "semantic:cargo_check".to_string(),
                verified: Some(result.passed),
                state: to.as_str().to_string(),
                diagnostics: result.diagnostics,
                command: Some(result.command),
                note: if result.passed {
                    "cargo check passed".to_string()
                } else {
                    "cargo check failed -- see diagnostics; disk content was not reverted"
                        .to_string()
                },
                suggested_next: self.filter_sn(sn),
            })
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditTransactionStatusParams {
    /// tx_id from maintenance_status/repair_consistency output, or the
    /// tx_id an edit_lines/edit_symbol/format_files response already
    /// returns.
    pub(crate) tx_id: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct EditTransactionStatusOutput {
    pub(crate) tx_id: String,
    pub(crate) path: String,
    pub(crate) state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) replay_state: Option<String>,
    pub(crate) base_digest: String,
    pub(crate) proposed_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct BatchStatusParams {
    /// tx_ids collected from a set of edit_lines/edit_symbol/format_files
    /// responses that together made up one multi-file change.
    pub(crate) tx_ids: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BatchStatusEntry {
    pub(crate) tx_id: String,
    pub(crate) path: String,
    pub(crate) state: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct BatchStatusOutput {
    pub(crate) total: usize,
    pub(crate) by_state: std::collections::BTreeMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) not_found: Vec<String>,
    pub(crate) all_done: bool,
    pub(crate) any_failed: bool,
    pub(crate) transactions: Vec<BatchStatusEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct MaintenanceJobOutput {
    pub(crate) job_kind: String,
    pub(crate) state: String,
    pub(crate) attempts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) triggered_by_tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_completed_at: Option<String>,
}

impl From<calm_core::maintenance::MaintenanceJob> for MaintenanceJobOutput {
    fn from(j: calm_core::maintenance::MaintenanceJob) -> Self {
        Self {
            job_kind: j.kind.as_str().to_string(),
            state: j.state.as_str().to_string(),
            attempts: j.attempts,
            triggered_by_tx_id: j.triggered_by_tx_id,
            last_error: j.last_error,
            last_completed_at: j.last_completed_at.map(epoch_to_iso8601),
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct MaintenanceStatusOutput {
    pub(crate) jobs: Vec<MaintenanceJobOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RetryMaintenanceParams {
    /// "scip_refresh" or "embed_refresh".
    pub(crate) job_kind: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RetryMaintenanceOutput {
    pub(crate) job_kind: String,
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct RepairConsistencyParams {
    /// Exact tx_id to check. At least one of tx_id/path is required.
    #[serde(default)]
    pub(crate) tx_id: Option<String>,
    /// Repo-relative path -- resolves to that path's most recent
    /// transaction. At least one of tx_id/path is required.
    #[serde(default)]
    pub(crate) path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct RepairConsistencyOutput {
    pub(crate) tx_id: String,
    pub(crate) path: String,
    pub(crate) cached_state: String,
    pub(crate) replayed_state: String,
    pub(crate) cache_matches_replay: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disk_digest: Option<String>,
    pub(crate) proposed_digest: String,
    pub(crate) disk_matches_proposed: bool,
    pub(crate) needs_rescan: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct VerifyChangeParams {
    /// tx_id from an edit_lines/edit_symbol response (or
    /// edit_transaction_status/repair_consistency output).
    pub(crate) tx_id: String,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct VerifyChangeOutput {
    pub(crate) tx_id: String,
    pub(crate) path: String,
    /// "none" (nothing to verify -- see `note`), "unsupported" (language
    /// not covered yet), or "semantic:cargo_check" (the one real tier this
    /// first slice has).
    pub(crate) tier: String,
    /// `None` when `tier` is "none"/"unsupported" -- nothing was actually
    /// run, so there's no pass/fail to report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) verified: Option<bool>,
    /// The transaction's state after this call: unchanged when `tier` is
    /// "none"/"unsupported", else DONE or FAILED depending on the check
    /// result.
    pub(crate) state: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) diagnostics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<String>,
    pub(crate) note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}
