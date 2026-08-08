//! Drift guard for the three derived-logic version consts introduced in P1
//! (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md):
//! `indexer::semantic_facts::SOURCE_EXTRACTION_VERSION`,
//! `graph::digest::GRAPH_DERIVATION_VERSION`,
//! `indexer::package_deps::PACKAGE_GRAPH_VERSION`. Each is folded into
//! `indexer::refresh::InputCatalog::index_input_snapshot`'s fingerprint (see
//! `configuration_reconciliation_reparses_unchanged_sources` and
//! `persisted_input_snapshot_distinguishes_context_from_configuration_drift`
//! in `indexer/refresh.rs` for proof that mechanism itself works) so a
//! binary upgrade that changes what these consts' owning logic PRODUCES
//! forces the right re-derivation on an already-indexed install instead of
//! the delta indexer silently skipping unchanged source bytes and leaving
//! stale rows behind.
//!
//! That mechanism is only as good as the discipline of actually bumping the
//! const when behavior changes -- these three tests are the enforcement,
//! mirroring `tools.rs`'s `UPDATE_TOOLSNAPS` pattern: each hashes the exact
//! derived rows a small frozen fixture produces and pins it to a checked-in
//! literal below. A failure here means extraction/derivation OUTPUT changed
//! for that fixture. Before touching the expected-hash literal, ask: does
//! this change need the matching `*_VERSION` const bumped too, so an
//! already-indexed install self-heals instead of silently keeping stale
//! rows? If yes, bump BOTH in the same commit -- updating the hash alone is
//! not, by itself, proof that question was asked.

use rusqlite::Connection;
use std::path::Path;

fn index_fresh(root: &Path) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    calm_core::db::schema::init_db(&conn).unwrap();
    let phase = std::sync::Arc::new(std::sync::RwLock::new(
        calm_core::types::IndexingPhase::Scanning,
    ));
    calm_core::indexer::pipeline::run_indexing_pipeline(&mut conn, root, phase).unwrap();
    conn
}

// ---------------------------------------------------------------------
// SOURCE_EXTRACTION_VERSION
// ---------------------------------------------------------------------

/// One language (Python -- cheapest to parse, no external toolchain), one
/// class with a base (`extends`), one explicit `raise`, one `self.` field
/// write -- enough surface to catch a change to either
/// `extract_type_relations_from_tree`'s or `extract_effects_from_tree`'s
/// output shape. Per-language grammar quirks already have their own
/// dedicated coverage in `semantic_facts.rs`'s unit tests; this fixture's
/// job is narrower: pin what actually lands in the DB end-to-end.
const SOURCE_EXTRACTION_FIXTURE: &str = "class BaseService:\n    pass\n\n\nclass Service(BaseService):\n    def save(self):\n        self.cache = {}\n        raise ValueError(\"boom\")\n";

fn source_extraction_snapshot(conn: &Connection) -> String {
    let mut relations: Vec<String> = conn
        .prepare(
            "SELECT from_symbol, relation_kind, target_text, to_symbol, confidence \
             FROM type_relations ORDER BY from_symbol, relation_kind, target_text",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut effects: Vec<String> = conn
        .prepare(
            "SELECT symbol_qn, effect_kind, target_text, confidence \
             FROM symbol_effects ORDER BY symbol_qn, effect_kind, target_text",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    relations.sort();
    effects.sort();
    format!(
        "type_relations:\n{}\nsymbol_effects:\n{}",
        relations.join("\n"),
        effects.join("\n")
    )
}

/// Regenerate via: read the assertion failure's "actual hash" and paste it
/// in below alongside a `SOURCE_EXTRACTION_VERSION` bump.
const EXPECTED_SOURCE_EXTRACTION_HASH: &str = "1473f45c2586b01e";

#[test]
fn source_extraction_fixture_is_pinned_to_its_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("service.py"), SOURCE_EXTRACTION_FIXTURE).unwrap();

    let conn = index_fresh(&root);
    let snapshot = source_extraction_snapshot(&conn);
    let actual_hash = calm_core::indexer::pipeline::hash_content(&snapshot);
    assert_eq!(
        actual_hash, EXPECTED_SOURCE_EXTRACTION_HASH,
        "type_relations/symbol_effects extraction output changed for the frozen \
         fixture (raw snapshot below).\nIf intentional: bump \
         SOURCE_EXTRACTION_VERSION in crates/calm-core/src/indexer/semantic_facts.rs \
         AND update EXPECTED_SOURCE_EXTRACTION_HASH here, in the SAME commit -- \
         this keeps an already-indexed install's incremental reindex from \
         silently skipping the reparse it now needs (see \
         docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md P1).\n\
         actual hash: {actual_hash}\nsnapshot:\n{snapshot}"
    );
}

// ---------------------------------------------------------------------
// GRAPH_DERIVATION_VERSION
// ---------------------------------------------------------------------

/// A call edge (`save` -> `helper`) plus a type relation and an effect, so
/// the fixture exercises digest's callee rollup, role-tagging, and T1 fact
/// inclusion together -- the surface `compute_digests` actually renders.
const GRAPH_DERIVATION_FIXTURE: &str = "class BaseService:\n    pass\n\n\nclass Service(BaseService):\n    def helper(self):\n        return 1\n\n    def save(self):\n        self.helper()\n        raise ValueError(\"boom\")\n";

fn graph_derivation_snapshot(conn: &Connection) -> String {
    let mut rows: Vec<String> = conn
        .prepare(
            "SELECT symbol_qn, rendered_text, recursive_component, truncated \
             FROM symbol_digests ORDER BY symbol_qn",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows.sort();
    rows.join("\n")
}

const EXPECTED_GRAPH_DERIVATION_HASH: &str = "21967d703b275a3f";

#[test]
fn graph_derivation_fixture_is_pinned_to_its_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("service.py"), GRAPH_DERIVATION_FIXTURE).unwrap();

    let conn = index_fresh(&root);
    let snapshot = graph_derivation_snapshot(&conn);
    let actual_hash = calm_core::indexer::pipeline::hash_content(&snapshot);
    assert_eq!(
        actual_hash, EXPECTED_GRAPH_DERIVATION_HASH,
        "symbol_digests rendering changed for the frozen fixture (raw snapshot \
         below).\nIf intentional: bump GRAPH_DERIVATION_VERSION in \
         crates/calm-core/src/graph/digest.rs AND update \
         EXPECTED_GRAPH_DERIVATION_HASH here, in the SAME commit (see \
         docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md P1).\n\
         actual hash: {actual_hash}\nsnapshot:\n{snapshot}"
    );
}

// ---------------------------------------------------------------------
// PACKAGE_GRAPH_VERSION
// ---------------------------------------------------------------------

/// Two ecosystems (Cargo + npm) in one fixture, each with a runtime and a
/// dev dependency, so the fixture exercises `dependency_kind` classification
/// across parsers, not just one manifest shape.
const CARGO_TOML_FIXTURE: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";
const PACKAGE_JSON_FIXTURE: &str =
    "{\"dependencies\":{\"react\":\"^18.0.0\"},\"devDependencies\":{\"vitest\":\"^1.0.0\"}}";

fn package_graph_snapshot(conn: &Connection) -> String {
    let mut rows: Vec<String> = conn
        .prepare(
            "SELECT manifest_path, ecosystem, dependency_name, version_spec, dependency_kind \
             FROM package_dependencies ORDER BY manifest_path, dependency_name",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rows.sort();
    rows.join("\n")
}

const EXPECTED_PACKAGE_GRAPH_HASH: &str = "5b40820be0315d4a";

#[test]
fn package_graph_fixture_is_pinned_to_its_version() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Cargo.toml"), CARGO_TOML_FIXTURE).unwrap();
    std::fs::write(root.join("package.json"), PACKAGE_JSON_FIXTURE).unwrap();

    let conn = index_fresh(&root);
    let snapshot = package_graph_snapshot(&conn);
    let actual_hash = calm_core::indexer::pipeline::hash_content(&snapshot);
    assert_eq!(
        actual_hash, EXPECTED_PACKAGE_GRAPH_HASH,
        "package_dependencies extraction changed for the frozen fixture (raw \
         snapshot below).\nIf intentional: bump PACKAGE_GRAPH_VERSION in \
         crates/calm-core/src/indexer/package_deps.rs AND update \
         EXPECTED_PACKAGE_GRAPH_HASH here, in the SAME commit (see \
         docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md P1).\n\
         actual hash: {actual_hash}\nsnapshot:\n{snapshot}"
    );
}
