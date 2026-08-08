//! PR E (issue #66, docs/plans/2026-08-08-derived-artifact-hardening-
//! execution-plan.md): closes the gap `scripts/gen-status.sh --check`
//! deliberately doesn't cover. That script already catches FORMAT/PRESENCE
//! drift in `docs/guarantee-levels.toml` (a stale entry vs. the rendered
//! `docs/status.generated.md`) -- but nothing previously asserted that the
//! behavior a given `level = "enforced"` entry *describes* is still backed
//! by a real, findable test. A refactor that silently weakened an enforced
//! guarantee (e.g. `txn.begin_before_write`) would pass CI as long as the
//! TOML entry's prose stayed put.
//!
//! Two checks, deliberately kept lightweight (not a semantic re-verification
//! framework -- that's what the referenced test itself is for):
//! 1. Every `level = "enforced"` entry has a non-empty `test` field.
//! 2. Every `test` field's bare function name resolves to a real `fn`
//!    definition somewhere under `crates/` -- catches a stale reference
//!    left behind by a rename/deletion, not just a field that was never
//!    filled in.
//!
//! Scoped to `enforced` only, per the issue's own instruction ("Start with
//! the enforced-level entries") -- advisory/best_effort/optional/
//! provider_dependent/unsupported each describe a DELIBERATE absence of an
//! unconditional guarantee, so there is no single behavior a test could
//! assert holds across the board the way an `enforced` entry's can.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct GuaranteeCatalog {
    behavior: Vec<Behavior>,
}

#[derive(Deserialize)]
struct Behavior {
    id: String,
    level: String,
    #[serde(default)]
    test: Option<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/calm-core is always two levels under the repo root")
        .to_path_buf()
}

fn load_catalog() -> GuaranteeCatalog {
    let path = repo_root().join("docs/guarantee-levels.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    toml::from_str(&text)
        .unwrap_or_else(|e| panic!("{} did not parse as valid TOML: {e}", path.display()))
}

/// True when `fn <bare_name>(` (the last `::`-separated segment of
/// `qualified_test_ref`, e.g. `calm-server::tools::tests::foo` -> `foo`)
/// appears as a real function definition somewhere under `crates/`. A
/// substring match on the exact `fn <name>(` shape, not a full parse --
/// matches this repo's own `gen-status.sh` precedent of a small, honest,
/// purpose-built check over a general-purpose tool.
fn test_fn_exists(qualified_test_ref: &str) -> bool {
    let bare_name = qualified_test_ref
        .rsplit("::")
        .next()
        .unwrap_or(qualified_test_ref);
    let needle = format!("fn {bare_name}(");
    let walker = ignore::WalkBuilder::new(repo_root().join("crates")).build();
    for entry in walker.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path())
            && content.contains(&needle)
        {
            return true;
        }
    }
    false
}

#[test]
fn every_enforced_guarantee_has_a_mapped_contract_test() {
    let catalog = load_catalog();
    let missing: Vec<&str> = catalog
        .behavior
        .iter()
        .filter(|b| b.level == "enforced" && b.test.as_deref().unwrap_or("").is_empty())
        .map(|b| b.id.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "these `level = \"enforced\"` guarantees in docs/guarantee-levels.toml have no mapped \
         `test` field (issue #66) -- a semantic regression in their behavior would pass CI \
         silently. Add `test = \"crate::module::path::test_fn_name\"` pointing at the test that \
         exercises each: {missing:?}"
    );
}

#[test]
fn every_mapped_contract_test_reference_still_resolves() {
    let catalog = load_catalog();
    let stale: Vec<String> = catalog
        .behavior
        .iter()
        .filter_map(|b| {
            let test_ref = b.test.as_deref()?;
            (!test_fn_exists(test_ref)).then(|| format!("{} -> {test_ref}", b.id))
        })
        .collect();
    assert!(
        stale.is_empty(),
        "these guarantee.test references in docs/guarantee-levels.toml no longer resolve to a \
         real `fn` anywhere under crates/ (renamed or deleted since the entry was written) -- \
         update the `test` field to point at whatever now covers the behavior: {stale:?}"
    );
}
