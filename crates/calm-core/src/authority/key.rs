//! `.calm/control.key` -- CCK-09
//! (docs/plans/2026-08-08-master-change-control-execution-blueprint.md).
//! Dedicated signing key for [`super::review::ReviewAuthority`], kept in
//! its own file rather than sharing `memory.key` or `audit.key` (§2's
//! kept design decision: "the ledger and project-memory notes protect
//! different data with different blast radii if a key leaks, and there's
//! no reason to couple their compromise" -- `ledger.rs`'s own words for
//! `audit.key` vs `memory.key`, equally true of a THIRD key protecting a
//! third kind of record). Mirrors `ledger::load_or_create_ledger_key`'s
//! write-then-restrict-to-0600 pattern exactly; duplicated rather than
//! shared for the same reason that function gives for not sharing
//! `memory.rs`'s own loader.

use hmac::{Hmac, Mac};
use rand::TryRngCore;
use rusqlite::Connection;
use std::path::Path;

type HmacSha256 = Hmac<sha2::Sha256>;

const CONTROL_KEY_LEN: usize = 32;
const CONTROL_KEY_FILENAME: &str = "control.key";

fn load_or_create_control_key(calm_dir: &Path) -> std::io::Result<[u8; CONTROL_KEY_LEN]> {
    let key_path = calm_dir.join(CONTROL_KEY_FILENAME);

    if let Ok(bytes) = std::fs::read(&key_path)
        && bytes.len() == CONTROL_KEY_LEN
    {
        let mut key = [0u8; CONTROL_KEY_LEN];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    std::fs::create_dir_all(calm_dir)?;
    let mut key = [0u8; CONTROL_KEY_LEN];
    rand::rngs::OsRng
        .try_fill_bytes(&mut key)
        .map_err(std::io::Error::other)?;
    std::fs::write(&key_path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(key)
}

/// The control key for whatever project `conn` (a **state.db** connection)
/// is actually open against -- derived from the connection's own file
/// path, same `ledger_key_for_conn` pattern (see that function's doc
/// comment for the full rationale, including why `Ok(None)` for a
/// path-less `:memory:` connection is the correct fallback and not a
/// silent downgrade).
pub(crate) fn control_key_for_conn(
    conn: &Connection,
) -> std::io::Result<Option<[u8; CONTROL_KEY_LEN]>> {
    let Some(db_path) = conn.path() else {
        return Ok(None);
    };
    let Some(calm_dir) = Path::new(db_path).parent() else {
        return Ok(None);
    };
    load_or_create_control_key(calm_dir).map(Some)
}

/// `hmac-sha256:<hex>` over `domain || "\0" || payload` -- the domain tag
/// is per-purpose separation on top of the single shared `control.key`
/// (blueprint §2: "per-purpose HMAC domain separators"), so a signature
/// minted for one purpose (e.g. `"review-authority-v1"`) can never be
/// replayed as valid for a different future purpose that reuses this same
/// key file, even though both derive from the same key bytes.
pub(crate) fn sign(key: &[u8], domain: &str, payload: &str) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(domain.as_bytes());
    mac.update(b"\0");
    mac.update(payload.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2 + "hmac-sha256:".len());
    hex.push_str("hmac-sha256:");
    for b in bytes {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Constant-time-equality is not needed here beyond what `==` on two
/// `String`s already gives -- this compares a freshly-recomputed
/// signature against one read back from `state.db`, not a
/// network-observable secret comparison an attacker could time.
pub(crate) fn verify(key: &[u8], domain: &str, payload: &str, signature: &str) -> bool {
    sign(key, domain, payload) == signature
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_or_create_control_key_persists_across_calls() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create_control_key(dir.path()).unwrap();
        let b = load_or_create_control_key(dir.path()).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_dirs_get_different_keys() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = load_or_create_control_key(dir_a.path()).unwrap();
        let b = load_or_create_control_key(dir_b.path()).unwrap();
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn control_key_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create_control_key(dir.path()).unwrap();
        let perms = std::fs::metadata(dir.path().join(CONTROL_KEY_FILENAME))
            .unwrap()
            .permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    #[test]
    fn control_key_for_memory_connection_is_none() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(control_key_for_conn(&conn).unwrap(), None);
    }

    #[test]
    fn sign_is_deterministic_for_the_same_inputs() {
        let key = [7u8; CONTROL_KEY_LEN];
        let a = sign(&key, "review-authority-v1", "payload");
        let b = sign(&key, "review-authority-v1", "payload");
        assert_eq!(a, b);
    }

    #[test]
    fn different_domains_produce_different_signatures_for_the_same_payload() {
        let key = [7u8; CONTROL_KEY_LEN];
        let a = sign(&key, "review-authority-v1", "payload");
        let b = sign(&key, "some-other-purpose-v1", "payload");
        assert_ne!(
            a, b,
            "domain separation must change the signature even with an identical payload"
        );
    }

    #[test]
    fn verify_accepts_a_matching_signature_and_rejects_a_tampered_one() {
        let key = [7u8; CONTROL_KEY_LEN];
        let sig = sign(&key, "review-authority-v1", "payload");
        assert!(verify(&key, "review-authority-v1", "payload", &sig));
        assert!(!verify(
            &key,
            "review-authority-v1",
            "tampered-payload",
            &sig
        ));
        assert!(!verify(
            &key,
            "review-authority-v1",
            "payload",
            "hmac-sha256:not-a-real-signature"
        ));
    }
}
