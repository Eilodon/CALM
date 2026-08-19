//! `PendingReview` -- audit 2026-08-10 follow-up ("calm review"). A durable,
//! MCP-protocol-independent second channel for the independent review
//! `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW` requires (CCK-23,
//! `calm-server/src/tools/edit.rs`).
//!
//! Verified empirically the same day: a connected MCP client can complete
//! CALM's own elicitation round-trip (`[edit] elicit_hub_confirm`) --  no
//! timeout, no capability error -- while never actually showing the
//! question to a human at all. CALM had no way to detect that, and (until
//! this module) no alternative review channel whatsoever: any client in
//! that situation was a permanent, silent dead end for every `risk=="high"`
//! edit, with no way to legitimately unblock it.
//!
//! A `PendingReview` row is never itself authority to write anything --
//! `edit_lines_impl_gated` still requires BOTH `status == "approved"` AND a
//! freshly recomputed content fingerprint that matches `fingerprint` here
//! (see `fingerprint_edit_lines`/`fingerprint_edit_symbol`, CCK-29c) before
//! treating a retry as reviewed -- the same trust tier `ElicitGate::Approved`
//! already requires, just reached through a different channel. `calm-cli
//! review approve` (the only writer of `status = "approved"`) requires a
//! real TTY (`std::io::IsTerminal`) specifically so an agent with ordinary
//! non-interactive shell access can't just script past this the way it
//! could a config toggle -- see that command's own module doc comment for
//! the full rationale and its honestly-stated limits.

use rusqlite::{Connection, OptionalExtension, params};

fn now_epoch_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn new_review_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("REVIEW-{nanos:016x}-{counter:08x}-{}", std::process::id())
}

/// Default lifetime for a pending review: long enough for a human to
/// actually notice and act on it out-of-band (this is an asynchronous,
/// walk-up-to-a-terminal-later flow, unlike elicitation's short live-wait
/// timeout) but not indefinite -- an ancient, forgotten pending review
/// approving stale content would be a stale-authorization footgun.
pub const PENDING_REVIEW_DEFAULT_TTL_SECS: f64 = 24.0 * 60.0 * 60.0;

#[derive(Debug, Clone, PartialEq)]
pub struct PendingReview {
    pub review_id: String,
    pub tool: String,
    pub path: String,
    pub fingerprint: String,
    pub diff_preview: String,
    pub risk: Option<String>,
    pub hub_kind: Option<String>,
    pub reason: Option<String>,
    pub status: String,
    pub created_at: f64,
    pub expires_at: f64,
    pub decided_at: Option<f64>,
    pub decided_by: Option<String>,
}

/// Everything a caller must supply to open one pending review.
pub struct NewPendingReview<'a> {
    pub tool: &'a str,
    pub path: &'a str,
    pub fingerprint: &'a str,
    pub diff_preview: &'a str,
    pub risk: Option<&'a str>,
    pub hub_kind: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub ttl_secs: f64,
}

#[allow(clippy::type_complexity)]
fn row_to_pending_review(row: &rusqlite::Row) -> rusqlite::Result<PendingReview> {
    Ok(PendingReview {
        review_id: row.get(0)?,
        tool: row.get(1)?,
        path: row.get(2)?,
        fingerprint: row.get(3)?,
        diff_preview: row.get(4)?,
        risk: row.get(5)?,
        hub_kind: row.get(6)?,
        reason: row.get(7)?,
        status: row.get(8)?,
        created_at: row.get(9)?,
        expires_at: row.get(10)?,
        decided_at: row.get(11)?,
        decided_by: row.get(12)?,
    })
}

const SELECT_COLUMNS: &str = "review_id, tool, path, fingerprint, diff_preview, risk, hub_kind, \
     reason, status, created_at, expires_at, decided_at, decided_by";

/// Opens one pending review in `status = "pending"`. Best-effort by design
/// (mirrors `insert_approval_receipt`'s posture): a caller here is already
/// mid-refusal of the underlying edit (this only records that a human CAN
/// review it out-of-band) -- a write failure here must not turn an
/// already-correct refusal into a harder failure.
pub fn insert_pending_review(
    conn: &Connection,
    new: &NewPendingReview,
) -> rusqlite::Result<String> {
    let review_id = new_review_id();
    let created_at = now_epoch_secs();
    let expires_at = created_at + new.ttl_secs.max(0.0);
    conn.execute(
        "INSERT INTO pending_reviews \
         (review_id, tool, path, fingerprint, diff_preview, risk, hub_kind, reason, status, \
          created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10)",
        params![
            review_id,
            new.tool,
            new.path,
            new.fingerprint,
            new.diff_preview,
            new.risk,
            new.hub_kind,
            new.reason,
            created_at,
            expires_at,
        ],
    )?;
    Ok(review_id)
}

pub fn get_pending_review(
    conn: &Connection,
    review_id: &str,
) -> rusqlite::Result<Option<PendingReview>> {
    conn.query_row(
        &format!("SELECT {SELECT_COLUMNS} FROM pending_reviews WHERE review_id = ?1"),
        params![review_id],
        row_to_pending_review,
    )
    .optional()
}

/// `status_filter` narrows to one status (typically `"pending"`); `None`
/// lists every row regardless of status. Newest first.
pub fn list_pending_reviews(
    conn: &Connection,
    status_filter: Option<&str>,
) -> rusqlite::Result<Vec<PendingReview>> {
    let sql = match status_filter {
        Some(_) => format!(
            "SELECT {SELECT_COLUMNS} FROM pending_reviews WHERE status = ?1 \
             ORDER BY created_at DESC"
        ),
        None => format!("SELECT {SELECT_COLUMNS} FROM pending_reviews ORDER BY created_at DESC"),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = match status_filter {
        Some(s) => stmt.query_map(params![s], row_to_pending_review)?,
        None => stmt.query_map([], row_to_pending_review)?,
    };
    rows.collect()
}

/// Marks a `status = "pending"` row `"approved"` or `"declined"`, recording
/// who/when. Returns `Ok(false)` (not an error) if `review_id` doesn't
/// exist or isn't `"pending"` anymore (already decided, or expired) --
/// callers (the CLI) turn that into a clear message rather than a generic
/// DB error.
fn decide_pending_review(
    conn: &Connection,
    review_id: &str,
    new_status: &str,
    decided_by: &str,
) -> rusqlite::Result<bool> {
    let decided_at = now_epoch_secs();
    let updated = conn.execute(
        "UPDATE pending_reviews SET status = ?1, decided_at = ?2, decided_by = ?3 \
         WHERE review_id = ?4 AND status = 'pending' AND expires_at > ?2",
        params![new_status, decided_at, decided_by, review_id],
    )?;
    Ok(updated > 0)
}

pub fn approve_pending_review(
    conn: &Connection,
    review_id: &str,
    decided_by: &str,
) -> rusqlite::Result<bool> {
    decide_pending_review(conn, review_id, "approved", decided_by)
}

pub fn decline_pending_review(
    conn: &Connection,
    review_id: &str,
    decided_by: &str,
) -> rusqlite::Result<bool> {
    decide_pending_review(conn, review_id, "declined", decided_by)
}

/// Outcome of `decide_via_agent_relay` -- one variant per case its two
/// front-ends (the MCP tool `review_decide_via_agent_relay` in calm-server,
/// and the CLI `calm review approve-via-agent-relay`/`decline-via-agent-relay`
/// in calm-cli) each already need to report back in their own idiom (a JSON
/// `ErrorDetail` for the former, an exit code/message for the latter).
#[derive(Debug, Clone, PartialEq)]
pub enum AgentRelayOutcome {
    /// `"approved"` or `"declined"`.
    Decided(&'static str),
    NotFound,
    /// Carries the review's actual current status (already decided, or
    /// -- same row, same message -- expired).
    AlreadyDecided(String),
    DigestMismatch,
    /// The review was decided or expired between the status check and the
    /// write -- caller should re-fetch and retry.
    Race,
}

/// Shared body of the "agent relay" decision channel: the deliberately
/// WEAKER, opt-in (`EditConfig::elicit_via_agent_relay`) sibling of the
/// TTY-gated `calm review approve`/`decline` (`decide_pending_review` above).
/// Both front-ends that expose this channel -- the MCP tool
/// `review_decide_via_agent_relay` and the CLI's `*-via-agent-relay`
/// subcommands -- call this exact function, so the one safety-relevant
/// check it performs (that `diff_digest` equals `hash_content` of the
/// review's own CURRENT `diff_preview`, proving the caller is referencing
/// the real, current diff and not a guess or stale copy) lives in exactly
/// one place rather than two copies that could drift. See
/// `EditConfig::elicit_via_agent_relay`'s doc comment for the full tradeoff
/// this channel accepts -- callers are responsible for the config-flag gate
/// and for not calling this before a human has actually seen the diff and
/// answered; this function itself cannot verify either.
pub fn decide_via_agent_relay(
    conn: &Connection,
    review_id: &str,
    diff_digest: &str,
    approve: bool,
) -> rusqlite::Result<AgentRelayOutcome> {
    let Some(review) = get_pending_review(conn, review_id)? else {
        return Ok(AgentRelayOutcome::NotFound);
    };
    if review.status != "pending" {
        return Ok(AgentRelayOutcome::AlreadyDecided(review.status));
    }
    let expected_digest = crate::indexer::pipeline::hash_content(&review.diff_preview);
    if diff_digest != expected_digest {
        return Ok(AgentRelayOutcome::DigestMismatch);
    }
    let decided_by = "agent_relay_after_elicitation";
    let ok = if approve {
        approve_pending_review(conn, review_id, decided_by)?
    } else {
        decline_pending_review(conn, review_id, decided_by)?
    };
    Ok(if ok {
        AgentRelayOutcome::Decided(if approve { "approved" } else { "declined" })
    } else {
        AgentRelayOutcome::Race
    })
}

/// The retry-time lookup `edit_lines_impl_gated` uses: an unexpired,
/// `status = "approved"` row for this exact `path` + content `fingerprint`.
/// Content-addressed by construction (same rationale as
/// `EvidenceSnapshot::compute_with_recorded_freshness`'s own doc comment):
/// if the proposed edit changed since the review was opened, `fingerprint`
/// changes and this simply misses -- there is no separate staleness check
/// to remember to run.
pub fn find_approved_matching(
    conn: &Connection,
    path: &str,
    fingerprint: &str,
) -> rusqlite::Result<Option<PendingReview>> {
    conn.query_row(
        &format!(
            "SELECT {SELECT_COLUMNS} FROM pending_reviews \
             WHERE path = ?1 AND fingerprint = ?2 AND status = 'approved' AND expires_at > ?3 \
             ORDER BY decided_at DESC LIMIT 1"
        ),
        params![path, fingerprint, now_epoch_secs()],
        row_to_pending_review,
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_state_db;
    use crate::db::state_migrations::migrate_state_db_to_current;

    fn state_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_state_db(&conn).unwrap();
        migrate_state_db_to_current(&conn).unwrap();
        conn
    }

    fn new_review(tool: &str, path: &str, fingerprint: &str) -> NewPendingReview<'static> {
        // 'static via leak is fine in tests only -- keeps call sites terse.
        NewPendingReview {
            tool: Box::leak(tool.to_string().into_boxed_str()),
            path: Box::leak(path.to_string().into_boxed_str()),
            fingerprint: Box::leak(fingerprint.to_string().into_boxed_str()),
            diff_preview: "@@ lines 2-2 @@\n- old\n+ new\n",
            risk: Some("high"),
            hub_kind: None,
            reason: Some("test reason"),
            ttl_secs: PENDING_REVIEW_DEFAULT_TTL_SECS,
        }
    }

    #[test]
    fn insert_then_get_round_trips_every_field() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        let got = get_pending_review(&conn, &id).unwrap().unwrap();
        assert_eq!(got.review_id, id);
        assert_eq!(got.tool, "edit_lines");
        assert_eq!(got.path, "a.py");
        assert_eq!(got.fingerprint, "sha256:abc");
        assert_eq!(got.status, "pending");
        assert_eq!(got.risk.as_deref(), Some("high"));
        assert!(got.decided_at.is_none());
        assert!(got.decided_by.is_none());
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let conn = state_conn();
        assert_eq!(get_pending_review(&conn, "REVIEW-nope").unwrap(), None);
    }

    #[test]
    fn approve_flips_status_and_is_found_by_find_approved_matching() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        assert!(approve_pending_review(&conn, &id, "cli_manual_review").unwrap());
        let got = get_pending_review(&conn, &id).unwrap().unwrap();
        assert_eq!(got.status, "approved");
        assert_eq!(got.decided_by.as_deref(), Some("cli_manual_review"));
        assert!(got.decided_at.is_some());

        let found = find_approved_matching(&conn, "a.py", "sha256:abc")
            .unwrap()
            .unwrap();
        assert_eq!(found.review_id, id);
    }

    #[test]
    fn find_approved_matching_misses_on_a_different_fingerprint() {
        // Content-addressed: the proposal changed since review was opened.
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        approve_pending_review(&conn, &id, "cli_manual_review").unwrap();
        assert_eq!(
            find_approved_matching(&conn, "a.py", "sha256:different").unwrap(),
            None
        );
    }

    #[test]
    fn decline_prevents_a_later_approve() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        assert!(decline_pending_review(&conn, &id, "cli_manual_review").unwrap());
        // Already decided -- a second decision attempt must not silently
        // flip it (WHERE status = 'pending' in decide_pending_review).
        assert!(!approve_pending_review(&conn, &id, "cli_manual_review").unwrap());
        let got = get_pending_review(&conn, &id).unwrap().unwrap();
        assert_eq!(got.status, "declined");
    }

    #[test]
    fn decide_returns_false_for_an_unknown_review_id() {
        let conn = state_conn();
        assert!(!approve_pending_review(&conn, "REVIEW-nope", "cli_manual_review").unwrap());
    }

    #[test]
    fn expired_approval_is_not_found_by_find_approved_matching() {
        let conn = state_conn();
        let mut review = new_review("edit_lines", "a.py", "sha256:abc");
        review.ttl_secs = -1.0; // already expired the instant it's created
        let id = insert_pending_review(&conn, &review).unwrap();
        // decide_pending_review's own WHERE clause also checks
        // `expires_at > decided_at`, so an already-expired row can't even
        // be approved after the fact.
        assert!(!approve_pending_review(&conn, &id, "cli_manual_review").unwrap());
    }

    #[test]
    fn list_pending_reviews_filters_by_status_and_orders_newest_first() {
        let conn = state_conn();
        let first =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:1")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second =
            insert_pending_review(&conn, &new_review("edit_symbol", "b.py", "sha256:2")).unwrap();
        approve_pending_review(&conn, &first, "cli_manual_review").unwrap();

        let pending = list_pending_reviews(&conn, Some("pending")).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].review_id, second);

        let all = list_pending_reviews(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].review_id, second, "newest first");
    }

    #[test]
    fn agent_relay_approves_on_matching_digest() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        let review = get_pending_review(&conn, &id).unwrap().unwrap();
        let digest = crate::indexer::pipeline::hash_content(&review.diff_preview);
        let outcome = decide_via_agent_relay(&conn, &id, &digest, true).unwrap();
        assert_eq!(outcome, AgentRelayOutcome::Decided("approved"));
        let got = get_pending_review(&conn, &id).unwrap().unwrap();
        assert_eq!(got.status, "approved");
        assert_eq!(got.decided_by.as_deref(), Some("agent_relay_after_elicitation"));
    }

    #[test]
    fn agent_relay_declines_on_matching_digest() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        let review = get_pending_review(&conn, &id).unwrap().unwrap();
        let digest = crate::indexer::pipeline::hash_content(&review.diff_preview);
        let outcome = decide_via_agent_relay(&conn, &id, &digest, false).unwrap();
        assert_eq!(outcome, AgentRelayOutcome::Decided("declined"));
    }

    #[test]
    fn agent_relay_refuses_a_stale_or_guessed_digest() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        let outcome = decide_via_agent_relay(&conn, &id, "not-the-real-digest", true).unwrap();
        assert_eq!(outcome, AgentRelayOutcome::DigestMismatch);
        // Refused -- must not have flipped status.
        let got = get_pending_review(&conn, &id).unwrap().unwrap();
        assert_eq!(got.status, "pending");
    }

    #[test]
    fn agent_relay_reports_not_found_for_unknown_id() {
        let conn = state_conn();
        let outcome = decide_via_agent_relay(&conn, "REVIEW-nope", "whatever", true).unwrap();
        assert_eq!(outcome, AgentRelayOutcome::NotFound);
    }

    #[test]
    fn agent_relay_reports_already_decided() {
        let conn = state_conn();
        let id =
            insert_pending_review(&conn, &new_review("edit_lines", "a.py", "sha256:abc")).unwrap();
        let review = get_pending_review(&conn, &id).unwrap().unwrap();
        let digest = crate::indexer::pipeline::hash_content(&review.diff_preview);
        decline_pending_review(&conn, &id, "cli_manual_review").unwrap();
        let outcome = decide_via_agent_relay(&conn, &id, &digest, true).unwrap();
        assert_eq!(outcome, AgentRelayOutcome::AlreadyDecided("declined".to_string()));
    }
}
