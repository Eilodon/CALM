//! `plan_change` + `review_change` -- CCK-11
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! The structured, reviewable front door onto CCK-10's authority-minting
//! path (`ReviewAuthority::mint`), as an alternative to `edit_context`'s
//! own single-symbol compat wrapper (`mint_review_authority_for_edit_context`
//! in `guardrails.rs`, still the default path for a plain edit).
//!
//! `plan_change` declares a [`calm_core::change::ChangeIntent`] (what a
//! caller says it's about to do, and why) without writing anything or
//! minting authority. `review_change` is the ONLY place in this pair that
//! calls `ReviewAuthority::mint` -- and refuses to do so
//! (`APPROVAL_REQUIRED`) unless `approved: true` is explicitly set, the
//! blueprint's "review_change returns authority_id only after required
//! human/MRTR approval" invariant. Whether that flag was set because a
//! human answered a chat prompt, an MRTR elicitation round-trip completed,
//! or some other out-of-band process approved it is a client-side
//! concern -- CALM's contract is the refusal, not the mechanism, matching
//! how `edit_lines`/`edit_symbol`'s own lighter `confirm: bool` gate
//! already works one tier down.
//!
//! **Known Phase-1 scope limit** (the blueprint's own phasing: CCK-11 is
//! Phase 1, true multi-file `ChangeSet` staging/commit is Phase 2,
//! CCK-13-18): a `ChangeIntent` can already name several targets (ahead of
//! Phase 2 landing, see `change_intent_targets`'s own doc comment), and
//! `review_change` mints ONE authority scoped to all of them via
//! `target_scope_digest` -- but that digest requires an EXACT set match
//! (see `authority::review`'s own doc comment), so the minted authority is
//! only spendable by a SINGLE `edit_lines`/`edit_symbol` call whose touched
//! set matches the declared targets exactly (e.g. several hunks in one
//! file, or an `edit_symbol` call resolving one of several declared
//! symbols isn't enough on its own). Independently spending each target of
//! a genuinely multi-file change is Phase 2 `ChangeSet` territory, not
//! this pair's job.

use super::common::*;
use super::*;

/// One file (optionally symbol-scoped) target of a `plan_change` call --
/// wire shape for `calm_core::change::ChangeIntentTarget`.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ChangeTargetParam {
    pub(crate) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) qualified_name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct PlanChangeParams {
    /// Declared `change::classify::ChangeKind` name: `"add"`, `"delete"`,
    /// `"whitespace"`, `"comment"`, `"doc_only"`, `"visibility"`,
    /// `"signature"`, `"manifest"`, `"test_only"`, or `"body"`.
    pub(crate) kind: String,
    /// Free text for a human/reviewer -- never compared or matched against
    /// to authorize anything (invariant #3); `kind` is the only field
    /// checked against reality (see `kind_mismatch` on the response).
    pub(crate) reason: String,
    /// At least one target. Order doesn't matter for idempotency (see
    /// `plan_change`'s own doc comment) or for the minted authority's
    /// `target_scope_digest`, both of which sort before hashing.
    pub(crate) targets: Vec<ChangeTargetParam>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct PlanChangeOutput {
    /// Pass this to `review_change` once a human/MRTR approves it.
    pub(crate) change_id: String,
    /// `true` when this call matched a prior `plan_change` call's declared
    /// `kind`+`targets` exactly and returned that intent's existing
    /// `change_id` instead of minting a new one.
    pub(crate) reused: bool,
    pub(crate) snapshot_id: String,
    /// `true` when at least one target's current uncommitted diff (working
    /// tree vs. `git show HEAD`) doesn't classify as the declared `kind` --
    /// best-effort: silently `false` for a target with no git history, no
    /// diff yet, or an unreadable file, since there's nothing to compare.
    pub(crate) kind_mismatch: bool,
    /// The first mismatching target's actually-observed kind, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) observed_kind: Option<String>,
    /// One human-readable line per mismatching target.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) mismatch_notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct ReviewChangeParams {
    pub(crate) change_id: String,
    /// The required human/MRTR approval signal -- `review_change` NEVER
    /// mints an authority unless this is explicitly `true` (omitted or
    /// `false` both refuse with `APPROVAL_REQUIRED`). See this module's own
    /// doc comment for what "approval" means here.
    #[serde(default)]
    pub(crate) approved: bool,
    /// Free text audit trail of who/what approved -- like `reason`
    /// elsewhere in this codebase, never itself a permission signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) approver: Option<String>,
    /// Seconds from mint until the authority expires -- defaults to 30
    /// minutes (`DEFAULT_REVIEW_TTL_SECS`), validated by `AuthorityTtl`
    /// (finite, positive, <= 24h).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ttl_secs: Option<f64>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ReviewChangeOutput {
    pub(crate) change_id: String,
    /// Pass this alongside `change_id` to `edit_lines`/`edit_symbol` to
    /// spend it -- single-use, consumed on the first successful write.
    pub(crate) authority_id: String,
    pub(crate) authority_expires_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suggested_next: Option<SuggestedNext>,
}

/// Matches `mint_review_authority_for_edit_context`'s own default
/// (guardrails.rs) -- 30 minutes is long enough to cover a human/MRTR
/// approval round-trip without becoming a long-lived standing credential.
const DEFAULT_REVIEW_TTL_SECS: f64 = 1800.0;

/// Content-addressed key `plan_change` dedups repeated calls on -- order-
/// independent (sorted before hashing) so the same declared kind/targets
/// always produce the same key regardless of the order a caller happened
/// to list targets in, same pattern `authority::review::target_scope_digest`
/// already uses for the same reason.
fn plan_idempotency_key(
    kind: calm_core::change::ChangeKind,
    targets: &[calm_core::change::ChangeIntentTarget],
) -> String {
    let mut canonical: Vec<String> = targets
        .iter()
        .map(|t| format!("{}\u{0}{}", t.path, t.qualified_name.as_deref().unwrap_or("")))
        .collect();
    canonical.sort();
    let material = format!("plan-change-v1\n{}\n{}\n", kind.as_str(), canonical.join("\n"));
    calm_core::digest::evidence_digest(material.as_bytes())
}

/// Best-effort: `None` for a target with no git history yet (new/untracked
/// file), a non-git project, or git simply not on `PATH` -- callers treat
/// that identically to "nothing to compare" (invariant #2 is about a
/// disagreement that IS visible never being silently accepted, not about
/// manufacturing a disagreement where none can be observed).
fn git_show_head(project_root: &std::path::Path, path: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["show", &format!("HEAD:{path}")])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Checks every target's current uncommitted diff (if any) against
/// `declared` -- `(kind_mismatch, first_observed_kind, notes)`. Invariant
/// #2: a declared/observed disagreement must never be silently accepted,
/// so this always runs (not opt-in), but stays advisory (surfaced in the
/// response, not a hard refusal) -- `plan_change` declares an intent, it
/// doesn't gate a write, and a caller may not have touched every target
/// yet at plan time.
fn check_declared_vs_observed(
    project_root: &std::path::Path,
    declared: calm_core::change::ChangeKind,
    targets: &[calm_core::change::ChangeIntentTarget],
) -> (bool, Option<String>, Vec<String>) {
    let mut mismatch_notes = Vec::new();
    let mut first_observed: Option<String> = None;
    for target in targets {
        let full_path = match super::edit::resolve_repo_path(project_root, &target.path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let new_text = match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let Some(old_text) = git_show_head(project_root, &target.path) else {
            continue;
        };
        if old_text == new_text {
            continue;
        }
        let ext = std::path::Path::new(&target.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language = calm_core::indexer::lang_constants::language_for_extension(ext).unwrap_or("");
        let observed = calm_core::change::classify::classify_observed_change(
            &calm_core::change::ObservedChangeInput {
                path: &target.path,
                language,
                is_test: false,
                old_text: Some(&old_text),
                new_text: Some(&new_text),
                old_signature: None,
                new_signature: None,
            },
        );
        if calm_core::change::kinds_mismatch(
            calm_core::change::ChangeIntentKind(declared),
            observed,
        ) {
            mismatch_notes.push(format!(
                "{}: declared {} but the current uncommitted diff looks like {}",
                target.path,
                declared.as_str(),
                observed.0.as_str()
            ));
            if first_observed.is_none() {
                first_observed = Some(observed.0.as_str().to_string());
            }
        }
    }
    (!mismatch_notes.is_empty(), first_observed, mismatch_notes)
}

#[rmcp::tool_router(router = "change_tool_router", vis = "pub(crate)")]
impl CalmServer {
    #[tool(
        name = "plan_change",
        description = "Declares a ChangeIntent (what you're about to do, and why) as a formal, reviewable record, before writing anything -- returns a change_id to hand to review_change once a human/MRTR approves it. Idempotent: repeated calls with the same kind+targets return the same change_id instead of minting a new one each time. Surfaces (not blocks) a declared-vs-observed kind mismatch when a target's current uncommitted diff doesn't match what you declared. USE WHEN: a change needs formal review/authority rather than the lighter edit_context+confirm+reason gate. NOT FOR: the write itself (edit_lines/edit_symbol still do that) or a single quick low-risk edit (edit_context is enough there).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(crate) fn plan_change(
        &self,
        Parameters(p): Parameters<PlanChangeParams>,
    ) -> Json<ToolOutcome<PlanChangeOutput>> {
        Json(self.timed_tool("plan_change", || {
            let kind = match calm_core::change::ChangeKind::parse(&p.kind) {
                Some(k) => k,
                None => {
                    return ToolOutcome::error(error_detail(
                        "INVALID_CHANGE_KIND",
                        &format!(
                            "{:?} is not a known ChangeKind -- see plan_change's own \
                             parameter schema for the valid names",
                            p.kind
                        ),
                        false,
                    ));
                }
            };
            if p.targets.is_empty() {
                return ToolOutcome::error(error_detail(
                    "TARGETS_REQUIRED",
                    "plan_change needs at least one target",
                    false,
                ));
            }
            let targets: Vec<calm_core::change::ChangeIntentTarget> = p
                .targets
                .iter()
                .map(|t| calm_core::change::ChangeIntentTarget {
                    path: t.path.clone(),
                    qualified_name: t.qualified_name.clone(),
                })
                .collect();
            let idempotency_key = plan_idempotency_key(kind, &targets);

            let mut state_conn = match calm_core::db::conn::open_state_writer(&self.state_db_path)
            {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
                }
            };

            let (change_id, snapshot_id, reused) = match calm_core::change::find_change_intent_by_idempotency_key(
                &state_conn,
                &idempotency_key,
            ) {
                Ok(Some(existing)) => (existing.intent_id, existing.snapshot_id, true),
                Ok(None) => {
                    let snapshot = {
                        let conn = match self.make_read_conn() {
                            Ok(c) => c,
                            Err(e) => return db_error(e),
                        };
                        match calm_core::authority::EvidenceSnapshot::compute(
                            &conn,
                            &self.project_root,
                        ) {
                            Ok(s) => s,
                            Err(e) => {
                                return ToolOutcome::error(error_detail(
                                    "SNAPSHOT_ERROR",
                                    &e.to_string(),
                                    true,
                                ));
                            }
                        }
                    };
                    let tx = match state_conn.transaction() {
                        Ok(tx) => tx,
                        Err(e) => {
                            return ToolOutcome::error(error_detail(
                                "STATE_DB_ERROR",
                                &e.to_string(),
                                true,
                            ));
                        }
                    };
                    if let Err(e) = snapshot.persist(&tx) {
                        return ToolOutcome::error(error_detail(
                            "STATE_DB_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                    let intent = calm_core::change::ChangeIntent::new(
                        calm_core::change::ChangeIntentKind(kind),
                        p.reason.clone(),
                        snapshot.snapshot_id.clone(),
                        targets.clone(),
                    );
                    if let Err(e) =
                        calm_core::change::insert_change_intent(&tx, &intent, Some(&idempotency_key))
                    {
                        return ToolOutcome::error(error_detail(
                            "STATE_DB_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                    if let Err(e) = tx.commit() {
                        return ToolOutcome::error(error_detail(
                            "STATE_DB_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                    (intent.intent_id, snapshot.snapshot_id, false)
                }
                Err(e) => {
                    return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
                }
            };

            let (kind_mismatch, observed_kind, mismatch_notes) =
                check_declared_vs_observed(&self.project_root, kind, &targets);

            ToolOutcome::success(PlanChangeOutput {
                change_id,
                reused,
                snapshot_id,
                kind_mismatch,
                observed_kind,
                mismatch_notes,
                suggested_next: suggested(
                    "review_change",
                    "Get human/MRTR approval, then mint a ReviewAuthority for this change_id",
                ),
            })
        }))
    }

    #[tool(
        name = "review_change",
        description = "Mints a ReviewAuthority for a change_id from plan_change -- ONLY when approved:true (the required human/MRTR approval step; omitted or false always refuses with APPROVAL_REQUIRED, never mints). Pass the returned authority_id alongside change_id to edit_lines/edit_symbol to spend it in one write. USE WHEN: a plan_change'd intent has been reviewed and approved. NOT FOR: minting without a real approval, or a multi-file change that needs independent per-file authorization (Phase 2 ChangeSet territory -- see this module's own doc comment).",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(crate) fn review_change(
        &self,
        Parameters(p): Parameters<ReviewChangeParams>,
    ) -> Json<ToolOutcome<ReviewChangeOutput>> {
        Json(self.timed_tool("review_change", || {
            if !p.approved {
                return ToolOutcome::error(error_detail(
                    "APPROVAL_REQUIRED",
                    "review_change never mints an authority without an explicit human/MRTR \
                     approval -- pass approved:true only after that approval has actually \
                     happened",
                    true,
                ));
            }
            let ttl = match calm_core::authority::AuthorityTtl::from_secs(
                p.ttl_secs.unwrap_or(DEFAULT_REVIEW_TTL_SECS),
            ) {
                Ok(t) => t,
                Err(e) => {
                    return ToolOutcome::error(error_detail("INVALID_TTL", &e.to_string(), true));
                }
            };

            let intent = {
                let state_read_conn = match self.make_state_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                match calm_core::change::get_change_intent(&state_read_conn, &p.change_id) {
                    Ok(Some(i)) => i,
                    Ok(None) => {
                        return ToolOutcome::error(error_detail(
                            "CHANGE_NOT_FOUND",
                            &format!("no plan_change intent with change_id {}", p.change_id),
                            false,
                        ));
                    }
                    Err(e) => {
                        return ToolOutcome::error(error_detail(
                            "STATE_DB_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                }
            };
            if intent.targets.is_empty() {
                return ToolOutcome::error(error_detail(
                    "NO_TARGETS",
                    "this change_id has no declared targets to authorize",
                    false,
                ));
            }

            let (snapshot, graph_generation, caller_set_digest) = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                let snapshot = match calm_core::authority::EvidenceSnapshot::compute(
                    &conn,
                    &self.project_root,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        return ToolOutcome::error(error_detail(
                            "SNAPSHOT_ERROR",
                            &e.to_string(),
                            true,
                        ));
                    }
                };
                let graph_generation: i64 = conn
                    .query_row(
                        "SELECT generation FROM graph_generation_state WHERE id = 1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                // Union caller set across every symbol-scoped target -- the
                // same "cover every touched symbol" spirit target_scope_digest
                // (authority::review) already uses, generalized to mint time
                // since a ChangeIntent can name more than one target (see
                // this module's own doc comment for the Phase-1 spending
                // limit that still applies).
                let mut caller_qns: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for t in &intent.targets {
                    if let Some(qn) = &t.qualified_name {
                        caller_qns.extend(super::edit::caller_symbol_set(&conn, qn));
                    }
                }
                let caller_set_digest =
                    Self::caller_set_digest(&caller_qns.into_iter().collect::<Vec<_>>());
                (snapshot, graph_generation, caller_set_digest)
            };

            let policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
            let policy_digest = policy.digest();
            let principal = format!("session:{}", self.session_id);

            let mut state_conn = match calm_core::db::conn::open_state_writer(&self.state_db_path)
            {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
                }
            };
            let tx = match state_conn.transaction() {
                Ok(tx) => tx,
                Err(e) => {
                    return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
                }
            };
            if let Err(e) = snapshot.persist(&tx) {
                return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
            }
            let authority = match calm_core::authority::ReviewAuthority::mint(
                &tx,
                calm_core::authority::MintParams {
                    intent_id: &intent.intent_id,
                    snapshot_id: &snapshot.snapshot_id,
                    graph_generation,
                    caller_set_digest: &caller_set_digest,
                    policy_digest: &policy_digest,
                    principal: &principal,
                    ttl_secs: ttl,
                    targets: &intent.targets,
                },
            ) {
                Ok(a) => a,
                Err(e) => {
                    return ToolOutcome::error(error_detail("MINT_FAILED", &e.to_string(), true));
                }
            };
            if let Err(e) = tx.commit() {
                return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
            }

            tracing::info!(
                target: crate::telemetry::AUDIT_TARGET,
                session_id = self.session_id,
                decision = "change_approved",
                reason_code = "CHANGE_APPROVED",
                change_id = p.change_id,
                authority_id = authority.authority_id,
                approver = p.approver.as_deref().unwrap_or("unspecified"),
            );

            ToolOutcome::success(ReviewChangeOutput {
                change_id: p.change_id,
                authority_id: authority.authority_id,
                authority_expires_at: authority.expires_at,
                suggested_next: suggested(
                    "edit_lines",
                    "Spend this authority_id (with change_id) on your write",
                ),
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server(name: &str) -> (std::path::PathBuf, CalmServer) {
        let dir = std::env::temp_dir().join(format!("ci_change_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server = CalmServer::new(dir.clone(), dir.join("index.db")).unwrap();
        (dir, server)
    }

    fn plan_params(kind: &str, targets: Vec<(&str, Option<&str>)>) -> PlanChangeParams {
        PlanChangeParams {
            kind: kind.to_string(),
            reason: "test".to_string(),
            targets: targets
                .into_iter()
                .map(|(path, qn)| ChangeTargetParam {
                    path: path.to_string(),
                    qualified_name: qn.map(|s| s.to_string()),
                })
                .collect(),
        }
    }

    #[test]
    fn plan_change_rejects_an_unknown_kind() {
        let (dir, server) = test_server("unknown_kind");
        let out = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "not_a_real_kind",
            vec![("a.rs", None)],
        )));
        let v = serde_json::to_value(&out.0).unwrap();
        assert_eq!(v["error"]["code"], "INVALID_CHANGE_KIND", "response: {v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_change_rejects_empty_targets() {
        let (dir, server) = test_server("empty_targets");
        let out = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![],
        )));
        let v = serde_json::to_value(&out.0).unwrap();
        assert_eq!(v["error"]["code"], "TARGETS_REQUIRED", "response: {v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_plan_change_calls_are_idempotent() {
        let (dir, server) = test_server("idempotent");
        let params = plan_params("body", vec![("a.rs", Some("a.rs::f"))]);
        let first = server.plan_change(rmcp::handler::server::wrapper::Parameters(
            plan_params("body", vec![("a.rs", Some("a.rs::f"))]),
        ));
        let first_v = serde_json::to_value(&first.0).unwrap();
        assert_eq!(first_v["reused"], false, "response: {first_v}");
        let first_id = first_v["change_id"].as_str().unwrap().to_string();

        let second = server.plan_change(rmcp::handler::server::wrapper::Parameters(params));
        let second_v = serde_json::to_value(&second.0).unwrap();
        assert_eq!(second_v["reused"], true, "response: {second_v}");
        assert_eq!(second_v["change_id"], first_id, "response: {second_v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_change_surfaces_a_declared_vs_observed_kind_mismatch() {
        let (dir, server) = test_server("kind_mismatch");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@example.com"])
            .current_dir(&dir)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(&dir)
            .status()
            .unwrap();
        std::fs::write(dir.join("a.py"), "def f():\n    return 1\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "a.py"])
            .current_dir(&dir)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(&dir)
            .status()
            .unwrap();
        // Real code change on disk, but declared as doc_only below.
        std::fs::write(dir.join("a.py"), "def f():\n    return 2\n").unwrap();

        let out = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "doc_only",
            vec![("a.py", None)],
        )));
        let v = serde_json::to_value(&out.0).unwrap();
        assert_eq!(v["kind_mismatch"], true, "response: {v}");
        assert_eq!(v["observed_kind"], "body", "response: {v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_change_refuses_to_mint_without_approval() {
        let (dir, server) = test_server("no_approval");
        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.rs", Some("a.rs::f"))],
        )));
        let plan_v = serde_json::to_value(&plan.0).unwrap();
        let change_id = plan_v["change_id"].as_str().unwrap().to_string();

        let out = server.review_change(rmcp::handler::server::wrapper::Parameters(
            ReviewChangeParams {
                change_id,
                approved: false,
                approver: None,
                ttl_secs: None,
            },
        ));
        let v = serde_json::to_value(&out.0).unwrap();
        assert_eq!(v["error"]["code"], "APPROVAL_REQUIRED", "response: {v}");
        assert!(v.get("authority_id").is_none(), "must never mint: {v}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_change_on_unknown_change_id_is_not_found() {
        let (dir, server) = test_server("not_found");
        let out = server.review_change(rmcp::handler::server::wrapper::Parameters(
            ReviewChangeParams {
                change_id: "INT-does-not-exist".to_string(),
                approved: true,
                approver: Some("alice".to_string()),
                ttl_secs: None,
            },
        ));
        let v = serde_json::to_value(&out.0).unwrap();
        assert_eq!(v["error"]["code"], "CHANGE_NOT_FOUND", "response: {v}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approved_review_change_mints_an_authority_spendable_by_edit_lines() {
        use super::super::edit::ElicitGate;
        let (dir, server) = test_server("approved_mint_spend");
        std::fs::write(dir.join("a.py"), "def helper():\n    return 1\n").unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::helper', 'helper', 'function', 'python', 'a.py', 1, 2, '', '', 'helper', 0, 0, 0)",
                [],
            )
            .unwrap();
        }

        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.py", Some("a.py::helper"))],
        )));
        let plan_v = serde_json::to_value(&plan.0).unwrap();
        let change_id = plan_v["change_id"].as_str().unwrap().to_string();

        let review = server.review_change(rmcp::handler::server::wrapper::Parameters(
            ReviewChangeParams {
                change_id: change_id.clone(),
                approved: true,
                approver: Some("alice".to_string()),
                ttl_secs: None,
            },
        ));
        let review_v = serde_json::to_value(&review.0).unwrap();
        let authority_id = review_v["authority_id"]
            .as_str()
            .expect(&format!("review_change must mint: {review_v}"))
            .to_string();

        let hash = calm_core::edit::range_checksum("def helper():\n    return 1\n", 2, 2).unwrap();
        let params = super::super::edit::EditLinesParams {
            change_id: Some(change_id),
            authority_id: Some(authority_id),
            path: "a.py".into(),
            edits: vec![super::super::edit::EditHunkParam {
                old_text: None,
                start_line: 2,
                end_line: 2,
                expected_hash: Some(hash),
                new_text: "    return 2\n".into(),
            }],
            confirm: false,
            reason: None,
            cites: None,
        };
        let mut ask = None;
        let out = server.edit_lines_flow(&params, ElicitGate::Off, &mut ask);
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["applied"], true, "response: {v}");
        assert_eq!(
            std::fs::read_to_string(dir.join("a.py")).unwrap(),
            "def helper():\n    return 2\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
