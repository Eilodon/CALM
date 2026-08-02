//! Trust-boundary content digest — SHA-256, domain-separated from
//! `indexer::pipeline::hash_content` (FNV-1a).
//!
//! `hash_content` stays exactly what it is: an unkeyed, fast, non-adversarial
//! cache/stale-write guard (`file_index.hash`, edit range conflict checks).
//! It is not collision-resistant against an adversarial source (a project
//! whose content is itself untrusted input — see `sanitize.rs`), and it must
//! never be asked to be. `evidence_digest` is for the opposite job: identity
//! of records meant to be trusted at a boundary — edit-transaction digests
//! (WS-1), review-token payloads (WS-2), and any future receipt/ledger
//! record (WS-4/WS-5). Never used for `file_index.hash` or any other
//! cache/perf path — that would just make hot paths slower for no benefit,
//! since cache invalidation was never the adversarial-input problem.
//!
//! SHA-256 (not BLAKE3) is a deliberate choice, not a default: VHEATM's own
//! provenance/receipt/approval-token layer
//! (`vheatm_control.provenance.sha256_digest`,
//! `vheatm_control.tool_broker._canonical_digest`, and every
//! `schemas/*.schema.json` identity pattern, all `^[a-f0-9]{64}$`) is
//! SHA-256 end to end. Matching it here means a future CALM↔VHEATM receipt
//! handoff (see docs/plans/2026-08-01-calm-master-upgrade-plan.md §4) never
//! needs a translation layer at the boundary. Revisit only with a measured
//! hot-path bottleneck, not a hunch — see
//! docs/plans/2026-08-02-phase1-p0-execution-plan.md §3.2.

use sha2::{Digest, Sha256};

/// SHA-256 of `content`, hex-encoded and prefixed `sha256:` so a digest used
/// in the wrong place (e.g. accidentally compared against a `fast_hash`
/// value) fails loudly instead of silently matching.
pub fn evidence_digest(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_digest_is_stable_and_collision_domain_separated() {
        let a = evidence_digest(b"hello world");
        let b = evidence_digest(b"hello world");
        let c = evidence_digest(b"hello world!");
        assert_eq!(a, b, "same input must hash identically every call");
        assert_ne!(a, c, "different input must not collide");
        assert!(
            a.starts_with("sha256:"),
            "digest must carry an explicit algorithm prefix, got {a:?}"
        );
        assert_eq!(
            a, "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
            "must be real SHA-256, not a placeholder — verified via `sha256sum` for \"hello world\""
        );
    }

    #[test]
    fn evidence_digest_of_empty_content_is_well_defined() {
        let digest = evidence_digest(b"");
        assert_eq!(
            digest, "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "empty input is a valid, well-known SHA-256 vector, not a special case"
        );
    }
}
