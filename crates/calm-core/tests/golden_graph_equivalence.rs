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

fn run_rounds_for_seed(seed: u64) {
    let mut rng = XorShift64::new(seed);
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    copy_dir_recursive(&fixture_root(), &root);

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
        let db_b = index_fresh(&root);
        assert_graph_equal(
            &db_a,
            &db_b,
            &format!("seed={seed} round={} op={mutation:?}", round + 1),
        );
    }
}

/// 3 seeds × (1 seed-file round + 5 mutation rounds), every round comparing
/// the continued DB against a from-scratch index — 18 full comparisons. The
/// "Done when" bar of 3 consecutive green runs maps onto the 3 seeds here
/// plus CI re-runs; a failure message always carries (seed, round, op) for
/// exact reproduction.
#[test]
fn golden_equivalence_continued_vs_fresh_across_mutation_rounds() {
    for seed in [1u64, 2, 3] {
        run_rounds_for_seed(seed);
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
