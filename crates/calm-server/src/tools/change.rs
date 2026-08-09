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
//! (`APPROVAL_REQUIRED`) unless `approved: true` is explicitly set.
//!
//! **CCK-23 correction (audit 2026-08-09):** `approved: true` here is
//! CLIENT SELF-ATTESTATION, not a server-verified human/MRTR round-trip --
//! whether a human actually looked at this is entirely a client-side
//! concern this module cannot see or verify, matching how `edit_lines`/
//! `edit_symbol`'s own lighter `confirm: bool` gate already works one tier
//! down. That's an acceptable bar for low/medium-risk changes (self-review
//! is a legitimate approver class), but it is NOT independent review for a
//! high-risk touch. The authority this mints for a high-risk target is
//! spendable ONLY if `edit_lines`/`edit_symbol`'s own unconditional
//! `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` check (edit.rs, CCK-23) is
//! separately satisfied via a real elicitation round-trip -- self-
//! attestation here is never sufficient on its own for that tier.
//!
//! **CCK-26 (audit follow-up):** `review_change` now also refuses to mint
//! at all -- `INDEPENDENT_REVIEW_NOT_AVAILABLE_HERE`, no authority handed
//! out -- when a real `calm_core::policy::evaluate()` decision (built from
//! a genuine `RiskVector` over the declared targets, not a placeholder)
//! says the required approver class is `Human`. This is on top of, not a
//! replacement for, CCK-23's spend-time backstop above: this module's
//! refusal saves a wasted authority for the common case, edit_lines/
//! edit_symbol's check is what actually cannot be bypassed.
//!
//! **CCK-27 (audit follow-up):** `plan_change`'s idempotency dedup keys on
//! `kind`+`targets` only, never `snapshot_id` -- a repeated call after
//! evidence drifted (source/config/graph/provider state) now supersedes
//! the stale intent (`change::intent::IntentStatus::Superseded`) and mints
//! a fresh one against current evidence, instead of silently handing back
//! a `change_id` whose declared picture no longer matches reality.
//! `review_change` refuses to mint against a superseded intent
//! (`INTENT_SUPERSEDED`).
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
use rusqlite::OptionalExtension;

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
    /// CCK-27 (audit follow-up): set when this call's declared kind+targets
    /// matched a prior intent, but evidence had drifted since that intent
    /// was declared -- that stale intent was marked `superseded` (see
    /// `change::intent::IntentStatus`) rather than reused, and this
    /// `change_id` replaces it. `review_change` on the superseded id now
    /// refuses with `INTENT_SUPERSEDED`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) superseded_change_id: Option<String>,
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
    /// Client self-attestation, not a server-verified approval -- `review_change`
    /// NEVER mints an authority unless this is explicitly `true` (omitted or
    /// `false` both refuse with `APPROVAL_REQUIRED`). Adequate for low/medium-risk
    /// changes only: for a high-risk target, edit_lines/edit_symbol separately
    /// require real independent review regardless of this flag. See this module's
    /// own doc comment for what "approval" means here.
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
        .map(|t| {
            format!(
                "{}\u{0}{}",
                t.path,
                t.qualified_name.as_deref().unwrap_or("")
            )
        })
        .collect();
    canonical.sort();
    let material = format!(
        "plan-change-v1\n{}\n{}\n",
        kind.as_str(),
        canonical.join("\n")
    );
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
        let language =
            calm_core::indexer::lang_constants::language_for_extension(ext).unwrap_or("");
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
        description = "Declares a ChangeIntent (what you're about to do, and why) as a formal, reviewable record, before writing anything -- returns a change_id to hand to review_change once a human/MRTR approves it. Idempotent ONLY while evidence hasn't drifted: repeated calls with the same kind+targets return the same change_id, but if source/config/graph/provider state changed since the matching intent was declared, that intent is superseded and this call mints a fresh change_id against current evidence instead (superseded_change_id on the response names the one replaced -- review_change on it now refuses with INTENT_SUPERSEDED). Surfaces (not blocks) a declared-vs-observed kind mismatch when a target's current uncommitted diff doesn't match what you declared. USE WHEN: a change needs formal review/authority rather than the lighter edit_context+confirm+reason gate. NOT FOR: the write itself (edit_lines/edit_symbol still do that) or a single quick low-risk edit (edit_context is enough there).",
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

            // CCK-27 (audit follow-up): the snapshot is now computed
            // unconditionally, up front -- a repeated call needs it too,
            // to decide whether a prior intent for this exact kind+targets
            // is still current or has drifted (see below), not just a
            // fresh call minting one for the first time.
            let snapshot = {
                let conn = match self.make_read_conn() {
                    Ok(c) => c,
                    Err(e) => return db_error(e),
                };
                match calm_core::authority::EvidenceSnapshot::compute(&conn, &self.project_root) {
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

            let mut state_conn = match calm_core::db::conn::open_state_writer(&self.state_db_path) {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "STATE_DB_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };

            let existing = match calm_core::change::find_change_intent_by_idempotency_key(
                &state_conn,
                &idempotency_key,
            ) {
                Ok(existing) => existing,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "STATE_DB_ERROR",
                        &e.to_string(),
                        true,
                    ));
                }
            };

            // CCK-27 (audit follow-up): `find_change_intent_by_idempotency_key`
            // only ever returns an `Active` intent (`supersede_change_intent`
            // clears a superseded row's key), so `existing` here is always the
            // current answer for this kind+targets -- IF its declared
            // `snapshot_id` still matches evidence right now. When it does,
            // this is a genuine repeated call: reuse it verbatim, unchanged
            // from CCK-07's original behavior. When it doesn't, evidence
            // drifted since that intent was declared -- reusing it would hand
            // a human reviewing it via `review_change` a `change_id` whose
            // `kind_mismatch`/`observed_kind` picture no longer reflects
            // reality, so supersede it and mint a fresh intent against
            // current evidence instead (`superseded_change_id` on the
            // response names the one this replaced).
            let (change_id, snapshot_id, reused, superseded_change_id) = if let Some(existing) =
                &existing
                && existing.snapshot_id == snapshot.snapshot_id
            {
                (
                    existing.intent_id.clone(),
                    existing.snapshot_id.clone(),
                    true,
                    None,
                )
            } else {
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
                let stale_intent_id = existing.map(|e| e.intent_id);
                let write_result = match &stale_intent_id {
                    Some(old_id) => calm_core::change::supersede_change_intent(
                        &tx,
                        old_id,
                        &intent,
                        Some(&idempotency_key),
                    ),
                    None => calm_core::change::insert_change_intent(
                        &tx,
                        &intent,
                        Some(&idempotency_key),
                    ),
                };
                if let Err(e) = write_result {
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
                (
                    intent.intent_id,
                    snapshot.snapshot_id,
                    false,
                    stale_intent_id,
                )
            };

            let (kind_mismatch, observed_kind, mismatch_notes) =
                check_declared_vs_observed(&self.project_root, kind, &targets);

            ToolOutcome::success(PlanChangeOutput {
                change_id,
                reused,
                snapshot_id,
                superseded_change_id,
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
        description = "Mints a ReviewAuthority for a change_id from plan_change -- ONLY when approved:true (omitted or false always refuses with APPROVAL_REQUIRED, never mints). approved:true is CLIENT SELF-ATTESTATION (not server-verified human/MRTR proof) -- adequate for low/medium-risk changes, but for a high-risk target the resulting authority is only spendable if edit_lines/edit_symbol's own independent-review check is separately satisfied via a real elicitation round-trip. Pass the returned authority_id alongside change_id to edit_lines/edit_symbol to spend it in one write. USE WHEN: a plan_change'd intent has been reviewed and approved. NOT FOR: minting without a real approval, or a multi-file change that needs independent per-file authorization (Phase 2 ChangeSet territory -- see this module's own doc comment).",
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
                    "review_change never mints an authority without approved:true -- this is \
                     self-attestation the caller must only set after a real approval \
                     happened (CALM cannot verify the mechanism from here; see this \
                     module's own doc comment). For a high-risk target, this alone is not \
                     independent review -- edit_lines/edit_symbol separately enforce that.",
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
            // CCK-27 (audit follow-up): a superseded intent's declared
            // snapshot is known-stale (that's what supersession means --
            // see change::intent::IntentStatus) -- minting against it would
            // authorize a human/MRTR approval that was never actually given
            // for current evidence. The replacement intent from the
            // plan_change call that superseded this one is the one to
            // review instead.
            if intent.status == calm_core::change::IntentStatus::Superseded {
                return ToolOutcome::error(error_detail(
                    "INTENT_SUPERSEDED",
                    &format!(
                        "change_id {} was superseded by {} after evidence drifted -- \
                         review_change that one instead (re-run plan_change if you no \
                         longer have it)",
                        p.change_id,
                        intent
                            .superseded_by_intent_id
                            .as_deref()
                            .unwrap_or("<unknown>"),
                    ),
                    false,
                ));
            }
            if intent.targets.is_empty() {
                return ToolOutcome::error(error_detail(
                    "NO_TARGETS",
                    "this change_id has no declared targets to authorize",
                    false,
                ));
            }

            let (snapshot, graph_generation, caller_set_digest, risk_vector) = {
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
                // CCK-23/WS2 (audit follow-up): "No stale evidence may grant
                // authority" was never actually enforced -- this capability
                // existed with zero production callers. The actual tiered
                // check (Degraded refused outright for every tier; High risk
                // additionally requires Reconciled, not just Current) needs
                // `policy_decision.required_approver_class`, which isn't
                // known yet at this point -- see the check right after
                // `policy_decision` is computed below.
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
                // CCK-26/WS1 (audit follow-up): a real RiskVector, built from
                // every declared target -- caller_count_level/is_hub/hub_kind
                // straight from `symbols`, risk_rule_floor from this project's
                // config.risk_rules, uncertain_zero_caller from the same
                // compute_touch_risk classify_gate's own legacy write gate
                // uses (edit.rs). signature_changed/touches_uncovered_code are
                // still NOT wired in this pass (no live diff exists yet at
                // review_change time to detect a real signature change from;
                // no verified path-format contract yet against
                // CoverageData::is_covered's absolute-path key -- see WS2) --
                // documented gap, default false, never risk-elevated by their
                // absence here.
                let mut caller_count_level = calm_core::policy::RiskLevel::Low;
                let mut is_hub = false;
                let mut hub_kind: Option<String> = None;
                let mut risk_rule_floor: Option<calm_core::policy::RiskLevel> = None;
                // WS1 (audit follow-up): real uncertain_zero_caller, wired via
                // the SAME compute_touch_risk the live edit_lines/edit_symbol
                // gate uses (edit.rs) -- no live diff exists yet at
                // review_change time, so each target's own full symbol range
                // stands in for "the range this change touches" (no
                // proposed_hunks to detect a signature change from either,
                // hence `&[]` there -- signature_changed stays false below,
                // same documented under-approximation as before).
                let mut uncertain_zero_caller = false;
                for t in &intent.targets {
                    if let Some(qn) = &t.qualified_name {
                        caller_qns.extend(super::edit::caller_symbol_set(&conn, qn));
                        if let Ok(Some((
                            caller_count,
                            sym_is_hub,
                            sym_hub_kind,
                            line_start,
                            line_end,
                        ))) = conn
                            .query_row(
                                "SELECT caller_count, is_hub, hub_kind, line_start, line_end \
                                 FROM symbols WHERE qualified_name = ?1",
                                rusqlite::params![qn],
                                |r| {
                                    Ok((
                                        r.get::<_, i64>(0)?,
                                        r.get::<_, bool>(1)?,
                                        r.get::<_, Option<String>>(2)?,
                                        r.get::<_, i64>(3)?,
                                        r.get::<_, i64>(4)?,
                                    ))
                                },
                            )
                            .optional()
                        {
                            if let Some(level) = calm_core::policy::RiskLevel::parse(
                                super::detail::risk_level_from_caller_count(caller_count),
                            ) {
                                caller_count_level = caller_count_level.max(level);
                            }
                            if sym_is_hub {
                                is_hub = true;
                                if hub_kind.is_none() {
                                    hub_kind = sym_hub_kind;
                                }
                            }
                            if !uncertain_zero_caller {
                                let (_, _, _, sym_uncertain, _, _) =
                                    super::edit::compute_touch_risk(
                                        &conn,
                                        &t.path,
                                        &[(line_start, line_end)],
                                        &self.coverage.read_ok(),
                                        &self.config().risk_rules,
                                        &[],
                                    );
                                uncertain_zero_caller = sym_uncertain.is_some();
                            }
                        }
                    }
                    if let Some((level_str, _glob)) =
                        calm_core::config::risk_floor_for_path(&self.config().risk_rules, &t.path)
                        && let Some(level) = calm_core::policy::RiskLevel::parse(level_str)
                    {
                        risk_rule_floor = Some(risk_rule_floor.map_or(level, |cur| cur.max(level)));
                    }
                }
                let caller_set_digest =
                    Self::caller_set_digest(&caller_qns.into_iter().collect::<Vec<_>>());
                let (kind_mismatch, _observed, _notes) =
                    check_declared_vs_observed(&self.project_root, intent.kind.0, &intent.targets);
                let touches_manifest = intent
                    .targets
                    .iter()
                    .any(|t| calm_core::change::classify::is_manifest_path(&t.path));
                let risk_vector = calm_core::policy::RiskVector {
                    caller_count_level,
                    is_hub,
                    hub_kind,
                    signature_changed: false,
                    uncertain_zero_caller,
                    risk_rule_floor,
                    kind_mismatch,
                    touches_manifest,
                    touches_uncovered_code: false,
                };
                (snapshot, graph_generation, caller_set_digest, risk_vector)
            };

            let policy = calm_core::policy::loader::load_policy_or_warn(&self.project_root);
            let policy_digest = policy.digest();
            let principal = format!("session:{}", self.session_id);

            // CCK-26 (audit follow-up): a REAL PolicyEngine::evaluate() decision,
            // not just a policy-config digest. For a Human-required change,
            // review_change's approved:true self-attestation (already confirmed
            // true above -- this handler never reaches here otherwise) is not
            // independent review -- only edit_lines/edit_symbol's own
            // unconditional HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW check (CCK-23)
            // is, so refuse to mint here rather than hand out an authority that
            // can only ever be inert for the write it was meant to cover.
            let policy_decision = calm_core::policy::evaluate(&risk_vector, &policy);
            // Checked BEFORE the freshness tier below (WS2, audit
            // follow-up): this refusal is unconditional for Human tier
            // regardless of evidence freshness -- review_change has no real
            // approval channel at all, so a Human-required change is
            // refused here whether evidence is Current, Degraded, or (were
            // it ever reachable from this endpoint) Reconciled. Keeping it
            // first means the error a caller sees names the actual reason
            // review_change can never mint this authority, not an
            // evidence-freshness detail that's beside the point for a tier
            // this endpoint can never serve anyway.
            if policy_decision.required_approver_class == calm_core::policy::ApproverClass::Human {
                return ToolOutcome::error(error_detail(
                    "INDEPENDENT_REVIEW_NOT_AVAILABLE_HERE",
                    &format!(
                        "this change is \"{}\" risk ({}) -- review_change's approved:true \
                         self-attestation is not independent review at this tier. Spend this \
                         change via edit_lines/edit_symbol with [edit] elicit_hub_confirm \
                         enabled, which performs the actual human/MRTR round-trip",
                        policy_decision.aggregate_risk.as_str(),
                        policy_decision.reasons.join("; "),
                    ),
                    true,
                ));
            }
            // WS2 (audit follow-up): the tiered freshness bar -- Degraded is
            // refused outright for every tier (unchanged from CCK-23's Phase
            // 0 blanket refusal). High risk additionally requires Reconciled
            // rather than merely Current, but that tier is already refused
            // unconditionally above for this endpoint -- reachable here only
            // for Low/Medium, where Current already meets the bar, same as
            // before this check existed.
            if !snapshot
                .freshness_class
                .meets_bar_for(policy_decision.required_approver_class)
            {
                return ToolOutcome::error(error_detail(
                    "EVIDENCE_NOT_FRESH",
                    "index/config drifted since the last reconciliation (freshness=degraded) \
                     -- cannot mint a ReviewAuthority against stale evidence",
                    true,
                ));
            }

            let mut state_conn = match calm_core::db::conn::open_state_writer(&self.state_db_path) {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutcome::error(error_detail(
                        "STATE_DB_ERROR",
                        &e.to_string(),
                        true,
                    ));
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
                return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
            }
            let policy_decision_digest = policy_decision.digest();
            let risk_vector_digest = risk_vector.digest();
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
                    policy_decision_digest: &policy_decision_digest,
                    risk_vector_digest: &risk_vector_digest,
                    required_approver_class: policy_decision.required_approver_class,
                },
            ) {
                Ok(a) => a,
                Err(e) => {
                    return ToolOutcome::error(error_detail("MINT_FAILED", &e.to_string(), true));
                }
            };
            // WS3 (audit follow-up): a durable record that this authority's
            // required_approver_class was actually satisfied -- for
            // SelfReviewed, that's exactly the approved:true self-
            // attestation this handler already required above (checked at
            // the top of this function, long before minting was even
            // attempted). For Human, review_change never reaches this point
            // at all (refused earlier as INDEPENDENT_REVIEW_NOT_AVAILABLE_HERE),
            // so every receipt written here is genuinely self_attested,
            // never a Human-tier claim in disguise.
            if let Err(e) = calm_core::authority::insert_approval_receipt(
                &tx,
                &calm_core::authority::ApprovalReceipt {
                    change_id: Some(&intent.intent_id),
                    authority_id: Some(&authority.authority_id),
                    subject_digest: &policy_decision_digest,
                    approved_by: &principal,
                    mechanism: "self_attested",
                    tx_id: None,
                },
            ) {
                return ToolOutcome::error(error_detail("STATE_DB_ERROR", &e.to_string(), true));
            }
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
        let first = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.rs", Some("a.rs::f"))],
        )));
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
    fn plan_change_supersedes_a_stale_intent_when_evidence_drifts_between_calls() {
        // CCK-27 (audit follow-up) reproduction: plan_change(body A) -> INT-1@S1;
        // source drifts to S2 (a reindex/watcher event, simulated here by a
        // direct file_index insert); plan_change(body A) again must mint
        // INT-2@S2, not silently hand back the now-stale INT-1 -- and
        // INT-1 must no longer be reviewable.
        let (dir, server) = test_server("supersede_on_drift");
        let index_db_path = dir.join("index.db");

        let first = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.rs", Some("a.rs::f"))],
        )));
        let first_v = serde_json::to_value(&first.0).unwrap();
        assert_eq!(first_v["reused"], false, "response: {first_v}");
        assert_eq!(first_v["superseded_change_id"], serde_json::Value::Null);
        let first_id = first_v["change_id"].as_str().unwrap().to_string();
        let first_snapshot = first_v["snapshot_id"].as_str().unwrap().to_string();

        {
            let conn = rusqlite::Connection::open(&index_db_path).unwrap();
            conn.execute(
                "INSERT INTO file_index (path, hash, last_indexed) VALUES (?1, ?2, 0)",
                rusqlite::params!["b.rs", "some-hash"],
            )
            .unwrap();
        }

        let second = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.rs", Some("a.rs::f"))],
        )));
        let second_v = serde_json::to_value(&second.0).unwrap();
        assert_eq!(
            second_v["reused"], false,
            "a drifted intent must not be silently reused: {second_v}"
        );
        let second_id = second_v["change_id"].as_str().unwrap().to_string();
        assert_ne!(second_id, first_id);
        assert_eq!(
            second_v["superseded_change_id"], first_id,
            "response: {second_v}"
        );
        assert_ne!(second_v["snapshot_id"].as_str().unwrap(), first_snapshot);

        let review = server.review_change(rmcp::handler::server::wrapper::Parameters(
            ReviewChangeParams {
                change_id: first_id,
                approved: true,
                approver: None,
                ttl_secs: None,
            },
        ));
        let review_v = serde_json::to_value(&review.0).unwrap();
        assert_eq!(
            review_v["error"]["code"], "INTENT_SUPERSEDED",
            "response: {review_v}"
        );
        assert!(
            review_v["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&second_id),
            "response: {review_v}"
        );

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
        {
            // CCK-23: review_change now refuses to mint against a Degraded
            // snapshot (index_input_state fail-closes to Unknown/Degraded
            // when never persisted) -- mirror the real production path
            // (daemon bootstrap / watch_supervisor refresh) so this test
            // fixture reflects a reconciled repo, not an untouched one.
            let conn = server.db();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
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
            .unwrap_or_else(|| panic!("review_change must mint: {review_v}"))
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

    #[test]
    // WS3 (audit follow-up): review_change's approved:true self-attestation
    // must leave a durable approval_receipts row behind, not just the
    // signed required_approver_class claim on the authority itself.
    fn review_change_writes_a_self_attested_approval_receipt() {
        let (dir, server) = test_server("review_change_writes_approval_receipt");
        std::fs::write(dir.join("a.py"), "def helper():\n    return 1\n").unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::helper', 'helper', 'function', 'python', 'a.py', 1, 2, '', '', 'helper', 1, 0, 0)",
                [],
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
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
            .unwrap_or_else(|| panic!("review_change must mint: {review_v}"))
            .to_string();

        let state_conn = server.state_db();
        let (stored_change_id, stored_authority_id, mechanism, decision, approved_by): (
            String,
            String,
            String,
            String,
            String,
        ) = state_conn
            .query_row(
                "SELECT change_id, authority_id, mechanism, decision, approved_by \
                 FROM approval_receipts WHERE authority_id = ?1",
                rusqlite::params![authority_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap_or_else(|e| panic!("expected exactly one approval_receipts row: {e}"));
        assert_eq!(stored_change_id, change_id);
        assert_eq!(stored_authority_id, authority_id);
        assert_eq!(mechanism, "self_attested");
        assert_eq!(decision, "approved");
        assert!(approved_by.starts_with("session:"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // WS1 (audit follow-up, P0 flagship regression): review_change approves
    // a change DECLARED doc_only, but the actual edit_lines call changes
    // the function body -- not a doc/comment line. Before WS1,
    // target/snapshot/caller/policy-config all still matched at spend time,
    // so the authority verified regardless of what kind of change was
    // actually spent. Now the real RiskVector/PolicyDecision, recomputed at
    // spend time from the actual before/after file content, disagrees with
    // what review_change minted (kind_mismatch flips true), and the spend
    // is refused.
    fn review_change_doc_only_then_body_edit_via_edit_lines_is_rejected() {
        use super::super::edit::ElicitGate;
        let (dir, server) = test_server("doc_only_then_body_edit");
        std::fs::write(dir.join("a.py"), "def helper():\n    return 1\n").unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::helper', 'helper', 'function', 'python', 'a.py', 1, 2, '', '', 'helper', 1, 0, 0)",
                [],
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
        }

        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "doc_only",
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
            .unwrap_or_else(|| {
                panic!(
                    "review_change must mint for a doc_only-declared low-risk change: {review_v}"
                )
            })
            .to_string();

        // The actual spend is a BODY edit (changes the return value), not a
        // doc/comment change -- exactly what was declared and reviewed.
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
        assert_ne!(
            v["applied"], true,
            "a doc_only-reviewed authority must not authorize a real body edit: {v}"
        );
        let code = v["error"]["code"].as_str().unwrap_or_default();
        assert!(
            code == "AUTHORITY_STALE_RISK_VECTOR" || code == "AUTHORITY_STALE_POLICY_DECISION",
            "expected a stale risk/policy-decision refusal, got: {v}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a.py")).unwrap(),
            "def helper():\n    return 1\n",
            "file must be unchanged after a rejected spend"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // WS1: regression guard for the fix above -- a doc_only-reviewed
    // change spent as a REAL doc-only edit (only the docstring text
    // differs, no code line changes) must still succeed. Proves the new
    // spend-time RiskVector check doesn't false-positive-block a matching,
    // legitimate edit.
    fn review_change_doc_only_then_matching_doc_edit_via_edit_lines_succeeds() {
        use super::super::edit::ElicitGate;
        let (dir, server) = test_server("doc_only_then_matching_doc_edit");
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    \"\"\"old doc\"\"\"\n    return 1\n",
        )
        .unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::helper', 'helper', 'function', 'python', 'a.py', 1, 3, '', '', 'helper', 1, 0, 0)",
                [],
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
        }

        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "doc_only",
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
            .unwrap_or_else(|| {
                panic!(
                    "review_change must mint for a doc_only-declared low-risk change: {review_v}"
                )
            })
            .to_string();

        let original = "def helper():\n    \"\"\"old doc\"\"\"\n    return 1\n";
        let hash = calm_core::edit::range_checksum(original, 2, 2).unwrap();
        let params = super::super::edit::EditLinesParams {
            change_id: Some(change_id),
            authority_id: Some(authority_id),
            path: "a.py".into(),
            edits: vec![super::super::edit::EditHunkParam {
                old_text: None,
                start_line: 2,
                end_line: 2,
                expected_hash: Some(hash),
                new_text: "    \"\"\"new doc\"\"\"\n".into(),
            }],
            confirm: false,
            reason: None,
            cites: None,
        };
        let mut ask = None;
        let out = server.edit_lines_flow(&params, ElicitGate::Off, &mut ask);
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(
            v["applied"], true,
            "a matching doc-only edit must be authorized: {v}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a.py")).unwrap(),
            "def helper():\n    \"\"\"new doc\"\"\"\n    return 1\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // WS1 (audit follow-up, claim 7): a multi-target authority's
    // caller_set_digest is minted as the UNION of every declared target's
    // callers (review_change). Before this fix, edit_lines' spend-time
    // digest came from only the first/primary touched symbol -- for an
    // authority over {foo, bar} with DIFFERENT callers each, spending a
    // hunk that touches both would almost always fail STALE_CALLER_SET
    // even though nothing was actually stale. Proves it now matches.
    fn multi_target_authority_caller_digest_matches_at_mint_and_spend() {
        use super::super::edit::ElicitGate;
        let (dir, server) = test_server("multi_target_caller_digest");
        let original = "def foo():\n    return 1\n\ndef bar():\n    return 2\n";
        std::fs::write(dir.join("a.py"), original).unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::foo', 'foo', 'function', 'python', 'a.py', 1, 2, '', '', 'foo', 1, 0, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('a.py::bar', 'bar', 'function', 'python', 'a.py', 4, 5, '', '', 'bar', 1, 0, 0)",
                [],
            )
            .unwrap();
            // Deliberately DIFFERENT callers for foo vs bar, so their union
            // differs from either symbol's own individual caller set.
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, ruled_out_by_scip) \
                 VALUES ('a.py::caller_a', 'a.py::foo', 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, ruled_out_by_scip) \
                 VALUES ('a.py::caller_b', 'a.py::bar', 0)",
                [],
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
        }

        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("a.py", Some("a.py::foo")), ("a.py", Some("a.py::bar"))],
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
            .unwrap_or_else(|| {
                panic!("review_change must mint for a two-target body change: {review_v}")
            })
            .to_string();

        let hash_foo = calm_core::edit::range_checksum(original, 2, 2).unwrap();
        let hash_bar = calm_core::edit::range_checksum(original, 5, 5).unwrap();
        let params = super::super::edit::EditLinesParams {
            change_id: Some(change_id),
            authority_id: Some(authority_id),
            path: "a.py".into(),
            edits: vec![
                super::super::edit::EditHunkParam {
                    old_text: None,
                    start_line: 2,
                    end_line: 2,
                    expected_hash: Some(hash_foo),
                    new_text: "    return 10\n".into(),
                },
                super::super::edit::EditHunkParam {
                    old_text: None,
                    start_line: 5,
                    end_line: 5,
                    expected_hash: Some(hash_bar),
                    new_text: "    return 20\n".into(),
                },
            ],
            confirm: false,
            reason: None,
            cites: None,
        };
        let mut ask = None;
        let out = server.edit_lines_flow(&params, ElicitGate::Off, &mut ask);
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(
            v["applied"], true,
            "a multi-target authority's union caller digest must match at spend time: {v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn review_change_refuses_self_attestation_for_a_high_risk_target() {
        // CCK-26 (audit follow-up): completes what CCK-23 deferred --
        // review_change itself now runs a real RiskVector -> PolicyDecision
        // and refuses to mint when the required approver class is Human,
        // rather than only relying on edit_lines/edit_symbol's spend-time
        // backstop. Uses the same risk_rules-escalation technique as
        // edit_lines_gates_a_low_fan_in_symbol_whose_path_matches_a_risk_rule
        // (tools.rs) to force "high" risk independent of caller_count.
        let (dir, server) = test_server("review_change_refuses_human_required");
        std::fs::create_dir_all(dir.join("auth")).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"risk_rules": [{"glob": "auth/**", "minimum": "high"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("auth/login.py"),
            "def check_token():\n    return True\n",
        )
        .unwrap();
        {
            let conn = server.db();
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point)
                 VALUES ('auth/login.py::check_token', 'check_token', 'function', 'python', 'auth/login.py', 1, 2, '', '', 'check_token', 2, 0, 0)",
                [],
            )
            .unwrap();
            let catalog = calm_core::indexer::refresh::InputCatalog::for_project(&dir);
            calm_core::indexer::refresh::persist_index_input_snapshot(&conn, &catalog).unwrap();
        }

        let plan = server.plan_change(rmcp::handler::server::wrapper::Parameters(plan_params(
            "body",
            vec![("auth/login.py", Some("auth/login.py::check_token"))],
        )));
        let plan_v = serde_json::to_value(&plan.0).unwrap();
        let change_id = plan_v["change_id"].as_str().unwrap().to_string();

        let review = server.review_change(rmcp::handler::server::wrapper::Parameters(
            ReviewChangeParams {
                change_id,
                approved: true,
                approver: Some("alice".to_string()),
                ttl_secs: None,
            },
        ));
        let review_v = serde_json::to_value(&review.0).unwrap();
        assert_eq!(
            review_v["error"]["code"], "INDEPENDENT_REVIEW_NOT_AVAILABLE_HERE",
            "a risk_rules-escalated high-risk target must refuse self-attestation: \
             response {review_v}"
        );
        assert!(
            review_v.get("authority_id").is_none(),
            "no authority should have been minted: {review_v}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
