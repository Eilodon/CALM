//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 6): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). Graph (re)construction: full rebuild, the public
//! rebuild-from-already-indexed-rows entry point, the incremental delta
//! path, and the shared caller_count refresh. Move-only -- no logic changed,
//! only relocated.
//!
//! `ResolutionCtx`/`SymbolCandidate`/`CallSiteRow`/`ResolutionMaps` stay
//! defined in `pipeline.rs` (not moved) -- pulled in via `super::` the same
//! as in slices 3-5. `rebuild_graph`/`incremental_graph_update`/
//! `IncrementalOutcome` are `pub(super)` since their only remaining callers
//! (`reindex_all_cancellable_with_phase`/`reindex_changed_cancellable`/
//! `reindex_paths`) are still in pipeline.rs (Wave 1 slice 7, not yet
//! extracted) -- verified via `callers()` before this move.
//! `rebuild_graph_from_index`/`refresh_caller_counts` stay `pub` and are
//! re-exported by `pipeline.rs` at their unchanged `crate::indexer::
//! pipeline::X` paths -- both have real external callers (verified via
//! `callers()`: `rebuild_graph_from_index` from calm-server's `lib.rs` and
//! `indexer/refresh.rs`; `refresh_caller_counts` from `lsp/overlay.rs`,
//! `scip/mod.rs` x2, calm-server's `scip_overlay.rs`, calm-cli's `main.rs`
//! x2 -- nine external call sites total).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use crate::indexer::edges::insert_call_edges_batch;

use super::{
    CallSiteRow, DELTA_QUERY_CHUNK_SIZE, GraphMode, MAX_INCREMENTAL_DELTA_PATHS, ResolutionMaps,
    build_resolution_context, cached_resolution_maps, insert_ambiguity_groups_batch,
    invalidate_resolution_maps_cache, resolve_import_targets, resolve_sites_to_edges,
};

pub(super) fn rebuild_graph(
    tx: &rusqlite::Transaction,
    project_root: &std::path::Path,
    churn_since: &str,
    hub_config: &crate::config::HubThresholdConfig,
    maps: &ResolutionMaps,
    ignore: &[String],
) -> rusqlite::Result<()> {
    // WS4 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D4):
    // moved ahead of `build_resolution_context` -- `build_inheritance_closure`
    // (called from inside it) reads `type_relations.to_symbol`/`confidence`,
    // which this call is what actually resolves for the CURRENT pass. It was
    // previously called much later in this same function (after
    // `resolve_sites_to_edges` already ran), which would have made every
    // rebuild's inheritance closure exactly one pass stale. Verified safe to
    // move: `resolve_cross_file_type_relations` only reads `symbols` (fully
    // populated before `rebuild_graph` is ever called) and `type_relations`
    // itself (populated per-file by `extract_file_data`, also already done)
    // -- it has no dependency on `ctx`/`sites`/`call_edges` at all, so moving
    // WHEN it runs changes no other pass's behavior.
    crate::graph::type_resolve::resolve_cross_file_type_relations(tx)?;

    let ctx = build_resolution_context(tx, &maps.namespace_map)?;

    // Stable, explicit order (Phase B plan T2/A-3) so full and future
    // incremental resolution attribute `seen_pairs` dedup identically in
    // `resolve_sites_to_edges` — see that function's doc comment.
    let sites: Vec<CallSiteRow> = {
        let mut stmt = tx.prepare(
            "SELECT id, from_path, enclosing_qn, callee_name, call_line, callee_start_byte, \
                    callee_end_byte, identity_version, confidence, receiver, target_class, \
                    looks_option_or_result_chained, module_hint, edge_kind, arg_count, \
                    import_path, target_type_kind, target_type_qn \
             FROM call_sites ORDER BY id",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, i64>(11)? != 0,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
                r.get::<_, Option<String>>(17)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let (edges, ambiguity_groups) = resolve_sites_to_edges(&ctx, &sites);

    tx.execute("DELETE FROM call_edges", [])?;
    insert_call_edges_batch(tx, &edges)?;
    tx.execute("DELETE FROM ambiguity_groups", [])?;
    insert_ambiguity_groups_batch(tx, &ambiguity_groups)?;
    // Every `formal` row at this point came from the stack-graphs upgrade
    // already baked into `confidence` at extraction time, carried through
    // unchanged by `resolve_sites_to_edges`'s confidence-assignment loop —
    // the SCIP overlay (`scip::ingest::ingest_occurrences`) is a separate,
    // later UPDATE pass that runs after this one and sets `formal_source =
    // 'scip'` itself. Cheaper than threading a new field through
    // `CallSiteData`/`CallEdge`/`insert_call_edges_batch` for what's
    // otherwise a one-shot fact true immediately after every fresh rebuild.
    tx.execute(
        "UPDATE call_edges SET formal_source = 'stack_graphs', evidence_state = 'fresh' \
         WHERE edge_confidence = 'formal' AND formal_source IS NULL",
        [],
    )?;
    refresh_caller_counts(tx)?;
    resolve_import_targets(tx, maps)?;
    crate::graph::coreness::compute_coreness(tx)?;
    crate::graph::hub::update_is_hub_flags(tx, hub_config)?;
    crate::graph::boundary::update_boundary_ambiguous_flags(tx)?;
    crate::graph::churn::update_churn_scores(tx, project_root, churn_since)?;
    // WS4: resolve_cross_file_type_relations already ran at the top of this
    // function (before build_resolution_context needed its output) -- see
    // that call's own comment. Nothing between there and here writes to
    // `symbols`/`type_relations`, so re-running it here would be a pure,
    // wasteful no-op, not a second real pass.
    crate::graph::digest::compute_digests(tx)?;
    crate::indexer::package_deps::compute_package_dependencies(tx, project_root, ignore)?;
    Ok(())
}

/// Rebuild graph-derived state from the already indexed source rows.
///
/// Metadata/context inputs can change resolution without changing a source
/// file.  Their refresh must therefore evict cached maps and execute the same
/// graph construction used by a full index, but must not reparse every file.
///
/// This deliberately runs in one writer transaction and increments the graph
/// generation exactly once, so external evidence is coherently marked stale
/// until the next overlay pass refreshes it.
pub fn rebuild_graph_from_index(
    conn: &mut Connection,
    project_root: &Path,
) -> rusqlite::Result<GraphMode> {
    let config = crate::config::load_config_or_warn(project_root);
    invalidate_resolution_maps_cache(project_root);
    let maps = cached_resolution_maps(project_root);
    let tx = conn.transaction()?;
    rebuild_graph(
        &tx,
        project_root,
        &config.hotspots.default_since,
        &config.hub_threshold,
        &maps,
        &config.ignore,
    )?;
    // Runs AFTER rebuild_graph (whose compute_digests already read+stamped
    // the PRE-increment generation onto every symbol_digests row) --
    // verified 2026-08-08 (derived-artifact hardening audit, PR C) that
    // this off-by-one is deliberate, not a bug: see graph/digest.rs's
    // module doc comment ("No generation-fencing staleness check") for why
    // symbol_digests.graph_generation is correct-by-construction on every
    // full recompute regardless of the number stamped on it, and is never
    // compared against graph_generation_state.generation for correctness.
    // Do not "fix" this ordering without re-reading that comment first.
    tx.execute(
        "UPDATE graph_generation_state SET generation = generation + 1 WHERE id = 1",
        [],
    )?;
    tx.commit()?;
    Ok(GraphMode::Full)
}

/// Result of an `incremental_graph_update` pass — lets the caller set
/// `ReindexSummary::graph_mode` precisely (plan T4/L6), since only this
/// function's own delta-expansion (step 1 inside it) can know whether the
/// fallback threshold was hit, and why.
pub(super) enum IncrementalOutcome {
    Applied,
    /// Fell back to a full `rebuild_graph` internally — already done by the
    /// time this returns. Payload is a human-readable reason (delta size),
    /// not matched on.
    FellBackToFull(String),
}

/// Phase B plan §3: scoped call-graph re-resolve for exactly the files a
/// reindex pass touched, instead of `rebuild_graph`'s full sweep. Only ever
/// called when the caller's own `ReindexSummary` is non-noop — `delta_seed`
/// is `ReindexSummary::changed_paths` (changed ∪ deleted rel_paths) and
/// `names_delta` is `ReindexSummary::names_delta` (old_names ∪ new_names
/// union across those same paths, plan D2). Shares `build_resolution_context`/
/// `resolve_sites_to_edges` with `rebuild_graph` verbatim (plan D4) — the
/// ONLY differences from a full rebuild are (a) which `call_sites` get
/// loaded and (b) how much of `call_edges` gets deleted first; see plan
/// §3.1 for the proof that `delta_paths` below is sufficient to catch every
/// input `resolve_sites_to_edges` depends on.
#[allow(clippy::too_many_arguments)]
pub(super) fn incremental_graph_update(
    tx: &rusqlite::Transaction,
    project_root: &std::path::Path,
    churn_since: &str,
    delta_seed: &[String],
    names_delta: &HashSet<String>,
    hub_config: &crate::config::HubThresholdConfig,
    maps: &ResolutionMaps,
    ignore: &[String],
) -> rusqlite::Result<IncrementalOutcome> {
    // Step 1 (plan D1): delta_paths = delta_seed ∪ {from_path of call_sites
    // whose callee_name ∈ names_delta} — a site in an UNCHANGED file that
    // calls a renamed/added/removed name must still be re-resolved, or its
    // stale edge survives alongside a newly-ambiguous one (audit finding
    // L1). Chunked (plan A-1): names_delta can be arbitrarily large (every
    // symbol name in every changed/deleted file this pass), unlike
    // delta_paths itself, which is bounded by the fallback check below.
    let mut delta_paths: HashSet<String> = delta_seed.iter().cloned().collect();
    let names: Vec<&str> = names_delta.iter().map(String::as_str).collect();
    for chunk in names.chunks(DELTA_QUERY_CHUNK_SIZE) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT DISTINCT from_path FROM call_sites WHERE callee_name IN ({placeholders})"
        );
        let mut stmt = tx.prepare(&sql)?;
        let found = stmt
            .query_map(rusqlite::params_from_iter(chunk.iter().copied()), |r| {
                r.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        delta_paths.extend(found);
    }

    // Step 2 (plan D6): full rebuild is always correct; past this size the
    // delta-expansion + scoped-DELETE overhead isn't worth it over just
    // doing the full pass. Falls back INTERNALLY (not by returning an error
    // for the caller to react to) so there is exactly one place that knows
    // this decision, matching D4's one-resolver principle.
    if delta_paths.len() > MAX_INCREMENTAL_DELTA_PATHS {
        let reason = format!(
            "delta_paths.len()={} > {MAX_INCREMENTAL_DELTA_PATHS}",
            delta_paths.len()
        );
        rebuild_graph(tx, project_root, churn_since, hub_config, maps, ignore)?;
        return Ok(IncrementalOutcome::FellBackToFull(reason));
    }

    // From here delta_paths.len() <= MAX_INCREMENTAL_DELTA_PATHS (50), well
    // under any SQLite variable-count ceiling — no chunking needed for the
    // two queries below, unlike step 1's names_delta-driven expansion.
    let path_refs: Vec<&str> = delta_paths.iter().map(String::as_str).collect();
    let placeholders = vec!["?"; path_refs.len()].join(",");

    // Step 3 (plan D1): scoped DELETE — every edge belongs to exactly one
    // from_path, so this partitions cleanly; scip-inserted edges with
    // from_path ∈ delta are correctly deleted too (that file's content just
    // changed, the background overlay will re-insert what's still true).
    tx.execute(
        &format!("DELETE FROM call_edges WHERE from_path IN ({placeholders})"),
        rusqlite::params_from_iter(path_refs.iter().copied()),
    )?;

    // Step 4 (plan D5): dangling sweep, unconditional every pass — catches
    // scip-inserted edges (no call_sites backing) whose target symbol was
    // just deleted, which no from_path-scoped re-resolve above would ever
    // touch.
    tx.execute(
        "DELETE FROM call_edges WHERE to_symbol NOT IN (SELECT qualified_name FROM symbols)",
        [],
    )?;

    // Step 5 (plan D4): same shared resolver full rebuild uses — still one
    // global SELECT over symbols (accepted cost, see build_resolution_context's
    // own doc comment; scoping this too is Phase B+ backlog).
    //
    // WS4: resolve_cross_file_type_relations moved ahead of this call for
    // the same reason as rebuild_graph's identical move (see that
    // function's own comment) -- build_resolution_context's
    // build_inheritance_closure reads type_relations.to_symbol/confidence,
    // which this is what resolves for the current pass. Global, not
    // delta-scoped, matching this function's own existing note above about
    // build_resolution_context itself always doing one full global SELECT
    // regardless of delta size.
    crate::graph::type_resolve::resolve_cross_file_type_relations(tx)?;
    let ctx = build_resolution_context(tx, &maps.namespace_map)?;

    // Step 6: re-resolve exactly delta_paths' sites. `ORDER BY id` matches
    // rebuild_graph's own load (plan A-3/T2) so "first site wins" seen_pairs
    // dedup attribution agrees between the two paths on identical input.
    let sites: Vec<CallSiteRow> = {
        let sql = format!(
            "SELECT id, from_path, enclosing_qn, callee_name, call_line, callee_start_byte, \
                    callee_end_byte, identity_version, confidence, receiver, target_class, \
                    looks_option_or_result_chained, module_hint, edge_kind, arg_count, \
                    import_path, target_type_kind, target_type_qn \
             FROM call_sites WHERE from_path IN ({placeholders}) ORDER BY id"
        );
        let mut stmt = tx.prepare(&sql)?;
        stmt.query_map(rusqlite::params_from_iter(path_refs.iter().copied()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, i64>(11)? != 0,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
                r.get::<_, Option<String>>(17)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let (edges, ambiguity_groups) = resolve_sites_to_edges(&ctx, &sites);
    insert_call_edges_batch(tx, &edges)?;
    tx.execute(
        &format!("DELETE FROM ambiguity_groups WHERE from_path IN ({placeholders})"),
        rusqlite::params_from_iter(path_refs.iter().copied()),
    )?;
    insert_ambiguity_groups_batch(tx, &ambiguity_groups)?;
    // Same one-shot fact as rebuild_graph's identical UPDATE (see its own
    // comment): a `formal` row here came from the stack-graphs upgrade
    // already baked into `confidence` at extraction time. Global, not
    // scoped to delta — matches plan §3 step 6, correct because rows
    // outside delta already carry their own formal_source from a prior
    // pass.
    tx.execute(
        "UPDATE call_edges SET formal_source = 'stack_graphs', evidence_state = 'fresh' \
         WHERE edge_confidence = 'formal' AND formal_source IS NULL",
        [],
    )?;

    // Step 7 (plan D3): identical 5 global metric passes, same order as
    // rebuild_graph — equivalence-by-construction holds for all of them
    // once the edge set matches, since every one is a pure function of
    // current DB state, not of how the edges got there.
    refresh_caller_counts(tx)?;
    resolve_import_targets(tx, maps)?;
    crate::graph::coreness::compute_coreness(tx)?;
    crate::graph::hub::update_is_hub_flags(tx, hub_config)?;
    crate::graph::boundary::update_boundary_ambiguous_flags(tx)?;
    crate::graph::churn::update_churn_scores(tx, project_root, churn_since)?;
    // WS4: resolve_cross_file_type_relations already ran above, before
    // build_resolution_context needed its output -- see that call's own
    // comment (mirrors rebuild_graph's identical move).
    crate::graph::digest::compute_digests(tx)?;
    crate::indexer::package_deps::compute_package_dependencies(tx, project_root, ignore)?;

    Ok(IncrementalOutcome::Applied)
}

/// Recompute every symbol's `caller_count` from `call_edges`, using the same
/// "confirmed caller" definition as the `callers` tool's `direct_count`
/// (`ruled_out_by_scip = 0` and not `ambiguous`-confidence): an `ambiguous`
/// edge is index-time fan-out to every same-named candidate when a call's
/// receiver type couldn't be resolved (e.g. `x.as_str()` fanning out to every
/// `as_str` method in the repo), not a confirmed caller of any one of them.
/// Counting it here — the previous behavior — inflated `caller_count` nearly
/// identically across every same-named symbol regardless of real usage,
/// corrupting the hub/coreness ranking and `dead_code_confidence` (which
/// short-circuits to "not dead" on `caller_count > 0`) built on top of it.
///
/// Called both from [`rebuild_graph`] (after every full/incremental index)
/// and, separately, after the SCIP overlay pass (`scip::run_overlay`) flips
/// `ruled_out_by_scip`/`edge_confidence` on existing edges — that pass runs
/// after this function's other caller, so without a second refresh
/// afterward, `caller_count` would immediately go stale again relative to
/// the very columns this filter depends on.
pub fn refresh_caller_counts(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE symbols SET caller_count = \
            (SELECT COUNT(DISTINCT from_symbol) FROM call_edges \
             WHERE to_symbol = symbols.qualified_name \
               AND ruled_out_by_scip = 0 \
               AND edge_confidence != 'ambiguous')",
        [],
    )?;
    Ok(())
}
