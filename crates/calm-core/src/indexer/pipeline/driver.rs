//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 7): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). The pipeline driver: full-reindex entry points
//! (`run_indexing_pipeline`/`_cancellable`, `reindex_all_cancellable`/
//! `_with_phase`), incremental reindex (`reindex_changed`/`_cancellable`),
//! and exact-path reindex (`reindex_paths`), plus their private
//! `remove_file_rows`/`names_for_path` row-deletion helpers. Move-only -- no
//! logic changed, only relocated.
//!
//! `ReindexSummary`/`GraphMode`/`ExtractedFile`/`ExtractedBatchRow`/
//! `CallSiteData`/`PARSE_BATCH_SIZE` stay defined in `pipeline.rs` (not
//! moved) -- pulled in via `super::` the same as in slices 3-6.
//! `PipelineOutcome`/`ReindexOutcome` move here as `pub enum`s (together
//! with their attached `#[derive(...)]`/doc-comment block -- verified
//! byte-exact against disk immediately before this move; the line range
//! recorded in this slice's handoff doc started one attribute short, which
//! would have silently dropped `PipelineOutcome`'s derives) and are
//! re-exported by `pipeline.rs` at their unchanged `crate::indexer::
//! pipeline::X` paths -- both have real external callers (verified via
//! `callers()`: `PipelineOutcome` from `calm-core/src/indexer/refresh.rs`
//! and `calm-server/src/lib.rs::bootstrap`; `ReindexOutcome` from the same
//! two files plus `calm-core/tests/golden_graph_equivalence.rs`).
//!
//! `cached_formal_resolver`/`cached_resolution_maps`/
//! `invalidate_resolution_maps_cache`/`is_manifest_path`/
//! `needs_call_site_identity_baseline`/`rebuild_call_site_identity_baseline`
//! are still plain private items in `pipeline.rs` (Wave 1 slices 8/9, not
//! yet extracted) -- pulled in via `super::` for now, same pattern as
//! `rebuild_graph`/`incremental_graph_update` in slice 6's `graph.rs`.

use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{
    ExtractedBatchRow, ExtractedFile, GraphMode, IncrementalOutcome, PARSE_BATCH_SIZE,
    ReindexSummary, cached_formal_resolver, cached_resolution_maps, collect_source_files,
    extract_file_data, hash_content, incremental_graph_update, invalidate_resolution_maps_cache,
    is_manifest_path, mtime_secs, needs_call_site_identity_baseline, now_secs, persist_file,
    read_source_capped, rebuild_call_site_identity_baseline, rebuild_graph, rel_path,
    upsert_file_index,
};
use crate::indexer::lang_constants::{is_recognized_unparsed_extension, language_for_extension};

/// Drop all rows belonging to a single file (symbols, call sites, file_index).
/// Call edges are rebuilt globally by [`rebuild_graph`], so they are not touched here.
fn remove_file_rows(tx: &rusqlite::Transaction, rel: &str) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM symbols WHERE path = ?1", [rel])?;
    tx.execute("DELETE FROM call_sites WHERE from_path = ?1", [rel])?;
    tx.execute("DELETE FROM import_edges WHERE from_path = ?1", [rel])?;
    tx.execute("DELETE FROM file_index WHERE path = ?1", [rel])?;
    tx.execute("DELETE FROM code_chunks WHERE path = ?1", [rel])?;
    tx.execute("DELETE FROM type_relations WHERE source_path = ?1", [rel])?;
    tx.execute("DELETE FROM symbol_effects WHERE source_path = ?1", [rel])?;
    Ok(())
}

/// Bare `name`s currently persisted for `path`, read BEFORE
/// `remove_file_rows` clears them — the `old_names` half of Phase B plan
/// D2's `names_delta = old_names ∪ new_names` union; the `new_names` half
/// comes straight from a freshly parsed `ExtractedFile.symbols`, no second
/// SELECT needed there.
fn names_for_path(tx: &rusqlite::Transaction, path: &str) -> rusqlite::Result<HashSet<String>> {
    let mut stmt = tx.prepare("SELECT DISTINCT name FROM symbols WHERE path = ?1")?;
    stmt.query_map([path], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()
}

/// Full (re)index of a project tree into `conn`.
///
/// Scan → extract symbols + call sites (tree-sitter) → rebuild graph
/// (caller_count, coreness, is_hub). Everything is one transaction so the graph
/// is never observed half-built.
/// Outcome of a cancellable pipeline run — distinguishes "finished" from
/// "bailed early because `cancel` returned true", so a caller on a shutdown
/// path can log/handle the two differently (a cancellation is not a
/// failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineOutcome {
    Completed,
    Cancelled,
}

/// Full (re)index of a project tree into `conn`.
///
/// Scan → extract symbols + call sites (tree-sitter) → rebuild graph
/// (caller_count, coreness, is_hub). Everything is one transaction so the graph
/// is never observed half-built.
pub fn run_indexing_pipeline(
    conn: &mut Connection,
    project_root: &Path,
    phase: std::sync::Arc<std::sync::RwLock<crate::types::IndexingPhase>>,
) -> rusqlite::Result<()> {
    run_indexing_pipeline_cancellable(conn, project_root, phase, &|| false).map(|_| ())
}

/// Same as `run_indexing_pipeline`, but checked against `cancel` between
/// parse batches — a full index of a large repo can take many seconds, and
/// without this a shutdown-triggered `CancellationToken` has nothing to stop
/// the in-flight `spawn_blocking` task it runs in, so the process can't exit
/// until the whole scan finishes (Tokio's runtime shutdown blocks on
/// outstanding blocking-pool tasks — see `serve_stdio_with_preset`'s SIGTERM
/// handler comment). Bailing mid-loop drops `tx` without committing — SQLite
/// rolls it back automatically, so a cancelled run leaves the graph exactly
/// as it was before this call, the same "never half-built" guarantee a
/// completed run has.
pub fn run_indexing_pipeline_cancellable(
    conn: &mut Connection,
    project_root: &Path,
    phase: std::sync::Arc<std::sync::RwLock<crate::types::IndexingPhase>>,
    cancel: &dyn Fn() -> bool,
) -> rusqlite::Result<PipelineOutcome> {
    reindex_all_cancellable_with_phase(conn, project_root, Some(&phase), cancel)
}

/// Rebuild every indexed source row and all derived graph state atomically.
///
/// This is the semantic counterpart to a new index database: configuration
/// inputs can change extraction itself (`entry_points`, ignores, language
/// policy), so a hash-only delta scan is insufficient after the watcher loses
/// provenance for a change. Unlike [`run_indexing_pipeline_cancellable`], it
/// deliberately does not publish a runtime indexing phase; callers such as a
/// watcher own their lifecycle independently.
pub fn reindex_all_cancellable(
    conn: &mut Connection,
    project_root: &Path,
    cancel: &dyn Fn() -> bool,
) -> rusqlite::Result<PipelineOutcome> {
    reindex_all_cancellable_with_phase(conn, project_root, None, cancel)
}

fn reindex_all_cancellable_with_phase(
    conn: &mut Connection,
    project_root: &Path,
    phase: Option<&std::sync::Arc<std::sync::RwLock<crate::types::IndexingPhase>>>,
    cancel: &dyn Fn() -> bool,
) -> rusqlite::Result<PipelineOutcome> {
    use crate::types::IndexingPhase;

    let set_phase = |next: IndexingPhase| {
        if let Some(phase) = phase {
            *phase.write().unwrap() = next;
        }
    };

    let config = crate::config::load_config_or_warn(project_root);
    let entry_point_patterns = config.entry_points;
    let ignore_patterns = config.ignore;

    // Initialize FormalResolver once per pipeline run; load rules for all supported
    // languages. Non-fatal if a language fails to load — that language falls back to
    // ConservativeResolver only.
    let formal = cached_formal_resolver();

    let mut files = Vec::new();
    collect_source_files(project_root, &ignore_patterns, &mut files);
    files.sort();

    if cancel() {
        return Ok(PipelineOutcome::Cancelled);
    }

    set_phase(IndexingPhase::Parsing);

    // Parse + resolve + persist in bounded batches: each batch is extracted in
    // parallel (pure CPU, no DB access) and persisted sequentially before the
    // next batch starts, so peak memory holds at most one batch of parsed
    // files instead of the whole project. `.map()` over an indexed parallel
    // iterator preserves order within a batch, and batches are processed in
    // the same sorted `files` order, so the result is byte-for-byte identical
    // to a fully sequential pipeline.
    let now = now_secs();
    let tx = conn.transaction()?;

    // Full reindex: clear everything. (Triggers keep the FTS tables in sync.)
    tx.execute("DELETE FROM call_sites", [])?;
    tx.execute("DELETE FROM import_edges", [])?;
    tx.execute("DELETE FROM symbols", [])?;
    tx.execute("DELETE FROM file_index", [])?;
    tx.execute("DELETE FROM code_chunks", [])?;
    // Bug fix 2026-08-08: these two were missing from the full-reindex clear
    // even though `remove_file_rows` (the per-file incremental path) already
    // clears them. Both tables key off `qualified_name`/`source_path`, not
    // `symbols.id` (see their schema.rs comments), so a stale row here is not
    // just orphaned garbage -- it silently re-attaches to whatever symbol the
    // NEXT full reindex assigns the same qualified name, corrupting T1 facts
    // and the Architecture Digest built from them for a symbol whose actual
    // semantics changed. See golden_graph_equivalence.rs for the regression
    // test locking this in (full-rebuild-on-old-DB must equal fresh-build).
    tx.execute("DELETE FROM type_relations", [])?;
    tx.execute("DELETE FROM symbol_effects", [])?;
    // A full baseline invalidates all cached SCIP results. In particular, D4's
    // byte-span identity migration must never let a line-derived cache key skip
    // the first exact overlay pass after rebuilding the graph.
    tx.execute("DELETE FROM scip_overlay_state", [])?;

    for batch in files.chunks(PARSE_BATCH_SIZE) {
        if cancel() {
            return Ok(PipelineOutcome::Cancelled);
        }
        // `lang: None` + `data: None` means a recognized-unparsed-extension
        // file (see `is_recognized_unparsed_extension`) — still earns a
        // `file_index` row below (path/hash/mtime, `language` NULL,
        // `symbol_count` 0), just with nothing to extract or persist.
        let extracted: Vec<ExtractedBatchRow> = batch
            .par_iter()
            .map(|file| {
                let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
                let lang = language_for_extension(ext);
                if lang.is_none() && !is_recognized_unparsed_extension(ext) {
                    return None;
                }
                let source = read_source_capped(file)?;
                let rel = rel_path(project_root, file);
                let hash = hash_content(&source);
                let mtime = mtime_secs(file);
                let data = lang.map(|lang| {
                    extract_file_data(&rel, lang, &source, &entry_point_patterns, formal)
                });
                Some((rel, lang, hash, mtime, data))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flatten()
            .collect();

        for (rel, lang, hash, mtime, data) in &extracted {
            if let Some(data) = data {
                persist_file(&tx, rel, hash, data)?;
            }
            upsert_file_index(
                &tx,
                rel,
                *lang,
                hash,
                *mtime,
                data.as_ref().map(|d| d.symbol_count).unwrap_or(0),
                now,
            )?;
        }
    }

    set_phase(IndexingPhase::BuildingEdges);

    let maps = cached_resolution_maps(project_root);
    rebuild_graph(
        &tx,
        project_root,
        &config.hotspots.default_since,
        &config.hub_threshold,
        &maps,
        &ignore_patterns,
    )?;
    // Deliberate off-by-one vs. symbol_digests.graph_generation -- see
    // rebuild_graph_from_index's identical UPDATE for the full rationale.
    tx.execute(
        "UPDATE graph_generation_state SET generation = generation + 1 WHERE id = 1",
        [],
    )?;
    tx.commit()?;

    set_phase(IndexingPhase::Ready);

    Ok(PipelineOutcome::Completed)
}
/// Incremental reindex: re-parse only files whose content hash changed (or are
/// new), drop rows for deleted files, then rebuild the graph once if anything
/// changed. Cheap to call repeatedly — the basis for the file watcher.
/// Outcome of a cancellable `reindex_changed` run — mirrors `PipelineOutcome`,
/// carrying the summary through on the completed path.
#[derive(Debug)]
pub enum ReindexOutcome {
    Completed(ReindexSummary),
    Cancelled,
}

/// Incremental reindex: re-parse only files whose content hash changed (or are
/// new), drop rows for deleted files, then rebuild the graph once if anything
/// changed. Cheap to call repeatedly — the basis for the file watcher.
pub fn reindex_changed(
    conn: &mut Connection,
    project_root: &Path,
) -> rusqlite::Result<ReindexSummary> {
    match reindex_changed_cancellable(conn, project_root, &|| false)? {
        ReindexOutcome::Completed(summary) => Ok(summary),
        ReindexOutcome::Cancelled => {
            unreachable!("cancel closure always returns false")
        }
    }
}

pub fn reindex_changed_cancellable(
    conn: &mut Connection,
    project_root: &Path,
    cancel: &dyn Fn() -> bool,
) -> rusqlite::Result<ReindexOutcome> {
    if needs_call_site_identity_baseline(conn)? {
        return rebuild_call_site_identity_baseline(conn, project_root, cancel);
    }

    let config = crate::config::load_config_or_warn(project_root);
    let entry_point_patterns = config.entry_points;
    let ignore_patterns = config.ignore;

    let formal = cached_formal_resolver();

    let existing: HashMap<String, String> = {
        let mut stmt = conn.prepare("SELECT path, hash FROM file_index")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    };

    let mut files = Vec::new();
    collect_source_files(project_root, &ignore_patterns, &mut files);
    files.sort();

    if cancel() {
        return Ok(ReindexOutcome::Cancelled);
    }

    // Read + hash every file in parallel, then decide sequentially which ones
    // actually changed before paying the parse+resolve cost on just those.
    struct Candidate {
        rel: String,
        // `None` for a recognized-unparsed-extension file (see
        // `is_recognized_unparsed_extension`) — included here (not filtered
        // out like a genuinely unrecognized extension) so its `file_index`
        // row stays in `seen_paths` below and doesn't get mistaken for a
        // deleted file on every incremental pass.
        lang: Option<&'static str>,
        source: String,
        hash: String,
        mtime: f64,
    }
    let candidates: Vec<Candidate> = files
        .par_iter()
        .map(|file| {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = language_for_extension(ext);
            if lang.is_none() && !is_recognized_unparsed_extension(ext) {
                return None;
            }
            let source = read_source_capped(file)?;
            let rel = rel_path(project_root, file);
            let hash = hash_content(&source);
            Some(Candidate {
                rel,
                lang,
                source,
                hash,
                mtime: mtime_secs(file),
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .flatten()
        .collect();

    let seen_paths: HashSet<String> = candidates.iter().map(|c| c.rel.clone()).collect();
    let changed: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| existing.get(&c.rel) != Some(&c.hash)) // unchanged — skip the parse
        .collect();

    // Parse + resolve + persist in bounded batches (see run_indexing_pipeline
    // for why: caps peak memory to one batch instead of every changed file).
    let now = now_secs();
    let tx = conn.transaction()?;
    let mut summary = ReindexSummary::default();

    for batch in changed.chunks(PARSE_BATCH_SIZE) {
        if cancel() {
            return Ok(ReindexOutcome::Cancelled);
        }
        let extracted: Vec<(&Candidate, Option<ExtractedFile>)> = batch
            .par_iter()
            .map(|c| {
                let data = c.lang.map(|lang| {
                    extract_file_data(&c.rel, lang, &c.source, &entry_point_patterns, formal)
                });
                (c, data)
            })
            .collect();

        for (c, data) in &extracted {
            summary.names_delta.extend(names_for_path(&tx, &c.rel)?);
            remove_file_rows(&tx, &c.rel)?;
            if let Some(data) = data {
                summary
                    .names_delta
                    .extend(data.symbols.iter().map(|s| s.name.clone()));
                persist_file(&tx, &c.rel, &c.hash, data)?;
            }
            upsert_file_index(
                &tx,
                &c.rel,
                c.lang,
                &c.hash,
                c.mtime,
                data.as_ref().map(|d| d.symbol_count).unwrap_or(0),
                now,
            )?;
            summary.changed += 1;
            summary.changed_paths.push(c.rel.clone());
        }
    }

    for path in existing.keys() {
        if !seen_paths.contains(path) {
            summary.names_delta.extend(names_for_path(&tx, path)?);
            remove_file_rows(&tx, path)?;
            summary.deleted += 1;
            summary.changed_paths.push(path.clone());
        }
    }

    if !summary.is_noop() {
        debug_assert!(
            !summary.changed_paths.is_empty(),
            "non-noop summary must have at least one changed_paths entry (Phase B plan T4 Failure Mode 3 guard)"
        );
        // Phase B plan T4b — see invalidate_resolution_maps_cache's doc
        // comment for why this can't just rely on cached_resolution_maps'
        // own mtime comparison.
        if summary.changed_paths.iter().any(|p| is_manifest_path(p)) {
            invalidate_resolution_maps_cache(project_root);
        }
        let maps = cached_resolution_maps(project_root);
        if config.indexing.incremental_graph {
            match incremental_graph_update(
                &tx,
                project_root,
                &config.hotspots.default_since,
                &summary.changed_paths,
                &summary.names_delta,
                &config.hub_threshold,
                &maps,
                &ignore_patterns,
            )? {
                IncrementalOutcome::Applied => summary.graph_mode = GraphMode::Incremental,
                IncrementalOutcome::FellBackToFull(reason) => {
                    summary.graph_mode = GraphMode::FullFallback(reason)
                }
            }
        } else {
            rebuild_graph(
                &tx,
                project_root,
                &config.hotspots.default_since,
                &config.hub_threshold,
                &maps,
                &ignore_patterns,
            )?;
            summary.graph_mode = GraphMode::Full;
        }
        // Deliberate off-by-one vs. symbol_digests.graph_generation -- see
        // rebuild_graph_from_index's identical UPDATE for the full rationale.
        tx.execute(
            "UPDATE graph_generation_state SET generation = generation + 1 WHERE id = 1",
            [],
        )?;
    }
    tx.commit()?;
    Ok(ReindexOutcome::Completed(summary))
}

/// Reindex exactly the given `rel_paths` — no repo walk, no full-repo hash
/// pass (unlike `reindex_changed`/`reindex_changed_cancellable`, which
/// `collect_source_files` + re-read + re-hash *every* file to discover what
/// changed even when the caller already knows precisely which file it just
/// wrote). Used by the edit tool (`tools/edit.rs`), which knows the exact
/// path from its own write. The `ChangeSet`/`WatchSupervisor` path now uses
/// this same exact-path fast path for safe source events; loss of observation
/// (`notify` rescan/error, unsafe rename, or configuration drift) deliberately
/// routes to [`reindex_all_cancellable`] so the fallback is equivalent to a
/// fresh index rather than a hash-only approximation.
///
/// A path no longer present on disk is treated as a deletion. A path whose
/// content hash is unchanged from `file_index` is skipped entirely — no
/// parse, no graph touch. When anything actually changed it updates the
/// call graph via `incremental_graph_update` (scoped re-resolve) when
/// `indexing.incremental_graph` is set, else the full `rebuild_graph` sweep
/// (Phase B T4) — this dirty-path entry's own win is skipping the O(repo
/// size) walk+hash every edit, independent of which graph path then runs.
pub fn reindex_paths(
    conn: &mut Connection,
    project_root: &Path,
    rel_paths: &[String],
) -> rusqlite::Result<ReindexSummary> {
    use rusqlite::OptionalExtension;

    if needs_call_site_identity_baseline(conn)? {
        let never_cancel = || false;
        return match rebuild_call_site_identity_baseline(conn, project_root, &never_cancel)? {
            ReindexOutcome::Completed(summary) => Ok(summary),
            ReindexOutcome::Cancelled => {
                unreachable!("the direct reindex path cannot be cancelled")
            }
        };
    }

    let config = crate::config::load_config_or_warn(project_root);

    let formal = cached_formal_resolver();

    let now = now_secs();
    let tx = conn.transaction()?;
    let mut summary = ReindexSummary::default();

    for rel in rel_paths {
        let abs = project_root.join(rel);
        let existing_hash: Option<String> = tx
            .query_row(
                "SELECT hash FROM file_index WHERE path = ?1",
                [rel.as_str()],
                |r| r.get(0),
            )
            .optional()?;

        if !abs.exists() {
            if existing_hash.is_some() {
                summary.names_delta.extend(names_for_path(&tx, rel)?);
                remove_file_rows(&tx, rel)?;
                summary.deleted += 1;
                summary.changed_paths.push(rel.clone());
            }
            continue;
        }

        let ext = abs.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = language_for_extension(ext);
        if lang.is_none() && !is_recognized_unparsed_extension(ext) {
            // Not a recognized file type — nothing to index. If a stale
            // row somehow exists for it (e.g. extension handling changed
            // between versions), leave it for a full reindex to reconcile
            // rather than guessing here.
            continue;
        }

        let Some(source) = read_source_capped(&abs) else {
            // Unreadable, or over MAX_INDEXABLE_FILE_BYTES (permissions,
            // binary content, an oversized file, or a TOCTOU delete
            // between the exists() check above and this read) — skip
            // rather than guess; a subsequent full/watcher reindex will
            // pick it up once it's readable (or gone) again.
            continue;
        };
        let hash = hash_content(&source);
        if existing_hash.as_deref() == Some(hash.as_str()) {
            continue; // content unchanged — skip parse entirely
        }

        let data =
            lang.map(|lang| extract_file_data(rel, lang, &source, &config.entry_points, formal));
        summary.names_delta.extend(names_for_path(&tx, rel)?);
        remove_file_rows(&tx, rel)?;
        if let Some(data) = &data {
            summary
                .names_delta
                .extend(data.symbols.iter().map(|s| s.name.clone()));
            persist_file(&tx, rel, &hash, data)?;
        }
        upsert_file_index(
            &tx,
            rel,
            lang,
            &hash,
            mtime_secs(&abs),
            data.as_ref().map(|d| d.symbol_count).unwrap_or(0),
            now,
        )?;
        summary.changed += 1;
        summary.changed_paths.push(rel.clone());
    }

    if !summary.is_noop() {
        debug_assert!(
            !summary.changed_paths.is_empty(),
            "non-noop summary must have at least one changed_paths entry (Phase B plan T4 Failure Mode 3 guard)"
        );
        // Phase B plan T4b — see invalidate_resolution_maps_cache's doc
        // comment for why this can't just rely on cached_resolution_maps'
        // own mtime comparison.
        if summary.changed_paths.iter().any(|p| is_manifest_path(p)) {
            invalidate_resolution_maps_cache(project_root);
        }
        let maps = cached_resolution_maps(project_root);
        if config.indexing.incremental_graph {
            match incremental_graph_update(
                &tx,
                project_root,
                &config.hotspots.default_since,
                &summary.changed_paths,
                &summary.names_delta,
                &config.hub_threshold,
                &maps,
                &config.ignore,
            )? {
                IncrementalOutcome::Applied => summary.graph_mode = GraphMode::Incremental,
                IncrementalOutcome::FellBackToFull(reason) => {
                    summary.graph_mode = GraphMode::FullFallback(reason)
                }
            }
        } else {
            rebuild_graph(
                &tx,
                project_root,
                &config.hotspots.default_since,
                &config.hub_threshold,
                &maps,
                &config.ignore,
            )?;
            summary.graph_mode = GraphMode::Full;
        }
        // Deliberate off-by-one vs. symbol_digests.graph_generation -- see
        // rebuild_graph_from_index's identical UPDATE for the full rationale.
        tx.execute(
            "UPDATE graph_generation_state SET generation = generation + 1 WHERE id = 1",
            [],
        )?;
    }
    tx.commit()?;
    Ok(summary)
}
