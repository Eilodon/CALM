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

/// Everything a caller must supply to [`ReviewAuthority::mint`]. Every
/// field here becomes a bound, signed value on the minted authority --
/// see the module doc comment for why there are exactly this many.
pub struct MintParams<'a> {
    pub intent_id: &'a str,
    pub snapshot_id: &'a str,
    pub graph_generation: i64,
    pub caller_set_digest: &'a str,
    pub policy_digest: &'a str,
    pub principal: &'a str,
    /// Seconds from now until the minted authority expires.
    pub ttl_secs: f64,
    pub targets: &'a [ChangeIntentTarget],
}

/// The current truth to check a stored authority against, at
/// [`ReviewAuthority::verify_and_consume`] time. Deliberately a separate
/// type from [`MintParams`] (not reused) even though the field sets
/// overlap almost entirely -- mint time and verify time compute these
/// values independently (often minutes apart, from different tool calls),
/// and giving them different types is a small, free reminder of that
/// rather than inviting a caller to accidentally reuse a stale `MintParams`
/// as if it were still current.
pub struct CurrentState<'a> {
    pub intent_id: &'a str,
    pub snapshot_id: &'a str,
    pub graph_generation: i64,
    pub caller_set_digest: &'a str,
    pub policy_digest: &'a str,
    pub principal: &'a str,
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
    StaleSnapshot,
    StaleGraphGeneration,
    StaleCallerSet,
    StaleAnalysisVersion,
    StalePolicy,
    WrongPrincipal,
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
    pub nonce: String,
    pub expires_at: f64,
    pub signature: String,
    pub created_at: f64,
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
fn signing_payload(
    authority_id: &str,
    intent_id: &str,
    snapshot_id: &str,
    graph_generation: i64,
    caller_set_digest: &str,
    analysis_version: &str,
    policy_digest: &str,
    principal: &str,
    nonce: &str,
    expires_at: f64,
) -> String {
    format!(
        "authority_id={authority_id}\nintent_id={intent_id}\nsnapshot_id={snapshot_id}\n\
         graph_generation={graph_generation}\ncaller_set_digest={caller_set_digest}\n\
         analysis_version={analysis_version}\npolicy_digest={policy_digest}\n\
         principal={principal}\nnonce={nonce}\nexpires_at={expires_at}\n"
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
        let expires_at = created_at + params.ttl_secs;
        let analysis_version = current_analysis_version();

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
                &nonce,
                expires_at,
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
            nonce,
            expires_at,
            signature,
            created_at,
        };

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
              analysis_version, policy_digest, principal, nonce, expires_at, signature, \
              created_at, consumed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL)",
            params![
                self.authority_id,
                self.intent_id,
                self.snapshot_id,
                self.graph_generation,
                self.caller_set_digest,
                self.analysis_version,
                self.policy_digest,
                self.principal,
                self.nonce,
                self.expires_at,
                self.signature,
                self.created_at,
            ],
        )?;
        for target in targets {
            state_conn.execute(
                "INSERT INTO review_authority_targets (authority_id, path, qualified_name) VALUES (?1, ?2, ?3)",
                params![self.authority_id, target.path, target.qualified_name],
            )?;
        }
        for (field_name, field_value) in [
            ("intent_id", self.intent_id.as_str()),
            ("snapshot_id", self.snapshot_id.as_str()),
            ("graph_generation", &self.graph_generation.to_string()),
            ("caller_set_digest", self.caller_set_digest.as_str()),
            ("analysis_version", self.analysis_version.as_str()),
            ("policy_digest", self.policy_digest.as_str()),
            ("principal", self.principal.as_str()),
        ] {
            state_conn.execute(
                "INSERT INTO review_authority_evidence (authority_id, field_name, field_value) VALUES (?1, ?2, ?3)",
                params![self.authority_id, field_name, field_value],
            )?;
        }
        Ok(())
    }

    fn load(state_conn: &Connection, authority_id: &str) -> Result<Option<Self>, AuthorityError> {
        let row: Option<(String, String, String, i64, String, String, String, String, String, f64, String, f64)> =
            state_conn
                .query_row(
                    "SELECT authority_id, intent_id, snapshot_id, graph_generation, caller_set_digest, \
                     analysis_version, policy_digest, principal, nonce, expires_at, signature, created_at \
                     FROM review_authorities WHERE authority_id = ?1",
                    params![authority_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                            r.get(9)?,
                            r.get(10)?,
                            r.get(11)?,
                        ))
                    },
                )
                .optional()?;
        Ok(row.map(
            |(
                authority_id,
                intent_id,
                snapshot_id,
                graph_generation,
                caller_set_digest,
                analysis_version,
                policy_digest,
                principal,
                nonce,
                expires_at,
                signature,
                created_at,
            )| Self {
                authority_id,
                intent_id,
                snapshot_id,
                graph_generation,
                caller_set_digest,
                analysis_version,
                policy_digest,
                principal,
                nonce,
                expires_at,
                signature,
                created_at,
            },
        ))
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
            &authority.nonce,
            authority.expires_at,
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

        let consumed_rows = state_conn.execute(
            "UPDATE review_authorities SET consumed_at = ?1 WHERE authority_id = ?2 AND consumed_at IS NULL",
            params![now_epoch_secs(), authority_id],
        )?;
        if consumed_rows == 0 {
            return Err(AuthorityError::AlreadyConsumed);
        }
        Ok(())
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

    fn base_current() -> CurrentState<'static> {
        CurrentState {
            intent_id: "INT-1",
            snapshot_id: "SNP-1",
            graph_generation: 5,
            caller_set_digest: "callers-1",
            policy_digest: "policy-1",
            principal: "session:abc",
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
            ttl_secs: 300.0,
            targets,
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
        let mut params = mint_params(&[]);
        params.ttl_secs = -1.0; // already expired the instant it's minted
        let authority = ReviewAuthority::mint(&conn, params).unwrap();
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
            &authority.nonce,
            authority.expires_at,
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
    fn evidence_rows_are_persisted_for_every_bound_field() {
        let dir = tempfile::tempdir().unwrap();
        let conn = real_state_conn(dir.path());
        seed_intent_and_snapshot(&conn);
        let authority = ReviewAuthority::mint(&conn, mint_params(&[])).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_authority_evidence WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 7, "one evidence row per bound field");
    }

    #[test]
    fn deleting_the_authority_cascades_to_targets_and_evidence() {
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
        let evidence_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM review_authority_evidence WHERE authority_id = ?1",
                params![authority.authority_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(targets_left, 0);
        assert_eq!(evidence_left, 0);
    }

    #[test]
    fn current_analysis_version_is_deterministic() {
        assert_eq!(current_analysis_version(), current_analysis_version());
    }
}
