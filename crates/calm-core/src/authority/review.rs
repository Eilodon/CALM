//! `ReviewAuthority` -- CCK-09 (#65,
//! docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! A signed, single-use, snapshot-bound authority object: the durable,
//! structured replacement for `EditContextReview`'s session-local
//! `HashMap` entry. Invariant #2 (no stale evidence may grant authority)
//! and #3 (natural language is never a permission primitive) are both
//! enforced structurally here -- every field [`verify_and_consume`] checks
//! is a value, never a free-text match, and a single divergence in ANY of
//! them refuses the whole authority rather than trying to decide which
//! divergences are "close enough".
//!
//! **Adjustment (blueprint's own note on this PR):** `graph_generation` is
//! already a live, enforced staleness check (`STALE_GRAPH_AUTHORITY` in
//! `calm-server/src/tools/edit.rs`) -- this object folds that exact field
//! in as one of its own bound fields (kept, not reinvented) rather than
//! leaving two separate authority-validation paths that could disagree.
//!
//! Bound fields (9, matching the blueprint's §12 staleness-field count):
//! `intent_id`, `snapshot_id` (covers both "target source" and
//! "provider state" -- both are already inputs to
//! `authority::snapshot::EvidenceSnapshot`'s own digest, see that
//! module's doc comment; a second, separate provider-state field here
//! would just be a second, potentially-disagreeing copy of the same
//! signal), `graph_generation` (also embedded in `snapshot_id`, kept as
//! its own field too so a mismatch can be reported as the existing
//! `STALE_GRAPH_AUTHORITY` code rather than a generic "stale snapshot"),
//! `caller_set_digest`, `analysis_version`, `policy_digest`, `principal`,
//! plus the single-use `nonce` and `expires_at` that make the object a
//! capability rather than a plain record.
//!
//! **CCK-R5** (audit follow-up on this same blueprint): adds a 10th bound
//! field, `target_scope_digest` -- the original 9 never bound WHICH file/
//! symbol(s) the authority actually covers (`review_authority_targets` was
//! persisted for audit purposes only, never part of the signed payload),
//! so an authority minted for one symbol was, structurally, just as valid
//! for a different one as long as the OTHER 9 fields happened to still
//! match. See [`target_scope_digest`]'s own doc comment.
//!
//! **Known, permanent scope limit (audit 2026-08-10, CCK-29c):** this
//! object is scope-bound and risk-bound, never content-bound -- it names
//! WHICH target(s) and WHAT RISK CATEGORY a spend may exercise
//! (`target_scope_digest`, `risk_vector_digest`, `policy_decision_digest`),
//! never the literal bytes that end up written. This is structural, not an
//! oversight: every current mint path (`edit_context`'s compat wrapper,
//! `review_change`, `plan_change`) mints BEFORE the exact diff exists, so
//! there is no proposal content here to bind against. Two different
//! same-risk edits to the same target (e.g. two different `doc_only`
//! patches) are therefore both equally valid spends of one authority --
//! call this a "scope+risk-bound authority", not an "exact mutation
//! authority". For the one tier where a swap between "what was reviewed"
//! and "what got spent" is a real cross-principal attack -- `Human`-
//! required approvals, where a DIFFERENT party (the human) is the reviewer
//! -- exact-content binding lives one layer up, at the elicitation seal
//! (`HubEditStateSeal`/`hub_mrtr_decide` in `calm-server/src/tools/edit.rs`),
//! which binds the human's answer to a SHA-256 digest of the literal
//! proposed hunks, not just this authority's scope+risk. For `SelfReviewed`
//! spends there is no separate reviewer to deceive (the minting principal
//! and the spending principal are the same session), so content-binding at
//! this layer would add a check without a threat model behind it.

use rusqlite::{Connection, OptionalExtension, params};

use crate::authority::key::{control_key_for_conn, sign, verify};
use crate::change::intent::ChangeIntentTarget;

const SIGNING_DOMAIN: &str = "review-authority-v1";

/// Content digest of every versioned constant that affects analysis
/// correctness -- a CALM binary upgrade mid-session (new indexer, new
/// resolver) changes this even when neither `graph_generation` nor any
/// `EvidenceSnapshot` field does, since those track *when* the graph was
/// last rebuilt, not *what code* rebuilt it.
pub fn current_analysis_version() -> String {
    let material = format!(
        "analysis-version-v1\ngraph_derivation_version={}\npackage_graph_version={}\nsource_extraction_version={}\n",
        crate::graph::digest::GRAPH_DERIVATION_VERSION,
        crate::indexer::package_deps::PACKAGE_GRAPH_VERSION,
        crate::indexer::semantic_facts::SOURCE_EXTRACTION_VERSION,
    );
    crate::digest::evidence_digest(material.as_bytes())
}

/// Canonical, content-addressed digest of a target set -- order-independent
/// (sorted before hashing) so the same set of targets always binds
/// identically regardless of what order a caller happened to list them in.
/// CCK-R5 (audit follow-up): bound into the authority's signature via
/// [`MintParams::targets`]/[`CurrentState::targets`], so an authority
/// minted for one symbol/file cannot be silently reused (or partially
/// reused -- see the module doc comment) for a different one.
/// `edit_lines`/`edit_symbol` compute `current` from EVERY symbol an edit
/// actually touches, not just the first, so touching anything outside the
/// authorized scope changes this digest and fails `verify_and_consume`.
pub fn target_scope_digest(targets: &[ChangeIntentTarget]) -> String {
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
    let material = format!("target-scope-v1\n{}\n", canonical.join("\n"));
    crate::digest::evidence_digest(material.as_bytes())
}

/// Longest lifetime a [`ReviewAuthority`] may be minted with: 24 hours. A
/// review authority is meant to cover one edit shortly after its evidence
/// was gathered (`EvidenceSnapshot`'s own freshness window is much
/// shorter still), not to become a long-lived standing credential.
pub const AUTHORITY_TTL_MAX_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Validated lifetime for a minted [`ReviewAuthority`] -- CCK-R5.4 (audit
/// follow-up): replaces a raw `f64 ttl_secs` a caller could pass as NaN,
/// infinite, negative (yielding an authority born already-expired, which
/// `mint()` used to silently accept without complaint), or absurdly long
/// (a multi-year "review" authority). Validation happens once, at
/// construction, so everywhere downstream that reads
/// [`AuthorityTtl::as_secs`] can trust the value without re-checking it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuthorityTtl(f64);

#[derive(Debug, PartialEq)]
pub enum AuthorityTtlError {
    NotFinite,
    NotPositive,
    TooLong { secs: f64, max: f64 },
}

impl std::fmt::Display for AuthorityTtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFinite => write!(f, "authority TTL must be a finite number of seconds"),
            Self::NotPositive => write!(f, "authority TTL must be greater than zero seconds"),
            Self::TooLong { secs, max } => {
                write!(f, "authority TTL of {secs}s exceeds the maximum of {max}s")
            }
        }
    }
}

impl std::error::Error for AuthorityTtlError {}

impl AuthorityTtl {
    pub fn from_secs(secs: f64) -> Result<Self, AuthorityTtlError> {
        if !secs.is_finite() {
            return Err(AuthorityTtlError::NotFinite);
        }
        if secs <= 0.0 {
            return Err(AuthorityTtlError::NotPositive);
        }
        if secs > AUTHORITY_TTL_MAX_SECS {
            return Err(AuthorityTtlError::TooLong {
                secs,
                max: AUTHORITY_TTL_MAX_SECS,
            });
        }
        Ok(Self(secs))
    }

    pub fn as_secs(self) -> f64 {
        self.0
    }
}

/// Everything a caller must supply to [`ReviewAuthority::mint`]. Every
/// field here becomes a bound, signed value on the minted authority --
/// see the module doc comment for why there are exactly this many.
/// `target_scope_digest` is deliberately NOT a separate field here --
/// `mint` derives it from `targets` itself ([`target_scope_digest`]), so
/// there is exactly one way to compute it and a caller can never pass a
/// digest that doesn't actually match the targets it also passed.
pub struct MintParams<'a> {
    pub intent_id: &'a str,
    pub snapshot_id: &'a str,
    pub graph_generation: i64,
    pub caller_set_digest: &'a str,
    pub policy_digest: &'a str,
    pub principal: &'a str,
    /// How long from now until the minted authority expires -- CCK-R5.4:
    /// validated at construction (see [`AuthorityTtl::from_secs`]), so
    /// `mint` can never be handed a NaN, infinite, non-positive, or
    /// unreasonably long TTL.
    pub ttl_secs: AuthorityTtl,
    pub targets: &'a [ChangeIntentTarget],
    /// CCK-26 (audit follow-up): the real `PolicyEngine::evaluate()`
    /// decision that justified this mint -- signed and persisted for
    /// audit/provenance. NOT YET re-verified fresh at spend time (a
    /// `target_scope_digest`-style staleness check across these three is a
    /// natural follow-up, not done in this pass).
    pub policy_decision_digest: &'a str,
    pub risk_vector: &'a crate::policy::RiskVector,
    pub required_approver_class: crate::policy::ApproverClass,
}

/// The current truth to check a stored authority against, at
/// [`ReviewAuthority::verify_and_consume`] time. Deliberately a separate
/// type from [`MintParams`] (not reused) even though the field sets
/// overlap almost entirely -- mint time and verify time compute these
/// values independently (often minutes apart, from different tool calls),
/// and giving them different types is a small, free reminder of that
/// rather than inviting a caller to accidentally reuse a stale `MintParams`
/// as if it were still current. `targets` should be every symbol/file the
/// PROPOSED edit actually touches (CCK-R5) -- not just the one the caller
/// believes it's editing -- so a touch outside the authorized scope is
/// caught structurally rather than trusted.
pub struct CurrentState<'a> {
    pub intent_id: &'a str,
    pub snapshot_id: &'a str,
    pub graph_generation: i64,
    pub caller_set_digest: &'a str,
    pub policy_digest: &'a str,
    pub principal: &'a str,
    pub targets: &'a [ChangeIntentTarget],
    /// WS1 (audit follow-up, 2026-08-09): the real `PolicyDecision`/
    /// `RiskVector` digests computed from the ACTUAL proposed diff at
    /// spend time -- not the caller-supplied targets alone. Lets
    /// `verify_only` catch a proposal whose real risk/kind has drifted
    /// from what was reviewed (e.g. a `doc_only`-reviewed change spent as
    /// a real body edit), the same way `target_scope_digest` already
    /// catches a touch outside the reviewed scope.
    pub policy_decision_digest: &'a str,
    pub risk_vector: &'a crate::policy::RiskVector,
}

#[derive(Debug, PartialEq)]
pub enum AuthorityError {
    NotFound,
    /// The stored signature doesn't match a fresh HMAC over the stored
    /// fields -- either the row was tampered with out-of-band, or it was
    /// never legitimately minted (a forged `authority_id`).
    ForgedSignature,
    Expired,
    /// Either already consumed before this call, or consumed by a
    /// concurrent caller racing this one -- see `verify_and_consume`'s
    /// doc comment for why both collapse to the same variant.
    AlreadyConsumed,
    WrongIntent,
    /// CCK-R5: the touched symbol/file set no longer matches what this
    /// authority was minted for -- either a different target entirely, or
    /// the same target plus an extra one a wider edit also happened to
    /// overlap.
    WrongTargetScope,
    StaleSnapshot,
    StaleGraphGeneration,
    StaleCallerSet,
    StaleAnalysisVersion,
    StalePolicy,
    WrongPrincipal,
    /// WS1: the real `RiskVector` derived from the proposed diff at spend
    /// time no longer matches what was bound at mint -- e.g. touching more
    /// (or riskier) code than the reviewed proposal did.
    StaleRiskVector,
    /// WS1: the real `PolicyDecision` (aggregate risk / required approver
    /// class) derived from the proposed diff at spend time no longer
    /// matches what was bound at mint -- e.g. a `doc_only`-reviewed change
    /// spent as a real body edit, which escalates required approval.
    StalePolicyDecision,
    Db(rusqlite::Error),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no review authority with that id"),
            Self::ForgedSignature => write!(
                f,
                "review authority signature does not match its stored fields"
            ),
            Self::Expired => write!(f, "review authority has expired"),
            Self::AlreadyConsumed => {
                write!(f, "review authority was already consumed (single-use)")
            }
            Self::WrongIntent => {
                write!(f, "review authority was not minted for this change intent")
            }
            Self::WrongTargetScope => write!(
                f,
                "the touched symbol/file set does not match what this review authority was minted for"
            ),
            Self::StaleSnapshot => write!(
                f,
                "review authority's bound EvidenceSnapshot no longer matches current index state"
            ),
            Self::StaleGraphGeneration => write!(
                f,
                "STALE_GRAPH_AUTHORITY: graph_generation changed since this authority was minted"
            ),
            Self::StaleCallerSet => write!(f, "caller set changed since this authority was minted"),
            Self::StaleAnalysisVersion => write!(
                f,
                "analysis version changed since this authority was minted (binary upgraded?)"
            ),
            Self::StalePolicy => write!(f, "policy changed since this authority was minted"),
            Self::WrongPrincipal => {
                write!(f, "review authority was minted for a different principal")
            }
            Self::StaleRiskVector => write!(
                f,
                "the proposed change's real risk profile no longer matches what this review authority was minted for"
            ),
            Self::StalePolicyDecision => write!(
                f,
                "the proposed change's real policy decision no longer matches what this review authority was minted for"
            ),
            Self::Db(e) => write!(f, "review authority db error: {e}"),
        }
    }
}

impl From<rusqlite::Error> for AuthorityError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewAuthority {
    pub authority_id: String,
    pub intent_id: String,
    pub snapshot_id: String,
    pub graph_generation: i64,
    pub caller_set_digest: String,
    pub analysis_version: String,
    pub policy_digest: String,
    pub principal: String,
    pub target_scope_digest: String,
    pub nonce: String,
    pub expires_at: f64,
    pub signature: String,
    pub created_at: f64,
    /// CCK-26: see [`MintParams`]'s matching fields.
    pub policy_decision_digest: String,
    pub risk_vector_digest: String,
    pub required_approver_class: crate::policy::ApproverClass,
    /// Wave 10 (item 2/3): the raw `RiskVector` `risk_vector_digest` above
    /// is a hash OF, reconstructed by `load` from `review_authorities`'
    /// `mint_*` columns. NOT itself part of the signed payload -- always
    /// re-verify `self.minted_risk_vector.digest() ==
    /// self.risk_vector_digest` (the signature-covered field) before
    /// trusting it for anything, since these columns alone could be
    /// tampered with independently of the signature. A pre-migration or
    /// otherwise-untrustworthy row reconstructs to a best-effort (usually
    /// weakest-possible) vector that legitimately fails that check --
    /// treat that as "raw fields unavailable", not as a real claim.
    pub minted_risk_vector: crate::policy::RiskVector,
}

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{nanos:016x}-{counter:08x}-{}", std::process::id())
}

/// Canonical (field-order-fixed) payload every signature is computed over
/// -- shared by mint (sign) and verify (re-derive and compare), so the two
/// can never accidentally diverge in field order or formatting.
#[allow(clippy::too_many_arguments)]
fn signing_payload(
    authority_id: &str,
    intent_id: &str,
    snapshot_id: &str,
    graph_generation: i64,
    caller_set_digest: &str,
    analysis_version: &str,
    policy_digest: &str,
    principal: &str,
    target_scope_digest: &str,
    nonce: &str,
    expires_at: f64,
    policy_decision_digest: &str,
    risk_vector_digest: &str,
    required_approver_class: &str,
) -> String {
    format!(
        "authority_id={authority_id}\nintent_id={intent_id}\nsnapshot_id={snapshot_id}\n\
         graph_generation={graph_generation}\ncaller_set_digest={caller_set_digest}\n\
         analysis_version={analysis_version}\npolicy_digest={policy_digest}\n\
         principal={principal}\ntarget_scope_digest={target_scope_digest}\n\
         nonce={nonce}\nexpires_at={expires_at}\n\
         policy_decision_digest={policy_decision_digest}\nrisk_vector_digest={risk_vector_digest}\n\
         required_approver_class={required_approver_class}\n"
    )
}

impl ReviewAuthority {
    /// Mints, signs, and persists a new authority in one call --
    /// `state_conn` must be a real on-disk state.db connection (a
    /// path-less `:memory:` connection has no `control.key` to sign with;
    /// see `key::control_key_for_conn`, and every real writer connection
    /// always has a real path).
    pub fn mint(state_conn: &Connection, params: MintParams) -> Result<Self, AuthorityError> {
        let key = control_key_for_conn(state_conn)
            .map_err(|e| AuthorityError::Db(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?
            .ok_or(AuthorityError::NotFound)?; // no real key => refuse to mint, same fail-closed posture as the ledger

        let authority_id = new_id("AUTH");
        let nonce = new_id("NONCE");
        let created_at = now_epoch_secs();
        let expires_at = created_at + params.ttl_secs.as_secs();
        let analysis_version = current_analysis_version();
        let target_scope_digest = target_scope_digest(params.targets);
        // Wave 10 (item 2/3): derived here, not caller-supplied, so it can
        // never drift from the raw RiskVector persisted below.
        let risk_vector_digest = params.risk_vector.digest();

        let signature = sign(
            &key,
            SIGNING_DOMAIN,
            &signing_payload(
                &authority_id,
                params.intent_id,
                params.snapshot_id,
                params.graph_generation,
                params.caller_set_digest,
                &analysis_version,
                params.policy_digest,
                params.principal,
                &target_scope_digest,
                &nonce,
                expires_at,
                params.policy_decision_digest,
                &risk_vector_digest,
                params.required_approver_class.as_str(),
            ),
        );

        let authority = Self {
            authority_id,
            intent_id: params.intent_id.to_string(),
            snapshot_id: params.snapshot_id.to_string(),
            graph_generation: params.graph_generation,
            caller_set_digest: params.caller_set_digest.to_string(),
            analysis_version,
            policy_digest: params.policy_digest.to_string(),
            principal: params.principal.to_string(),
            target_scope_digest,
            nonce,
            expires_at,
            signature,
            created_at,
            policy_decision_digest: params.policy_decision_digest.to_string(),
            risk_vector_digest,
            required_approver_class: params.required_approver_class,
            minted_risk_vector: params.risk_vector.clone(),
        };

        // CCK-R5: snapshot persist + intent insert (by the caller, before
        // this) + this mint are meant to be wrapped in one transaction by
        // the caller -- see `mint_review_authority_for_edit_context`
        // (calm-server/src/tools/guardrails.rs) for the actual BEGIN/COMMIT
        // this module doesn't own itself (it only ever sees `state_conn`,
        // never opens or commits a transaction on it).
        authority.persist(state_conn, params.targets)?;
        Ok(authority)
    }

    fn persist(
        &self,
        state_conn: &Connection,
        targets: &[ChangeIntentTarget],
    ) -> rusqlite::Result<()> {
        state_conn.execute(
            "INSERT INTO review_authorities \
             (authority_id, intent_id, snapshot_id, graph_generation, caller_set_digest, \
              analysis_version, policy_digest, principal, target_scope_digest, nonce, \
              expires_at, signature, created_at, consumed_at, \
              policy_decision_digest, risk_vector_digest, required_approver_class, \
              mint_caller_count_level, mint_is_hub, mint_hub_kind, mint_signature_changed, \
              mint_uncertain_zero_caller, mint_risk_rule_floor, mint_kind_mismatch, \
              mint_touches_manifest, mint_touches_uncovered_code) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL, ?14, ?15, ?16, \
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                self.authority_id,
                self.intent_id,
                self.snapshot_id,
                self.graph_generation,
                self.caller_set_digest,
                self.analysis_version,
                self.policy_digest,
                self.principal,
                self.target_scope_digest,
                self.nonce,
                self.expires_at,
                self.signature,
                self.created_at,
                self.policy_decision_digest,
                self.risk_vector_digest,
                self.required_approver_class.as_str(),
                self.minted_risk_vector.caller_count_level.as_str(),
                self.minted_risk_vector.is_hub,
                self.minted_risk_vector.hub_kind,
                self.minted_risk_vector.signature_changed,
                self.minted_risk_vector.uncertain_zero_caller,
                self.minted_risk_vector.risk_rule_floor.map(|l| l.as_str()),
                self.minted_risk_vector.kind_mismatch,
                self.minted_risk_vector.touches_manifest,
                self.minted_risk_vector.touches_uncovered_code,
            ],
        )?;
        for target in targets {
            state_conn.execute(
                "INSERT INTO review_authority_targets (authority_id, path, qualified_name) VALUES (?1, ?2, ?3)",
                params![self.authority_id, target.path, target.qualified_name],
            )?;
        }
        Ok(())
    }

    fn load(state_conn: &Connection, authority_id: &str) -> Result<Option<Self>, AuthorityError> {
        state_conn
            .query_row(
                "SELECT authority_id, intent_id, snapshot_id, graph_generation, caller_set_digest, \
                 analysis_version, policy_digest, principal, nonce, expires_at, signature, \
                 created_at, target_scope_digest, policy_decision_digest, risk_vector_digest, \
                 required_approver_class, mint_caller_count_level, mint_is_hub, mint_hub_kind, \
                 mint_signature_changed, mint_uncertain_zero_caller, mint_risk_rule_floor, \
                 mint_kind_mismatch, mint_touches_manifest, mint_touches_uncovered_code \
                 FROM review_authorities WHERE authority_id = ?1",
                params![authority_id],
                |r| -> rusqlite::Result<Self> {
                    let required_approver_class_raw: String = r.get("required_approver_class")?;
                    let required_approver_class = crate::policy::ApproverClass::parse(
                        &required_approver_class_raw,
                    )
                    .ok_or(rusqlite::Error::InvalidColumnType(
                        15,
                        "required_approver_class".to_string(),
                        rusqlite::types::Type::Text,
                    ))?;
                    // Wave 10 (item 2/3): best-effort reconstruction -- an
                    // unparseable mint_caller_count_level (a pre-migration
                    // row, still at its DEFAULT '' from the ALTER) falls
                    // back to the WEAKEST level rather than erroring the
                    // whole load. Safe by construction: `verify_only`'s own
                    // fallback path independently re-derives THIS
                    // reconstruction's digest and refuses to trust it
                    // unless that matches the SIGNED `risk_vector_digest` --
                    // a default/bogus reconstruction can never
                    // coincidentally produce that digest, so it just falls
                    // through to the existing strict-equality behavior
                    // instead of ever being treated as a real, weaker-than-
                    // actual mint-time claim.
                    let mint_caller_count_level_raw: String = r.get("mint_caller_count_level")?;
                    let mint_risk_rule_floor_raw: Option<String> = r.get("mint_risk_rule_floor")?;
                    let minted_risk_vector = crate::policy::RiskVector {
                        caller_count_level: crate::policy::RiskLevel::parse(
                            &mint_caller_count_level_raw,
                        )
                        .unwrap_or(crate::policy::RiskLevel::Low),
                        is_hub: r.get("mint_is_hub")?,
                        hub_kind: r.get("mint_hub_kind")?,
                        signature_changed: r.get("mint_signature_changed")?,
                        uncertain_zero_caller: r.get("mint_uncertain_zero_caller")?,
                        risk_rule_floor: mint_risk_rule_floor_raw
                            .and_then(|s| crate::policy::RiskLevel::parse(&s)),
                        kind_mismatch: r.get("mint_kind_mismatch")?,
                        touches_manifest: r.get("mint_touches_manifest")?,
                        touches_uncovered_code: r.get("mint_touches_uncovered_code")?,
                    };
                    Ok(Self {
                        authority_id: r.get("authority_id")?,
                        intent_id: r.get("intent_id")?,
                        snapshot_id: r.get("snapshot_id")?,
                        graph_generation: r.get("graph_generation")?,
                        caller_set_digest: r.get("caller_set_digest")?,
                        analysis_version: r.get("analysis_version")?,
                        policy_digest: r.get("policy_digest")?,
                        principal: r.get("principal")?,
                        target_scope_digest: r.get("target_scope_digest")?,
                        nonce: r.get("nonce")?,
                        expires_at: r.get("expires_at")?,
                        signature: r.get("signature")?,
                        created_at: r.get("created_at")?,
                        policy_decision_digest: r.get("policy_decision_digest")?,
                        risk_vector_digest: r.get("risk_vector_digest")?,
                        required_approver_class,
                        minted_risk_vector,
                    })
                },
            )
            .optional()
            .map_err(AuthorityError::from)
    }

    /// Verifies `authority_id` against `current`, then atomically consumes
    /// it (single-use) -- `Ok(())` only when every check passes AND this
    /// call is the one that wins the consume race. Checks run in an order
    /// that never trusts a field before the signature covering it has
    /// been confirmed: signature first (a forged row's other fields mean
    /// nothing), then expiry, then every bound field against `current`,
    /// and only then the atomic consume -- a concurrent caller that wins
    /// that last race collapses to the same `AlreadyConsumed` a replay
    /// attempt would get, which is the correct outcome for both (this
    /// call must not proceed as authorized either way).
    pub fn verify_and_consume(
        state_conn: &Connection,
        authority_id: &str,
        current: &CurrentState,
    ) -> Result<(), AuthorityError> {
        Self::verify_only(state_conn, authority_id, current)?;
        Self::consume(state_conn, authority_id, None)
    }

    /// CCK-25: every check `verify_and_consume` did, MINUS the final
    /// consume -- split out so `authorize_and_begin_edit` below can run
    /// verification, `txn::begin_internal`, and `consume` as three steps
    /// of ONE outer transaction, instead of `verify_and_consume` committing
    /// its own consume before the caller (previously) got anywhere near
    /// `txn::begin`. `pub` (not just an internal helper): callers like
    /// `edit_lines_impl_gated` also use this standalone, read-only, to
    /// decide whether the legacy gate can be skipped WITHOUT spending the
    /// authority yet -- only `authorize_and_begin_edit`'s later call, right
    /// before the durable transaction opens, actually consumes it. Calling
    /// this alone can never grant permission by itself, so exposing it
    /// publicly doesn't weaken single-use semantics.
    pub fn verify_only(
        state_conn: &Connection,
        authority_id: &str,
        current: &CurrentState,
    ) -> Result<(), AuthorityError> {
        let authority = Self::load(state_conn, authority_id)?.ok_or(AuthorityError::NotFound)?;

        let key = control_key_for_conn(state_conn)
            .map_err(|e| AuthorityError::Db(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?
            .ok_or(AuthorityError::NotFound)?;
        let payload = signing_payload(
            &authority.authority_id,
            &authority.intent_id,
            &authority.snapshot_id,
            authority.graph_generation,
            &authority.caller_set_digest,
            &authority.analysis_version,
            &authority.policy_digest,
            &authority.principal,
            &authority.target_scope_digest,
            &authority.nonce,
            authority.expires_at,
            &authority.policy_decision_digest,
            &authority.risk_vector_digest,
            authority.required_approver_class.as_str(),
        );
        if !verify(&key, SIGNING_DOMAIN, &payload, &authority.signature) {
            return Err(AuthorityError::ForgedSignature);
        }

        if now_epoch_secs() > authority.expires_at {
            return Err(AuthorityError::Expired);
        }

        // Field-by-field against the caller's current truth -- every
        // mismatch gets its own variant so a denial can name exactly
        // which staleness dimension fired, same precision
        // STALE_GRAPH_AUTHORITY's own error already has today.
        if authority.intent_id != current.intent_id {
            return Err(AuthorityError::WrongIntent);
        }
        // CCK-R5: recomputed from EVERY symbol/file `current.targets` says
        // the proposed edit actually touches -- not trusted as a
        // caller-supplied digest, so a caller can't pass a stale or
        // mismatched one and have it silently accepted.
        if authority.target_scope_digest != target_scope_digest(current.targets) {
            return Err(AuthorityError::WrongTargetScope);
        }
        if authority.graph_generation != current.graph_generation {
            return Err(AuthorityError::StaleGraphGeneration);
        }
        if authority.snapshot_id != current.snapshot_id {
            return Err(AuthorityError::StaleSnapshot);
        }
        if authority.caller_set_digest != current.caller_set_digest {
            return Err(AuthorityError::StaleCallerSet);
        }
        if authority.analysis_version != current_analysis_version() {
            return Err(AuthorityError::StaleAnalysisVersion);
        }
        if authority.policy_digest != current.policy_digest {
            return Err(AuthorityError::StalePolicy);
        }
        if authority.principal != current.principal {
            return Err(AuthorityError::WrongPrincipal);
        }
        // WS1: the real risk/policy decision for the ACTUAL proposed diff,
        // recomputed by the caller at spend time (see `CurrentState`'s own
        // doc comment) -- catches a proposal whose real risk/kind drifted
        // from what was reviewed at mint time (e.g. a `doc_only`-reviewed
        // change spent as a real body edit), which `target_scope_digest`
        // alone cannot: that digest only binds WHICH symbols/files were
        // touched, never WHAT KIND of change was made to them.
        let current_risk_vector_digest = current.risk_vector.digest();
        let risk_vector_exact_match = authority.risk_vector_digest == current_risk_vector_digest;
        if !risk_vector_exact_match {
            // Wave 10 (item 2): an exact-digest mismatch used to always
            // fail outright, even when the real drift only ever made the
            // spend LESS risky than what was already reviewed and
            // approved. Fall back to a sound per-field `spend ⊑ mint`
            // comparison (`RiskVector::is_covered_by`) -- but only once the
            // persisted mint-time raw fields are confirmed trustworthy:
            // `load` reconstructs `minted_risk_vector` from columns that
            // are NOT themselves part of the signed payload, so re-deriving
            // ITS digest and requiring it match the signature-covered
            // `risk_vector_digest` is what closes that gap (a pre-
            // migration row, or a tampered column, fails this and
            // correctly falls through to the strict denial below, same as
            // before this fallback existed).
            let minted_fields_trustworthy =
                authority.minted_risk_vector.digest() == authority.risk_vector_digest;
            if !minted_fields_trustworthy
                || !current
                    .risk_vector
                    .is_covered_by(&authority.minted_risk_vector)
            {
                return Err(AuthorityError::StaleRiskVector);
            }
        }
        // Wave 10 (item 2): only enforced on the EXACT-match path above --
        // once risk_vector_digest matches AND policy_digest already
        // matched (checked earlier), `evaluate` being a pure function of
        // (risk_vector, policy) means policy_decision_digest is already
        // guaranteed identical by construction; this check exists to catch
        // a caller that supplied a policy_decision_digest it didn't
        // actually derive that way. On the ⊑ fallback path just above,
        // `current`'s decision is provably no MORE demanding than
        // `minted`'s (`RiskVector::is_covered_by`'s own doc comment:
        // covered implies `aggregate_risk` is no higher, and
        // `ApproverClass::from_risk` is monotone in that same level) --
        // whatever approval class was already satisfied for the minted
        // authority already covers a same-or-lower-risk spend, so
        // re-checking the exact decision digest here would just reject
        // spends this fallback already proved safe.
        if risk_vector_exact_match
            && authority.policy_decision_digest != current.policy_decision_digest
        {
            return Err(AuthorityError::StalePolicyDecision);
        }
        Ok(())
    }

    /// Wave 9 (audit follow-up, "doable now" per the WrongTargetScope vs
    /// StaleRiskVector split): unlike every other field folded into a
    /// digest-only `AuthorityError` variant, `target_scope_digest`'s raw
    /// inputs ARE recoverable without a schema migration -- every mint call
    /// site persists the exact same `targets` it minted against as a
    /// `ChangeIntent` row (`intent.targets`, see e.g.
    /// `mint_review_authority_for_edit_context` in calm-server) before ever
    /// calling `mint`, and `intent_id` is a bound, signed field on the
    /// authority itself. Best-effort/read-only: `None` on any lookup
    /// failure (authority gone, intent gone) rather than an error -- this
    /// is purely diagnostic enrichment for a WRONG_TARGET_SCOPE message,
    /// never itself a source of truth for authorization.
    pub fn minted_targets_for_mismatch_detail(
        state_conn: &Connection,
        authority_id: &str,
    ) -> Option<Vec<crate::change::ChangeIntentTarget>> {
        let authority = Self::load(state_conn, authority_id).ok().flatten()?;
        let intent = crate::change::get_change_intent(state_conn, &authority.intent_id)
            .ok()
            .flatten()?;
        Some(intent.targets)
    }

    /// CCK-25: the atomic single-use consume, split out of
    /// `verify_and_consume` -- `consumed_by_tx_id` provenance-binds this
    /// consume to the exact `edit_transactions` row it authorized (`None`
    /// preserves `verify_and_consume`'s old standalone behavior for its
    /// existing callers/tests).
    fn consume(
        state_conn: &Connection,
        authority_id: &str,
        consumed_by_tx_id: Option<&str>,
    ) -> Result<(), AuthorityError> {
        let consumed_rows = state_conn.execute(
            "UPDATE review_authorities SET consumed_at = ?1, consumed_by_tx_id = ?2 \
             WHERE authority_id = ?3 AND consumed_at IS NULL",
            params![now_epoch_secs(), consumed_by_tx_id, authority_id],
        )?;
        if consumed_rows == 0 {
            return Err(AuthorityError::AlreadyConsumed);
        }
        Ok(())
    }

    /// CCK-25 (P1 fix, audit 2026-08-09): the write path's actual choke
    /// point -- verify, open the durable `edit_transactions` row (bound to
    /// this authority via its new `authority_id` column), and consume the
    /// authority (bound back via `consumed_by_tx_id`), all inside ONE
    /// `BEGIN IMMEDIATE`/`COMMIT`. Before this, `verify_and_consume` ran
    /// (and committed its own consume) hundreds of lines before
    /// `txn::begin` -- if `txn::begin` then failed, the authority was
    /// already permanently burned with no durable transaction and no file
    /// write to show for it (an "orphaned burned authority"). Now either
    /// all three steps land together, or none do; nothing touches the
    /// filesystem until this returns `Ok`.
    pub fn authorize_and_begin_edit(
        state_conn: &Connection,
        authority_id: &str,
        current: &CurrentState,
        project_id: &str,
        path: &str,
        base_digest: &str,
        proposed_digest: &str,
    ) -> Result<crate::txn::EditTransaction, AuthorizeEditError> {
        state_conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<crate::txn::EditTransaction, AuthorizeEditError> {
            Self::verify_only(state_conn, authority_id, current)?;
            let tx = crate::txn::begin_internal(
                state_conn,
                project_id,
                path,
                base_digest,
                proposed_digest,
                Some(authority_id),
            )?;
            Self::consume(state_conn, authority_id, Some(&tx.tx_id))?;
            Ok(tx)
        })();
        match result {
            Ok(tx) => {
                state_conn.execute_batch("COMMIT;")?;
                Ok(tx)
            }
            Err(e) => {
                let _ = state_conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    }
}

/// CCK-25: the union of what can fail inside `authorize_and_begin_edit` --
/// authority verification/consume, `edit_transactions` insertion, or the
/// wrapping transaction itself.
#[derive(Debug)]
pub enum AuthorizeEditError {
    Authority(AuthorityError),
    Txn(crate::txn::TxnError),
    Db(rusqlite::Error),
}

impl std::fmt::Display for AuthorizeEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authority(e) => write!(f, "{e}"),
            Self::Txn(e) => write!(f, "{e}"),
            Self::Db(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AuthorizeEditError {}

impl From<AuthorityError> for AuthorizeEditError {
    fn from(e: AuthorityError) -> Self {
        Self::Authority(e)
    }
}

impl From<crate::txn::TxnError> for AuthorizeEditError {
    fn from(e: crate::txn::TxnError) -> Self {
        Self::Txn(e)
    }
}

impl From<rusqlite::Error> for AuthorizeEditError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::{STATE_DB_SCHEMA_VERSION, init_state_db};
    use crate::db::state_migrations::migrate_state_db_to_current;
    use std::path::Path;

    /// A real on-disk connection -- `mint`/`verify_and_consume` both
    /// need `control_key_for_conn` to find a real path, which `:memory:`
    /// never has.
    fn real_state_conn(dir: &Path) -> Connection {
        let db_path = dir.join(".calm").join("state.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let conn = Connection::open(&db_path).unwrap();
        init_state_db(&conn).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        assert_eq!(
            conn.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0))
                .unwrap(),
            STATE_DB_SCHEMA_VERSION
        );
        conn
    }

    /// review_authorities.intent_id/snapshot_id are real FK columns --
    /// seeds the rows every mint_params()/base_current() fixture below
    /// references ("SNP-1"/"INT-1") so mint() doesn't need its own
    /// change_intents/evidence_snapshots round trip just to be exercised
    /// in isolation here (that round trip is already covered by
    /// change::store's own tests).
    fn seed_intent_and_snapshot(conn: &Connection) {
        conn.execute(
            "INSERT INTO evidence_snapshots \
             (snapshot_id, source_catalog_digest, graph_generation, freshness_class, created_at) \
             VALUES ('SNP-1', 'digest-1', 5, 'current', 0.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO change_intents (intent_id, kind, reason, snapshot_id, created_at) \
             VALUES ('INT-1', 'body', 'test fixture', 'SNP-1', 0.0)",
            [],
        )
        .unwrap();
    }

    /// Wave 10 (item 2/3): a single, process-wide fixture `RiskVector`
    /// both `mint_params()` and `base_current()` bind a `&'static`
    /// reference to -- gives every existing test a MATCHING risk vector on
    /// both mint and spend sides by default (so the exact-digest fast path
    /// in `verify_only` still applies, unchanged, for every test that
    /// doesn't deliberately vary it), without needing to thread a
    /// caller-supplied `RiskVector` through 40+ call sites.
    fn base_risk_vector() -> &'static crate::policy::RiskVector {
        static VECTOR: std::sync::OnceLock<crate::policy::RiskVector> = std::sync::OnceLock::new();
        VECTOR.get_or_init(|| crate::policy::RiskVector {
            caller_count_level: crate::policy::RiskLevel::Low,
            is_hub: false,
            hub_kind: None,
            signature_changed: false,
            uncertain_zero_caller: false,
            risk_rule_floor: None,
            kind_mismatch: false,
            touches_manifest: false,
            touches_uncovered_code: false,
        })
    }

    /// Every existing test in this module mints/verifies with an empty
    /// target scope by default (`mint_params(&[])` paired with this) --
    /// `wrong_target_scope_is_rejected` below is the one that varies it.
    fn base_current() -> CurrentState<'static> {
        CurrentState {
            intent_id: "INT-1",
            snapshot_id: "SNP-1",
            graph_generation: 5,
            caller_set_digest: "callers-1",
            policy_digest: "policy-1",
            principal: "session:abc",
            targets: &[],
            // Matches mint_params()'s own values below -- WS1: a test that
            // wants a real mismatch overrides just this field via
            // `..base_current()`, same pattern every other field here uses.
            policy_decision_digest: "policy-decision-1",
            risk_vector: base_risk_vector(),
        }
    }

    fn mint_params<'a>(targets: &'a [ChangeIntentTarget]) -> MintParams<'a> {
        MintParams {
            intent_id: "INT-1",
            snapshot_id: "SNP-1",
            graph_generation: 5,
            caller_set_digest: "callers-1",
            policy_digest: "policy-1",
            principal: "session:abc",
            ttl_secs: AuthorityTtl::from_secs(300.0).unwrap(),
            targets,
            policy_decision_digest: "policy-decision-1",
            risk_vector: base_risk_vector(),
            required_approver_class: crate::policy::ApproverClass::SelfReviewed,
        }
    }

    #[test]
    fn mint_then_verify_with_matching_current_state_succeeds_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();

        ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current())
            .unwrap();
        let replay =
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current());
        assert_eq!(replay, Err(AuthorityError::AlreadyConsumed));
    }

    #[test]
    fn authorize_and_begin_edit_binds_transaction_and_authority_together() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();

        let tx = ReviewAuthority::authorize_and_begin_edit(
            &conn,
            &authority.authority_id,
            &base_current(),
            "proj",
            "f.rs",
            "base-digest",
            "proposed-digest",
        )
        .unwrap();

        let bound_authority_id: Option<String> = conn
            .query_row(
                "SELECT authority_id FROM edit_transactions WHERE tx_id = ?1",
                params![tx.tx_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            bound_authority_id.as_deref(),
            Some(authority.authority_id.as_str()),
            "edit_transactions.authority_id must be set, not left NULL"
        );

        let bound_tx_id: Option<String> = conn
            .query_row(
                "SELECT consumed_by_tx_id FROM review_authorities WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            bound_tx_id.as_deref(),
            Some(tx.tx_id.as_str()),
            "review_authorities.consumed_by_tx_id must point back at the same tx_id"
        );
    }

    #[test]
    fn authorize_and_begin_edit_rolls_back_the_transaction_insert_when_consume_fails() {
        // Deterministic substitute for a crash between "authority verified /
        // edit_transactions row inserted" and "authority consumed" (same
        // technique as txn::begin_is_atomic_with_its_seq_1_event): force the
        // consume step specifically to fail (by consuming the authority
        // first, so a second attempt hits AlreadyConsumed) and assert the
        // edit_transactions row that begin_internal already inserted inside
        // that same not-yet-committed transaction did NOT survive.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();

        let tx1 = ReviewAuthority::authorize_and_begin_edit(
            &conn,
            &authority.authority_id,
            &base_current(),
            "proj",
            "f.rs",
            "base-1",
            "proposed-1",
        )
        .unwrap();

        let count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM edit_transactions", [], |r| r.get(0))
            .unwrap();

        // Same (now-consumed) authority again -- verify_only still passes
        // (signature/expiry/fields are all still valid), begin_internal
        // inserts a SECOND edit_transactions row, then consume fails with
        // AlreadyConsumed -- the whole transaction must roll back, taking
        // that second INSERT with it.
        let err = ReviewAuthority::authorize_and_begin_edit(
            &conn,
            &authority.authority_id,
            &base_current(),
            "proj",
            "f.rs",
            "base-2",
            "proposed-2",
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                AuthorizeEditError::Authority(AuthorityError::AlreadyConsumed)
            ),
            "expected AlreadyConsumed, got {err:?}"
        );

        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM edit_transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count_before, count_after,
            "the second call's edit_transactions insert must have been rolled back, \
             not left behind as an orphan alongside the still-failed authority spend"
        );
        // The first, successful spend must still be intact -- rollback of
        // the second attempt must not have touched it.
        let still_bound: Option<String> = conn
            .query_row(
                "SELECT consumed_by_tx_id FROM review_authorities WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_bound.as_deref(), Some(tx1.tx_id.as_str()));
    }

    #[test]
    fn unknown_authority_id_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let err =
            ReviewAuthority::verify_and_consume(&conn, "AUTH-does-not-exist", &base_current());
        assert_eq!(err, Err(AuthorityError::NotFound));
    }

    #[test]
    fn forged_signature_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        conn.execute(
            "UPDATE review_authorities SET signature = 'hmac-sha256:0000' WHERE authority_id = ?1",
            params![authority.authority_id],
        )
        .unwrap();
        let err =
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current());
        assert_eq!(err, Err(AuthorityError::ForgedSignature));
    }

    #[test]
    fn tampering_with_any_stored_field_is_caught_by_the_signature_check() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        // Tamper with graph_generation directly in the DB, bypassing the
        // API -- the signature was computed over the ORIGINAL value, so
        // this must fail the signature check before it ever reaches the
        // graph_generation comparison.
        conn.execute(
            "UPDATE review_authorities SET graph_generation = 999 WHERE authority_id = ?1",
            params![authority.authority_id],
        )
        .unwrap();
        let err =
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current());
        assert_eq!(err, Err(AuthorityError::ForgedSignature));
    }

    #[test]
    fn expired_authority_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        // AuthorityTtl (CCK-R5.4) rejects a non-positive TTL at construction,
        // so mint() itself can no longer produce an already-expired
        // authority -- simulate time having passed instead, by rewriting
        // expires_at into the past directly on the stored row and
        // re-deriving its signature over the new value (same pattern
        // changed_analysis_version_is_rejected uses below for a different
        // field).
        let past_expiry = authority.created_at - 1.0;
        let key = control_key_for_conn(&conn).unwrap().unwrap();
        let payload = signing_payload(
            &authority.authority_id,
            &authority.intent_id,
            &authority.snapshot_id,
            authority.graph_generation,
            &authority.caller_set_digest,
            &authority.analysis_version,
            &authority.policy_digest,
            &authority.principal,
            &authority.target_scope_digest,
            &authority.nonce,
            past_expiry,
            &authority.policy_decision_digest,
            &authority.risk_vector_digest,
            authority.required_approver_class.as_str(),
        );
        let resigned = sign(&key, SIGNING_DOMAIN, &payload);
        conn.execute(
            "UPDATE review_authorities SET expires_at = ?1, signature = ?2 WHERE authority_id = ?3",
            params![past_expiry, resigned, authority.authority_id],
        )
        .unwrap();

        let err =
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current());
        assert_eq!(err, Err(AuthorityError::Expired));
    }

    #[test]
    fn wrong_intent_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.intent_id = "INT-2";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::WrongIntent)
        );
    }

    #[test]
    fn changed_target_source_snapshot_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.snapshot_id = "SNP-2";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StaleSnapshot)
        );
    }

    #[test]
    fn changed_caller_set_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.caller_set_digest = "callers-2";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StaleCallerSet)
        );
    }

    #[test]
    fn changed_graph_generation_is_rejected_as_stale_graph_authority() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.graph_generation = 6;
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StaleGraphGeneration)
        );
    }

    #[test]
    fn changed_policy_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.policy_digest = "policy-2";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StalePolicy)
        );
    }

    #[test]
    // WS1 (audit follow-up): mirrors changed_policy_is_rejected exactly,
    // for the two new mint<->spend risk/policy-decision digest checks --
    // the core mechanism that closes the "review doc_only, spend a real
    // body edit on the same symbol" bypass (see StalePolicyDecision below).
    fn changed_risk_vector_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        // Mint against the weakest possible vector (base_risk_vector) --
        // a spend that's genuinely RISKIER (is_hub=true, which the mint
        // never saw) must still be rejected: not just a different digest,
        // but a real, uncovered escalation the Wave 10 fallback correctly
        // refuses to paper over.
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        let mut riskier_vector = base_risk_vector().clone();
        riskier_vector.is_hub = true;
        current.risk_vector = &riskier_vector;
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StaleRiskVector)
        );
    }

    #[test]
    fn spend_less_risky_than_mint_is_covered_by_the_fallback_check() {
        // Wave 10 (item 2): before this fallback existed, ANY digest
        // mismatch failed outright -- even when the real drift only ever
        // made the spend safer than what was already reviewed. Mint
        // against a MORE risky vector than what's actually spent; the
        // fallback must accept it since it's providably no riskier.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let mut riskier_mint_vector = base_risk_vector().clone();
        riskier_mint_vector.touches_uncovered_code = true;
        let mut params = mint_params(&[]);
        params.risk_vector = &riskier_mint_vector;
        let authority = ReviewAuthority::mint(&conn, params).unwrap();

        // Spend with the weaker, base vector -- digest differs from what
        // was minted, but is provably no riskier.
        let current = base_current();
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Ok(())
        );
    }

    #[test]
    fn tampered_mint_columns_are_not_trusted_by_the_fallback_check() {
        // Wave 10 (item 2/3): the mint_* columns are NOT part of the
        // signed payload -- simulates an out-of-band tamper (or a
        // pre-migration row) that claims a MORE permissive mint than was
        // actually signed. The fallback must never trust this: it re-
        // derives the reconstructed vector's own digest and requires it
        // match the SIGNED risk_vector_digest before using it at all.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();

        conn.execute(
            "UPDATE review_authorities SET mint_is_hub = 1 WHERE authority_id = ?1",
            rusqlite::params![authority.authority_id],
        )
        .unwrap();

        let mut current = base_current();
        // Would be "covered" by the tampered claim (mint_is_hub = 1) if it
        // were trusted -- must NOT be, since the signed digest still says
        // the mint's real is_hub was false.
        let mut spend_vector = base_risk_vector().clone();
        spend_vector.is_hub = true;
        current.risk_vector = &spend_vector;

        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StaleRiskVector),
            "a mint_* column that doesn't match the signed risk_vector_digest must never be \
             trusted for the fallback check"
        );
    }

    #[test]
    fn changed_policy_decision_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.policy_decision_digest = "policy-decision-2";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::StalePolicyDecision)
        );
    }

    #[test]
    fn wrong_principal_class_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        let mut current = base_current();
        current.principal = "session:different";
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::WrongPrincipal)
        );
    }

    #[test]
    fn changed_analysis_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();
        // Simulate a binary upgrade between mint and verify: the stored
        // analysis_version no longer matches what current_analysis_version()
        // computes right now.
        conn.execute(
            "UPDATE review_authorities SET analysis_version = 'stale-version' WHERE authority_id = ?1",
            params![authority.authority_id],
        )
        .unwrap();
        // Also re-sign so this test isolates the analysis-version check
        // from the (already separately tested) signature/tamper check --
        // re-derive the exact same key this connection would use.
        let key = control_key_for_conn(&conn).unwrap().unwrap();
        let payload = signing_payload(
            &authority.authority_id,
            &authority.intent_id,
            &authority.snapshot_id,
            authority.graph_generation,
            &authority.caller_set_digest,
            "stale-version",
            &authority.policy_digest,
            &authority.principal,
            &authority.target_scope_digest,
            &authority.nonce,
            authority.expires_at,
            &authority.policy_decision_digest,
            &authority.risk_vector_digest,
            authority.required_approver_class.as_str(),
        );
        let resigned = sign(&key, SIGNING_DOMAIN, &payload);
        conn.execute(
            "UPDATE review_authorities SET signature = ?1 WHERE authority_id = ?2",
            params![resigned, authority.authority_id],
        )
        .unwrap();

        let err =
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &base_current());
        assert_eq!(err, Err(AuthorityError::StaleAnalysisVersion));
    }

    #[test]
    fn targets_are_persisted_alongside_the_authority() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let targets = vec![ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: None,
        }];
        let authority = ReviewAuthority::mint(&conn, mint_params(&targets)).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_authority_targets WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn deleting_the_authority_cascades_to_targets() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let targets = vec![ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: None,
        }];
        let authority = ReviewAuthority::mint(&conn, mint_params(&targets)).unwrap();

        conn.execute(
            "DELETE FROM review_authorities WHERE authority_id = ?1",
            params![authority.authority_id],
        )
        .unwrap();
        let targets_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_authority_targets WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(targets_left, 0);
    }

    #[test]
    fn duplicate_targets_within_one_authority_are_rejected() {
        // CCK-R6 (audit follow-up): UNIQUE(authority_id, path,
        // qualified_name) on review_authority_targets -- persist() inserts
        // one row per element of `targets` with no dedup, so a caller
        // passing the same (path, qualified_name) twice must now surface a
        // real constraint violation instead of silently storing a
        // duplicate row.
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let target = ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: Some("a.rs::f".to_string()),
        };
        let duplicated = vec![target.clone(), target];
        let err = ReviewAuthority::mint(&conn, mint_params(&duplicated)).unwrap_err();
        assert!(
            matches!(err, AuthorityError::Db(_)),
            "expected a DB constraint error, got {err:?}"
        );
    }

    #[test]
    fn current_analysis_version_is_deterministic() {
        assert_eq!(current_analysis_version(), current_analysis_version());
    }

    #[test]
    fn wrong_target_scope_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let minted_targets = vec![ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: Some("a.rs::f".to_string()),
        }];
        let authority = ReviewAuthority::mint(&conn, mint_params(&minted_targets)).unwrap();

        let different_targets = vec![ChangeIntentTarget {
            path: "b.rs".to_string(),
            qualified_name: Some("b.rs::g".to_string()),
        }];
        let mut current = base_current();
        current.targets = &different_targets;
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::WrongTargetScope)
        );
    }

    #[test]
    fn touching_an_extra_symbol_beyond_the_minted_scope_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let minted_targets = vec![ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: Some("a.rs::f".to_string()),
        }];
        let authority = ReviewAuthority::mint(&conn, mint_params(&minted_targets)).unwrap();

        // Same minted target PLUS one more the edit also happened to
        // overlap -- a superset must not be silently accepted either.
        let wider_targets = vec![
            minted_targets[0].clone(),
            ChangeIntentTarget {
                path: "a.rs".to_string(),
                qualified_name: Some("a.rs::g".to_string()),
            },
        ];
        let mut current = base_current();
        current.targets = &wider_targets;
        assert_eq!(
            ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current),
            Err(AuthorityError::WrongTargetScope)
        );
    }

    #[test]
    fn target_scope_digest_is_order_invariant() {
        let a = ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: Some("a.rs::f".to_string()),
        };
        let b = ChangeIntentTarget {
            path: "b.rs".to_string(),
            qualified_name: None,
        };
        assert_eq!(
            target_scope_digest(&[a.clone(), b.clone()]),
            target_scope_digest(&[b, a]),
        );
    }

    #[test]
    fn matching_target_scope_in_a_different_order_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let a = ChangeIntentTarget {
            path: "a.rs".to_string(),
            qualified_name: Some("a.rs::f".to_string()),
        };
        let b = ChangeIntentTarget {
            path: "b.rs".to_string(),
            qualified_name: None,
        };
        let minted_targets = vec![a.clone(), b.clone()];
        let authority = ReviewAuthority::mint(&conn, mint_params(&minted_targets)).unwrap();

        let reordered_targets = vec![b, a];
        let mut current = base_current();
        current.targets = &reordered_targets;
        ReviewAuthority::verify_and_consume(&conn, &authority.authority_id, &current).unwrap();
    }

    #[test]
    fn authority_ttl_rejects_non_positive_seconds() {
        assert_eq!(
            AuthorityTtl::from_secs(0.0),
            Err(AuthorityTtlError::NotPositive)
        );
        assert_eq!(
            AuthorityTtl::from_secs(-1.0),
            Err(AuthorityTtlError::NotPositive)
        );
    }

    #[test]
    fn authority_ttl_rejects_non_finite_seconds() {
        assert_eq!(
            AuthorityTtl::from_secs(f64::NAN),
            Err(AuthorityTtlError::NotFinite)
        );
        assert_eq!(
            AuthorityTtl::from_secs(f64::INFINITY),
            Err(AuthorityTtlError::NotFinite)
        );
        assert_eq!(
            AuthorityTtl::from_secs(f64::NEG_INFINITY),
            Err(AuthorityTtlError::NotFinite)
        );
    }

    #[test]
    fn authority_ttl_rejects_durations_beyond_the_max() {
        let too_long = AUTHORITY_TTL_MAX_SECS + 1.0;
        assert_eq!(
            AuthorityTtl::from_secs(too_long),
            Err(AuthorityTtlError::TooLong {
                secs: too_long,
                max: AUTHORITY_TTL_MAX_SECS,
            })
        );
        // The max itself is inclusive, not one past it.
        assert!(AuthorityTtl::from_secs(AUTHORITY_TTL_MAX_SECS).is_ok());
    }

    #[test]
    fn authority_ttl_accepts_and_round_trips_a_valid_duration() {
        let ttl = AuthorityTtl::from_secs(300.0).unwrap();
        assert_eq!(ttl.as_secs(), 300.0);
    }
}
