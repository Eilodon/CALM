//! Tool-response envelopes (`ToolOutcome`/`ResolvedOutcome`), error/caveat
//! construction, symbol resolution (`resolve_symbol`/`CandidateRow`), and the
//! `SuggestedNext` helpers — extracted verbatim from `tools/common.rs`
//! (2026-07-28 hotspot split). Every item was `pub(crate)` in `common` and is
//! re-exported through it (`pub(crate) use crate::tools::outcome::*;`), so both
//! the `use super::common::*;` glob across `tools/*.rs` and any explicit
//! `common::…` reference keep resolving unchanged. Logic is byte-for-byte
//! identical to the pre-split version.
use super::common::*;
use super::*;

// ---------------------------------------------------------------------------
// Shared output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize, JsonSchema, Clone)]
pub(crate) struct SuggestedNext {
    pub(crate) tool: String,
    pub(crate) reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<serde_json::Value>,
    /// Plan 3 §3.5(b): `Some(true)` iff skipping `tool` is actually
    /// hook-enforced (currently only the edit_context/edit_lines/edit_symbol
    /// → diff_impact hints set this, via `suggested_gated`) — every other
    /// hint is left unset (`None`), meaning advisory-only. Lets an agent
    /// tell "you'll be blocked if you skip this" apart from "you probably
    /// want this next" without re-deriving it from AGENTS.md prose each time.
    #[schemars(description = "true iff skipping `tool` is hook-enforced, not just advisory.")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gate: Option<bool>,
    /// P1 (audit follow-up, 2026-08-23): names any `tool` param that is
    /// deliberately left OUT of `args` because this call site has no safe
    /// value to prefill it with (e.g. `review_decide_via_agent_relay`'s
    /// `approve` -- only a real human decision belongs there, never a
    /// guess). `None` means `args` (if present) is already directly
    /// callable as-is. Sparing an agent that dispatches on `tool` name
    /// alone from treating an incomplete `args` object as a ready-to-call
    /// "exact next call" when a required field is still missing.
    #[schemars(
        description = "Names any `tool` param intentionally left out of `args` because only a human can safely supply it (e.g. `approve`). Absent means `args` is already complete."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) required_user_input: Option<Vec<String>>,
}

pub(crate) fn suggested(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: None,
        required_user_input: None,
    })
}

pub(crate) fn suggested_with_args(
    tool: &str,
    reason: &str,
    args: serde_json::Value,
) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: Some(args),
        gate: None,
        required_user_input: None,
    })
}

/// P1 (audit follow-up, 2026-08-23): like `suggested_with_args`, but for
/// the rare call site whose `args` is missing a field that must come from
/// a real human decision, never a guess -- see `SuggestedNext::
/// required_user_input`'s own doc comment.
pub(crate) fn suggested_with_args_requiring(
    tool: &str,
    reason: &str,
    args: serde_json::Value,
    required_user_input: &[&str],
) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: Some(args),
        gate: None,
        required_user_input: Some(required_user_input.iter().map(|s| s.to_string()).collect()),
    })
}

/// Plan 3 §3.5(b): same as `suggested`, but for the 2 hints backed by an
/// actual hook enforcement (the pending-diff_impact gate) rather than a
/// convention — sets `gate: Some(true)` so an agent can tell "mandatory,
/// you will be blocked" from "recommended" without re-reading AGENTS.md.
pub(crate) fn suggested_gated(tool: &str, reason: &str) -> Option<SuggestedNext> {
    Some(SuggestedNext {
        tool: tool.into(),
        reason: reason.into(),
        args: None,
        gate: Some(true),
        required_user_input: None,
    })
}

/// `router.has_route(tool)` already means exactly "registered and not
/// disabled" (rmcp's own definition) — this used to re-derive the same
/// answer from the `preset` STRING via a separate `preset_tools` match, a
/// second mechanism that could disagree with what `tool_router_for_preset`
/// actually did to the router. Querying the router directly makes it the
/// single source of truth for "is this tool available", matching what
/// `tool_router_for_preset`'s own doc comment already promises for
/// `list_tools`/`call_tool` — now `suggested_next` filtering shares that
/// guarantee too instead of running a parallel, driftable computation.
pub(crate) fn is_tool_available(
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
    tool: &str,
) -> bool {
    router.has_route(tool)
}

pub(crate) fn filter_suggested_next(
    sn: Option<SuggestedNext>,
    router: &rmcp::handler::server::router::tool::ToolRouter<CalmServer>,
) -> Option<SuggestedNext> {
    match &sn {
        Some(s) if !is_tool_available(router, &s.tool) => None,
        _ => sn,
    }
}

/// Typed `{"error": {"code","message","recoverable"}}` envelope.
pub(crate) fn error_output(code: &str, message: &str, recoverable: bool) -> ErrorOutput {
    ErrorOutput {
        error: error_detail(code, message, recoverable),
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorOutput {
    pub(crate) error: ErrorDetail,
}

/// Wave 3 (audit follow-up, 2026-08-23): the structured shape of a
/// `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`/`DIFF_DIGEST_MISMATCH` error's
/// `review` field -- lets a calling agent relay-approve/decline without
/// ever reading `pending_reviews` directly or reimplementing
/// `hash_content` itself. `diff_digest` is always `hash_content(&diff_preview)`,
/// computed server-side at the moment this packet is built -- never a
/// value the caller supplied or guessed.
#[derive(Serialize, JsonSchema)]
pub(crate) struct PendingReviewPacket {
    pub(crate) review_id: String,
    pub(crate) diff_preview: String,
    pub(crate) diff_digest: String,
    pub(crate) expires_at: f64,
}

impl PendingReviewPacket {
    pub(crate) fn from_pending(review: &calm_core::authority::PendingReview) -> Self {
        let diff_digest = calm_core::indexer::pipeline::hash_content(&review.diff_preview);
        PendingReviewPacket {
            review_id: review.review_id.clone(),
            diff_preview: review.diff_preview.clone(),
            diff_digest,
            expires_at: review.expires_at,
        }
    }
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ErrorDetail {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) recoverable: bool,
    /// Wave 3: set only on `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` (the
    /// initial refusal) and `DIFF_DIGEST_MISMATCH` (the refetch-and-retry
    /// case) -- everything an agent needs to relay-approve/decline via
    /// `review_decide_via_agent_relay` without a separate read.
    #[schemars(
        description = "Set only on HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW / DIFF_DIGEST_MISMATCH — everything needed to relay-approve/decline via review_decide_via_agent_relay."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) review: Option<Box<PendingReviewPacket>>,
    /// Wave 3: the exact next tool call this error implies, when there is
    /// one unambiguous next step -- reuses `SuggestedNext` so a caller
    /// already handling `suggested_next` elsewhere needs no new shape.
    /// Boxed (like `review` above) so this rarely-populated pair doesn't
    /// inflate every `ErrorDetail`/`Result<_, ErrorDetail>` on the hot,
    /// common path -- clippy's `result_large_err` flagged the unboxed form.
    #[schemars(
        description = "The exact next tool call this error implies, if unambiguous — same shape as suggested_next."
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next_call: Option<Box<SuggestedNext>>,
}

/// Typed not-found envelope for `ResolvedOutcome::not_found`.
pub(crate) fn not_found_error(symbol: &str) -> ErrorOutput {
    error_output(
        "NOT_FOUND",
        &format!("Symbol '{symbol}' not found in index"),
        false,
    )
}

/// Typed `{"error": {"code": "DB_ERROR", ...}}` for the read-connection
/// failure every read-only tool guards against. All tools now emit this
/// shape via `ToolOutcome::error` / `ResolvedOutcome::error`. `review`/
/// `next_call` default to `None` -- use `ErrorDetail::with_review` to
/// attach them at the 2 call sites that have a real pending review.
pub(crate) fn error_detail(code: &str, message: &str, recoverable: bool) -> ErrorDetail {
    ErrorDetail {
        code: code.into(),
        message: message.into(),
        recoverable,
        review: None,
        next_call: None,
    }
}

impl ErrorDetail {
    /// Wave 3: attaches a structured review packet + the exact next call
    /// (`review_decide_via_agent_relay` with `review_id` prefilled) to an
    /// already-built `ErrorDetail` -- chains onto `error_detail(...)` at
    /// the `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`/`DIFF_DIGEST_MISMATCH`
    /// call sites only.
    pub(crate) fn with_review(mut self, review: &calm_core::authority::PendingReview) -> Self {
        let packet = PendingReviewPacket::from_pending(review);
        // P1 (audit follow-up, 2026-08-23): `review_id`/`diff_digest` are
        // the only 2 fields this call site can safely prefill -- `approve`
        // is a real human decision, never a guess, so it's named in
        // `required_user_input` instead of silently omitted from an
        // otherwise ready-looking `args` object.
        self.next_call = suggested_with_args_requiring(
            "review_decide_via_agent_relay",
            "Relay this review's approve/decline decision -- diff_digest is already the \
             correct current value, echo it back verbatim, and approve must be filled in with \
             the human's real decision",
            serde_json::json!({
                "review_id": packet.review_id,
                "diff_digest": packet.diff_digest,
            }),
            &["approve"],
        )
        .map(Box::new);
        self.review = Some(Box::new(packet));
        self
    }
}

/// Wave 11 (item 3, "unified continuation state machine"): classifies an
/// error `code` into the vocabulary `ToolOutcome`/`ResolvedOutcome` expose
/// on every response's top-level `state` field -- `needs_context` (fix/
/// supply missing input, disambiguate, or resolve a different way),
/// `needs_human_review` (a human or independent reviewer must act before
/// this can proceed), `needs_verification` (evidence/authority is stale --
/// re-derive fresh state, don't just retry blindly), or `blocked` (a hard
/// stop: infra failure, security boundary, or integrity refusal not
/// fixable by supplying different params). `ready` (success, no `error`)
/// is set directly by `ToolOutcome::success`/`ResolvedOutcome::success`,
/// never by this function.
///
/// Every code this codebase currently emits (grepped from every
/// `error_detail`/`error_output`/`ErrorDetail{..}` call site, 2026-08-22)
/// has an explicit arm below -- not left to guess from `recoverable` alone,
/// which turned out NOT to reliably predict the right bucket (most
/// `INVALID_*`/`*_REQUIRED` codes are `recoverable: false` yet are exactly
/// "fix your params and retry", i.e. `needs_context`, not `blocked`). A
/// code added later that isn't listed here falls through to the `_` arm,
/// which reuses `recoverable`'s own existing per-call-site meaning as a
/// conservative (not necessarily precise) default rather than panicking.
fn classify_error_state(code: &str, recoverable: bool) -> &'static str {
    match code {
        // A human or independent reviewer must act (approve/decline,
        // confirm, complete an elicitation round-trip) before this can
        // proceed -- no amount of retrying or gathering more context
        // substitutes for that.
        "HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW"
        | "CONFIRM_REQUIRED"
        | "APPROVAL_REQUIRED"
        | "INDEPENDENT_REVIEW_NOT_AVAILABLE_HERE"
        | "ELICITATION_PENDING"
        | "ELICITATION_TIMEOUT"
        | "ELICITATION_FAILED"
        | "AGENT_RELAY_DISABLED"
        | "REVIEW_ALREADY_DECIDED" => "needs_human_review",

        // The human side of this already happened -- a real reviewer
        // declined, or forged/tampered evidence was caught. Retrying,
        // gathering context, or asking for review again is not the fix;
        // audit follow-up (2026-08-23) moved USER_DECLINED here from
        // needs_human_review (its own message says "do not retry ...
        // let them decide", which is a hard stop, not a pending review)
        // and added AUTHORITY_FORGED_SIGNATURE (a security-relevant
        // refusal, not a param the caller can just fix).
        "USER_DECLINED" | "AUTHORITY_FORGED_SIGNATURE" => "blocked",

        // Evidence/authority this call relied on is stale, unconfirmed, or
        // has drifted since it was gathered -- re-derive fresh state
        // (re-run edit_context, re-mint, re-check), not a blind retry.
        // Audit follow-up (2026-08-23): DIFF_DIGEST_MISMATCH moved here
        // from needs_human_review -- its own message says "fetch it fresh
        // ... and echo hash_content of THAT exact text", a caller-side
        // refetch-and-recompute action, not a human decision. The
        // AUTHORITY_STALE_* family below are the same AuthorityError
        // variants STALE_GRAPH_AUTHORITY/STALE_CALLER_SET already sit
        // next to, just reached via a different call site (edit.rs's
        // AuthorityError match, not classify_gate's own staleness checks)
        // -- they were previously falling through to the `recoverable`
        // default below, which is unlabeled and easy to lose track of.
        "STALE_GRAPH_AUTHORITY"
        | "STALE_CALLER_SET"
        | "STALE_SYMBOL"
        | "STALE_AMBIGUOUS"
        | "AUTHORITY_SNAPSHOT_CHECK_FAILED"
        | "AUTHORITY_SNAPSHOT_DEGRADED_SINCE_MINT"
        | "AUTHORITY_STALE_SNAPSHOT"
        | "AUTHORITY_STALE_ANALYSIS_VERSION"
        | "AUTHORITY_STALE_POLICY"
        | "AUTHORITY_STALE_RISK_VECTOR"
        | "AUTHORITY_STALE_POLICY_DECISION"
        | "EVIDENCE_NOT_FRESH"
        | "VERIFICATION_SNAPSHOT_CHANGED"
        | "VERIFICATION_SNAPSHOT_UNREADABLE"
        | "INTENT_SUPERSEDED"
        | "UNCERTAIN_ZERO_CALLER"
        | "DIFF_DIGEST_MISMATCH"
        // Wave 6 (item e): the range changed between pagination pages --
        // same "re-derive fresh state" semantics as the entries above
        // (restart pagination from the beginning), not a param the
        // caller got wrong.
        | "RANGE_CHANGED_SINCE_PAGINATION"
        | "FITNESS_CHECK_FAILED" => "needs_verification",

        // Hard stop: an infra failure, a security/containment boundary, or
        // a data-integrity refusal -- not fixable by supplying different
        // params or gathering more context.
        "DB_ERROR"
        | "DB_LOCKED"
        | "STATE_DB_ERROR"
        | "AUTHORITY_DB_ERROR"
        | "INTERNAL"
        | "READ_FAILED"
        | "WRITE_FAILED"
        | "FILE_NOT_READABLE"
        | "REPARSE_FAILED"
        | "SNAPSHOT_ERROR"
        | "CARGO_SPAWN_FAILED"
        | "LSP_REFRESH_FAILED"
        | "SCIP_REFRESH_FAILED"
        | "MAINTENANCE_RETRY_FAILED"
        | "TRANSACTION_INIT_FAILED"
        | "TX_JOURNAL_ERROR"
        | "EDIT_LOCK_FAILED"
        | "MINT_FAILED"
        | "QUERY_FAILED"
        | "FEATURE_UNAVAILABLE"
        | "EMBEDDINGS_NOT_READY"
        | "PATH_ESCAPES_PROJECT_ROOT"
        | "LOSSY_WRITE_REJECTED" => "blocked",

        // The majority bucket: malformed/incomplete params, an unresolved
        // or ambiguous name, or a lookup that needs a different query --
        // fixable by the caller supplying more/different input and calling
        // again, no review or fresh-evidence round-trip needed. Audit
        // follow-up (2026-08-23): PARSE_ERROR moved here from blocked (its
        // own message says "nothing written", i.e. the caller just needs
        // to fix the proposed content and resubmit) and REASON_NOT_GROUNDED
        // moved here from needs_human_review (its own message tells the
        // caller exactly which param to fix -- `cites`/`reason` -- not a
        // human decision). The AUTHORITY_* family below are AuthorityError
        // variants (edit.rs's authority match) that fell through to the
        // `recoverable` default before this audit -- each is fixable by
        // the caller re-deriving/re-minting the right authority, same as
        // the existing STALE_SYMBOL-style entries above, just not stale
        // evidence specifically (a wrong/expired/consumed/mismatched-scope
        // authority instead).
        "AMBIGUOUS_MATCH"
        | "AUTHORITY_ALREADY_CONSUMED"
        | "AUTHORITY_EXPIRED"
        | "AUTHORITY_NOT_FOUND"
        | "AUTHORITY_WRONG_INTENT"
        | "AUTHORITY_WRONG_PRINCIPAL"
        | "AUTHORITY_WRONG_TARGET_SCOPE"
        | "BOUNDARY_AMBIGUOUS"
        | "CHANGE_NOT_FOUND"
        | "INVALID_AUTHORITY_PARAMS"
        | "INVALID_CHANGE_KIND"
        | "INVALID_HUNKS"
        | "INVALID_INPUT"
        | "INVALID_PARAMS"
        | "INVALID_POSITION"
        | "INVALID_RANGE"
        | "INVALID_SCOPE"
        | "INVALID_TTL"
        | "MATCH_NOT_FOUND"
        | "MISSING_TARGET"
        | "NOT_FOUND"
        | "NO_CARGO_MANIFEST"
        | "NO_TARGETS"
        | "ORIENTATION_REQUIRED"
        | "PARSE_ERROR"
        | "PATH_REQUIRED"
        | "PATH_RESOLUTION_FAILED"
        | "REASON_NOT_GROUNDED"
        | "REVIEW_NOT_FOUND"
        | "TARGETS_REQUIRED"
        | "TX_NOT_FOUND"
        | "UNKNOWN_JOB_KIND"
        | "UNKNOWN_TOOLSET"
        | "EDIT_CONTEXT_REQUIRED"
        // Wave 4b (audit follow-up, 2026-08-23): fixable by removing the
        // duplicated line from new_text, or switching to scope=
        // "decorated_declaration" -- both caller-side, no review needed.
        | "DUPLICATE_DECORATION_RISK" => "needs_context",

        _ => {
            if recoverable {
                "needs_context"
            } else {
                "blocked"
            }
        }
    }
}
pub(crate) fn db_error<T>(e: impl std::fmt::Display) -> ToolOutcome<T> {
    ToolOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Same as `db_error`, for tools whose success path can also be
/// `Ambiguous` (anything built on `resolve_symbol`).
pub(crate) fn db_error_resolved<T>(e: impl std::fmt::Display) -> ResolvedOutcome<T> {
    ResolvedOutcome::error(error_detail(
        "DB_ERROR",
        &format!("db connection failed: {e}"),
        true,
    ))
}

/// Shared success/error envelope for tools with no ambiguous-name branch
/// (i.e. no `resolve_symbol` call).
///
/// NOT a `#[serde(untagged)]` enum: rmcp 2.2.0's `Json<T>` requires T's
/// JSON Schema to have root `"type": "object"` (`schema_for_output`
/// panics otherwise — an untagged enum's schema is a bare `oneOf`/`anyOf`
/// with no top-level `"type"`). So this is a genuine struct with optional/
/// flattened fields instead. Exactly one of `error` / the flattened `T` is
/// ever `Some` at a time — enforced by only constructing through `error`/
/// `success` below, never a struct literal — which reproduces the exact
/// same wire shape tools emitted as a bare JSON string before this type
/// existed (`{"error": {...}}` or `T`'s fields directly at the root).
#[derive(Serialize, JsonSchema)]
pub(crate) struct ToolOutcome<T> {
    /// Wave 11 (item 3, "unified continuation state machine"): present on
    /// every response regardless of success/error -- `"ready"` on success,
    /// or one of `needs_context`/`needs_human_review`/`needs_verification`/
    /// `blocked` on error (see `classify_error_state`'s own doc comment).
    /// Lets an agent dispatch on ONE top-level field instead of
    /// maintaining its own per-`code` remediation table across ~70
    /// distinct codes.
    ///
    /// Named `flow_state`, not the more obvious `state` (audit follow-up,
    /// 2026-08-23): several tool-specific success structs (e.g.
    /// `EditTransactionStatusOutput`, `VerifyChangeOutput` in txn.rs) have
    /// their own unrelated `state` field, and this struct's `success: T` is
    /// `#[serde(flatten)]`ed -- a same-named field there would silently
    /// serialize as a SECOND `"state"` JSON key (serde's flatten writes
    /// each field independently; nothing dedupes it), with most JSON
    /// parsers then last-write-wins-ing over whichever value serialized
    /// second. `flow_state` is deliberately namespaced so no domain output
    /// struct will ever pick the same name by coincidence again.
    pub(crate) flow_state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
}

impl<T> ToolOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        let flow_state = classify_error_state(&detail.code, detail.recoverable);
        ToolOutcome {
            flow_state,
            error: Some(detail),
            success: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ToolOutcome {
            flow_state: "ready",
            error: None,
            success: Some(value),
        }
    }

    /// P1 (audit follow-up, 2026-08-23): like `success`, but for the rare
    /// call site whose `T` carries its own real mid-flow state (e.g.
    /// `EditTransactionStatusOutput`/`VerifyChangeOutput`'s `state` field
    /// reporting `VerifyPending`/`Failed`) that the flat `"ready"` default
    /// would otherwise contradict -- an agent dispatching on the top-level
    /// `flow_state` alone (the whole point of Wave 11 item 3) would
    /// wrongly read "nothing more to do" for a transaction still awaiting
    /// `verify_change`, or one that already failed. `flow_state` should
    /// still be drawn from the same `classify_error_state` vocabulary
    /// (`needs_verification`/`blocked`/etc.) for consistency with the
    /// error side.
    pub(crate) fn success_with_flow_state(value: T, flow_state: &'static str) -> Self {
        ToolOutcome {
            flow_state,
            error: None,
            success: Some(value),
        }
    }

    /// Bridges `edit_symbol` (which resolves a name first) into the same
    /// `ResolvedOutcome` envelope as other `resolve_symbol`-based tools.
    pub(crate) fn into_resolved(self) -> ResolvedOutcome<T> {
        if let Some(detail) = self.error {
            ResolvedOutcome::error(detail)
        } else if let Some(value) = self.success {
            ResolvedOutcome::success(value)
        } else {
            ResolvedOutcome::error(error_detail("INTERNAL", "empty ToolOutcome", false))
        }
    }
}

/// Structured, machine-checkable hint attached to a tool result whose
/// literal content (empty list / not-found) could otherwise be misread as
/// proof of absence. `class` lets a safety gate branch without parsing
/// `message`; `message` is the human-readable explanation. Design mirrors
/// zzet/gortex's `ZeroEdgeCaveat` (Apache-2.0) — reimplemented against
/// CALM's own resolver shape, not a line-for-line port.
#[derive(Serialize, JsonSchema)]
pub(crate) struct Caveat {
    pub(crate) class: &'static str,
    pub(crate) message: String,
}

impl Caveat {
    /// The queried symbol did not resolve to anything in the index at all
    /// — the most common cause of an unpleasant "0 results" surprise, and
    /// almost always a typo, wrong case, or a file in an excluded path
    /// rather than the symbol genuinely not existing.
    pub(crate) fn not_found(symbol: &str) -> Self {
        Caveat {
            class: "not_found",
            message: format!(
                "no symbol named '{symbol}' is in the index — likely a typo, wrong \
                 case, or the file lives in an excluded path (target/, node_modules/, \
                 dist/, build/, __pycache__/, venv/, legacy/, dotdirs). CALM also \
                 does not index standard-library, language-builtin, or third-party \
                 dependency symbols (Rust's println!, Python's len, etc.) — if \
                 '{symbol}' is one of those, its absence here is expected, not a \
                 search failure, and search(kind=\"hybrid\") will only surface your \
                 own code that USES it, never its definition. Otherwise run \
                 search(kind=\"hybrid\") to find the exact name before concluding it \
                 doesn't exist — a not-found result here is not proof the symbol is \
                 unused or absent from the codebase."
            ),
        }
    }

    /// The symbol resolved, but the specific edge/usage query on it came
    /// back with zero rows. Distinct from `not_found`: the symbol is real,
    /// but static analysis may simply not see how it's reached (dynamic
    /// dispatch, reflection, string-based invocation, or a public API
    /// consumed outside this repo).
    pub(crate) fn no_direct_usage(symbol: &str) -> Self {
        Caveat {
            class: "no_direct_usage",
            message: format!(
                "'{symbol}' has zero direct callers in the index. This can mean \
                 genuine dead code, but it can also mean call sites use dynamic \
                 dispatch, reflection, or string-based invocation that static \
                 analysis can't resolve, or that '{symbol}' is a public API consumed \
                 outside this repo. Do not treat this as proof of no usage without \
                 also checking dependencies() and the symbol's exported visibility."
            ),
        }
    }

    /// Same "zero direct callers" situation as `no_direct_usage`, but for a
    /// symbol the parser already flagged `is_entry_point` at index time
    /// (`detect_entry_point`/`rust_attr_is_dispatch_signal` and their
    /// per-language equivalents — `main`, a trait-dispatch protocol method,
    /// a decorator/annotation-registered handler, or a macro-attribute
    /// dispatch target such as an rmcp `#[tool(name = "...")]` MCP handler).
    /// For these, zero static callers isn't a "maybe dead, maybe hidden"
    /// judgment call — it's the expected, permanent shape: the real
    /// invocation happens through a mechanism (framework dispatch table,
    /// operator/protocol sugar, decorator registration) that never appears
    /// as a literal call-site token in source, so no amount of indexing —
    /// tree-sitter or a compiler-grade SCIP overlay alike — can ever
    /// populate this count. Distinct wording from `no_direct_usage` so a
    /// caller doesn't read this as a dead-code hint to chase down.
    pub(crate) fn entry_point_dispatch(symbol: &str) -> Self {
        Caveat {
            class: "entry_point_dispatch",
            message: format!(
                "'{symbol}' has zero direct callers in the index, but is flagged as a \
                 language/framework entry point (e.g. an rmcp #[tool(...)] MCP handler, \
                 `main`, a trait-dispatch protocol method, a decorator/annotation-registered \
                 handler, or similar). Its real invocation happens through a mechanism \
                 CALM's static call graph can't lexically capture — a near-zero caller_count \
                 here is expected by design, not a dead-code signal."
            ),
        }
    }

    /// WS3 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, V3
    /// Law 4 "unknown != nonexistent"): one or more call sites elsewhere in
    /// the repo tried to resolve a call to this bare name, found more than
    /// `MAX_CALLEE_CANDIDATES` same-named symbols to choose from, and were
    /// recorded in `ambiguity_groups` instead of an edge to any specific
    /// target -- this symbol may be one of those unresolved candidates.
    /// These sites are invisible to `direct`/`ambiguous` alike, so a low or
    /// zero `direct_count` for a common name is not proof of low usage.
    pub(crate) fn unresolved_ambiguity_groups(
        symbol: &str,
        site_count: usize,
        max_candidates: usize,
    ) -> Self {
        Caveat {
            class: "unresolved_ambiguity_group",
            message: format!(
                "{site_count} call site(s) elsewhere reference the name '{symbol}' but had \
                 more than MAX_CALLEE_CANDIDATES same-named candidates to choose from (up to \
                 {max_candidates} at the widest) -- '{symbol}' may be one of them, but none of \
                 these sites were recorded as an edge to any specific target, so they do not \
                 appear in direct/ambiguous above. Not enough evidence to resolve them \
                 automatically; treat a low direct_count for a common name with caution."
            ),
        }
    }

    /// Some, but not all, of a `symbols_batch` call's requested
    /// `qualified_names` matched nothing in the index. Names the first
    /// few missing ids so the caller doesn't have to diff the request
    /// against `results` to see which ones failed.
    pub(crate) fn batch_some_not_found(missing: &[String]) -> Self {
        let sample: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
        Caveat {
            class: "batch_some_not_found",
            message: format!(
                "{} of the requested qualified_names were not found in the index \
                 (e.g. {}). symbols_batch does no fuzzy matching — a near-miss id \
                 comes back found:false rather than silently substituting the \
                 closest name. Run search(kind=\"hybrid\") to get the exact \
                 qualified_name for each missing entry.",
                missing.len(),
                sample.join(", "),
            ),
        }
    }
}

/// Same as `ToolOutcome<T>`, plus the `ambiguous` branch every
/// `resolve_symbol`-based tool can also produce — same flatten-based,
/// root-`type:object` reasoning as `ToolOutcome<T>` above.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ResolvedOutcome<T> {
    /// See `ToolOutcome::flow_state`'s own doc comment -- same contract
    /// (including why it's named `flow_state`, not `state`), plus
    /// `ambiguous`/`not_found` both map onto `needs_context` (the caller
    /// needs one more piece of information: which candidate, or a
    /// different query).
    pub(crate) flow_state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorDetail>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    ambiguous: Option<AmbiguousResult>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    success: Option<T>,
    /// Advisory hint on an empty/not-found result. Never set alongside a
    /// populated `success` unless a tool opts in via `with_caveat` (e.g.
    /// `callers` on zero direct callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    caveat: Option<Caveat>,
}

impl<T> ResolvedOutcome<T> {
    pub(crate) fn error(detail: ErrorDetail) -> Self {
        let flow_state = classify_error_state(&detail.code, detail.recoverable);
        ResolvedOutcome {
            flow_state,
            error: Some(detail),
            ambiguous: None,
            success: None,
            caveat: None,
        }
    }

    /// Bridges the existing `SymbolResolution` match arms: `NotFound`/
    /// `Ambiguous` map onto their typed shape here. `Found` is left to the
    /// caller — it needs tool-specific work (`track_symbol`, health
    /// lookups, ...) that doesn't belong in a generic helper.
    pub(crate) fn not_found(symbol: &str) -> Self {
        let mut out = Self::error(not_found_error(symbol).error);
        out.caveat = Some(Caveat::not_found(symbol));
        out
    }

    pub(crate) fn ambiguous(candidates: &[CandidateRow]) -> Self {
        ResolvedOutcome {
            flow_state: "needs_context",
            error: None,
            ambiguous: Some(to_ambiguous(candidates)),
            success: None,
            caveat: None,
        }
    }

    pub(crate) fn success(value: T) -> Self {
        ResolvedOutcome {
            flow_state: "ready",
            error: None,
            ambiguous: None,
            success: Some(value),
            caveat: None,
        }
    }

    /// Attaches an advisory caveat to an already-built success result —
    /// e.g. `callers` on a resolved symbol with zero direct callers. Never
    /// overrides `error`/`ambiguous`; only meaningful after `success`.
    pub(crate) fn with_caveat(mut self, caveat: Caveat) -> Self {
        self.caveat = Some(caveat);
        self
    }
}

// ---------------------------------------------------------------------------
// Ambiguity Contract — shared symbol resolver
// ---------------------------------------------------------------------------
//
// `symbols.name` is not unique: the same bare name can appear in many files,
// or more than once in one file (distinct classes' methods). Tools that take
// a bare `symbol` name must not silently pick one match via `LIMIT 1` — per
// CONTRACTS.md they return `AmbiguousResult` instead when the name has
// multiple matches and no `path` was given to disambiguate.

pub(crate) const MAX_AMBIGUOUS_CANDIDATES: usize = 10;

#[derive(Serialize, JsonSchema)]
pub(crate) struct AmbiguousCandidate {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) kind: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) class_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) caller_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct AmbiguousResult {
    pub(crate) ambiguous: bool,
    /// Total candidates matched before the display cap of
    /// `MAX_AMBIGUOUS_CANDIDATES`. `truncated` is `true` when `total >
    /// candidates.len()`, telling the caller there are more matches than
    /// shown and to narrow with `path`/`line` — the list is never silently
    /// presented as the complete set.
    pub(crate) total: usize,
    pub(crate) truncated: bool,
    pub(crate) candidates: Vec<AmbiguousCandidate>,
}

/// One `symbols` row matched by a bare-name (+ optional path) lookup.
/// Carries enough columns to populate either a concrete tool output (e.g.
/// `SymbolInfoOutput`) or an `AmbiguousCandidate` when the lookup turns out
/// to match more than one row.
pub(crate) struct CandidateRow {
    pub(crate) name: String,
    pub(crate) qualified_name: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) line_start: i64,
    pub(crate) line_end: i64,
    pub(crate) signature: String,
    pub(crate) docstring: String,
    pub(crate) caller_count: i64,
    pub(crate) is_hub: bool,
    pub(crate) language: String,
    pub(crate) class_context: Option<String>,
    pub(crate) is_entry_point: bool,
    pub(crate) is_test: bool,
    pub(crate) coreness: Option<i64>, // from symbols.coreness column
    pub(crate) boundary_ambiguous: bool,
}

impl CandidateRow {
    pub(crate) fn to_symbol_info(&self) -> SymbolInfoOutput {
        SymbolInfoOutput {
            name: self.name.clone(),
            qualified_name: self.qualified_name.clone(),
            kind: self.kind.clone(),
            path: self.path.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            // Extracted verbatim from source at index time — a default
            // parameter value or doc-comment example can embed a real secret,
            // so this must be redacted the same as `source()`'s body text.
            signature: Some(calm_core::sanitize::sanitize_source_output(&self.signature))
                .filter(|s| !s.is_empty()),
            docstring: Some(calm_core::sanitize::sanitize_source_output(&self.docstring))
                .filter(|s| !s.is_empty()),
            caller_count: self.caller_count,
            is_hub: self.is_hub,
            coreness: None, // set by handler based on edges_ready
            health: None,
            suggested_next: None,
            type_relations: None, // set by symbol_info's handler when populated
            effects: None,
            content_warning: None, // set by symbol_info's handler when populated
        }
    }

    pub(crate) fn to_ambiguous_candidate(&self) -> AmbiguousCandidate {
        AmbiguousCandidate {
            name: self.name.clone(),
            path: self.path.clone(),
            kind: self.kind.clone(),
            line_start: self.line_start,
            line_end: self.line_end,
            class_context: self.class_context.clone(),
            caller_count: Some(self.caller_count),
            language: Some(self.language.clone()).filter(|s| !s.is_empty()),
            signature: Some(self.signature.clone()).filter(|s| !s.is_empty()),
        }
    }
}

/// All `symbols` rows matching `name` (and `path`, when given). Unlike the
/// old per-tool `LIMIT 1` queries, this returns every match so callers can
/// detect ambiguity instead of guessing.
pub(crate) fn resolve_symbol_candidates(
    conn: &rusqlite::Connection,
    name: &str,
    path: Option<&str>,
    // 3.4 (Wave 3, P1-3): when set, the SOLE filter -- qualified_name is
    // already unique, so name/path are redundant and ignored. This is the
    // "identity chaining" fix: a caller that already has an exact
    // qualified_name (e.g. from a prior `search` result) can never land on
    // `Ambiguous`, even for a globally-common bare name, while still
    // flowing through this same function's live-verification (`verify_live`
    // in `resolve_symbol`) -- unlike the original draft's literal
    // "short-circuit resolve_symbol" wording, which would have bypassed
    // that check and reintroduced P0-1 staleness risk (see this wave's
    // research-pass note in the execution plan).
    qualified_name: Option<&str>,
) -> rusqlite::Result<Vec<CandidateRow>> {
    let sql = if qualified_name.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE qualified_name = ?1"
    } else if path.is_some() {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 AND path = ?2 ORDER BY path, line_start"
    } else {
        "SELECT name, qualified_name, kind, path, line_start, line_end, signature, docstring, caller_count, is_hub, language, class_context, is_entry_point, is_test, coreness, boundary_ambiguous
         FROM symbols WHERE name = ?1 ORDER BY path, line_start"
    };

    // audit F9: `?` on both statement-level failures below (a genuine DB/
    // schema problem) — a single malformed *row* still doesn't kill the
    // whole result set (see the `filter_map` further down, deliberately
    // unchanged), only a failure to even prepare/execute the query does.
    let mut stmt = conn.prepare(sql).inspect_err(|e| {
        tracing::warn!("resolve_symbol_candidates: prepare failed for {name:?}: {e}");
    })?;

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<CandidateRow> {
        Ok(CandidateRow {
            name: row.get(0)?,
            qualified_name: row.get(1)?,
            kind: row.get(2)?,
            path: row.get(3)?,
            line_start: row.get(4)?,
            line_end: row.get(5)?,
            signature: row.get(6)?,
            docstring: row.get(7)?,
            caller_count: row.get(8)?,
            is_hub: row.get::<_, i64>(9)? != 0,
            language: row.get(10)?,
            class_context: row.get(11)?,
            is_entry_point: row.get::<_, i64>(12)? != 0,
            is_test: row.get::<_, i64>(13)? != 0,
            coreness: row.get(14)?,
            boundary_ambiguous: row.get::<_, i64>(15)? != 0,
        })
    };

    let rows = if let Some(qn) = qualified_name {
        stmt.query_map(rusqlite::params![qn], map_row)
    } else if let Some(path) = path {
        stmt.query_map(rusqlite::params![name, path], map_row)
    } else {
        stmt.query_map(rusqlite::params![name], map_row)
    };

    match rows {
        Ok(iter) => Ok(iter.filter_map(|r| r.ok()).collect()),
        Err(e) => {
            tracing::warn!("resolve_symbol_candidates: query_map failed for {name:?}: {e}");
            Err(e)
        }
    }
}
pub(crate) enum SymbolResolution {
    NotFound,
    Ambiguous(Vec<CandidateRow>),
    /// Wave 6 (audit follow-up, P0-B): the second field is the EXACT file
    /// content `verify_live` read to produce this candidate -- `Some` in
    /// every case except the pre-existing "no `file_index` row, trust the
    /// DB row as-is" escape hatch inside `verify_live` (still `None` there;
    /// see that function's own doc comment). A consumer that needs to serve
    /// this symbol's source text (`source()`, `understand()`) MUST slice
    /// from these bytes instead of re-reading the file itself -- re-reading
    /// opens a TOCTOU window between "what was verified" and "what was
    /// served" that a write landing in between could exploit (a file
    /// changing between this read and a second, independent read is not
    /// covered by anything else in the resolve path). Callers that only
    /// need the resolved coordinates/identity (not the content) are free to
    /// ignore this field.
    Found(Box<CandidateRow>, Option<String>),
    /// Disk read or fresh re-parse failed while live-verifying a resolved
    /// candidate (see `resolve_symbol`'s doc comment) -- distinct from a DB
    /// query failure (which stays in the outer `rusqlite::Result::Err`).
    /// Never silently degrades to the stale DB coordinates (that was P0-1g).
    ReadFailed(ErrorDetail),
}

// 5.2 (Wave 5, truth-kernel-hardening plan): resolve_symbol's cap on how
// many DB-ambiguous candidates it will live-verify (one file read + hash
// each) before degrading back to today's un-reverified Ambiguous. Matches
// this crate's existing cap convention (callers/callees/skipped_files
// truncate at a similar order of magnitude, always with the true count
// preserved rather than silently dropped).
const MAX_LIVE_VERIFIED_CANDIDATES: usize = 20;

/// Resolve a bare symbol name (+ optional path, + optional disambiguating
/// `line`) to exactly one row. `path` narrows the candidate set (see
/// `resolve_symbol_candidates`) but does not by itself guarantee a unique
/// match — `name` + `path` is not a DB-enforced unique key (only
/// `qualified_name` is), so e.g. two same-named functions in the same file
/// (a common shape in this codebase: `#[cfg(feature = "x")]` real impl vs.
/// `#[cfg(not(feature = "x"))]` stub, both named identically) still resolve
/// as ambiguous even with `path` set. `line` breaks that tie: when given, it
/// narrows to whichever candidate's `[line_start, line_end]` contains it —
/// exactly the range every `Ambiguous` response already echoes back per
/// candidate, so a caller that got `ambiguous: true` can retry once with
/// the `line_start` of the one it meant. A `line` that matches none of the
/// candidates is ignored (falls back to the unnarrowed set) rather than
/// forcing `NotFound` — a stale/wrong hint should degrade to the old
/// behavior, not make an otherwise-resolvable symbol disappear.
///
/// 2026-08-20 truth-kernel Wave 1 (P0-1): once narrowed to exactly one DB
/// candidate, it is live-verified against disk in THIS SAME call before
/// being trusted (see `verify_live`) — an index-derived coordinate is a
/// hint until checked against live disk in the same call that uses it, per
/// docs/plans/2026-08-20-truth-kernel-hardening-execution-plan.md. This was
/// folded directly into `resolve_symbol` (not a separate opt-in function)
/// so every one of its 9 existing callers, and any future one, gets the
/// check automatically -- a second function callers must remember to
/// invoke could silently be skipped by a new call site, reintroducing P0-1.
pub(crate) fn resolve_symbol(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    name: &str,
    path: Option<&str>,
    line: Option<i64>,
    // 3.4 (Wave 3, P1-3): see resolve_symbol_candidates' own doc comment --
    // threaded straight through, `line`-narrowing below still applies but
    // is a no-op once qualified_name has already narrowed to one candidate.
    qualified_name: Option<&str>,
) -> rusqlite::Result<SymbolResolution> {
    let mut candidates = resolve_symbol_candidates(conn, name, path, qualified_name)?;
    if let Some(line) = line {
        let in_range = |c: &CandidateRow| c.line_start <= line && line <= c.line_end;
        if candidates.iter().any(in_range) {
            candidates.retain(in_range);
        }
    }
    if candidates.is_empty() {
        return Ok(SymbolResolution::NotFound);
    }
    if candidates.len() == 1 {
        return Ok(verify_live(conn, project_root, candidates.remove(0)));
    }
    // 5.2 (Wave 5, Wave-1 residual): a DB-ambiguous result is no longer
    // trusted as-is -- each candidate is live-verified the same way a lone
    // Found candidate always has been. A candidate that's vanished from
    // disk since indexing no longer poisons the whole ambiguity; if
    // verification narrows the set to exactly one survivor, this now
    // returns Found instead of a stale Ambiguous.
    if candidates.len() > MAX_LIVE_VERIFIED_CANDIDATES {
        return Ok(SymbolResolution::Ambiguous(candidates));
    }
    // Wave 6 (audit follow-up, P0-B): carries each survivor's verified
    // bytes alongside it so a narrow-to-one-candidate result below can
    // still return them in `Found` -- an ambiguous set drops them (nothing
    // downstream serves content from an unresolved `Ambiguous`).
    let mut still_live: Vec<(CandidateRow, Option<String>)> = Vec::with_capacity(candidates.len());
    for c in candidates {
        match verify_live(conn, project_root, c) {
            SymbolResolution::Found(c, bytes) => still_live.push((*c, bytes)),
            SymbolResolution::NotFound => {}
            // Fail closed: can't be sure of the whole set if even one
            // candidate's live-check itself failed to read/re-parse.
            SymbolResolution::ReadFailed(e) => return Ok(SymbolResolution::ReadFailed(e)),
            SymbolResolution::Ambiguous(_) => unreachable!("verify_live never returns Ambiguous"),
        }
    }
    match still_live.len() {
        0 => Ok(SymbolResolution::NotFound),
        1 => {
            let (c, bytes) = still_live.remove(0);
            Ok(SymbolResolution::Found(Box::new(c), bytes))
        }
        _ => Ok(SymbolResolution::Ambiguous(
            still_live.into_iter().map(|(c, _)| c).collect(),
        )),
    }
}

/// Live-verifies a single DB-resolved candidate against disk before
/// `resolve_symbol` trusts it (2026-08-20 truth-kernel Wave 1, P0-1a/b/d/f/g).
/// Fast path (unchanged file): one read + one FNV hash, no re-parse. Slow
/// path (file changed since index): re-parses live and re-matches by full
/// identity `(name, kind, class_context)` -- not bare name alone (P0-1f) --
/// rather than trust a DB line range that may now point at the wrong code,
/// or at nothing (P0-1g: never silently falls back to the stale range).
pub(crate) fn verify_live(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    c: CandidateRow,
) -> SymbolResolution {
    let indexed_hash: Option<String> = conn
        .query_row(
            "SELECT hash FROM file_index WHERE path = ?1",
            rusqlite::params![c.path],
            |r| r.get(0),
        )
        .ok();
    // No file_index row for this symbol's path -- should not happen for a
    // healthy index, but degrade rather than panic: can't live-verify
    // identity via hash, so trust the DB row's coordinates as-is (no worse
    // than pre-Wave-1 behavior). Wave 6 (P0-B): still attempt a live read
    // so a downstream consumer (source()/understand()) can serve THESE
    // bytes instead of re-reading the file itself a second time -- a
    // second, independent read would reopen its own TOCTOU window even
    // though this escape hatch already can't verify identity.
    //
    // Wave 7 (audit follow-up, P0-C): DOCUMENTED RESIDUAL, not silently
    // dropped -- this branch still returns `Found` for a row whose
    // identity was never hash-verified against `file_index` (trusted
    // DB coordinates, not a proven match, and `bytes: None` on a read
    // failure is silently indistinguishable from a verified match with
    // no readable content), which an external audit correctly flagged.
    // Tried tightening this to `ReadFailed` on read failure and reverted
    // it the same day: it broke 39+ existing tests (symbol_info/
    // callers/pattern_debt/path/etc.) that legitimately insert a
    // `symbols` row with no backing file on disk and rely on this exact
    // escape hatch still returning DB-only metadata (coreness,
    // caller_count, type_relations -- none of which need file bytes) --
    // a real, intentional graceful-degradation path, not an oversight.
    // Closing the identity gap properly needs a typed signal (a new
    // `SymbolResolution` variant, or an explicit `verified` flag on
    // `CandidateRow`) threaded through every one of this type's ~16 call
    // sites so each can choose degrade-with-a-flag vs fail-closed for
    // itself -- scoped as follow-up, not done this wave.
    let Some(indexed_hash) = indexed_hash else {
        let bytes = std::fs::read_to_string(project_root.join(&c.path)).ok();
        return SymbolResolution::Found(Box::new(c), bytes);
    };
    let full_path = project_root.join(&c.path);
    let live = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => {
            return SymbolResolution::ReadFailed(error_detail(
                "READ_FAILED",
                &format!("could not read {} to live-verify '{}': {e}", c.path, c.name),
                true,
            ));
        }
    };
    if calm_core::indexer::pipeline::hash_content(&live) == indexed_hash {
        return SymbolResolution::Found(Box::new(c), Some(live));
    }
    let symbols = match calm_core::indexer::parser::extract_symbols(&live, &c.language, &c.path) {
        Ok(s) => s,
        Err(e) => {
            return SymbolResolution::ReadFailed(error_detail(
                "REPARSE_FAILED",
                &format!(
                    "{} changed on disk since indexing and could not be re-parsed to \
                     verify '{}' is still live: {e}",
                    c.path, c.name
                ),
                true,
            ));
        }
    };
    let matches = match_live_symbol(&symbols, &c.name, &c.kind, c.class_context.as_deref());
    match matches.len() {
        0 => SymbolResolution::NotFound,
        1 => {
            let m = matches[0];
            let mut c = c;
            c.line_start = m.line_start as i64;
            c.line_end = m.line_end as i64;
            SymbolResolution::Found(Box::new(c), Some(live))
        }
        n => SymbolResolution::ReadFailed(error_detail(
            "STALE_AMBIGUOUS",
            &format!(
                "{} changed on disk since indexing and now has {n} equally-matching live \
                 symbols named '{}' (was uniquely indexed) -- the index is stale; call \
                 search/locate fresh rather than trust any cached range",
                c.path, c.name
            ),
            true,
        )),
    }
}

/// Match live-parsed symbols against a DB-known identity key -- `(name,
/// kind, class_context)`, not bare name alone (P0-1f: two same-named
/// methods in different `impl` blocks must not cross-match). Shared by
/// `resolve_symbol`'s live-reresolve path and `best_live_range`'s
/// insertion-anchor re-parse (`edit.rs`) so both use one matching rule.
/// Callers decide how to handle 2+ survivors -- this returns every tie,
/// never guesses one.
pub(crate) fn match_live_symbol<'a>(
    symbols: &'a [calm_core::indexer::parser::ParsedSymbol],
    name: &str,
    kind: &str,
    class_context: Option<&str>,
) -> Vec<&'a calm_core::indexer::parser::ParsedSymbol> {
    symbols
        .iter()
        .filter(|s| {
            s.name == name && s.kind.as_str() == kind && s.class_context.as_deref() == class_context
        })
        .collect()
}

#[cfg(test)]
mod classify_error_state_tests {
    use super::classify_error_state;

    // Wave 2 (audit follow-up, 2026-08-23): the 12 AuthorityError-derived
    // codes (edit.rs's `AE` match) that used to fall through to the
    // generic `recoverable` default -- each now has an explicit,
    // semantics-matched bucket.
    #[test]
    fn authority_error_codes_are_explicitly_classified() {
        let cases: &[(&str, &str)] = &[
            ("AUTHORITY_FORGED_SIGNATURE", "blocked"),
            ("AUTHORITY_NOT_FOUND", "needs_context"),
            ("AUTHORITY_EXPIRED", "needs_context"),
            ("AUTHORITY_ALREADY_CONSUMED", "needs_context"),
            ("AUTHORITY_WRONG_INTENT", "needs_context"),
            ("AUTHORITY_WRONG_TARGET_SCOPE", "needs_context"),
            ("AUTHORITY_WRONG_PRINCIPAL", "needs_context"),
            ("AUTHORITY_STALE_SNAPSHOT", "needs_verification"),
            ("AUTHORITY_STALE_ANALYSIS_VERSION", "needs_verification"),
            ("AUTHORITY_STALE_POLICY", "needs_verification"),
            ("AUTHORITY_STALE_RISK_VECTOR", "needs_verification"),
            ("AUTHORITY_STALE_POLICY_DECISION", "needs_verification"),
        ];
        for (code, expected) in cases {
            assert_eq!(
                classify_error_state(code, true),
                *expected,
                "code {code} should classify as {expected}"
            );
        }
    }

    // Wave 2: 4 codes whose own error message contradicted their previous
    // bucket -- see each arm's own doc comment in classify_error_state for
    // the message quoted as evidence.
    #[test]
    fn previously_mismapped_codes_now_match_their_own_message() {
        assert_eq!(classify_error_state("PARSE_ERROR", true), "needs_context");
        assert_eq!(
            classify_error_state("REASON_NOT_GROUNDED", true),
            "needs_context"
        );
        assert_eq!(
            classify_error_state("DIFF_DIGEST_MISMATCH", true),
            "needs_verification"
        );
        assert_eq!(classify_error_state("USER_DECLINED", false), "blocked");
    }
}
