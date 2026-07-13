use std::path::Path;

use hmac::{Hmac, Mac};
use rand::TryRngCore;
use rusqlite::Connection;
use serde::Serialize;
use sha2::Sha256;

use crate::analysis::config_drift::{build_real_path_index, resolve_reference};
use crate::analysis::doc_refs::extract_path_refs;
use crate::indexer::pipeline::hash_content;

type HmacSha256 = Hmac<Sha256>;

/// Plan 3 §3.5(d): defends `project_memory` against a note injected
/// out-of-band (e.g. a direct `INSERT`/`UPDATE` against the SQLite file,
/// bypassing `remember`'s MCP surface entirely) — `hash_content`
/// (indexer/pipeline.rs) is deliberately unkeyed FNV-1a and gives no
/// protection here: anyone who can write a row can trivially recompute a
/// matching unkeyed hash. A keyed MAC only verifies if the writer also had
/// the key file, which lives outside the SQLite file the attacker is
/// presumed to have write access to. 32 random bytes — the standard HMAC
/// key size for a 256-bit-output hash (matches SHA-256's block/output
/// size, no unnecessary size vs. HMAC-SHA256 security margin lost or
/// gained by picking a different length here).
const MAC_KEY_LEN: usize = 32;
const MAC_KEY_FILENAME: &str = "memory.key";

/// Reads the per-project MAC key from `.calm/memory.key`, generating one
/// with the OS CSPRNG on first use (lazy — most projects with this feature
/// available will never call `remember` in a given checkout, so eagerly
/// creating the key at `init_db` time would be pure waste). `0600` on
/// Unix so only the owning user can read it — a key any local user can
/// read defeats the point of a keyed MAC. Best-effort on non-Unix: the
/// permission narrowing is skipped rather than failing the whole
/// operation, matching this workspace's existing posture of degrading
/// gracefully rather than hard-failing on a platform-specific step
/// (see `build.rs`'s doc comment on never failing the build).
pub fn load_or_create_mac_key(project_root: &Path) -> std::io::Result<[u8; MAC_KEY_LEN]> {
    let calm_dir = project_root.join(".calm");
    let key_path = calm_dir.join(MAC_KEY_FILENAME);

    if let Ok(bytes) = std::fs::read(&key_path)
        && bytes.len() == MAC_KEY_LEN
    {
        let mut key = [0u8; MAC_KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }
    // Wrong length (truncated write, foreign file, ...) or unreadable — fall
    // through and regenerate rather than trust a key that isn't what this
    // function itself would have written.

    std::fs::create_dir_all(&calm_dir)?;
    let mut key = [0u8; MAC_KEY_LEN];
    // `try_fill_bytes`, not the panicking infallible adaptor: the OS
    // CSPRNG source can fail (rare, but real — e.g. a sandboxed/restricted
    // environment with no entropy source available), and this function
    // already returns `io::Result`, so there's a natural way to propagate
    // that instead of taking the whole server down over it.
    rand::rngs::OsRng
        .try_fill_bytes(&mut key)
        .map_err(std::io::Error::other)?;

    // Write-then-restrict, not create-with-mode: `std::fs::Permissions`
    // needs a real file to set on, and a narrower create mode isn't
    // portably expressible via `std::fs::write` alone without pulling in
    // a Unix-specific `OpenOptions::mode()` builder for what's otherwise
    // one line.
    std::fs::write(&key_path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// `topic` and `content` are joined with a NUL separator (never valid in
/// either field in practice, and even if it were, this only needs to be
/// *a* fixed unambiguous encoding, not a general-purpose one) — without a
/// separator, `("ab", "c")` and `("a", "bc")` would hash identically,
/// letting a forged row with a shuffled topic/content boundary still pass
/// verification.
fn mac_input(topic: &str, content: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(topic.len() + content.len() + 1);
    buf.extend_from_slice(topic.as_bytes());
    buf.push(0);
    buf.extend_from_slice(content.as_bytes());
    buf
}

/// Hex-encoded HMAC-SHA256(key, topic ‖ 0x00 ‖ content) — hex rather than
/// raw bytes so it round-trips through the existing `TEXT` column
/// (`content_mac`) and JSON output without a separate encoding step.
pub fn compute_mac(key: &[u8; MAC_KEY_LEN], topic: &str, content: &str) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&mac_input(topic, content));
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// `"ok"` (MAC present and verifies), `"mismatch"` (MAC present but does
/// NOT verify — the row was altered by something other than `remember`),
/// or `"unverified"` (no MAC stored at all — a note written before this
/// feature existed; `content_mac` is a nullable migration, not a
/// backfill, so pre-existing notes have no baseline to check against and
/// must NOT be reported as either "ok" or "mismatch").
pub fn verify_integrity(
    key: &[u8; MAC_KEY_LEN],
    topic: &str,
    content: &str,
    stored_mac: Option<&str>,
) -> &'static str {
    match stored_mac {
        None => "unverified",
        Some(stored) => {
            let expected = compute_mac(key, topic, content);
            // Constant-time-ish via length-then-full-compare is unnecessary
            // here: an attacker who can already read `content_mac` from the
            // SQLite file to time-compare against has local file access,
            // at which point they can also just read the plaintext `content`
            // column directly — timing side-channels on this comparison
            // leak nothing an on-disk read doesn't already.
            if stored == expected { "ok" } else { "mismatch" }
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StaleRef {
    pub reference: String,
    /// "changed" (file still exists but its content hash no longer matches
    /// what was captured at `remember` time) or "deleted" (file no longer
    /// exists at all).
    pub status: &'static str,
}

/// Extracts file-path-like references from a `remember`d note's `content`
/// and resolves each against the real project tree, returning
/// `(repo_relative_path, content_hash)` pairs for the ones that exist right
/// now — a "true when written" snapshot for `check_staleness` to later diff
/// against. References that don't resolve to anything are silently skipped:
/// there's nothing to snapshot, and `analysis::config_drift` is the tool for
/// flagging a broken reference itself, not this one.
pub fn capture_refs(
    project_root: &Path,
    ignore_patterns: &[String],
    content: &str,
) -> Vec<(String, String)> {
    let mut refs = extract_path_refs(content);
    refs.sort();
    refs.dedup();
    if refs.is_empty() {
        return Vec::new();
    }

    let real_paths = build_real_path_index(project_root, ignore_patterns);
    let mut out = Vec::new();
    for r in refs {
        let Some(resolved) = resolve_reference(project_root, &real_paths, &r) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(project_root.join(&resolved)) else {
            continue;
        };
        out.push((resolved, hash_content(&text)));
    }
    out
}

/// Replaces the full ref set for `topic` — same "upsert the whole thing"
/// contract as `remember` itself replacing `content` wholesale rather than
/// appending.
pub fn store_refs(
    conn: &Connection,
    topic: &str,
    refs: &[(String, String)],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM project_memory_refs WHERE topic = ?1",
        rusqlite::params![topic],
    )?;
    for (path, hash) in refs {
        conn.execute(
            "INSERT INTO project_memory_refs (topic, ref_path, ref_hash) VALUES (?1, ?2, ?3)",
            rusqlite::params![topic, path, hash],
        )?;
    }
    Ok(())
}

/// Re-checks every ref captured for `topic` against the live file: a
/// content-hash mismatch is `"changed"`, a file that no longer exists at all
/// is `"deleted"`. Empty result means either no refs were ever captured for
/// this topic, or all captured refs still match — callers that need to tell
/// those two cases apart should check whether any refs were stored at all
/// first (e.g. via a row count).
pub fn check_staleness(
    conn: &Connection,
    project_root: &Path,
    topic: &str,
) -> rusqlite::Result<Vec<StaleRef>> {
    let mut stmt =
        conn.prepare("SELECT ref_path, ref_hash FROM project_memory_refs WHERE topic = ?1")?;
    let rows: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![topic], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let mut stale = Vec::new();
    for (path, old_hash) in rows {
        match std::fs::read_to_string(project_root.join(&path)) {
            Ok(text) => {
                if hash_content(&text) != old_hash {
                    stale.push(StaleRef {
                        reference: path,
                        status: "changed",
                    });
                }
            }
            Err(_) => stale.push(StaleRef {
                reference: path,
                status: "deleted",
            }),
        }
    }
    stale.sort_by(|a, b| a.reference.cmp(&b.reference));
    Ok(stale)
}

/// Count of refs captured for `topic` — lets a caller distinguish "nothing
/// to check" (0) from "checked, all still fresh" (>0 captured, empty
/// `check_staleness` result).
pub fn ref_count(conn: &Connection, topic: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM project_memory_refs WHERE topic = ?1",
        rusqlite::params![topic],
        |r| r.get(0),
    )
}

/// Notes whose captured file references include `path` — the ambient-
/// injection candidate set for `edit_context`/`locate`'s `related_notes`
/// (docs/superskills/specs/2026-07-11-superskills-inspired-features.md #3
/// v2). Ordered most-recently-updated first. Deliberately does NOT apply
/// specificity-gating (hub files needing a symbol-name match), content-
/// safety filtering (`injection_warning`), or MAC verification (Plan 3
/// §3.5(d)) — those are policy decisions the caller makes with data
/// (`is_hub`, the resolved symbol name, the project's MAC key) this module
/// doesn't have; this just answers "which notes mention this file at all,"
/// returning `content_mac` alongside so the caller CAN verify without a
/// second round trip.
pub fn notes_for_path(
    conn: &Connection,
    path: &str,
    limit: usize,
) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT p.topic, p.content, p.content_mac FROM project_memory_refs r \
         JOIN project_memory p ON p.topic = r.topic \
         WHERE r.ref_path = ?1 \
         ORDER BY p.updated_at DESC LIMIT ?2",
    )?;
    stmt.query_map(rusqlite::params![path, limit as i64], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    fn temp_project(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ci_memory_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn capture_refs_skips_content_with_no_path_like_tokens() {
        let dir = temp_project("no_refs");
        let refs = capture_refs(&dir, &[], "just a plain rationale note, no file mentioned");
        assert!(refs.is_empty());
    }

    #[test]
    fn capture_refs_skips_unresolved_references() {
        let dir = temp_project("unresolved");
        let refs = capture_refs(&dir, &[], "see server.py for the old entry point");
        assert!(refs.is_empty(), "got {refs:?}");
    }

    #[test]
    fn capture_refs_snapshots_resolved_file_hash() {
        let dir = temp_project("snapshot");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "the gate lives in `fitness.rs`");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "fitness.rs");
        assert_eq!(refs[0].1, hash_content("// v1\n"));
    }

    #[test]
    fn check_staleness_empty_when_nothing_captured() {
        let conn = test_conn();
        let dir = temp_project("nothing_captured");
        let stale = check_staleness(&conn, &dir, "no-such-topic").unwrap();
        assert!(stale.is_empty());
        assert_eq!(ref_count(&conn, "no-such-topic").unwrap(), 0);
    }

    #[test]
    fn check_staleness_detects_changed_file() {
        let conn = test_conn();
        let dir = temp_project("changed");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        // File changes after the note was written.
        std::fs::write(dir.join("fitness.rs"), "// v2, totally different\n").unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reference, "fitness.rs");
        assert_eq!(stale[0].status, "changed");
    }

    #[test]
    fn check_staleness_detects_deleted_file() {
        let conn = test_conn();
        let dir = temp_project("deleted");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        std::fs::remove_file(dir.join("fitness.rs")).unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].reference, "fitness.rs");
        assert_eq!(stale[0].status, "deleted");
    }

    #[test]
    fn check_staleness_empty_when_file_unchanged() {
        let conn = test_conn();
        let dir = temp_project("unchanged");
        std::fs::write(dir.join("fitness.rs"), "// v1\n").unwrap();
        let refs = capture_refs(&dir, &[], "see `fitness.rs`");
        store_refs(&conn, "gate-rationale", &refs).unwrap();

        let stale = check_staleness(&conn, &dir, "gate-rationale").unwrap();
        assert!(stale.is_empty());
        assert_eq!(ref_count(&conn, "gate-rationale").unwrap(), 1);
    }

    #[test]
    fn store_refs_replaces_previous_set_for_same_topic() {
        let conn = test_conn();
        store_refs(&conn, "t", &[("a.rs".to_string(), "hash_a".to_string())]).unwrap();
        store_refs(&conn, "t", &[("b.rs".to_string(), "hash_b".to_string())]).unwrap();

        assert_eq!(ref_count(&conn, "t").unwrap(), 1);
        let dir = temp_project("replace");
        std::fs::write(dir.join("b.rs"), "hash_b").unwrap(); // arbitrary content, hash won't match "hash_b" literal
        // Only confirm the old "a.rs" ref is gone, not resurrected as stale.
        let stale = check_staleness(&conn, &dir, "t").unwrap();
        assert!(stale.iter().all(|s| s.reference != "a.rs"));
    }

    // Plan 3 §3.5(d) — HMAC integrity for project_memory.

    #[test]
    fn load_or_create_mac_key_persists_across_calls() {
        let dir = temp_project("mac_key_persist");
        let k1 = load_or_create_mac_key(&dir).unwrap();
        let k2 = load_or_create_mac_key(&dir).unwrap();
        assert_eq!(
            k1, k2,
            "second call must read back the same key, not regenerate"
        );
        assert!(dir.join(".calm").join("memory.key").is_file());
    }

    #[test]
    fn load_or_create_mac_key_different_projects_get_different_keys() {
        let d1 = temp_project("mac_key_a");
        let d2 = temp_project("mac_key_b");
        assert_ne!(
            load_or_create_mac_key(&d1).unwrap(),
            load_or_create_mac_key(&d2).unwrap(),
            "each project must get its own independently-random key"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_mac_key_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_project("mac_key_perms");
        load_or_create_mac_key(&dir).unwrap();
        let mode = std::fs::metadata(dir.join(".calm").join("memory.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "key file must not be group/other readable"
        );
    }

    #[test]
    fn compute_mac_is_deterministic_and_key_and_content_and_topic_sensitive() {
        let key_a = [1u8; MAC_KEY_LEN];
        let key_b = [2u8; MAC_KEY_LEN];
        let m1 = compute_mac(&key_a, "topic", "content");
        let m2 = compute_mac(&key_a, "topic", "content");
        assert_eq!(m1, m2, "same inputs must produce the same MAC");
        assert_ne!(
            m1,
            compute_mac(&key_b, "topic", "content"),
            "different key must change the MAC"
        );
        assert_ne!(
            m1,
            compute_mac(&key_a, "topic", "different"),
            "different content must change the MAC"
        );
        assert_ne!(
            m1,
            compute_mac(&key_a, "different", "content"),
            "different topic must change the MAC"
        );
    }

    #[test]
    fn compute_mac_does_not_confuse_topic_content_boundary() {
        // Without the NUL separator in `mac_input`, ("ab", "c") and ("a",
        // "bc") would hash identically — the exact ambiguity a forged row
        // could exploit to reuse a MAC computed for a different (topic,
        // content) split.
        let key = [7u8; MAC_KEY_LEN];
        assert_ne!(compute_mac(&key, "ab", "c"), compute_mac(&key, "a", "bc"),);
    }

    #[test]
    fn verify_integrity_reports_ok_mismatch_and_unverified() {
        let key = [9u8; MAC_KEY_LEN];
        let mac = compute_mac(&key, "t", "content");

        assert_eq!(verify_integrity(&key, "t", "content", Some(&mac)), "ok");
        assert_eq!(
            verify_integrity(&key, "t", "content", Some("0000not-a-real-mac")),
            "mismatch"
        );
        assert_eq!(
            verify_integrity(&key, "t", "tampered-content", Some(&mac)),
            "mismatch",
            "MAC computed for different content must not verify"
        );
        assert_eq!(
            verify_integrity(&key, "t", "content", None),
            "unverified",
            "no stored MAC (pre-feature note) must be unverified, not ok or mismatch"
        );
    }
}
