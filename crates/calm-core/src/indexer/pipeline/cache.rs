//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 8): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). The formal-resolver cache (one process-wide `FormalResolver`
//! instance, expensive to build) and the resolution-maps cache (per-
//! `project_root` `ResolutionMaps`, TTL + manifest-mtime invalidated), plus
//! the manifest-path predicate and the force-evict helper both caches share.
//! Move-only -- no logic changed, only relocated.
//!
//! `ResolutionMaps` itself stays defined in `pipeline.rs` (not moved,
//! already `pub`, shared with `resolve_import_targets`/`rebuild_graph`/
//! `incremental_graph_update`) -- pulled in via `super::ResolutionMaps`.
//!
//! Sibling-module wrinkle (not present in slices 1-6, first predicted in the
//! slice-7 handoff doc, confirmed live here): `driver.rs` and `graph.rs` are
//! both SIBLINGS of this new `cache` module, not ancestors -- Rust privacy
//! only grants ancestor/descendant visibility (a `pub(in path)` item is
//! visible in `path` and `path`'s descendants), so `cached_formal_resolver`/
//! `cached_resolution_maps`/`invalidate_resolution_maps_cache`/
//! `is_manifest_path` are `pub(super)` here -- `super` from `cache`'s own
//! perspective is `pipeline`, which both `driver` and `graph` descend from,
//! so `pub(super)` is exactly sufficient. Both sibling files' `use
//! super::{...}` import blocks were updated to pull these four names from
//! `super::cache::{...}` instead. Verified via `callers()` before the move:
//! real callers are `driver.rs` (`reindex_all_cancellable_with_phase`/
//! `reindex_changed_cancellable`/`reindex_paths`) and
//! `graph.rs::rebuild_graph_from_index`, plus 2 in-`pipeline.rs` test calls
//! to `cached_resolution_maps` (resolve automatically via the test module's
//! own `use super::*;`, same as every prior slice).
//!
//! Also fixes a slice-7 regression found while researching this slice:
//! `reindex_changed_cancellable`'s doc comment had been left behind in
//! `pipeline.rs` instead of moving with the function -- it silently merged
//! (no blank-line separator) into what was this cache's own doc comment,
//! since nothing was between them. Restored onto
//! `driver.rs::reindex_changed_cancellable` in the same commit as this move.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::ResolutionMaps;

/// Process-wide cache for the stack-graph rule sets `FormalResolver` loads
/// (`load_python`/`load_typescript`/`load_javascript`/`load_java`) — these
/// compile `.tsg` rule files via tree-sitter at construction time, which
/// measured live (this repo's own daemon, release build) is the single
/// most expensive step in every reindex call: ~5s, dwarfing the O(repo)
/// file-walk that Plan 3 §3.1 Phase A's `reindex_paths` removes — found
/// while dogfooding Phase A's own latency win and confirming it barely
/// moved end-to-end (see the plan doc's acceptance table). The rule sets
/// never change during a process's lifetime (nothing reconfigures them),
/// and `FormalResolver::resolve_file` takes `&self` only — read-only after
/// construction — so one shared instance, built once and reused by every
/// reindex call for the rest of the process's life, is both safe and the
/// actual dominant win here, bigger than Phase A's own file-walk removal.
static FORMAL_RESOLVER: std::sync::OnceLock<crate::resolver::formal::FormalResolver> =
    std::sync::OnceLock::new();

pub(super) fn cached_formal_resolver() -> &'static crate::resolver::formal::FormalResolver {
    FORMAL_RESOLVER.get_or_init(|| {
        let mut formal = crate::resolver::formal::FormalResolver::new();
        let _ = formal.load_python();
        let _ = formal.load_typescript();
        let _ = formal.load_javascript();
        let _ = formal.load_java();
        formal
    })
}

/// Cache entry for `cached_resolution_maps` — one per `project_root` (a
/// single-slot cache would return the wrong project's maps whenever more
/// than one `project_root` is used within the same process, which the test
/// suite does constantly via per-test temp dirs).
struct CachedResolutionMaps {
    built_at: std::time::Instant,
    cargo_toml_mtime: Option<std::time::SystemTime>,
    cargo_lock_mtime: Option<std::time::SystemTime>,
    composer_json_mtime: Option<std::time::SystemTime>,
    maps: ResolutionMaps,
}

static RESOLUTION_MAPS_CACHE: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, CachedResolutionMaps>>,
> = std::sync::OnceLock::new();

/// Fallback for the part `CrateMap`/`Psr4Map` genuinely can't cover by
/// mtime alone: neither `NamespaceMap::build` nor `PySysPathMap::build` is
/// manifest-driven at all — they walk every `.cs` / `.py` file in the repo
/// and read each one's content (see their own doc comments) — so there is no
/// single file whose mtime tracks "did those maps change". A pure TTL is the
/// honest answer here, not a gap: any edit to a `.cs`/`.py` file is already
/// at most this old before the next reindex sees a corrected map.
const RESOLUTION_MAPS_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Plan 3 §3.1 Phase D: `CrateMap`/`Psr4Map`/`NamespaceMap` were each
/// rebuilt from scratch on every single reindex call (3 call sites) —
/// `CrateMap::build` alone spawns a `cargo metadata` subprocess when
/// `cargo` is available. Cached per-`project_root`, invalidated on either
/// `Cargo.toml`/`Cargo.lock`/`composer.json`'s mtime changing (covers
/// `CrateMap`/`Psr4Map`, whose real inputs — verified by reading
/// `from_cargo_metadata`/`from_toml_scan`/`from_composer_json` — are
/// exactly these files, not the `*.csproj` the plan doc originally
/// guessed) or `RESOLUTION_MAPS_TTL` elapsing (the only correct answer for
/// `NamespaceMap`, see its doc comment above). All three maps are cheap to
/// `Clone` (small `HashMap`/`Vec` of strings) — cloned out of the lock
/// rather than holding it for the caller's `rebuild_graph` pass.
pub(super) fn cached_resolution_maps(project_root: &Path) -> ResolutionMaps {
    let file_mtime = |name: &str| {
        std::fs::metadata(project_root.join(name))
            .and_then(|m| m.modified())
            .ok()
    };
    let cargo_toml_mtime = file_mtime("Cargo.toml");
    let cargo_lock_mtime = file_mtime("Cargo.lock");
    let composer_json_mtime = file_mtime("composer.json");

    let cache_lock = RESOLUTION_MAPS_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = cache_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = cache.get(project_root) {
        let fresh_enough = c.built_at.elapsed() < RESOLUTION_MAPS_TTL;
        let manifests_unchanged = c.cargo_toml_mtime == cargo_toml_mtime
            && c.cargo_lock_mtime == cargo_lock_mtime
            && c.composer_json_mtime == composer_json_mtime;
        if fresh_enough && manifests_unchanged {
            return c.maps.clone();
        }
    }

    let maps = ResolutionMaps {
        crate_map: crate::indexer::crate_map::CrateMap::build(project_root),
        psr4: crate::indexer::psr4::Psr4Map::build(project_root),
        namespace_map: crate::indexer::csharp_namespace::NamespaceMap::build(project_root),
        pysys: crate::indexer::pysyspath::PySysPathMap::build(project_root),
        jvm: crate::indexer::jvm_package::JvmPackageMap::build(project_root),
        go: crate::indexer::go_module::GoModule::build(project_root),
    };
    cache.insert(
        project_root.to_path_buf(),
        CachedResolutionMaps {
            built_at: std::time::Instant::now(),
            cargo_toml_mtime,
            cargo_lock_mtime,
            composer_json_mtime,
            maps: maps.clone(),
        },
    );
    maps
}

/// Phase B plan T4b: the 3 manifest filenames `cached_resolution_maps`
/// tracks mtimes for (see its doc comment) — a standalone predicate so both
/// `reindex_paths` and `reindex_changed_cancellable` check the same thing.
/// Root-relative exact match only, matching that function's own
/// `project_root.join(name)` checks — a nested workspace member's manifest
/// doesn't affect this cache.
pub(super) fn is_manifest_path(rel: &str) -> bool {
    matches!(rel, "Cargo.toml" | "Cargo.lock" | "composer.json")
}

/// Phase B plan T4b (Risk Abductive-1 mitigation): force-evict
/// `project_root`'s `cached_resolution_maps` entry. Belt-and-suspenders on
/// top of that function's own mtime comparison, which is correct only to
/// the filesystem's mtime resolution — some filesystems round to 1s, so two
/// edits to the same manifest inside one second would otherwise look
/// "unchanged" and keep serving the first edit's stale maps. Called only
/// when this pass's own `changed_paths` already proves a manifest was
/// touched (a hard fact, not a heuristic), so an unconditional evict here is
/// free insurance rather than a guess. A no-op if nothing has been cached
/// yet for this `project_root`.
pub(super) fn invalidate_resolution_maps_cache(project_root: &Path) {
    if let Some(lock) = RESOLUTION_MAPS_CACHE.get() {
        lock.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(project_root);
    }
}
