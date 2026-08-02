use super::common::*;
use super::*;

// ---------------------------------------------------------------------------
// WS-1 durable edit-transaction / maintenance-outbox admin tools
// (docs/plans/2026-08-02-phase1-p0-execution-plan.md §4.7). All 4 are
// additive on top of the shadow-mode journal edit_lines/edit_symbol/
// format_files already write to (tools/edit.rs) -- none of these change what
// a normal edit does, they only expose what the journal already recorded.
//
// Split out of `recover_tool_router` into their own toolset (2026-08-02,
// docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md#part-1):
// unlike indexing_status/session_context -- the real stuck-session escape
// hatch `recover`'s SAFETY_FLOOR_TOOLSETS membership exists for -- these 4
// tools diagnose a subsystem (WS-1 shadow-mode transactions) that does not
// yet change real write-path behavior, so they should not (yet) be part of
// the non-disableable floor. See toolset.rs's `TOOLSET_NAMES` and
// `SAFETY_FLOOR_TOOLSETS` doc comments for where this toolset is registered
// and why it is deliberately excluded from the floor for now.
// ---------------------------------------------------------------------------

#[rmcp::tool_router(router = "txn_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "edit_transaction_status",
        description = "USE WHEN: you have a tx_id (from maintenance_status/repair_consistency, or a future edit_lines/edit_symbol/format_files response once WS-1 leaves shadow mode) and want to see what state that specific edit transaction reached. Read-only -- does not repair anything, see repair_consistency for that.",
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
            let conn = match self.make_read_conn() {
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
            let conn = match self.make_read_conn() {
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
            match calm_core::db::conn::open_writer(&self.db_path) {
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
                        crate::scip_overlay::run_all_coalesced(&self.project_root, &self.db_path);
                        Ok(())
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
            if let Ok(conn) = calm_core::db::conn::open_writer(&self.db_path) {
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
            let conn = match self.make_read_conn() {
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
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct EditTransactionStatusParams {
    /// tx_id from maintenance_status/repair_consistency output, or a future
    /// edit_lines/edit_symbol/format_files response once WS-1 leaves shadow
    /// mode.
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
