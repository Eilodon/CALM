//! Phase B T1 (docs/plans/2026-07-13-phase-b-incremental-graph-update.md §4):
//! golden-equivalence harness. Built and proven BEFORE any incremental-graph
//! code exists: every round compares a CONTINUED index (reindex_changed on a
//! long-lived DB — the code path `incremental_graph_update` will later hook
//! into) against a FRESH from-scratch index of the same tree. On current code
//! both sides end in the same full `rebuild_graph`, so any mismatch here is a
//! harness bug or a pre-existing nondeterminism (A-3/A-4 in the plan's risk
//! assessment) — which is exactly what this sanity stage exists to flush out.
//!
//! Comparison uses semantic keys only (never `call_edges.id` — plan D7):
//! edges as (from_symbol, to_symbol, call_site_line, edge_confidence,
//! edge_kind, from_path, to_path), symbols as (caller_count, coreness,
//! is_hub, hub_kind, boundary_ambiguous) per qualified_name. Valid only on
//! overlay-free DBs: incremental deliberately PRESERVES scip enrichment that
//! a full rebuild destroys, so post-overlay states are compared by dedicated
//! T5 tests instead, never by this fingerprint.

use calm_core::indexer::pipeline::{self, GraphMode};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// (from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind,
/// from_path, to_path)
type EdgeKey = (
    String,
    String,
    Option<i64>,
    String,
    String,
    Option<String>,
    Option<String>,
);
/// (caller_count, coreness, is_hub, hub_kind, boundary_ambiguous)
type SymbolMetrics = (i64, i64, i64, Option<String>, i64);

fn graph_fingerprint(conn: &Connection) -> (BTreeSet<EdgeKey>, BTreeMap<String, SymbolMetrics>) {
    let mut stmt = conn
        .prepare(
            "SELECT from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind, \
                    from_path, to_path FROM call_edges",
        )
        .unwrap();
    let edges: BTreeSet<EdgeKey> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let mut stmt = conn
        .prepare(
            "SELECT qualified_name, caller_count, coreness, is_hub, hub_kind, \
                    boundary_ambiguous FROM symbols",
        )
        .unwrap();
    let symbols: BTreeMap<String, SymbolMetrics> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                ),
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    (edges, symbols)
}

/// Panic with a readable, capped diff if the two DBs' graph fingerprints
/// differ in any way. `ctx` names the seed/round/mutation for reproduction.
fn assert_graph_equal(continued: &Connection, fresh: &Connection, ctx: &str) {
    let (edges_a, syms_a) = graph_fingerprint(continued);
    let (edges_b, syms_b) = graph_fingerprint(fresh);

    const CAP: usize = 15;
    let mut report = String::new();

    let only_a: Vec<_> = edges_a.difference(&edges_b).take(CAP).collect();
    let only_b: Vec<_> = edges_b.difference(&edges_a).take(CAP).collect();
    if !only_a.is_empty() || !only_b.is_empty() {
        report.push_str(&format!(
            "edge sets differ ({} continued-only, {} fresh-only; showing ≤{CAP} each)\n",
            edges_a.difference(&edges_b).count(),
            edges_b.difference(&edges_a).count(),
        ));
        for e in &only_a {
            report.push_str(&format!("  continued-only: {e:?}\n"));
        }
        for e in &only_b {
            report.push_str(&format!("  fresh-only:     {e:?}\n"));
        }
    }

    let qns: BTreeSet<&String> = syms_a.keys().chain(syms_b.keys()).collect();
    let mut metric_diffs = 0usize;
    for qn in qns {
        let (a, b) = (syms_a.get(qn), syms_b.get(qn));
        if a != b {
            metric_diffs += 1;
            if metric_diffs <= CAP {
                report.push_str(&format!(
                    "  metrics differ for {qn}: continued={a:?} fresh={b:?}\n"
                ));
            }
        }
    }
    if metric_diffs > 0 {
        report.push_str(&format!(
            "{metric_diffs} symbol(s) with differing (caller_count, coreness, is_hub, \
             hub_kind, boundary_ambiguous)\n"
        ));
    }

    assert!(
        report.is_empty(),
        "golden equivalence violated [{ctx}]:\n{report}"
    );
}

/// Deterministic PRNG (xorshift64*) — no dev-dependency on `rand`, and a
/// failure message that prints the seed reproduces the exact run.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // splitmix64 scramble so small seeds (1, 2, 3) don't start correlated
        let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        Self((z ^ (z >> 31)).max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn gen_range(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    /// Fisher–Yates permutation of 0..n.
    fn permutation(&mut self, n: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            v.swap(i, self.gen_range(i + 1));
        }
        v
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multi_lang_workspace")
}

fn index_fresh(root: &Path) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    calm_core::db::schema::init_db(&conn).unwrap();
    let phase = std::sync::Arc::new(std::sync::RwLock::new(
        calm_core::types::IndexingPhase::Scanning,
    ));
    calm_core::indexer::pipeline::run_indexing_pipeline(&mut conn, root, phase).unwrap();
    conn
}

/// Continued-index step: the same entry point the file watcher uses, and the
/// one whose graph stage T4 will switch to `incremental_graph_update`.
fn reindex_continued(
    conn: &mut Connection,
    root: &Path,
) -> calm_core::indexer::pipeline::ReindexSummary {
    match calm_core::indexer::pipeline::reindex_changed_cancellable(conn, root, &|| false).unwrap()
    {
        calm_core::indexer::pipeline::ReindexOutcome::Completed(summary) => summary,
        calm_core::indexer::pipeline::ReindexOutcome::Cancelled => {
            unreachable!("cancel closure is constant false")
        }
    }
}

/// Replace `needle` with `replacement` in one workspace file, asserting the
/// needle is present — a silent no-op mutation would make a round vacuous
/// and hide fixture drift (risk A-2).
fn replace_in_file(root: &Path, rel: &str, needle: &str, replacement: &str) {
    let path = root.join(rel);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(needle),
        "mutation needle {needle:?} not found in {rel} — generated file drifted from driver"
    );
    std::fs::write(&path, text.replace(needle, replacement)).unwrap();
}

/// Seed files written into the workspace copy before the baseline round.
/// They deliberately re-define names that already exist in the fixture
/// (`helper` in python, `Greet` after the collision-rename in go, `greet`
/// callers in js) so cross-file same-name narrowing branches actually fire
/// (risk A-2), while keeping every mutation inside driver-owned files so the
/// pristine fixture never needs language-aware editing. Each mutation kind
/// targets its own file, so the seeded round order never invalidates a later
/// mutation's target.
fn write_seed_files(root: &Path) {
    std::fs::write(
        root.join("python/gen_a.py"),
        "def helper(name):\n    return \"gen: \" + name\n\n\ndef gen_alpha():\n    a = helper(\"alpha\")\n    b = run()\n    return (a, b)\n",
    )
    .unwrap();
    std::fs::write(
        root.join("go/gen_a.go"),
        "package main\n\nfunc GenAlpha() string {\n\treturn Greet(\"gen\") + LocalUtil()\n}\n\nfunc LocalUtil() string {\n\treturn \"util\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("js/gen_a.js"),
        "function localUtil() {\n    return \"util\";\n}\n\nfunction genAlpha() {\n    return greet(\"gen\") + localUtil();\n}\n",
    )
    .unwrap();
    // Dedicated delete victim — nothing else ever touches this file.
    std::fs::write(
        root.join("js/gen_c.js"),
        "function genGamma() {\n    return greet(\"gamma\");\n}\n",
    )
    .unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mutation {
    /// Body-only change: no symbol name changes, resolution inputs for other
    /// files must be untouched.
    BodyEdit,
    /// Rename a function defined and called only inside one js file.
    RenameFn,
    /// New file whose `run`/`helper` definitions collide with existing
    /// fixture symbol names in the same language.
    AddFile,
    /// Remove a file whose symbol was a call-edge source.
    DeleteFile,
    /// The plan's L1 case: rename a go function to a name (`Greet`) already
    /// defined in ANOTHER file of the same language/package — call sites in
    /// unchanged files must re-resolve (and the harness proves fresh/continued
    /// agree on the outcome).
    CollisionRename,
}

const ALL_MUTATIONS: [Mutation; 5] = [
    Mutation::BodyEdit,
    Mutation::RenameFn,
    Mutation::AddFile,
    Mutation::DeleteFile,
    Mutation::CollisionRename,
];

fn apply_mutation(root: &Path, m: Mutation, rng: &mut XorShift64) {
    match m {
        Mutation::BodyEdit => {
            let tag = format!("gen#{}: ", rng.gen_range(1_000_000));
            replace_in_file(root, "python/gen_a.py", "gen: ", &tag);
        }
        Mutation::RenameFn => {
            replace_in_file(root, "js/gen_a.js", "localUtil", "localUtilV2");
        }
        Mutation::AddFile => {
            std::fs::write(
                root.join("python/gen_b.py"),
                "def run():\n    return \"gen-b run\"\n\n\ndef gen_beta():\n    return helper(\"beta\") + run()\n",
            )
            .unwrap();
        }
        Mutation::DeleteFile => {
            std::fs::remove_file(root.join("js/gen_c.js")).unwrap();
        }
        Mutation::CollisionRename => {
            replace_in_file(root, "go/gen_a.go", "LocalUtil", "Greet");
        }
    }
}

fn run_rounds_for_seed(seed: u64, incremental: bool) {
    let mut rng = XorShift64::new(seed);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir_recursive(&fixture_root(), &root);
    // Set the flag EXPLICITLY (never rely on the compile-time default) so
    // this driver tests exactly one mode regardless of what
    // `IndexingConfig::default()` currently is — T6.4 flips that default,
    // and without this an unrelated flip would silently change what every
    // round here compares. Written before the first index so `config.json`
    // never appears as a changed path in a later round (same reasoning as
    // `enable_incremental_graph`'s doc comment).
    if incremental {
        enable_incremental_graph(&root);
    } else {
        disable_incremental_graph(&root);
    }
    let expected_mode = if incremental {
        GraphMode::Incremental
    } else {
        GraphMode::Full
    };

    // DB A starts from the PRISTINE fixture so that round 0 (seed-file
    // creation) is already a real continued-vs-fresh comparison, not a
    // trivially-equal fresh-vs-fresh one.
    let mut db_a = index_fresh(&root);

    write_seed_files(&root);
    let summary = reindex_continued(&mut db_a, &root);
    assert!(
        summary.changed >= 4,
        "seed files not picked up as changes: {summary:?}"
    );
    assert_eq!(
        summary.graph_mode, expected_mode,
        "seed round took the wrong graph path (seed={seed})"
    );
    let db_b = index_fresh(&root);
    assert_graph_equal(&db_a, &db_b, &format!("seed={seed} round=0 op=SeedFiles"));

    // Guard against a vacuously-green harness: the seed files must actually
    // contribute symbols AND at least one call edge to the graph being
    // compared — if extraction of the generated files ever silently stops
    // (extension mapping change, parse failure), every later comparison
    // would still pass while exercising nothing.
    let (edges, symbols) = graph_fingerprint(&db_b);
    assert!(
        symbols.keys().any(|qn| qn.ends_with("::gen_alpha"))
            && symbols.keys().any(|qn| qn.ends_with("::GenAlpha")),
        "generated seed symbols missing from index — harness would be vacuous"
    );
    assert!(
        edges
            .iter()
            .any(|(from, ..)| from.ends_with("::gen_alpha") || from.ends_with("::GenAlpha")),
        "no call edges from generated seed files — harness would be vacuous"
    );

    for (round, &idx) in rng.permutation(ALL_MUTATIONS.len()).iter().enumerate() {
        let mutation = ALL_MUTATIONS[idx];
        apply_mutation(&root, mutation, &mut rng);
        let summary = reindex_continued(&mut db_a, &root);
        assert!(
            !summary.is_noop(),
            "mutation {mutation:?} produced a no-op reindex (seed={seed} round={})",
            round + 1
        );
        assert_eq!(
            summary.graph_mode,
            expected_mode,
            "mutation {mutation:?} took the wrong graph path (seed={seed} round={})",
            round + 1
        );
        let db_b = index_fresh(&root);
        assert_graph_equal(
            &db_a,
            &db_b,
            &format!("seed={seed} round={} op={mutation:?}", round + 1),
        );
    }
}

/// FULL-vs-full harness sanity: continued index (full `rebuild_graph`) vs
/// from-scratch index across 3 seeds × (1 seed-file round + 5 mutation
/// rounds) = 18 comparisons. Proves the fingerprint harness and pipeline
/// determinism (A-3/A-4) independently of the incremental path — the flag is
/// forced OFF here regardless of the compile-time default, so a future
/// default flip can't silently turn this into an incremental test.
#[test]
fn golden_equivalence_continued_vs_fresh_across_mutation_rounds() {
    for seed in [1u64, 2, 3] {
        run_rounds_for_seed(seed, false);
    }
}

/// INCREMENTAL-vs-full golden equivalence: identical 18 comparisons, but the
/// continued side runs `incremental_graph_update` (flag forced ON) while the
/// reference side is always a full from-scratch rebuild. The permanent,
/// CI-resident form of the plan's "golden 5-vòng xanh" bar (Phase B §4
/// Done-when): any delta-selection bug that lets a stale edge survive, or
/// drops one it shouldn't, surfaces as a fingerprint mismatch tagged with the
/// exact (seed, round, op). Overlay-free by construction (`index_fresh` runs
/// neither embeddings nor SCIP), the only state on which incremental == full
/// holds exactly (plan D7).
#[test]
fn golden_equivalence_incremental_vs_fresh_across_mutation_rounds() {
    for seed in [1u64, 2, 3] {
        run_rounds_for_seed(seed, true);
    }
}

/// A-4 from the plan's risk assessment: `compute_coreness` iterates HashMap/
/// HashSet internally, so prove the whole pipeline's OBSERVABLE output is
/// deterministic by indexing the identical tree twice from scratch and
/// requiring identical fingerprints. If this ever flakes, the pipeline had a
/// real nondeterminism before Phase B touched anything — fix that first.
#[test]
fn fresh_index_is_deterministic_on_identical_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir_recursive(&fixture_root(), &root);
    write_seed_files(&root);

    let db_1 = index_fresh(&root);
    let db_2 = index_fresh(&root);
    assert_graph_equal(&db_1, &db_2, "determinism: fresh vs fresh, same tree");
}

/// T5 (docs/plans/2026-07-13-phase-b-incremental-graph-update.md §4): turns
/// on `indexing.incremental_graph` for `root`. Must be written BEFORE the
/// first index of this tree so `config.json` never appears as a "changed
/// path" in any later round's delta (plan D8's mechanism —
/// `config::resolve_config_path`/`load_config` reads it fresh every call,
/// no caching to invalidate).
fn enable_incremental_graph(root: &Path) {
    std::fs::write(
        root.join("config.json"),
        r#"{"indexing":{"incremental_graph":true}}"#,
    )
    .unwrap();
}

/// T5 #1 (plan D1 proof, by-id not by-fingerprint): a body-only edit to one
/// file must leave every `call_edges.id` OUTSIDE that file's `from_path`
/// completely untouched. `assert_graph_equal`'s semantic-key comparison
/// alone can't distinguish "these rows were never touched" from "these rows
/// were deleted and reinserted identically" — this test asserts the
/// stronger, by-id claim that the scoped `DELETE ... WHERE from_path IN
/// delta_paths` (plan D1) never reaches an unrelated from_path's rows.
#[test]
fn unchanged_file_edges_survive_by_id() {
    // Self-contained, non-colliding fixture — deliberately NOT
    // multi_lang_workspace/write_seed_files, whose seed files intentionally
    // create same-named collisions (needed to exercise T1's narrowing
    // branches). A common name here would pull an unrelated file into
    // delta_paths via the names_delta callee_name fan-out even when its
    // resolved TARGET never changes (over-approximation by bare name, not a
    // bug — see plan Abductive Hypothesis 2), rebuilding its edges with new
    // ids and defeating this specific by-id claim.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    std::fs::write(
        root.join("edited.py"),
        "def edited_helper():\n    return \"v1\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("unrelated.py"),
        "def unrelated_target():\n    return \"u\"\n\n\ndef unrelated():\n    return unrelated_target()\n",
    )
    .unwrap();

    let mut db = index_fresh(&root);

    let ids_outside_target = |conn: &Connection| -> BTreeSet<i64> {
        let mut stmt = conn
            .prepare("SELECT id FROM call_edges WHERE from_path != 'edited.py'")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let before = ids_outside_target(&db);
    assert!(
        !before.is_empty(),
        "harness sanity: expected at least one call edge outside the mutation target"
    );

    // Body-only edit, name unchanged — no rename, no collision anywhere in
    // this 2-file fixture, so delta_paths must stay exactly {edited.py}.
    std::fs::write(
        root.join("edited.py"),
        "def edited_helper():\n    return \"v2\"\n",
    )
    .unwrap();
    let summary = pipeline::reindex_paths(&mut db, &root, &["edited.py".to_string()]).unwrap();
    assert!(!summary.is_noop());
    assert_eq!(
        summary.graph_mode,
        GraphMode::Incremental,
        "flag is on and delta is 1 file — must not silently fall back to full rebuild"
    );

    let after = ids_outside_target(&db);
    assert_eq!(
        before, after,
        "incremental update touched call_edges row(s) outside its own delta_paths"
    );
}

/// T5 #2 (plan D7/D5): SCIP-overlay enrichment (`ruled_out_by_scip`,
/// `formal_source`) on an edge from a file OUTSIDE the edit's delta must
/// survive an unrelated incremental update — the exact thing this file's
/// own `assert_graph_equal` fingerprint deliberately does NOT check (see the
/// module doc comment: incremental preserves what full rebuild destroys).
/// Sets the two columns by hand to the exact values
/// `scip::ingest::ingest_occurrences` itself would set, without needing a
/// real rust-analyzer/SCIP run.
#[test]
fn scip_flag_survives_edit_of_other_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir_recursive(&fixture_root(), &root);
    enable_incremental_graph(&root);
    write_seed_files(&root);

    let mut db = index_fresh(&root);

    let edge_id: i64 = db
        .query_row(
            "SELECT id FROM call_edges WHERE from_path = 'js/main.js' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("fixture must have at least one call edge from js/main.js (run() -> greet())");
    db.execute(
        "UPDATE call_edges SET ruled_out_by_scip = 1, formal_source = 'scip' WHERE id = ?1",
        [edge_id],
    )
    .unwrap();

    // BodyEdit only touches python/gen_a.py — js/main.js's callee name
    // (`greet`) has no relation to anything named in gen_a.py, so it must
    // fall entirely outside delta_paths, both directly and via names_delta
    // fan-out.
    let mut rng = XorShift64::new(11);
    apply_mutation(&root, Mutation::BodyEdit, &mut rng);
    let summary = reindex_continued(&mut db, &root);
    assert_eq!(summary.graph_mode, GraphMode::Incremental);

    let (ruled_out, formal_source): (i64, Option<String>) = db
        .query_row(
            "SELECT ruled_out_by_scip, formal_source FROM call_edges WHERE id = ?1",
            [edge_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or_else(|e| {
            panic!("edge id={edge_id} vanished after an unrelated incremental update: {e}")
        });
    assert_eq!(
        ruled_out, 1,
        "ruled_out_by_scip flag lost on an edge outside the edit's delta_paths"
    );
    assert_eq!(
        formal_source.as_deref(),
        Some("scip"),
        "formal_source lost on an edge outside the edit's delta_paths"
    );
}

/// T5 #3 (plan D1/D2): renaming a symbol in file A must drop the stale edge
/// from an UNTOUCHED file B that still calls the old name — B is pulled
/// into `delta_paths` solely through the `names_delta` fan-out
/// (`call_sites WHERE callee_name IN names_delta`), never because B itself
/// changed. Self-contained 2-file fixture (not `multi_lang_workspace`) so
/// this carries zero risk to the T1 golden tests' shared fixture/mutation
/// driver — same minimal shape as the real `go/helper.go`+`go/main.go` pair
/// (both files at tree root, so Go's same-directory preference still
/// applies exactly as it does in the real fixture).
#[test]
fn rename_reroutes_cross_file_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    std::fs::write(
        root.join("helper.go"),
        "package main\n\nfunc Helper() string {\n\treturn \"v1\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("main.go"),
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(Helper())\n}\n",
    )
    .unwrap();

    let mut db = index_fresh(&root);
    let caller_qn: String = db
        .query_row(
            "SELECT qualified_name FROM symbols WHERE name = 'main' AND path = 'main.go'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let before: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM call_edges WHERE from_symbol = ?1",
            [&caller_qn],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        before, 1,
        "sanity: main() must have exactly one outgoing call edge pre-rename"
    );

    let text = std::fs::read_to_string(root.join("helper.go")).unwrap();
    std::fs::write(root.join("helper.go"), text.replace("Helper", "Helper2")).unwrap();
    let summary = reindex_continued(&mut db, &root);
    assert!(!summary.is_noop());
    assert_eq!(summary.graph_mode, GraphMode::Incremental);

    let after: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM call_edges WHERE from_symbol = ?1",
            [&caller_qn],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after, 0,
        "stale edge to a renamed-away symbol must be dropped once the untouched caller file \
         is pulled into delta via names_delta fan-out"
    );
}

/// T5 #4 (plan §3.1 L1 case, the highest-value one): renaming a symbol in
/// file A to collide with an EXISTING same-named symbol in file B must turn
/// an unrelated, untouched caller's single `resolved` edge into two
/// `ambiguous` edges — proving the delta correctly re-resolves a site whose
/// candidate SET changed, not just one whose target vanished (T5 #3) or
/// whose target's shape changed (T5 #6). Self-contained 3-file fixture — no
/// need to touch `multi_lang_workspace`'s real `go/helper.go`/`go/main.go`:
/// that pair already demonstrates the identical mechanism in production
/// (`helper.go`'s own comment: "Greet is called from main.go with no
/// import"), this just isolates it.
#[test]
fn rename_collides_with_existing_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    std::fs::write(
        root.join("helper.go"),
        "package main\n\nfunc Greet(name string) string {\n\treturn \"Hello, \" + name\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("main.go"),
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(Greet(\"world\"))\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("gen.go"),
        "package main\n\nfunc LocalUtil() string {\n\treturn \"util\"\n}\n",
    )
    .unwrap();

    let mut db = index_fresh(&root);
    let caller_qn: String = db
        .query_row(
            "SELECT qualified_name FROM symbols WHERE name = 'main' AND path = 'main.go'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let before: Vec<String> = {
        let mut stmt = db
            .prepare("SELECT edge_confidence FROM call_edges WHERE from_symbol = ?1")
            .unwrap();
        stmt.query_map([&caller_qn], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        before.len(),
        1,
        "sanity: main() must have exactly one outgoing edge pre-collision, got {before:?}"
    );
    assert_ne!(
        before[0], "ambiguous",
        "sanity: pre-collision edge must not already be ambiguous, got {before:?}"
    );

    let text = std::fs::read_to_string(root.join("gen.go")).unwrap();
    std::fs::write(root.join("gen.go"), text.replace("LocalUtil", "Greet")).unwrap();
    let summary = reindex_continued(&mut db, &root);
    assert!(!summary.is_noop());
    assert_eq!(summary.graph_mode, GraphMode::Incremental);

    let after: Vec<String> = {
        let mut stmt = db
            .prepare("SELECT edge_confidence FROM call_edges WHERE from_symbol = ?1")
            .unwrap();
        stmt.query_map([&caller_qn], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        after.len(),
        2,
        "expected exactly 2 ambiguous candidates for Greet after collision rename, got {after:?}"
    );
    assert!(
        after.iter().all(|c| c == "ambiguous"),
        "post-collision edges must all be ambiguous, got {after:?}"
    );
}

/// T5 #5 (plan D5): a SCIP-inserted edge (no `call_sites` row backing it —
/// mirrors `scip::ingest::insert_missing_edges`) whose target symbol's file
/// gets deleted must be cleaned up by the unconditional dangling sweep
/// (`DELETE FROM call_edges WHERE to_symbol NOT IN (SELECT qualified_name
/// FROM symbols)`), even though the edge's OWN `from_path` is never in
/// `delta_paths` — the whole reason that sweep runs unconditionally on
/// every incremental pass instead of being scoped like the rest of it.
#[test]
fn scip_inserted_edge_cleaned_when_target_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    std::fs::write(root.join("helper.py"), "def target():\n    return \"v\"\n").unwrap();
    std::fs::write(root.join("other.py"), "def noop():\n    return 1\n").unwrap();

    let mut db = index_fresh(&root);
    let target_qn: String = db
        .query_row(
            "SELECT qualified_name FROM symbols WHERE name = 'target' AND path = 'helper.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let other_qn: String = db
        .query_row(
            "SELECT qualified_name FROM symbols WHERE name = 'noop' AND path = 'other.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "INSERT INTO call_edges \
         (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path, edge_kind, formal_source) \
         VALUES (?1, ?2, NULL, 'formal', 'other.py', 'helper.py', 'call', 'scip')",
        rusqlite::params![other_qn, target_qn],
    )
    .unwrap();
    let fake_edge_id = db.last_insert_rowid();

    std::fs::remove_file(root.join("helper.py")).unwrap();
    let summary = pipeline::reindex_paths(&mut db, &root, &["helper.py".to_string()]).unwrap();
    assert!(!summary.is_noop());
    assert_eq!(summary.graph_mode, GraphMode::Incremental);

    let still_exists: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM call_edges WHERE id = ?1)",
            [fake_edge_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !still_exists,
        "scip-inserted edge (no call_sites backing) survived after its target symbol's file \
         was deleted — dangling sweep failed"
    );
}

/// T5 #6 (plan D2, highest bug-detection value): a SIGNATURE-only change —
/// the callee's NAME stays the same, only its return type does — must still
/// re-resolve a chained call site in an UNTOUCHED file. Proves `names_delta`
/// is a union of every name in the changed file (D2), not a rename-only
/// symmetric diff: if it were symmetric-diff, "as_str" (unchanged name)
/// would never enter `names_delta` and `caller.rs` (never edited) would
/// keep its stale edge to a candidate that can no longer compile against
/// `.unwrap()`. Fixture mirrors the existing
/// `test_option_chained_call_excludes_non_option_candidates` unit test.
#[test]
fn sig_only_change_reresolves_chained_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    std::fs::write(
        root.join("a.rs"),
        "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("b.rs"),
        "pub struct Bar;\nimpl Bar {\n    pub fn as_str(&self) -> Option<&'static str> {\n        Some(\"b\")\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("caller.rs"),
        "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str().unwrap();\n}\n",
    )
    .unwrap();

    let mut db = index_fresh(&root);
    let caller_qn: String = db
        .query_row(
            "SELECT qualified_name FROM symbols WHERE name = 'caller'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let before: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM call_edges WHERE from_symbol = ?1 \
             AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'b.rs')",
            [&caller_qn],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        before, 1,
        "sanity: caller() must resolve to Bar::as_str pre-edit"
    );

    // Name unchanged, return type Option<&'static str> -> &'static str: the
    // exact D2 "signature-only change" case.
    std::fs::write(
        root.join("b.rs"),
        "pub struct Bar;\nimpl Bar {\n    pub fn as_str(&self) -> &'static str {\n        \"b\"\n    }\n}\n",
    )
    .unwrap();
    let summary = pipeline::reindex_paths(&mut db, &root, &["b.rs".to_string()]).unwrap();
    assert!(!summary.is_noop());
    assert_eq!(summary.graph_mode, GraphMode::Incremental);

    // Filtered to `as_str` targets specifically — `caller()` also calls
    // `get_something()` in the same file, a legitimate, unrelated edge that
    // must NOT disappear (an unfiltered COUNT(*) here would wrongly expect
    // 0 total and fail on that edge instead of proving anything about the
    // chained-call re-resolution this test actually targets).
    let after_as_str: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM call_edges WHERE from_symbol = ?1 \
             AND to_symbol IN (SELECT qualified_name FROM symbols WHERE name = 'as_str')",
            [&caller_qn],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        after_as_str, 0,
        "caller()'s chained .as_str().unwrap() must have zero targets once neither a.rs::as_str \
         nor b.rs::as_str returns Option/Result — a stale edge here means names_delta missed the \
         unchanged name \"as_str\" from the edited file (D2 union, not symmetric-diff)"
    );
}

/// T5 #7 (plan D6): once `delta_paths` exceeds `MAX_INCREMENTAL_DELTA_PATHS`
/// (50), `incremental_graph_update` must fall back to a full `rebuild_graph`
/// internally rather than run the scoped path at a size where the
/// delta-expansion query stops being worth it. The resulting graph must
/// still match an independent fresh index — full rebuild is always correct,
/// this only tests that the size check actually routes to it.
#[test]
fn delta_over_50_falls_back_full() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);
    let paths: Vec<String> = (0..51)
        .map(|i| {
            let name = format!("f{i}.py");
            std::fs::write(root.join(&name), format!("def f{i}():\n    return {i}\n")).unwrap();
            name
        })
        .collect();

    let mut db = index_fresh(&root);

    // Bump every file's content so all 51 register as changed — delta_seed
    // alone already exceeds the threshold, no names_delta fan-out needed.
    for (i, name) in paths.iter().enumerate() {
        std::fs::write(
            root.join(name),
            format!("def f{i}():\n    return {}\n", i + 1000),
        )
        .unwrap();
    }
    let summary = pipeline::reindex_paths(&mut db, &root, &paths).unwrap();
    assert!(!summary.is_noop());
    match &summary.graph_mode {
        GraphMode::FullFallback(reason) => {
            assert!(
                reason.contains("delta_paths.len()"),
                "unexpected fallback reason: {reason}"
            );
        }
        other => panic!("expected FullFallback for 51 changed paths, got {other:?}"),
    }

    let db_fresh = index_fresh(&root);
    assert_graph_equal(
        &db,
        &db_fresh,
        "delta_over_50_falls_back_full: post-fallback graph must match a fresh index",
    );
}

/// T5 #8: with the flag at its default `false`, every non-noop reindex must
/// report `GraphMode::Full` — locks the "off" branch of both
/// `reindex_paths` and `reindex_changed_cancellable` so a future edit near
/// the flag check can't silently start routing through
/// `incremental_graph_update` without a corresponding config change. Reuses
/// the same seed-files + mutation helpers T1 already proved safe, just adds
/// the `graph_mode` assertion neither existing T1 test makes.
#[test]
fn flag_off_is_bitwise_full_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir_recursive(&fixture_root(), &root);
    // Flag forced OFF explicitly — since plan T6.4 the compile-time default
    // is `true`, so this must actively disable to keep testing the OFF path.
    disable_incremental_graph(&root);

    let mut db = index_fresh(&root);
    write_seed_files(&root);
    let summary = reindex_continued(&mut db, &root);
    assert!(!summary.is_noop());
    assert_eq!(
        summary.graph_mode,
        GraphMode::Full,
        "flag defaults false — must never report Incremental/FullFallback"
    );

    let mut rng = XorShift64::new(42);
    apply_mutation(&root, Mutation::BodyEdit, &mut rng);
    let summary = reindex_continued(&mut db, &root);
    assert!(!summary.is_noop());
    assert_eq!(summary.graph_mode, GraphMode::Full);
}

/// Inverse of `enable_incremental_graph`: pins the flag OFF explicitly. Used
/// by the full-vs-full sanity driver and `flag_off_is_bitwise_full_rebuild`
/// so neither depends on `IndexingConfig::default()` — T6.4 flips that
/// default to `true`, and without an explicit `false` here those tests would
/// start exercising the incremental path (or, for the flag-off test, fail).
fn disable_incremental_graph(root: &Path) {
    std::fs::write(
        root.join("config.json"),
        r#"{"indexing":{"incremental_graph":false}}"#,
    )
    .unwrap();
}

/// Repo root of the live CALM checkout this test crate lives in
/// (`.../crates/calm-core` → `../..`). Canonicalized so `copy_repo_filtered`'s
/// recursion starts from a real absolute path.
fn real_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Copy `src` into `dst`, skipping every dot-directory (`.git`, `.calm`,
/// `.codegraph`, `.venv`, ...) and known build dir — exactly the set the
/// indexer itself never walks (`is_relevant_path` rejects any dot-prefixed
/// component), so the copy matches what the daemon sees. Two entries make
/// this non-optional rather than cosmetic: `.calm/index.db` is >170 MB, and
/// `.codegraph/daemon.sock` is a unix socket `std::fs::copy` errors on
/// (ENXIO). Only REGULAR files are copied — symlinks (some dangle, e.g.
/// under `benchmarks/.venv`) and special files are skipped, since a
/// `DirEntry::file_type` reports them as neither dir nor file.
fn copy_repo_filtered(src: &Path, dst: &Path) {
    const SKIP_DIRS: &[&str] = &[
        "target",
        "node_modules",
        "dist",
        "build",
        "__pycache__",
        "venv",
    ];
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ft = entry.file_type().unwrap();
        let name = entry.file_name();
        let name_str = name.to_str().unwrap_or("");
        if ft.is_dir() {
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str) {
                continue;
            }
            copy_repo_filtered(&entry.path(), &dst.join(&name));
        } else if ft.is_file() {
            std::fs::copy(entry.path(), dst.join(&name)).unwrap();
        }
    }
}

/// T6.1 (plan §4): the golden round the whole gate hinges on — run the
/// incremental path against a full rebuild on a copy of the ENTIRE real CALM
/// repo, not just the small fixture. The real repo is what exercises scale:
/// ~3k symbols and ~14k call sites form the "unchanged backdrop" while a
/// couple of seed files drive the delta, so `build_resolution_context`'s
/// global `by_name` map, the `names_delta` expansion query, and the
/// cross-language resolver all run at production size. Seed names are
/// deliberately UNIQUE (no collision with a hot repo name like `new`, whose
/// ~740 call sites would push the delta past the 50-file fallback and defeat
/// the Incremental assertion — plan Abductive-2 / Finding B). `#[ignore]`
/// because it indexes the real tree several times over (seconds each); run:
/// `cargo test -p calm-core --test golden_graph_equivalence -- --ignored`.
#[test]
#[ignore = "heavy: copies + indexes the whole real CALM repo several times; Phase B T6 gate, run with --ignored"]
fn golden_equivalence_incremental_vs_fresh_on_real_calm_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("calm");
    copy_repo_filtered(&real_repo_root(), &root);
    enable_incremental_graph(&root);

    // Seed files under a real crate's src/ so they index as Rust against the
    // full real symbol table. Two cross-calling functions with repo-unique
    // names: the delta touches a genuine intra-seed edge plus (after the
    // rename round) an untouched caller in the victim file.
    let seed_dir = root.join("crates/calm-core/src");
    let seed = seed_dir.join("phase_b_t6_seed.rs");
    std::fs::write(
        &seed,
        "pub fn phase_b_t6_alpha() -> String {\n    phase_b_t6_helper()\n}\npub fn phase_b_t6_helper() -> String {\n    String::from(\"v1\")\n}\n",
    )
    .unwrap();
    let victim = seed_dir.join("phase_b_t6_victim.rs");
    std::fs::write(
        &victim,
        "pub fn phase_b_t6_victim() -> String {\n    phase_b_t6_helper()\n}\n",
    )
    .unwrap();

    let mut db_a = index_fresh(&root);
    // Determinism at real scale (A-4): a from-scratch index of the identical
    // tree must fingerprint-match, or there is a pre-existing nondeterminism
    // to fix before trusting any incremental comparison.
    assert_graph_equal(
        &db_a,
        &index_fresh(&root),
        "real-repo baseline: fresh vs fresh",
    );

    let reindex = |db: &mut Connection, rel: &str| -> GraphMode {
        let summary = pipeline::reindex_paths(db, &root, &[rel.to_string()]).unwrap();
        assert!(!summary.is_noop(), "expected a real change for {rel}");
        summary.graph_mode
    };

    // Round 1 — body-only edit (name unchanged).
    std::fs::write(
        &seed,
        "pub fn phase_b_t6_alpha() -> String {\n    phase_b_t6_helper()\n}\npub fn phase_b_t6_helper() -> String {\n    String::from(\"v2\")\n}\n",
    )
    .unwrap();
    assert_eq!(
        reindex(&mut db_a, "crates/calm-core/src/phase_b_t6_seed.rs"),
        GraphMode::Incremental,
        "body edit of a repo-unique-named file must run incremental, not fall back"
    );
    assert_graph_equal(&db_a, &index_fresh(&root), "real-repo round=1 op=BodyEdit");

    // Round 2 — rename the helper; the untouched victim file's call to the
    // old name must re-resolve via the names_delta fan-out at real scale.
    std::fs::write(
        &seed,
        "pub fn phase_b_t6_alpha() -> String {\n    phase_b_t6_helper2()\n}\npub fn phase_b_t6_helper2() -> String {\n    String::from(\"v2\")\n}\n",
    )
    .unwrap();
    reindex(&mut db_a, "crates/calm-core/src/phase_b_t6_seed.rs");
    assert_graph_equal(&db_a, &index_fresh(&root), "real-repo round=2 op=Rename");

    // Round 3 — delete the victim file entirely.
    std::fs::remove_file(&victim).unwrap();
    reindex(&mut db_a, "crates/calm-core/src/phase_b_t6_victim.rs");
    assert_graph_equal(
        &db_a,
        &index_fresh(&root),
        "real-repo round=3 op=DeleteFile",
    );
}

/// A-1 (plan Risk/Assumptions): the `names_delta` expansion query chunks its
/// IN-list at `DELTA_QUERY_CHUNK_SIZE` (500) to stay under SQLite's variable
/// ceiling. No fixture or real file has >500 symbols, so that boundary never
/// fires naturally — this forces it with one generated file of 600 functions,
/// then edits it so all 600 names enter `names_delta` at once. Proves the
/// multi-chunk path both succeeds and produces a graph identical to a full
/// rebuild.
#[test]
fn names_delta_over_chunk_size_matches_full() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(&root).unwrap();
    enable_incremental_graph(&root);

    // 600 > DELTA_QUERY_CHUNK_SIZE (500): one file, 600 distinct names, all
    // of which land in names_delta on the body edit below.
    let mut big = String::new();
    for i in 0..600 {
        big.push_str(&format!("def chunk_fn_{i}():\n    return {i}\n\n\n"));
    }
    std::fs::write(root.join("big.py"), &big).unwrap();
    // A caller in another file so there's a cross-file edge to check.
    std::fs::write(
        root.join("caller.py"),
        "def caller():\n    return chunk_fn_0() + chunk_fn_599()\n",
    )
    .unwrap();

    let mut db = index_fresh(&root);

    // Change every body (names unchanged) — names_delta = all 600 names of
    // big.py (D2 union), forcing ≥2 chunks through the IN-list loop.
    let mut big2 = String::new();
    for i in 0..600 {
        big2.push_str(&format!("def chunk_fn_{i}():\n    return {}\n\n\n", i + 1));
    }
    std::fs::write(root.join("big.py"), &big2).unwrap();
    let summary = pipeline::reindex_paths(&mut db, &root, &["big.py".to_string()]).unwrap();
    assert!(!summary.is_noop());
    assert_eq!(
        summary.graph_mode,
        GraphMode::Incremental,
        "2 changed files (big.py + fan-out to caller.py) is well under the 50-file fallback"
    );

    assert_graph_equal(
        &db,
        &index_fresh(&root),
        "names_delta chunk-boundary: incremental must match full rebuild",
    );
}
