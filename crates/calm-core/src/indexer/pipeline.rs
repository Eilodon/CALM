#[cfg(test)]
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

use crate::indexer::chunker::CodeChunk;
use crate::indexer::parser::ParsedSymbol;

// PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
// Wave 1 first slice): move-only extraction of file-discovery + content/path/
// hash primitives into pipeline/discovery.rs (issue #67 hotspot split). Both
// re-exports preserve their pre-move public paths (crate::indexer::pipeline::
// hash_content / collect_source_files) unchanged -- verified via callers()
// before the move that both have real external callers reaching them that way.
mod discovery;
pub(crate) use discovery::mtime_secs;
pub use discovery::{collect_source_files, hash_content};
use discovery::{mark_file_index_skip_reason, read_source_capped, rel_path, upsert_file_index};

// PR#7 slice 2: move-only extraction of per-file parsing/resolution +
// persistence into pipeline/extraction.rs. formal_resolution_timeout_count
// re-exported at its unchanged crate::indexer::pipeline path (one real
// external caller, verified via callers() before the move).
mod extraction;
pub use extraction::formal_resolution_timeout_count;
#[cfg(test)]
use extraction::formally_resolved_names;
use extraction::{extract_file_data, persist_file};

// PR#7 slice 3: move-only extraction of resolution-context construction +
// inheritance-closure lookup into pipeline/context.rs. ResolutionCtx/
// SymbolCandidate stay defined here (resolve_sites_to_edges, not yet
// extracted, still reads ResolutionCtx's fields directly).
mod context;
use context::{build_resolution_context, resolve_via_inheritance_closure};

// PR#7 slice 4: move-only extraction of the resolver's central
// reconciliation pass (resolve_sites_to_edges, ~620 lines) into
// pipeline/reconcile.rs -- the largest, highest-blast-radius slice in the
// whole split. ResolutionCtx/SymbolCandidate/CallSiteRow stay in pipeline.rs
// (still shared with context.rs's slice-3 functions and rebuild_graph/
// incremental_graph_update, not yet extracted).
mod reconcile;
use reconcile::{insert_ambiguity_groups_batch, resolve_sites_to_edges};

// PR#7 slice 5: move-only extraction of import-target resolution (Rust crate
// map, JS/TS relative, Python packages, PHP PSR-4, C# namespaces, JVM
// packages, Go stdlib/package-dir) into pipeline/modules.rs.
// ResolutionMaps stays in pipeline.rs (already pub, not moved).
mod modules;
use modules::resolve_import_targets;

// PR#7 slice 6: move-only extraction of graph (re)construction (full
// rebuild, the public rebuild-from-index entry point, the incremental delta
// path, and the shared caller_count refresh) into pipeline/graph.rs.
// rebuild_graph_from_index/refresh_caller_counts re-exported unchanged at
// their crate::indexer::pipeline paths (real external callers, verified via
// callers() before the move).
mod graph;
use graph::{IncrementalOutcome, incremental_graph_update, rebuild_graph};
pub use graph::{rebuild_graph_from_index, refresh_caller_counts};

// PR#7 slice 7: move-only extraction of the pipeline driver -- full-reindex
// entry points (run_indexing_pipeline/_cancellable, reindex_all_cancellable/
// _with_phase), incremental reindex (reindex_changed/_cancellable), and
// exact-path reindex (reindex_paths) -- into pipeline/driver.rs.
// PipelineOutcome/ReindexOutcome re-exported unchanged at their
// crate::indexer::pipeline paths (real external callers, verified via
// callers() before the move).
mod driver;
pub use driver::{
    PipelineOutcome, ReindexOutcome, reindex_all_cancellable, reindex_changed,
    reindex_changed_cancellable, reindex_paths, run_indexing_pipeline,
    run_indexing_pipeline_cancellable,
};

/// Maximum number of same-named symbols a call may resolve to before it is
/// dropped as too ambiguous (conservative).
const MAX_CALLEE_CANDIDATES: usize = 20;

/// Phase B plan D6: above this many `delta_paths`, `incremental_graph_update`
/// falls back to a full `rebuild_graph` — the delta-expansion query plus a
/// scoped-but-wide DELETE/re-resolve stops being cheaper than just doing the
/// full pass, and full rebuild is always correct regardless.
const MAX_INCREMENTAL_DELTA_PATHS: usize = 50;

/// Chunk size for any `WHERE col IN (...)` built from a caller-sized list
/// (`names_delta`, potentially hundreds of entries for a file with many
/// symbols) — conservative vs. SQLite's own default `SQLITE_MAX_VARIABLE_
/// NUMBER` of 32766 (3.32+). Phase B plan A-1.
const DELTA_QUERY_CHUNK_SIZE: usize = 500;

/// Above this size, a file is skipped entirely (never read, never parsed) --
/// defense against a maliciously huge/pathological file turning `calm index`
/// into a resource-exhaustion vector (`SECURITY.md` names this class of gap
/// explicitly). 8 MiB comfortably covers real hand-written source files
/// (even generated ones tend to top out far lower) while bounding worst-case
/// per-file memory. Skipped files never get a `file_index` row, same as any
/// other unreadable file -- a subsequent targeted edit still works via
/// direct tools, just outside the graph.
const MAX_INDEXABLE_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// True if a symbol's stored `signature` string's return type is `Option<_>`
/// or `Result<_, _>` (bare `Option`/`Result` too, for generic/associated-type
/// signatures that elide the parameter). Looks at the segment after the last
/// `->` — the actual return position, not a `->` that might appear earlier in
/// a higher-order parameter type (`f: impl Fn() -> i32`). A missing `->`
/// (fields, non-function symbols) returns `false`.
fn signature_returns_option_or_result(sig: &str) -> bool {
    let Some(ret) = sig.rsplit("->").next() else {
        return false;
    };
    // Guard against `sig` not containing `->` at all, in which case
    // `rsplit("->").next()` returns the whole string unchanged.
    if !sig.contains("->") {
        return false;
    }
    let ret = ret.trim_start();
    // Take the return type's own name only — up to its first generic `<`,
    // a following space (e.g. a `where` clause or the opening `{`), or `(`
    // (a tuple/unit return) — then strip any module qualification down to
    // the final `::`-segment. Real-world Result/Option returns are routinely
    // qualified (`rusqlite::Result<()>`, `anyhow::Result<T>`,
    // `std::io::Result<T>`, a crate's own `Result<T> = ...` alias used via
    // its module path) rather than bare `Result`/`Option` — matching only
    // the bare form silently dropped every qualified case as a false
    // exclusion (verified: `crate::config::load_config`'s real
    // `anyhow::Result<Config>` return was being excluded here, deleting a
    // real call edge). Anchoring on the first `<`/space/`(` — not a naive
    // whole-string split on `::` — also keeps this correct for a qualified
    // path *inside* the generic args (`Result<foo::Bar, baz::Error>`), which
    // a blind `rsplit("::")` over the whole return-type string would corrupt.
    let type_name_end = ret.find(['<', ' ', '(']).unwrap_or(ret.len());
    let type_name = &ret[..type_name_end];
    let base = type_name.rsplit("::").next().unwrap_or(type_name);
    base == "Option" || base == "Result"
}

/// Files are parsed+resolved (and then persisted) in chunks of this size
/// rather than all at once, so peak memory holds at most one batch of
/// parsed-but-not-yet-persisted files instead of an entire large repo.
const PARSE_BATCH_SIZE: usize = 1000;

/// A persisted call site loaded for graph rebuild. The leading id is the
/// durable edge identity; byte spans are retained here for exact SCIP matching
/// even though the resolver itself only needs the selected callee name.
/// `receiver` (see `parser::RawCall::receiver`) rides along for
/// `resolve_sites_to_edges`'s weak-fallback signal below -- `Some` for any
/// `.`-receiver or `Type::`-path call, `None` only for a genuinely bare,
/// unqualified name.
type CallSiteRow = (
    i64,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    String,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    String,
    Option<i64>,
    // WS2: `import_path` — see `CallSiteData::import_path`'s doc comment.
    // Appended at the end (not inserted positionally) so every existing
    // positional destructure elsewhere in this file needs only one
    // trailing `_`/binding added, not a full renumbering.
    Option<String>,
    // PR#8 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    // Part E): target_type_kind/target_type_qn -- see `CallSiteData`'s own
    // doc comment on these two fields for the full semantics. Same
    // append-at-the-end convention as `import_path` above, for the same
    // reason (every existing positional destructure needs only trailing
    // additions, not renumbering).
    Option<String>,
    Option<String>,
    // PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    // Part E): callee_start_rel/callee_end_rel -- see CallSiteData's own
    // doc comment. Same append-at-the-end convention.
    Option<i64>,
    Option<i64>,
);

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Which graph-rebuild path a non-noop reindex actually took. `Full` is the
/// only variant that exists before Phase B T4 wires `incremental_graph_update`
/// in — `Incremental`/`FullFallback` are plumbed now (Phase B T3) so
/// `ReindexSummary`'s shape doesn't change again when T4 lands, but nothing
/// constructs them yet. See docs/plans/2026-07-13-phase-b-incremental-graph-update.md §4 T3/T4.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GraphMode {
    #[default]
    Full,
    Incremental,
    /// delta too large / flag off / etc. — human-readable, not matched on.
    FullFallback(String),
}

impl GraphMode {
    /// Stable, machine-parseable label for `indexing_status.graph_mode` and
    /// log lines (plan L6): `"full"`, `"incremental"`, or
    /// `"full_fallback:<reason>"`. Only the prefix before `:` is meant to be
    /// matched on — the reason tail is human diagnostics.
    pub fn label(&self) -> String {
        match self {
            GraphMode::Full => "full".to_string(),
            GraphMode::Incremental => "incremental".to_string(),
            GraphMode::FullFallback(reason) => format!("full_fallback:{reason}"),
        }
    }
}

/// Result of an incremental reindex pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReindexSummary {
    pub changed: usize,
    pub deleted: usize,
    /// Every path `remove_file_rows` ran against this pass — both
    /// content-changed (still on disk) and deleted files, i.e. exactly
    /// Phase B plan §3's `delta_seed = changed ∪ deleted rel_paths`. One flat
    /// list rather than two separate vecs since nothing downstream needs the
    /// changed/deleted split — `summary.changed`/`summary.deleted` above
    /// already carry those counts.
    pub changed_paths: Vec<String>,
    /// Phase B plan D2: `old_names ∪ new_names` for every path in
    /// `changed_paths` — `old_names` read from `symbols` before
    /// `remove_file_rows` clears them, `new_names` from the freshly parsed
    /// `ExtractedFile.symbols` (no second SELECT). Union, not symmetric
    /// difference: a signature-only change (name unchanged) still needs its
    /// name in here so return-shape-filter-dependent sites elsewhere
    /// re-resolve — see D2's full proof in the plan doc.
    pub names_delta: HashSet<String>,
    /// Which rebuild path this pass took — `Full` unconditionally until
    /// Phase B T4 wires incremental in. Left at `GraphMode::default()`
    /// (`Full`) when the pass was a no-op (no rebuild ran at all).
    pub graph_mode: GraphMode,
}

impl ReindexSummary {
    pub fn is_noop(&self) -> bool {
        self.changed == 0 && self.deleted == 0
    }
}

/// One call site's resolved fields, ready to persist into `call_sites`.
struct CallSiteData {
    enclosing_qn: String,
    callee: String,
    line: i64,
    callee_start_byte: Option<i64>,
    callee_end_byte: Option<i64>,
    /// PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    /// Part E): `callee_start_byte`/`callee_end_byte`, made RELATIVE to the
    /// enclosing symbol's own start byte instead of absolute within the
    /// file. `None` when there's no real enclosing symbol to be relative
    /// to (the `MODULE_ENCLOSING` sentinel case -- a top-level call) or
    /// when this row is a legacy v2/v1 identity, in which case
    /// `identity_version` stays at its pre-v3 value. Set together with
    /// `identity_version: 3` -- see `extract_file_data`'s call-site loop
    /// for how it's computed.
    callee_start_rel: Option<i64>,
    callee_end_rel: Option<i64>,
    identity_version: i64,
    confidence: String,
    receiver: Option<String>,
    target_class: Option<String>,
    looks_option_or_result_chained: bool,
    /// See `parser::module_hint_of` — the discarded module-path segment of a
    /// lowercase-qualified `::`-call (`crate::telemetry::timed_tool` →
    /// `Some("telemetry")`), used by `rebuild_graph` to disambiguate among
    /// same-named candidates by file when there's no `use` for `resolve_tier1`
    /// to match against.
    module_hint: Option<String>,
    /// `"call"` for every tree-sitter-derived call site (every language
    /// below); `"reference"` only for `indexer::sql`'s FROM/JOIN table reads
    /// (a view/proc reading a table is not invoking it) — see
    /// `call_edges.edge_kind`'s migration comment in `db::schema`.
    edge_kind: String,
    /// See `parser::RawCall::arg_count` / `count_arguments_node` — `None`
    /// for `indexer::sql`'s references (no call-argument concept) and for
    /// any language whose grammar's arg-count extraction isn't verified.
    arg_count: Option<i64>,
    /// WS2 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D1):
    /// `resolve_tier1`'s `ResolveResult::resolved_path` — the actual import
    /// binding target (`from foo import bar` → `Some("foo")`) when the
    /// callee resolved via `ctx.import_map`. Previously computed and
    /// immediately discarded (only `.confidence` was kept) — this preserves
    /// it so `resolve_sites_to_edges` can narrow candidates by the REAL
    /// import target instead of re-guessing among every same-named symbol.
    /// `None` when tier-1 resolved via a same-file symbol (no import
    /// involved) or didn't resolve at all — never a source of new
    /// candidates on its own, only a narrowing filter, same posture as
    /// `module_hint` above.
    import_path: Option<String>,
    /// PR#8 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    /// Part E): `'resolved' | 'external' | 'unresolved'`, `None` iff
    /// `target_class` itself is `None` (no receiver-type inference applies
    /// to this call site at all -- a free-function call, or a `.`-receiver
    /// whose type genuinely couldn't be inferred). `'resolved'` = the
    /// receiver's declared type is a real project symbol whose qualified
    /// name is known (`target_type_qn` holds it) -- this is the actual
    /// fix for the P0-shaped bug `target_class` alone has: two classes
    /// named `User` in different packages both set `target_class: Some(
    /// "User")`, indistinguishable to `by_name_class`'s bare-name key,
    /// until this field's qualified payload disambiguates them.
    /// `'external'` = the declared type resolved to something OUTSIDE this
    /// project (stdlib/third-party) -- a first-class state, not "local
    /// lookup missed", the type-level twin of PR#5's `external_crate_root`
    /// for calls; `target_type_qn` holds a canonical external reference
    /// (e.g. `"java.util.List"`) when derivable, `None` when not.
    /// `'unresolved'` = a receiver type was named in the source but this
    /// pass couldn't classify it as either resolved-local or external
    /// (e.g. a generic type parameter) -- `target_type_qn` is diagnostic
    /// text only here, never matched against.
    target_type_kind: Option<String>,
    /// The qualified identity payload for `target_type_kind` -- see that
    /// field's own doc comment for what it holds per variant.
    target_type_qn: Option<String>,
}

/// Everything extracted from a single file's source, before any DB I/O.
/// Building this is pure CPU work (tree-sitter parse + resolver tiers), so it
/// is safe to compute for every file in parallel; only [`persist_file`] below
/// touches the transaction, and that stays single-threaded.
struct ExtractedFile {
    symbols: Vec<ParsedSymbol>,
    import_edges: Vec<crate::indexer::edges::ImportEdge>,
    call_sites: Vec<CallSiteData>,
    symbol_count: usize,
    /// Layer-2 semantic-search code-body chunks (see `indexer::chunker`).
    /// Always computed when the `embeddings` feature is compiled in — cheap
    /// pure-CPU work done in the same parallel extraction pass as everything
    /// else here — and left empty otherwise, since nothing would ever embed
    /// or query them.
    chunks: Vec<CodeChunk>,
    /// Tier 1 semantic facts (2026-08-07 roadmap T1) -- see
    /// `indexer::semantic_facts`'s module doc comment for exactly what's
    /// captured per language. Always empty for the SQL/markdown/shallow
    /// branches below (none of them run `semantic_facts`'s tree-sitter
    /// walk), same "empty, not an error" posture `import_edges`/`call_sites`
    /// already have on those branches.
    type_relations: Vec<crate::indexer::edges::TypeRelationData>,
    effects: Vec<crate::indexer::edges::SymbolEffectData>,
}

/// One file's `(rel_path, language, hash, mtime, extracted_data)` from a
/// batch's parallel extraction pass in `run_indexing_pipeline` —
/// `language`/`extracted_data` are `None` for a recognized-unparsed-extension
/// file (see `is_recognized_unparsed_extension`), which still gets a
/// `file_index` row but nothing to persist.
type ExtractedBatchRow = (
    String,
    Option<&'static str>,
    String,
    f64,
    Option<ExtractedFile>,
);

/// Rebuild the call graph from the persisted `call_sites` against the current
/// symbol table, then refresh caller_count, coreness, and is_hub.
///
/// This is pure DB work (no file parsing), so incremental passes only re-parse
/// the files that actually changed while the graph stays globally consistent.
// (qualified_name, path, language) — candidate list entry shared by
// `by_name`/`by_name_class` lookups in `ResolutionCtx`.
type SymbolCandidate = (String, String, String);

/// Per-site candidate lookup tables shared by every call site in one
/// resolution pass — built once via `build_resolution_context`, consumed by
/// `resolve_sites_to_edges`. Split out of `rebuild_graph` (Phase B plan D4,
/// docs/plans/2026-07-13-phase-b-incremental-graph-update.md) so
/// `incremental_graph_update` can reuse the exact same resolution logic
/// instead of a second, divergence-prone copy.
struct ResolutionCtx<'a> {
    by_name: HashMap<String, Vec<SymbolCandidate>>,
    by_name_class: HashMap<(String, String), Vec<SymbolCandidate>>,
    sig_by_qn: HashMap<String, String>,
    path_lang: HashMap<String, String>,
    caller_usings: HashMap<String, HashSet<String>>,
    namespace_map: &'a crate::indexer::csharp_namespace::NamespaceMap,
    /// qualified_name → (declared arity, is_variadic) (see `ParsedSymbol::arity`/
    /// `arity_variadic`), for the B3/A' arity gate below — populated for
    /// Elixir (exact arity, `is_variadic` always `false`) and Go (minimum
    /// arity, `is_variadic` true when the last param is `...T`). Absent for
    /// every other language until its own arity extraction is verified.
    arity_by_qn: HashMap<String, (i64, bool)>,
    /// WS4 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D4):
    /// class/interface BARE name (matching `target_class`/`by_name_class`'s
    /// own keying, not a qualified_name -- see `build_inheritance_closure`'s
    /// doc comment for why) → its transitive ancestors (extends/implements),
    /// grouped by depth (`levels[0]` = direct parents, `levels[1]` =
    /// grandparents, ...), cycle-safe, depth-bounded. Level-grouped rather
    /// than a flat closest-first list specifically so
    /// `resolve_via_inheritance_closure` can tell "exactly one ancestor at
    /// the nearest depth declares this" (real, confident evidence) apart
    /// from "two DIFFERENT ancestors at the SAME nearest depth both declare
    /// it" (a genuine tie -- e.g. `interface Mixed extends IA, IB` where
    /// both declare the same method name -- which must never be resolved by
    /// picking whichever happens to come first in source order). See
    /// `build_inheritance_closure` for the hard `resolved`-confidence-only
    /// gate on which `type_relations` rows may feed this.
    inheritance_closure: HashMap<String, Vec<Vec<String>>>,
}

// PR#7 slice 8: move-only extraction of the formal-resolver cache and the
// resolution-maps cache (shared FormalResolver instance, per-project_root
// ResolutionMaps TTL/manifest-mtime cache, manifest-path predicate, force-
// evict helper) into pipeline/cache.rs (issue #67 hotspot split).
// ResolutionMaps itself stays in pipeline.rs (already pub, shared with
// resolve_import_targets/rebuild_graph/incremental_graph_update).
// cached_formal_resolver/cached_resolution_maps/
// invalidate_resolution_maps_cache/is_manifest_path are pub(super) in
// cache.rs (not plain private, unlike slices 1-6) since driver.rs and
// graph.rs -- siblings of cache.rs, not descendants -- both call into them;
// verified via callers() before the move. Also fixes a slice-7 regression
// found while researching this slice: reindex_changed_cancellable's doc
// comment had been orphaned in pipeline.rs (left behind, silently merged
// into this comment block) instead of moving to driver.rs with the
// function itself -- restored onto driver.rs::reindex_changed_cancellable
// in the same commit.
mod cache;
use cache::{
    cached_formal_resolver, cached_resolution_maps, invalidate_resolution_maps_cache,
    is_manifest_path,
};

/// Bundle of the 6 per-project-root, ecosystem-specific import/module
/// resolvers that `resolve_module_to_path` (and everything upstream of it —
/// `resolve_import_targets`, `rebuild_graph`, `incremental_graph_update`)
/// needs but never inspects independently of the others: they are always
/// built together (`cached_resolution_maps`) and always travel together.
/// Introduced (C2, Tier A resolver audit follow-up) once `resolve_module_to_path`
/// became the 3rd function on this path to hit clippy's `too_many_arguments`
/// threshold — see this file's `git blame`/history for the growing-parameter-list
/// comment that flagged this refactor and deliberately deferred it to its own
/// change. Cheap to `Clone` (each field already was).
#[derive(Clone)]
pub struct ResolutionMaps {
    crate_map: crate::indexer::crate_map::CrateMap,
    psr4: crate::indexer::psr4::Psr4Map,
    namespace_map: crate::indexer::csharp_namespace::NamespaceMap,
    pysys: crate::indexer::pysyspath::PySysPathMap,
    jvm: crate::indexer::jvm_package::JvmPackageMap,
    go: crate::indexer::go_module::GoModule,
}

// PR#7 slice 9 (final slice): move-only extraction of the D4 CallSite-
// identity migration (detection predicate, diagnostic-status recorder,
// one-time full-baseline reparse) into pipeline/identity_migration.rs
// (issue #67 hotspot split). needs_call_site_identity_baseline/
// rebuild_call_site_identity_baseline are pub(super) there since driver.rs
// -- a sibling of identity_migration.rs, not a descendant -- calls both;
// verified via callers() before the move that real callers are exactly
// driver.rs::reindex_changed_cancellable/reindex_paths (2 sites each).
// record_call_site_identity_migration_status stays plain private (its only
// caller, rebuild_call_site_identity_baseline, moved with it).
//
// This completes pipeline.rs's PR#7 split: only shared struct/type/const
// definitions, the 9 mod + import/re-export blocks, now_secs/
// signature_returns_option_or_result, and the untouched #[cfg(test)] mod
// tests block remain below.
mod identity_migration;
use identity_migration::{needs_call_site_identity_baseline, rebuild_call_site_identity_baseline};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::types::IndexingPhase;

    #[test]
    fn graph_mode_label_strings() {
        // Contract shared by indexing_status.graph_mode, the watcher log, and
        // edit_reindex_completed (Phase B T6.5) — only the prefix before `:`
        // is meant to be matched on.
        assert_eq!(GraphMode::Full.label(), "full");
        assert_eq!(GraphMode::Incremental.label(), "incremental");
        assert_eq!(
            GraphMode::FullFallback("delta_paths.len()=51 > 50".to_string()).label(),
            "full_fallback:delta_paths.len()=51 > 50"
        );
    }

    #[test]
    // UPGRADE_PLAN.md FIX1, Gate B: even with Gate A's sentinel seeded by
    // parser.rs, extract_file_data's own qn_by_loc lookup previously had no
    // entry for (MODULE_ENCLOSING, 0) and dropped the call anyway. This
    // exercises the full extract_file_data path (not just extract_calls) to
    // confirm the synthesized "{rel}::<module>" qualified_name actually
    // reaches call_sites.
    fn extract_file_data_js_module_level_call_gets_synthesized_module_qn() {
        let source = "describe('x', function(){ helper() });\nfunction helper(){}\n";
        let formal = crate::resolver::formal::FormalResolver::new();
        let data = extract_file_data("test.js", "javascript", source, &[], &formal);
        let call_summary: Vec<(&str, &str)> = data
            .call_sites
            .iter()
            .map(|cs| (cs.enclosing_qn.as_str(), cs.callee.as_str()))
            .collect();
        assert!(
            data.call_sites
                .iter()
                .any(|cs| cs.enclosing_qn == "test.js::<module>" && cs.callee == "helper"),
            "helper() should attribute to the synthesized module-level qualified name, \
             not be dropped: {call_summary:?}"
        );
        // The synthesized qn must NOT leak into `symbols` -- symbol_count
        // must equal exactly the 1 real named function (`helper`), not 2.
        let symbol_names: Vec<&str> = data.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            data.symbol_count, 1,
            "the <module> pseudo-caller must never become a real indexed symbol: {symbol_names:?}"
        );
    }

    #[test]
    fn extract_file_data_resolves_java_type_relations_and_effects() {
        let source = "class Foo extends Base implements Baz {\n    int x;\n    void m(int v) {\n        this.x = v;\n        if (v < 0) { throw new InvalidToken(); }\n    }\n}\n";
        let formal = crate::resolver::formal::FormalResolver::new();
        let data = extract_file_data("Foo.java", "java", source, &[], &formal);

        assert_eq!(data.type_relations.len(), 2, "{:?}", {
            data.type_relations
                .iter()
                .map(|r| {
                    (
                        r.from_symbol.as_str(),
                        r.relation_kind,
                        r.target_text.as_str(),
                    )
                })
                .collect::<Vec<_>>()
        });
        let extends = data
            .type_relations
            .iter()
            .find(|r| r.relation_kind == "extends")
            .expect("extends relation missing");
        assert_eq!(extends.from_symbol, "Foo.java::Foo");
        assert_eq!(extends.target_text, "Base");
        // Base is not declared in this file -- same-file-only resolution
        // in v1 must leave it textual/unresolved, never guessed.
        assert_eq!(extends.to_symbol, None);
        assert_eq!(extends.confidence, "textual");

        let implements = data
            .type_relations
            .iter()
            .find(|r| r.relation_kind == "implements")
            .expect("implements relation missing");
        assert_eq!(implements.target_text, "Baz");

        assert_eq!(data.effects.len(), 2, "{:?}", {
            data.effects
                .iter()
                .map(|e| (e.symbol_qn.as_str(), e.effect_kind, e.target_text.as_str()))
                .collect::<Vec<_>>()
        });
        let write = data
            .effects
            .iter()
            .find(|e| e.effect_kind == "write_field")
            .expect("write_field effect missing");
        assert_eq!(write.symbol_qn, "Foo.java::Foo::m");
        assert_eq!(write.target_text, "x");
        let throw = data
            .effects
            .iter()
            .find(|e| e.effect_kind == "explicit_throw")
            .expect("explicit_throw effect missing");
        assert_eq!(throw.symbol_qn, "Foo.java::Foo::m");
        assert_eq!(throw.target_text, "InvalidToken");
    }

    #[test]
    fn extract_file_data_resolves_type_relation_to_symbol_when_same_file() {
        // Baz IS declared in this same file -- to_symbol must resolve and
        // confidence must upgrade to "resolved".
        let source = "interface Baz {}\nclass Foo implements Baz {}\n";
        let formal = crate::resolver::formal::FormalResolver::new();
        let data = extract_file_data("Foo.ts", "typescript", source, &[], &formal);

        assert_eq!(data.type_relations.len(), 1);
        let rel = &data.type_relations[0];
        assert_eq!(rel.relation_kind, "implements");
        assert_eq!(rel.target_text, "Baz");
        assert_eq!(rel.to_symbol.as_deref(), Some("Foo.ts::Baz"));
        assert_eq!(rel.confidence, "resolved");
    }

    #[test]
    fn extract_file_data_resolves_rust_impl_trait_by_same_file_name_fallback() {
        // The struct's own definition line is NOT the impl block's line --
        // this only passes if the by-name fallback (not the exact (name,
        // line) lookup every other language gets) actually fires.
        let source = "trait Bar {\n    fn m(&self);\n}\n\nstruct Foo {\n    x: i32,\n}\n\nimpl Bar for Foo {\n    fn m(&self) {}\n}\n";
        let formal = crate::resolver::formal::FormalResolver::new();
        let data = extract_file_data("lib.rs", "rust", source, &[], &formal);

        assert_eq!(data.type_relations.len(), 1, "{:?}", {
            data.type_relations
                .iter()
                .map(|r| {
                    (
                        r.from_symbol.as_str(),
                        r.relation_kind,
                        r.target_text.as_str(),
                    )
                })
                .collect::<Vec<_>>()
        });
        let rel = &data.type_relations[0];
        assert_eq!(rel.relation_kind, "implements");
        assert_eq!(rel.from_symbol, "lib.rs::Foo");
        assert_eq!(rel.target_text, "Bar");
        assert_eq!(rel.to_symbol.as_deref(), Some("lib.rs::Bar"));
        assert_eq!(rel.confidence, "resolved");
    }

    #[test]
    fn extract_file_data_python_bases_and_write_effects() {
        let source = "class Foo(Base):\n    def __init__(self):\n        self.count = 0\n    def bump(self):\n        self.count += 1\n";
        let formal = crate::resolver::formal::FormalResolver::new();
        let data = extract_file_data("foo.py", "python", source, &[], &formal);

        assert_eq!(data.type_relations.len(), 1);
        assert_eq!(data.type_relations[0].from_symbol, "foo.py::Foo");
        assert_eq!(data.type_relations[0].target_text, "Base");

        let writes: Vec<&str> = data.effects.iter().map(|e| e.symbol_qn.as_str()).collect();
        assert_eq!(
            writes,
            vec!["foo.py::Foo::__init__", "foo.py::Foo::bump"],
            "both self.count writes (init and augmented-assign) must attribute to their own enclosing method"
        );
    }

    // ADR-A1: formal resolution can be cancelled (RESOLVE_TIMEOUT / a
    // deterministic work cap, see ADR-A2) -- `formally_resolved_names` must
    // distinguish that from "resolved and genuinely found nothing" instead
    // of silently treating both as an identical empty set with zero signal.
    #[test]
    fn formally_resolved_names_ok_returns_edge_names_unaffected() {
        // No before/after counter assertion here: `FORMAL_RESOLUTION_TIMEOUTS`
        // is a single process-global static shared by every test binary
        // thread, so a concurrently-running test that legitimately hits the
        // `Err` branch (e.g. the sibling test below) can bump it in the
        // middle of this test's window -- that's cargo's normal parallel
        // execution, not a bug. The "Ok path never increments" guarantee is
        // structural (the `Ok` match arm has no reference to the counter at
        // all -- see `formally_resolved_names`) and is exercised the other
        // direction by the sibling `_err_` test asserting a real increase.
        let edges = vec![crate::resolver::formal::FormalEdge {
            reference_symbol: "foo".to_string(),
            definition_symbol: "bar".to_string(),
        }];
        let result = formally_resolved_names(Ok(edges), "python", "mod.py");
        assert_eq!(result, HashSet::from(["foo".to_string()]));
    }

    #[test]
    fn formally_resolved_names_err_returns_empty_and_increments_counter() {
        let before = formal_resolution_timeout_count();
        let result = formally_resolved_names(
            Err(anyhow::anyhow!(
                "Path stitching cancelled: Cancelled at \"finding complete partial paths\""
            )),
            "python",
            "mod.py",
        );
        assert!(
            result.is_empty(),
            "Err path must degrade to an empty set -- same call-site behavior as before this fix"
        );
        assert!(
            formal_resolution_timeout_count() > before,
            "Err path must increment the observability counter -- a cancelled resolution must \
             never be silently indistinguishable from 'resolved, found nothing'"
        );
    }
    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn dummy_phase() -> std::sync::Arc<std::sync::RwLock<IndexingPhase>> {
        std::sync::Arc::new(std::sync::RwLock::new(IndexingPhase::Scanning))
    }

    #[test]
    fn graph_generation_advances_only_after_commit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let generation = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT generation FROM graph_generation_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        assert_eq!(generation(&conn), 0);
        assert_eq!(
            run_indexing_pipeline_cancellable(&mut conn, dir.path(), dummy_phase(), &|| true)
                .unwrap(),
            PipelineOutcome::Cancelled
        );
        assert_eq!(generation(&conn), 0);

        run_indexing_pipeline(&mut conn, dir.path(), dummy_phase()).unwrap();
        assert_eq!(generation(&conn), 1);
        run_indexing_pipeline(&mut conn, dir.path(), dummy_phase()).unwrap();
        assert_eq!(generation(&conn), 2);

        std::fs::write(dir.path().join("main.rs"), "fn changed() {}\n").unwrap();
        assert!(matches!(
            reindex_changed(&mut conn, dir.path()).unwrap(),
            ReindexSummary { changed: 1, .. }
        ));
        assert_eq!(generation(&conn), 3);

        std::fs::write(dir.path().join("main.rs"), "fn changed_again() {}\n").unwrap();
        assert!(matches!(
            reindex_paths(&mut conn, dir.path(), &["main.rs".to_string()]).unwrap(),
            ReindexSummary { changed: 1, .. }
        ));
        assert_eq!(generation(&conn), 4);
    }

    #[test]
    fn test_phase_advances_to_ready_after_pipeline() {
        use crate::types::IndexingPhase;
        use std::sync::{Arc, RwLock};

        let dir = std::env::temp_dir().join(format!("ci_idx_phase_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let phase = Arc::new(RwLock::new(IndexingPhase::Scanning));
        run_indexing_pipeline(&mut conn, &dir, phase.clone()).unwrap();

        assert_eq!(
            *phase.read().unwrap(),
            IndexingPhase::Ready,
            "Phase must be Ready after pipeline completes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_indexing_pipeline_empty_dir() {
        let dir = std::env::temp_dir().join(format!("ci_idx_empty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(run_indexing_pipeline(&mut conn, &dir, dummy_phase()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_indexing_pipeline_real_extraction() {
        let dir = std::env::temp_dir().join(format!("ci_idx_real_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\ndef main():\n    helper()\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 1);
        // `main` calls `helper()` twice (two distinct call sites) → two edges;
        // edges are keyed on (from, to, call-site line), not just (from, to), so
        // both sites are preserved rather than collapsed to one.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = 'a.py::main' AND to_symbol = 'a.py::helper'",
            ),
            2
        );
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: a recognized-unparsed-extension file (see
    /// `is_recognized_unparsed_extension`) must earn a `file_index` row
    /// (path/hash/mtime, `language` NULL, `symbol_count` 0) so it's visible
    /// as "recognized but unparsed" rather than invisible like a doc/image/
    /// lockfile — but must never get symbols/edges, since there is no
    /// extractor for it.
    #[test]
    fn test_run_indexing_pipeline_tracks_recognized_unparsed_extension_by_path_only() {
        let dir = std::env::temp_dir().join(format!("ci_idx_unparsed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("Token.sol"),
            "pragma solidity ^0.8.0;\ncontract Token {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1); // only a.py::hello

        let sol_language: Option<String> = conn
            .query_row(
                "SELECT language FROM file_index WHERE path = 'Token.sol'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sol_language, None,
            "recognized-unparsed row must have language = NULL"
        );
        let sol_symbol_count: i64 = conn
            .query_row(
                "SELECT symbol_count FROM file_index WHERE path = 'Token.sol'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sol_symbol_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the incremental-reindex trap this design closes: before
    /// the fix, `reindex_changed`'s file-collection step filtered out
    /// recognized-unparsed files entirely, so their `file_index` row (created
    /// by a prior full index) was absent from `seen_paths` and got deleted as
    /// if the file had disappeared — on literally the very next incremental
    /// pass, even with no changes on disk at all.
    #[test]
    fn test_reindex_changed_does_not_delete_recognized_unparsed_file_row() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_unparsed_reindex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("Token.sol"),
            "pragma solidity ^0.8.0;\ncontract Token {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 2);

        let summary = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            summary.deleted, 0,
            "an unchanged recognized-unparsed file must not be treated as deleted"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM file_index WHERE path = 'Token.sol'"
            ),
            1,
            "Token.sol's file_index row must survive an incremental reindex"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for 4.1a (docs/plans/2026-08-20-truth-kernel-hardening-
    /// execution-plan.md): before the fix, a file that was walked (still on
    /// disk, recognized extension) but transiently failed `read_source_capped`
    /// (simulated here by pushing it over `MAX_INDEXABLE_FILE_BYTES`) was
    /// dropped from `seen_paths` and treated exactly like a genuinely deleted
    /// file -- its indexed symbols/call_sites were removed even though the
    /// file still existed and would read fine on the very next pass.
    #[test]
    fn test_reindex_changed_does_not_delete_a_transiently_unreadable_file_row() {
        let dir = std::env::temp_dir().join(format!(
            "ci_idx_transient_unreadable_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("a.py");
        std::fs::write(&target, "def hello():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM symbols WHERE name = 'hello'"),
            1
        );

        // Simulate a transient read failure without deleting the file: push
        // it over the byte cap, same technique as
        // `read_source_capped_skips_a_file_over_the_byte_cap` above.
        let f = std::fs::File::create(&target).unwrap();
        f.set_len(MAX_INDEXABLE_FILE_BYTES + 1).unwrap();

        let summary = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            summary.deleted, 0,
            "a file that still exists on disk but transiently failed to read must not be counted as deleted"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM file_index WHERE path = 'a.py'"),
            1,
            "a.py's file_index row must survive a transient read failure"
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM symbols WHERE name = 'hello'"),
            1,
            "a.py's symbols must not be deleted just because this pass couldn't re-read it"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Plan 3 §3.1 Phase A: reindex_paths must touch ONLY the given paths —
    // no full-repo walk/hash, no re-scan of files not in the given list.
    #[test]
    fn test_reindex_paths_only_touches_given_paths_not_whole_repo() {
        let dir = std::env::temp_dir().join(format!("ci_idx_reindex_paths_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def a():\n    pass\n").unwrap();
        std::fs::write(dir.join("b.py"), "def b():\n    pass\n").unwrap();
        std::fs::write(dir.join("c.py"), "def c():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 3);

        let last_indexed_of = |conn: &Connection, path: &str| -> f64 {
            conn.query_row(
                "SELECT last_indexed FROM file_index WHERE path = ?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
        };
        let (b_before, c_before) = (
            last_indexed_of(&conn, "b.py"),
            last_indexed_of(&conn, "c.py"),
        );
        // A tick so a wrongly-touched row's timestamp would provably differ,
        // not just coincidentally match down to float precision.
        std::thread::sleep(std::time::Duration::from_millis(10));

        std::fs::write(
            dir.join("a.py"),
            "def a():\n    pass\n\ndef a2():\n    pass\n",
        )
        .unwrap();
        let summary = reindex_paths(&mut conn, &dir, &["a.py".to_string()]).unwrap();
        assert_eq!(summary.changed, 1);
        assert_eq!(summary.deleted, 0);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM symbols WHERE path = 'a.py'"),
            2,
            "a.py's new symbol (a2) must be picked up"
        );
        assert_eq!(
            (
                last_indexed_of(&conn, "b.py"),
                last_indexed_of(&conn, "c.py")
            ),
            (b_before, c_before),
            "b.py/c.py must not be re-scanned — reindex_paths only touches the given paths"
        );

        // Deletion: a given path no longer on disk drops its rows.
        std::fs::remove_file(dir.join("b.py")).unwrap();
        let summary = reindex_paths(&mut conn, &dir, &["b.py".to_string()]).unwrap();
        assert_eq!(summary.deleted, 1);
        assert_eq!(summary.changed, 0);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM file_index WHERE path = 'b.py'"),
            0
        );

        // A path that's neither on disk nor ever indexed — no-op, no error.
        let summary = reindex_paths(&mut conn, &dir, &["never_existed.py".to_string()]).unwrap();
        assert!(summary.is_noop());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_source_capped_skips_a_file_over_the_byte_cap() {
        let dir = std::env::temp_dir().join(format!("ci_idx_capped_read_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let small = dir.join("small.py");
        std::fs::write(&small, "def a():\n    pass\n").unwrap();
        assert_eq!(
            read_source_capped(&small).as_deref(),
            Ok("def a():\n    pass\n"),
            "a normal-sized file must still be read exactly as before"
        );

        let huge = dir.join("huge.py");
        // One byte over the cap — checked via metadata(), so this never
        // actually allocates/reads MAX_INDEXABLE_FILE_BYTES worth of data.
        let f = std::fs::File::create(&huge).unwrap();
        f.set_len(MAX_INDEXABLE_FILE_BYTES + 1).unwrap();
        let skip_reason = read_source_capped(&huge)
            .expect_err("a file over MAX_INDEXABLE_FILE_BYTES must be skipped, not read");
        assert!(
            skip_reason.starts_with("too_large:"),
            "skip reason must identify the cause as too_large, got {skip_reason:?}"
        );

        let exactly_at_cap = dir.join("at_cap.py");
        let f = std::fs::File::create(&exactly_at_cap).unwrap();
        f.set_len(MAX_INDEXABLE_FILE_BYTES).unwrap();
        assert!(
            read_source_capped(&exactly_at_cap).is_ok(),
            "a file exactly AT the cap must still be read (cap is an upper bound, not exclusive)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_indexing_pipeline_skips_an_oversized_file_but_still_indexes_the_rest() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_oversized_skip_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("normal.py"), "def normal():\n    pass\n").unwrap();
        let huge = dir.join("huge.py");
        let f = std::fs::File::create(&huge).unwrap();
        f.set_len(MAX_INDEXABLE_FILE_BYTES + 1).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Must not error out or hang the whole run just because one file in
        // the repo is pathologically large -- the run completes and indexes
        // every OTHER file normally.
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM file_index WHERE path = 'normal.py'"
            ),
            1,
            "the normal-sized sibling file must still be indexed"
        );
        // 4.1b: a skipped file now earns a placeholder file_index row (so
        // its skip is discoverable, e.g. via fitness_report) instead of no
        // row at all -- symbol_count stays 0 and skip_reason records why.
        let (huge_symbol_count, huge_skip_reason): (i64, Option<String>) = conn
            .query_row(
                "SELECT symbol_count, skip_reason FROM file_index WHERE path = 'huge.py'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(huge_symbol_count, 0, "a skipped file has nothing extracted");
        assert!(
            huge_skip_reason
                .as_deref()
                .is_some_and(|r| r.starts_with("too_large:")),
            "skip_reason must record why the file was skipped, got {huge_skip_reason:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Plan 3 §3.1 Phase D: cached_resolution_maps must (a) actually cache
    // (return the same content without rebuilding within TTL) and (b)
    // correctly invalidate the moment the manifest it read changes — using
    // an isolated temp dir rather than the shared `tests/fixtures/
    // rust_workspace` fixture so this doesn't mutate state other
    // (possibly parallel) tests read.
    #[test]
    fn test_cached_resolution_maps_hits_cache_then_invalidates_on_manifest_change() {
        let dir = std::env::temp_dir().join(format!("ci_idx_resmaps_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"resmaps_foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "").unwrap();

        let maps = cached_resolution_maps(&dir);
        assert_eq!(maps.crate_map.root_of("resmaps_foo"), Some("src"));

        // Rewrite Cargo.toml's package name WITHOUT touching mtime granularity
        // — sleep past a coarse (1s) filesystem mtime resolution so this is a
        // real, observable change, not a same-tick no-op.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"resmaps_bar\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let maps2 = cached_resolution_maps(&dir);
        assert_eq!(
            maps2.crate_map.root_of("resmaps_bar"),
            Some("src"),
            "Cargo.toml mtime changed — cache must rebuild, not serve the stale mapping"
        );
        assert_eq!(
            maps2.crate_map.root_of("resmaps_foo"),
            None,
            "old crate name must be gone after rebuild"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `.toml` specifically (not just the abstract extension registry) must
    /// earn a `file_index` row the same way `Token.sol` does above — this is
    /// the concrete case that motivated adding `toml` to
    /// `is_recognized_unparsed_extension`: `Cargo.toml`/`rust-toolchain.toml`
    /// were previously invisible to `file_index` entirely, which made
    /// `diff_impact` misreport an edit to them as "out_of_scope" instead of
    /// "recognized_unparsed", and `search(kind="glob")` couldn't find them by
    /// path at all.
    #[test]
    fn test_run_indexing_pipeline_tracks_toml_as_recognized_unparsed() {
        let dir = std::env::temp_dir().join(format!("ci_idx_toml_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def hello():\n    pass\n").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 2);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1); // only a.py::hello

        let toml_language: Option<String> = conn
            .query_row(
                "SELECT language FROM file_index WHERE path = 'Cargo.toml'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            toml_language, None,
            "Cargo.toml must be tracked as recognized-unparsed (language = NULL)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for B4: known FNV-1a 64-bit test vectors (from the FNV
    /// reference test suite), independent of this codebase's own algorithm —
    /// confirms `hash_content` is a real, portable FNV-1a and not just
    /// internally self-consistent.
    #[test]
    fn test_hash_content_matches_fnv1a_64_test_vectors() {
        assert_eq!(hash_content(""), "cbf29ce484222325");
        assert_eq!(hash_content("a"), "af63dc4c8601ec8c");
        assert_eq!(hash_content("foobar"), "85944171f73967e8");
    }

    #[test]
    fn test_hash_content_deterministic_across_calls() {
        let s = "def hello():\n    pass\n";
        assert_eq!(hash_content(s), hash_content(s));
        assert_ne!(hash_content(s), hash_content("different content"));
    }

    #[test]
    fn test_config_ignore_excludes_dir_and_glob() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ignorecfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(dir.join("a.py"), "def kept():\n    pass\n").unwrap();
        std::fs::write(dir.join("vendor/b.py"), "def excluded_dir():\n    pass\n").unwrap();
        std::fs::write(dir.join("app.min.js"), "function excludedGlob() {}\n").unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"ignore": ["vendor", "*.min.js"]}"#,
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::kept'",
            ),
            1
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM file_index"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_entry_points_config_escape_hatch() {
        let dir = std::env::temp_dir().join(format!("ci_idx_entrycfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def custom_entry():\n    pass\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"entry_points": ["a.py::custom_entry"]}"#,
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT is_entry_point FROM symbols WHERE qualified_name = 'a.py::custom_entry'",
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT is_entry_point FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test: `rebuild_graph` used to hardcode `HubThresholdConfig::default()`
    /// instead of loading the project's `config.json`, so a custom `hub_threshold`
    /// (like `entry_points`'s config escape hatch above) was silently ignored.
    #[test]
    fn test_hub_threshold_config_escape_hatch() {
        let dir = std::env::temp_dir().join(format!("ci_idx_hubcfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\n\
             def caller_a():\n    helper()\n\n\
             def caller_b():\n    helper()\n\n\
             def caller_c():\n    helper()\n",
        )
        .unwrap();

        let mut conn_default = Connection::open_in_memory().unwrap();
        init_db(&conn_default).unwrap();
        run_indexing_pipeline(&mut conn_default, &dir, dummy_phase()).unwrap();
        assert_eq!(
            count(
                &conn_default,
                "SELECT is_hub FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0,
            "default min_callers=5 should not flag a 3-caller symbol as hub"
        );

        std::fs::write(
            dir.join("config.json"),
            r#"{"hub_threshold": {"min_callers": 1, "top_pct": 100, "min_callers_bridge": 1, "coreness_pct": 100}}"#,
        )
        .unwrap();
        let mut conn_custom = Connection::open_in_memory().unwrap();
        init_db(&conn_custom).unwrap();
        run_indexing_pipeline(&mut conn_custom, &dir, dummy_phase()).unwrap();
        assert_eq!(
            count(
                &conn_custom,
                "SELECT is_hub FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1,
            "custom min_callers=1/top_pct=100 should flag the same symbol as hub"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_alias_resolution_edge() {
        let dir = std::env::temp_dir().join(format!("ci_idx_alias_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // main calls helper indirectly through a local alias `x = helper`.
        std::fs::write(
            dir.join("a.py"),
            "def helper():\n    pass\n\ndef main():\n    x = helper\n    x()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The alias is de-referenced, so the edge points at helper.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = 'a.py::main' AND to_symbol = 'a.py::helper'",
            ),
            1,
            "alias x=helper should resolve the call to helper"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_imports_and_cross_file_resolved_confidence() {
        let dir = std::env::temp_dir().join(format!("ci_idx_imp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(
            dir.join("main.py"),
            "from helper import helper\n\ndef run():\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // import_edges populated and to_path resolved to the in-project file.
        let (to_path, module): (String, String) = conn
            .query_row(
                "SELECT COALESCE(to_path,''), module_name FROM import_edges WHERE from_path = 'main.py'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(module, "helper");
        assert_eq!(
            to_path, "helper.py",
            "import target resolved to in-project file"
        );

        // The cross-file call through the import is labelled "resolved".
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.py::run' AND to_symbol = 'helper.py::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "resolved",
            "imported call should be resolved, not textual"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `use super::common::*;` written inside an inline `#[cfg(test)] mod
    /// tests` means one module up from *that block* — i.e. `tools` itself,
    /// whose files live in `src/tools/`. Resolving `super::` purely from the
    /// importing file's parent directory (`src/`) finds nothing, which is
    /// what used to happen for every such import.
    #[test]
    fn test_rust_super_import_inside_inline_mod_resolves_to_own_module_dir() {
        let dir = std::env::temp_dir().join(format!("ci_idx_supermod_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/tools")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"supermod\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub mod tools;\n").unwrap();
        std::fs::write(
            dir.join("src/tools.rs"),
            "pub mod common;\n\n#[cfg(test)]\nmod tests {\n    use super::common::*;\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/tools/common.rs"), "pub fn helper() {}\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: String = conn
            .query_row(
                "SELECT COALESCE(to_path,'') FROM import_edges \
                 WHERE from_path = 'src/tools.rs' AND module_name = 'super::common'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path, "src/tools/common.rs",
            "`super::` inside an inline mod resolves against the file's own module dir"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An ordinary intra-package `from pkg.helper import helper` resolves
    /// against the importing file's own package root — here `app/`, since
    /// `app/main.py` is in no package itself — not against the project root.
    #[test]
    fn test_python_dotted_import_resolves_against_package_root() {
        let dir = std::env::temp_dir().join(format!("ci_idx_pypkg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app/pkg")).unwrap();
        std::fs::write(dir.join("app/main.py"), "from pkg.helper import helper\n").unwrap();
        std::fs::write(dir.join("app/pkg/__init__.py"), "").unwrap();
        std::fs::write(dir.join("app/pkg/helper.py"), "def helper():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: String = conn
            .query_row(
                "SELECT COALESCE(to_path,'') FROM import_edges \
                 WHERE from_path = 'app/main.py' AND module_name = 'pkg.helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path, "app/pkg/helper.py",
            "dotted Python import resolves under the importing file's package root"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A script that prepends a sibling directory to `sys.path` before
    /// importing from it — the shape every runner under `benchmarks/` in this
    /// very repo uses. Without reading that insertion there is no path from
    /// `mcp_client` to `bench/lib/mcp_client.py` at all.
    #[test]
    fn test_python_import_resolves_through_sys_path_insert() {
        let dir = std::env::temp_dir().join(format!("ci_idx_pysys_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bench/runner")).unwrap();
        std::fs::create_dir_all(dir.join("bench/lib")).unwrap();
        std::fs::write(
            dir.join("bench/runner/run.py"),
            "import sys\nfrom pathlib import Path\n\
             sys.path.insert(0, str(Path(__file__).resolve().parents[1] / \"lib\"))\n\
             from mcp_client import Client\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("bench/lib/mcp_client.py"),
            "class Client:\n    pass\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: String = conn
            .query_row(
                "SELECT COALESCE(to_path,'') FROM import_edges \
                 WHERE from_path = 'bench/runner/run.py' AND module_name = 'mcp_client'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path, "bench/lib/mcp_client.py",
            "import resolves through the file's own sys.path.insert"
        );

        // The stdlib import in the same file must stay unresolved — the new
        // roots must not invent an in-project target for `sys`.
        let sys_to: String = conn
            .query_row(
                "SELECT COALESCE(to_path,'') FROM import_edges \
                 WHERE from_path = 'bench/runner/run.py' AND module_name = 'sys'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sys_to, "", "stdlib import must remain unresolved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CommonJS `require()` (real Node.js code, not just ES `import`) must
    /// feed `import_map`/`import_edges` exactly like `import ... from ...`
    /// does — see `indexer::imports::parse_js_require`.
    #[test]
    fn test_commonjs_require_cross_file_resolved_confidence() {
        let dir = std::env::temp_dir().join(format!("ci_idx_require_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("helper.js"),
            "function helper() {}\nmodule.exports = { helper };\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.js"),
            "const { helper } = require('./helper');\n\nfunction run() {\n    helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let (to_path, module): (String, String) = conn
            .query_row(
                "SELECT COALESCE(to_path,''), module_name FROM import_edges WHERE from_path = 'main.js'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(module, "./helper");
        assert_eq!(
            to_path, "helper.js",
            "require() target resolved to in-project file"
        );

        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.js::run' AND to_symbol = 'helper.js::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "resolved",
            "call through require() should be resolved, not textual"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for a real production bug found via a live QA pass on
    /// KARMA (a TS/NodeNext codebase): `"moduleResolution": "node16"` /
    /// `"nodenext"` / `"bundler"` requires source files to import a sibling
    /// `.ts` module using the *compiled-output* extension (`./runtime.js`
    /// referring to `runtime.ts` on disk) — before this fix, every import
    /// shaped like this failed to resolve at all (362/362 such edges had a
    /// NULL `to_path` in KARMA's real index), silently breaking
    /// `dependencies()`'s `imported_by` for any file imported this way.
    #[test]
    fn test_js_extension_import_resolves_to_ts_sibling() {
        let dir = std::env::temp_dir().join(format!("ci_idx_jsext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("runtime.ts"),
            "export class Runtime {\n    start() {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("index.ts"),
            "import { Runtime } from \"./runtime.js\";\n\nfunction main() {\n    new Runtime().start();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'index.ts' AND module_name = './runtime.js'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path.as_deref(),
            Some("runtime.ts"),
            "a `.js`-suffixed relative import must resolve to the real `.ts` source file"
        );

        let imported_by: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_edges WHERE to_path = 'runtime.ts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            imported_by, 1,
            "dependencies()'s imported_by relies on to_path being populated"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Go's own language rule: an import path whose FIRST element contains no
    /// dot is reserved for the standard library (everything else is a domain,
    /// `github.com/...`/`example.com/...`). Without that rule a bare `import
    /// "errors"` binds to whatever `errors.go` the project happens to have —
    /// measured on the real gin corpus, where `errors`/`path`/`context` all
    /// mis-resolved to gin's own same-named files, silently feeding the
    /// `dependencies` tool and the `[[boundaries]]` fitness gate a wrong edge.
    #[test]
    fn test_go_stdlib_import_does_not_bind_to_same_named_project_file() {
        let dir = std::env::temp_dir().join(format!("ci_idx_gostd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("helper")).unwrap();
        std::fs::write(dir.join("go.mod"), "module example.com/proj\n\ngo 1.21\n").unwrap();
        // A project file whose name collides with a stdlib package.
        std::fs::write(
            dir.join("errors.go"),
            "package proj\n\nfunc NewError() string {\n\treturn \"boom\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("helper/helper.go"),
            "package helper\n\nfunc Help() string {\n\treturn \"ok\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.go"),
            "package proj\n\nimport (\n\t\"errors\"\n\t\"example.com/proj/helper\"\n)\n\n\
             func run() error {\n\thelper.Help()\n\treturn errors.New(\"x\")\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let stdlib_target: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'main.go' AND module_name = 'errors'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stdlib_target, None,
            "Go stdlib `errors` bound to the project's own errors.go"
        );

        // The guard must be specific to stdlib, not a blanket "Go never resolves".
        let first_party: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'main.go' AND module_name = 'example.com/proj/helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            first_party.as_deref(),
            Some("helper/helper.go"),
            "a genuine in-module Go import must still resolve"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Maven/Gradle put sources under `src/main/java/<package path>`, so the
    /// generic project-root/`src/` guesses never find them and EVERY Java
    /// import stayed `to_path = NULL` — measured 0 of 22 genuinely first-party
    /// imports resolved on spring-petclinic. The package declaration inside
    /// each file is the layout-agnostic answer (it works for Gradle custom
    /// source sets and Bazel too, which a hardcoded `src/main/java` prefix
    /// would not). `dependencies`' `imported_by` and the `[[boundaries]]`
    /// fitness gate both read `to_path`.
    #[test]
    fn test_java_import_resolves_via_package_declaration_under_maven_layout() {
        let dir = std::env::temp_dir().join(format!("ci_idx_jvmpkg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pkg_dir = dir.join("src/main/java/org/example/app/model");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("Person.java"),
            "package org.example.app.model;\n\npublic class Person {\n\
             \tpublic String name() { return \"x\"; }\n}\n",
        )
        .unwrap();
        let svc_dir = dir.join("src/main/java/org/example/app");
        std::fs::write(
            svc_dir.join("Service.java"),
            "package org.example.app;\n\n\
             import org.example.app.model.Person;\n\
             import java.io.Serializable;\n\n\
             public class Service {\n\tpublic String go() { return new Person().name(); }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let first_party: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE module_name = 'org.example.app.model.Person'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            first_party.as_deref(),
            Some("src/main/java/org/example/app/model/Person.java"),
            "a first-party Java import must resolve through its package declaration"
        );

        // The JDK shares the `java`/`org` root segments with the source tree
        // itself (`src/main/java/org/...`), so a directory-name heuristic would
        // wrongly claim this one. Only a real package declaration can tell.
        let jdk: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges WHERE module_name = 'java.io.Serializable'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jdk, None, "a JDK import must not resolve to a project file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: `Type::method()` (a scoped-path call, no `.` receiver) must
    /// resolve *only* against `Type`, not fan out to every same-named symbol
    /// project-wide. Two structs (`StructA`, `StructB`) each define `fn new()`;
    /// `caller` calls `StructA::new()` (must resolve to exactly that one) and
    /// `HashMap::new()` (an external/undefined type in this fixture — must
    /// resolve to nothing at all, not to `StructA::new`/`StructB::new` via the
    /// old unscoped `by_name["new"]` fallback).
    #[test]
    fn test_type_path_call_resolves_scoped_not_fanned_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_typepath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct StructA;
impl StructA {
    fn new() -> Self {
        StructA
    }
}
",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct StructB;
impl StructB {
    fn new() -> Self {
        StructB
    }
}
",
        )
        .unwrap();
        std::fs::write(
            dir.join("c.rs"),
            "fn caller() {
    let _a = StructA::new();
    let _m = HashMap::new();
}
",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // Correctly scoped: caller -> StructA::new, and only StructA::new.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
            ),
            1,
            "StructA::new() must resolve to StructA's own new(), scoped via target_class"
        );
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "inferred",
            "type-path call is tier-2 inferred, not textual"
        );

        // Not fanned out: must NOT also point at StructB::new.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'b.rs')",
            ),
            0,
            "StructA::new() must not also resolve to the unrelated StructB::new()"
        );

        // HashMap::new() names an undefined type in this fixture — must resolve
        // to nothing (old behavior: fell back to matching every `new` in the
        // project, i.e. both StructA::new and StructB::new).
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') AND to_symbol LIKE '%StructB%'",
            ),
            0,
            "HashMap::new() must not resolve to any project symbol"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller')",
            ),
            1,
            "caller must have exactly one outgoing edge total (StructA::new only)"
        );

        // The call_sites row for HashMap::new() itself was correctly scoped to
        // "HashMap" (not left NULL/unscoped) — it just has no project-side match.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_sites WHERE callee_name = 'new' AND target_class = 'HashMap'",
            ),
            1,
            "HashMap::new() call site must be scoped to target_class='HashMap'"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression: tier-1's `file_symbols.contains(bare_name)` check matches
    /// on the callee name alone, with no idea a receiver type was ever named
    /// — so when the *caller's own file* happens to define something with
    /// the same bare name as an unrelated `Type::method()` call's method
    /// (extremely likely for "new"), tier-1 used to "resolve" first and
    /// short-circuit past the type-path scoping fix entirely (it only ran
    /// when tier-1 came back textual), reintroducing the exact fan-out bug
    /// for this specific, common case. `a.rs` defines its own `LocalType`
    /// with `fn new()` *and* calls `Vec::new()` in the same file — the
    /// local "new" must not cause `Vec::new()` to also match
    /// `OtherType::new()` defined in a completely different file.
    #[test]
    fn test_type_path_call_not_shadowed_by_same_file_bare_name_match() {
        let dir = std::env::temp_dir().join(format!("ci_idx_typepath2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct LocalType;\nimpl LocalType {\n    fn new() -> Self {\n        LocalType\n    }\n}\nfn caller() {\n    let _v: Vec<i32> = Vec::new();\n    let _l = LocalType::new();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct OtherType;\nimpl OtherType {\n    fn new() -> Self {\n        OtherType\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The intentional local call resolves correctly.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'a.rs')",
            ),
            1,
            "LocalType::new() must resolve to the local new(), scoped via target_class"
        );

        // Vec::new() must NOT fan out to OtherType::new() in b.rs just
        // because a.rs's own file_symbols happens to contain "new" too.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'new' AND path = 'b.rs')",
            ),
            0,
            "Vec::new() must not resolve to the unrelated OtherType::new() in b.rs"
        );

        // Exactly one outgoing edge total: the local LocalType::new() call.
        // (Vec::new() correctly resolves to nothing — Vec isn't a project type.)
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller')",
            ),
            1,
            "caller must have exactly one outgoing edge total"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the free-function (non-method) analog of the fan-out
    /// bug above: private same-named helpers in different files (the common
    /// `fn test_conn()` / `fn setup_db()` test-fixture pattern) must not fan
    /// out to each other just because they share a bare name — `by_name` in
    /// `rebuild_graph` has no per-file scoping, so before the same-file
    /// preference was added, every call to `helper()` got an edge to BOTH
    /// files' `helper()`, not just its own. The `Type::method()` fix above
    /// (`test_type_path_call_resolves_scoped_not_fanned_out`) never covered
    /// this plain by-name path, so the identical bug survived here.
    #[test]
    fn test_bare_call_prefers_same_file_over_global_fan_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_barefanout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "fn helper() -> i32 {\n    1\n}\nfn caller_a() {\n    let _ = helper();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "fn helper() -> i32 {\n    2\n}\nfn caller_b() {\n    let _ = helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_a') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            1,
            "caller_a's helper() must resolve to a.rs's own helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_a') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "caller_a's helper() must NOT also fan out to b.rs's unrelated helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_b') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            1,
            "caller_b's helper() must resolve to b.rs's own helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_b') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            0,
            "caller_b's helper() must NOT also fan out to a.rs's unrelated helper()"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WS2 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D1):
    /// `resolve_tier1`'s import-binding evidence (`resolved_path`) was
    /// computed and immediately discarded before this change -- only
    /// `.confidence` survived, so a bare imported call amid enough
    /// same-named decoys to exceed `MAX_CALLEE_CANDIDATES` (20) had no way
    /// to recover its real target once the static candidate algebra's
    /// unscoped by-name fallback gave up. This directly exercises the fix:
    /// 22 decoy `bar()` definitions (deliberately > 20) plus one real
    /// `from lib import bar` binding -- the resulting edge must still land
    /// on `lib.py`'s `bar`, not a decoy, and not vanish.
    #[test]
    fn test_import_path_narrows_candidates_past_max_callee_candidates() {
        let dir = std::env::temp_dir().join(format!("ci_idx_importpath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.py"), "def bar():\n    return 1\n").unwrap();
        std::fs::write(
            dir.join("caller.py"),
            "from lib import bar\n\n\ndef use():\n    return bar()\n",
        )
        .unwrap();
        for i in 0..22 {
            std::fs::write(
                dir.join(format!("decoy_{i}.py")),
                "def bar():\n    return 0\n",
            )
            .unwrap();
        }

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The capture itself: call_sites.import_path must carry the real
        // import target, not be silently dropped at extraction time.
        let import_path: String = conn
            .query_row(
                "SELECT import_path FROM call_sites WHERE from_path = 'caller.py' AND callee_name = 'bar'",
                [],
                |r| r.get(0),
            )
            .expect("caller.py's bar() call site must have a captured import_path");
        assert_eq!(
            import_path, "lib",
            "resolve_tier1's resolved_path must be 'lib', not dropped"
        );

        // The narrowing itself: the edge must target lib.py's bar, not any
        // decoy, and must not vanish (the pre-fix MAX_CALLEE_CANDIDATES
        // drop-to-nothing behavior).
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'use') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'bar' AND path = 'lib.py')",
            ),
            1,
            "use()'s bar() call must resolve to lib.py's bar via the import binding"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'use') \
                 AND to_symbol != (SELECT qualified_name FROM symbols WHERE name = 'bar' AND path = 'lib.py')",
            ),
            0,
            "use()'s bar() call must NOT fan out to any of the 22 decoys"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WS3 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D3,
    /// V3 Law 4 "unknown != nonexistent"): a call site whose surviving
    /// candidate set genuinely exceeds `MAX_CALLEE_CANDIDATES` (20) --
    /// no import binding, no module hint, no same-file/same-dir/same-
    /// namespace signal to narrow it, unlike
    /// `test_import_path_narrows_candidates_past_max_callee_candidates`
    /// above -- used to vanish as a silent zero-edge site. It must now
    /// surface as an `ambiguity_groups` row instead: 25 same-named free
    /// `helper()` definitions (deliberately > 20) plus one unqualified call
    /// to `helper()`.
    #[test]
    fn test_overflow_candidates_recorded_as_ambiguity_group_not_dropped_silently() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ambgroup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("caller.py"), "def use():\n    return helper()\n").unwrap();
        for i in 0..25 {
            std::fs::write(
                dir.join(format!("decoy_{i}.py")),
                "def helper():\n    return 0\n",
            )
            .unwrap();
        }

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The drop: 25 candidates, none narrowed by any positive scoping
        // signal, must NOT materialize as 25 (or any) call_edges rows --
        // this function's own doc comment explicitly rejects
        // materializing the full candidate set as edges.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'use')",
            ),
            0,
            "use()'s helper() call must produce zero edges, not fan out to all 25 decoys"
        );

        // The record: exactly one ambiguity_groups row, not silence.
        let (from_path, candidate_group_key, candidate_count, reason): (
            String,
            String,
            i64,
            String,
        ) = conn
            .query_row(
                "SELECT from_path, candidate_group_key, candidate_count, reason \
                 FROM ambiguity_groups",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect(
                "the dropped call site must be recorded in ambiguity_groups, not silently lost",
            );
        assert_eq!(from_path, "caller.py");
        assert_eq!(candidate_group_key, "helper");
        assert_eq!(
            candidate_count, 25,
            "must record the true candidate count, not a truncated one"
        );
        assert_eq!(reason, "unscoped_candidates_exceeded_max_callee_candidates");
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM ambiguity_groups"),
            1,
            "exactly one call site is ambiguous here -- must not duplicate or under-record"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.3 V1: Go's compilation unit is the directory (package = dir), and a
    // bare call like `Helper()` never carries a qualifier for module_hint to
    // key off — without the same-dir tier this falls straight through to
    // global fan-out (or an empty edge set once there are >MAX_CALLEE_CANDIDATES
    // same-named functions repo-wide).
    fn test_go_same_directory_call_resolves_not_fanned_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_go_samedir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkga")).unwrap();
        std::fs::create_dir_all(dir.join("pkgb")).unwrap();
        std::fs::write(
            dir.join("pkga/helper.go"),
            "package pkga\n\nfunc Helper() int {\n\treturn 1\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkga/caller.go"),
            "package pkga\n\nfunc CallerA() int {\n\treturn Helper()\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkgb/helper.go"),
            "package pkgb\n\nfunc Helper() int {\n\treturn 2\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkgb/caller.go"),
            "package pkgb\n\nfunc CallerB() int {\n\treturn Helper()\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'CallerA') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Helper' AND path = 'pkga/helper.go')",
            ),
            1,
            "CallerA's Helper() must resolve to its own directory's Helper"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'CallerA') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Helper' AND path = 'pkgb/helper.go')",
            ),
            0,
            "CallerA's Helper() must NOT fan out to pkgb's unrelated Helper()"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'CallerB') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Helper' AND path = 'pkgb/helper.go')",
            ),
            1,
            "CallerB's Helper() must resolve to its own directory's Helper"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // B2 (root-caused 2026-08-12, benchmarks/b2_call_graph_quality): a
    // `.`-receiver call whose type this indexer never tracks (e.g.
    // `row.get(0)` on a value that came back from a function call, with no
    // `let x: T` annotation for tier-2 to read) fell through
    // resolve_sites_to_edges's unscoped by-name fallback and confidently
    // resolved to whatever unrelated same-named function happens to be the
    // ONLY one of that name in the whole repo. Verified live against this
    // exact codebase: crates/calm-core/src/analysis/coverage.rs's own
    // `row.get(..)` calls (a real rusqlite::Row::get, receiver="row",
    // target_class NULL) were among 1114 false `textual` edges all pointing
    // at the unrelated crates/calm-core/src/txn.rs::get -- the only "get" in
    // the entire Rust symbol table -- which collapsed B2's inferred/
    // resolved/textual-tier precision to exactly 0.0 in CI.
    fn test_unresolved_receiver_method_call_does_not_fan_out_to_unrelated_same_named_function() {
        let dir = std::env::temp_dir().join(format!(
            "ci_idx_rust_receiver_fallback_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The one, unrelated, genuinely-local `get` in this fixture -- same
        // Result<Option<_>, _> shape as txn::get, so it would previously
        // survive the return-shape filter too, not just the bare-name one.
        std::fs::write(
            dir.join("txn.rs"),
            "pub fn get(id: &str) -> Result<Option<i32>, String> {\n    Ok(Some(1))\n}\n",
        )
        .unwrap();
        // `row`'s type comes from `fetch_row()`'s return value with no `let
        // row: T = ..` annotation -- not self/this, not a typed binding, so
        // tier-2 cannot infer a class for it and target_class stays None,
        // same as the real coverage.rs case this reproduces.
        std::fs::write(
            dir.join("reader.rs"),
            "fn read_first() -> Result<i32, String> {\n    let row = fetch_row();\n    row.get(0)\n}\n\nfn fetch_row() -> Row {\n    unimplemented!()\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'read_first') \
                   AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'get' AND path = 'txn.rs') \
                   AND edge_confidence != 'ambiguous'",
            ),
            0,
            "row.get(0) on an untracked-type receiver must NOT confidently resolve to the unrelated local txn.rs::get"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // Issue #72: a call explicitly rooted at an external crate
    // (`std::fs::write`) must NOT fall through the unscoped by-name fallback and
    // misresolve to an unrelated local same-named function (`txn.rs::write`).
    // The qualification itself proves it is not local. Exercised via the pure
    // syntactic pipeline (no SCIP overlay), which is where the misresolution
    // lived (SCIP masks it when a rust-analyzer index is available).
    fn test_external_crate_root_call_does_not_bind_to_local_same_named_fn() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ext_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The only local `write` in the fixture -- the decoy the std call used
        // to misbind to.
        std::fs::write(
            dir.join("txn.rs"),
            "pub fn write(data: &str) -> usize {\n    data.len()\n}\n",
        )
        .unwrap();
        // `run` calls ONLY std::fs::write -- there is no local write() call at
        // all, so any edge from `run` to txn.rs::write is the misresolution bug.
        std::fs::write(
            dir.join("main.rs"),
            "fn run() {\n    std::fs::write(\"/tmp/x\", \"hi\").unwrap();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE to_symbol = (SELECT qualified_name FROM symbols \
                                    WHERE name = 'write' AND path = 'txn.rs')",
            ),
            0,
            "std::fs::write (external crate root) must not bind to the unrelated local txn.rs::write"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.3 V1: a same-package (no-import-needed) qualified static call —
    // `Helper.greet()` — with a same-named class in an unrelated package's
    // directory must not fan out to that unrelated Helper.
    fn test_java_same_package_call_resolves_not_fanned_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_java_samedir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pkga")).unwrap();
        std::fs::create_dir_all(dir.join("pkgb")).unwrap();
        std::fs::write(
            dir.join("pkga/Helper.java"),
            "package pkga;\n\nclass Helper {\n    static int greet() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkga/Main.java"),
            "package pkga;\n\nclass Main {\n    static int callHelper() {\n        return Helper.greet();\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkgb/Helper.java"),
            "package pkgb;\n\nclass Helper {\n    static int greet() {\n        return 2;\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'callHelper') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND path = 'pkga/Helper.java')",
            ),
            1,
            "Main.callHelper()'s Helper.greet() must resolve to pkga's own Helper"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'callHelper') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND path = 'pkgb/Helper.java')",
            ),
            0,
            "Main.callHelper()'s Helper.greet() must NOT fan out to pkgb's unrelated Helper"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "lang-elixir")]
    // B3 (Tier B audit): `greet/1` and `greet/2` are different clauses, not
    // overloads of one symbol — before the arity gate in
    // resolve_sites_to_edges, both landed in the same `by_name["greet"]`
    // bucket and same_file's own bare-name match couldn't tell them apart
    // (both live in the same file here), so a 1-arg call site fanned out to
    // BOTH and got marked ambiguous instead of resolving to the one real
    // target. Single-file fixture on purpose: it's the harder case (unlike
    // Java's package/dir split above) since same_file itself is what used
    // to conflate them.
    fn test_elixir_arity_disambiguates_same_name_different_arity_clauses() {
        let dir = std::env::temp_dir().join(format!("ci_idx_elixir_arity_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("greeter.ex"),
            "defmodule Greeter do\n  def greet(name) do\n    \"Hi \" <> name\n  end\n\n  def greet(name, greeting) do\n    greeting <> name\n  end\n\n  def call_with_one_arg do\n    greet(\"world\")\n  end\n\n  def call_with_two_args do\n    greet(\"world\", \"Hello\")\n  end\nend\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'call_with_one_arg') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND arity = 1) \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "greet(\"world\") (1 arg) must resolve to the arity-1 clause, confidently"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'call_with_one_arg') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND arity = 2)",
            ),
            0,
            "greet(\"world\") (1 arg) must NOT fan out to the arity-2 clause"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'call_with_two_args') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND arity = 2) \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "greet(\"world\", \"Hello\") (2 args) must resolve to the arity-2 clause, confidently"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'call_with_two_args') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'greet' AND arity = 1)",
            ),
            0,
            "greet(\"world\", \"Hello\") (2 args) must NOT fan out to the arity-1 clause"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // A' pass (2026-07-29 self-audit): generalizes the B3 Elixir arity gate
    // to Go. `same_dir` (P1.3, checked BEFORE arity in the cascade) cannot
    // narrow this on its own since the two colliding `Greet`s live in two
    // DIFFERENT directories, neither matching the caller's own -- exactly
    // the case arity narrowing exists to cover once same_file/same_dir both
    // come up empty. Go can't have two same-named funcs in one package
    // (unlike Elixir's same-file multi-clause case above), so this needs 3
    // separate directories: caller's own (no Greet at all), and two
    // unrelated packages each defining a different-arity Greet.
    fn test_go_arity_disambiguates_cross_package_same_name_different_arity() {
        let dir = std::env::temp_dir().join(format!("ci_idx_go_arity_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("moda")).unwrap();
        std::fs::create_dir_all(dir.join("modb")).unwrap();
        std::fs::create_dir_all(dir.join("caller")).unwrap();
        std::fs::write(
            dir.join("moda/greet.go"),
            "package moda\nfunc Greet(name string) string {\n\treturn \"Hi \" + name\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("modb/greet.go"),
            "package modb\nfunc Greet(name, greeting string) string {\n\treturn greeting + name\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller/main.go"),
            "package main\nfunc main() {\n\tGreet(\"world\")\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND path = 'moda/greet.go') \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "Greet(\"world\") (1 arg) must resolve confidently to moda's arity-1 Greet"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND path = 'modb/greet.go')",
            ),
            0,
            "Greet(\"world\") (1 arg) must NOT fan out to modb's arity-2 Greet"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // A' pass: a variadic candidate must accept a call with MORE than its own
    // minimum arity, while an unrelated same-named candidate with a
    // DIFFERENT exact arity that doesn't match the call's real arg count
    // stays correctly excluded -- proves the gate does `n >= min_arity` for
    // a variadic candidate, not a plain `==` (which would wrongly exclude
    // this legitimate variadic match too).
    fn test_go_arity_variadic_candidate_accepts_more_than_minimum_args() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_go_arity_variadic_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("moda")).unwrap();
        std::fs::create_dir_all(dir.join("modb")).unwrap();
        std::fs::create_dir_all(dir.join("caller")).unwrap();
        std::fs::write(
            dir.join("moda/greet.go"),
            "package moda\nfunc Greet(prefix string, rest ...string) string {\n\treturn prefix\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("modb/greet.go"),
            "package modb\nfunc Greet(a, b string) string {\n\treturn a + b\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller/main.go"),
            "package main\nfunc main() {\n\tGreet(\"x\", \"y\", \"z\")\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND path = 'moda/greet.go') \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "Greet(\"x\", \"y\", \"z\") (3 args) must resolve to moda's variadic Greet (min arity 1)"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND path = 'modb/greet.go')",
            ),
            0,
            "3 args must NOT match modb's fixed arity-2 Greet"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WS5 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D5):
    /// UPDATED (2026-08-19) from its original P1.3 V1 shape, which asserted
    /// `modb::helper` must NOT co-surface at all. That original assertion
    /// and this plan's own flagship WS5 fixture
    /// (`benchmarks/resolution_precision/fixtures/E_same_dir_decoy_vs_true_target`)
    /// are the IDENTICAL structural shape -- a bare call, one same-directory
    /// definition, one definition in a sibling directory, no other
    /// distinguishing signal -- just with the two directories' roles
    /// swapped in the narrative (this test called the local one "the real
    /// one" and the sibling "unrelated"; the fixture calls the local one
    /// "a decoy" and the sibling "the real target"). CALM's resolver cannot
    /// structurally tell those two narratives apart: C/C++ have no
    /// package/namespace-to-directory correspondence at all (unlike Go/Java,
    /// where an unqualified same-directory reference is real, compiler-
    /// enforced scoping -- see this function's own comment at the
    /// `same_dir_is_real_scoping` check, and the still-hard-filtered
    /// `test_go_same_directory_call_resolves_not_fanned_out`/
    /// `test_java_same_package_call_resolves_not_fanned_out` above). So for
    /// C specifically, "confidently exclude the sibling-directory
    /// candidate" was never actually justified by anything more than
    /// "usually true" -- exactly the D5 defect (same_dir as a destructive
    /// filter) this plan sets out to fix. The corrected, intentional
    /// behavior: BOTH candidates survive as `ambiguous`, with the same-
    /// directory one at `candidate_rank = 0` (preferred for ordering) and
    /// the sibling-directory one at `candidate_rank = 1` -- non-destructive,
    /// not a silent loss, not a false confident pick either way.
    #[test]
    fn test_c_same_directory_call_ranks_local_first_but_keeps_sibling_candidate() {
        let dir = std::env::temp_dir().join(format!("ci_idx_c_samedir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("moda")).unwrap();
        std::fs::create_dir_all(dir.join("modb")).unwrap();
        std::fs::write(
            dir.join("moda/helper.c"),
            "int helper(void) {\n    return 1;\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("moda/main.c"),
            "int helper(void);\nint caller_a(void) {\n    return helper();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("modb/helper.c"),
            "int helper(void) {\n    return 2;\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let edges: Vec<(String, i64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT to_path, candidate_rank, edge_confidence FROM call_edges \
                     WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller_a')",
                )
                .unwrap();
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        };
        assert_eq!(
            edges.len(),
            2,
            "both moda's and modb's helper() must survive as candidates, not be \
             dropped or collapsed to one: got {edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|(p, rank, _)| p == "moda/helper.c" && *rank == 0),
            "moda's own helper() (same directory as the caller) must be rank 0 \
             (preferred): got {edges:?}"
        );
        assert!(
            edges
                .iter()
                .any(|(p, rank, _)| p == "modb/helper.c" && *rank == 1),
            "modb's sibling-directory helper() must survive at rank 1 (alternate), \
             not be silently dropped: got {edges:?}"
        );
        assert!(
            edges.iter().all(|(_, _, c)| c == "ambiguous"),
            "neither candidate has real scoping evidence over the other (C has no \
             package/namespace concept) -- both must be ambiguous, not one falsely \
             confident: got {edges:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cross-language false callee: a bare-name fallback (nothing in the
    /// caller's own file matches, so the global by-name fan-out kicks in)
    /// must never resolve to a same-named symbol written in a DIFFERENT
    /// language — that's never a real call, just an incidental name
    /// collision (e.g. Python `helper` and Rust `helper` sharing a bare
    /// name). Regression for the missing same-language filter in
    /// `rebuild_graph`'s candidate lookup, which used to fan out to every
    /// same-named symbol in the whole multi-language repo.
    #[test]
    fn test_same_language_filter_excludes_cross_language_false_callee() {
        let dir = std::env::temp_dir().join(format!("ci_idx_crosslang_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `main` has no same-named `helper` in its own file, so resolution
        // falls through to the global by-name fan-out fallback — exactly the
        // path that never filtered by language before this fix.
        std::fs::write(dir.join("a.py"), "def main():\n    helper()\n").unwrap();
        std::fs::write(dir.join("c.py"), "def helper():\n    pass\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn helper() {}\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'c.py')",
            ),
            1,
            "main()'s helper() must resolve to the same-language (Python) helper in c.py"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "main()'s helper() must NEVER resolve to the unrelated Rust helper() in b.rs"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'b.rs::helper'",
            ),
            0,
            "the Rust helper() must show zero callers — it was never actually called"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same-language, cross-module false callee (the "odra vs soroban" case):
    /// two unrelated types in DIFFERENT files (same language) share a method
    /// name (`execute`), and the caller reaches one of them through a typed
    /// struct FIELD (`self.engine.execute()`), not a local variable. Split
    /// across three files so tier-1's same-file `file_symbols` match (which
    /// takes priority over tier-2 whenever it fires) never fires here —
    /// `execute` is defined in neither odra.rs nor soroban.rs's caller file
    /// (main.rs), forcing resolution through tier-2's receiver-type lookup,
    /// same as the real cross-module case this bug was found in. Before the
    /// field-type-map fix, struct fields were invisible to `type_map`, so
    /// tier-2 had no receiver type to key off of and fell back to the global
    /// by-name fan-out — matching both `execute` methods (marked
    /// `ambiguous`) instead of resolving to the one the field is actually
    /// declared as.
    #[test]
    fn test_field_type_map_resolves_same_language_cross_module_method_call() {
        let dir = std::env::temp_dir().join(format!("ci_idx_fieldtypemap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("odra.rs"),
            "pub struct OdraEngine;\nimpl OdraEngine {\n    pub fn execute(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("soroban.rs"),
            "pub struct SorobanEngine;\nimpl SorobanEngine {\n    pub fn execute(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.rs"),
            "struct Odra {\n    engine: OdraEngine,\n}\n\
             impl Odra {\n    fn run(&self) {\n        self.engine.execute();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'execute' AND class_context = 'OdraEngine')",
            ),
            1,
            "self.engine.execute() must resolve to OdraEngine::execute via the field's declared type"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'execute' AND class_context = 'SorobanEngine')",
            ),
            0,
            "self.engine.execute() must NOT also fan out to the unrelated SorobanEngine::execute"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run')",
            ),
            1,
            "exactly one call edge from run() — not fanned out/marked ambiguous"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.4: `shape->area()` — C/C++'s pointer-member-access form. Regression
    // test for the `split_receiver_callee` gap this session found: `->` was
    // never recognized at all (only `.`/`::`), so this call previously
    // extracted callee="shape" (the receiver text, truncated at the first
    // non-ident byte) with no receiver, not callee="area" with receiver
    // "shape" — meaning C/C++ member calls via `->` never had a chance to
    // reach Tier-2 at all, regardless of type_map support.
    fn test_cpp_pointer_member_call_resolves_via_field_type() {
        let dir = std::env::temp_dir().join(format!("ci_idx_cpp_typemap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("circle.cpp"),
            "struct Circle {\n    double area() { return 1.0; }\n};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("square.cpp"),
            "struct Square {\n    double area() { return 2.0; }\n};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.cpp"),
            "struct Container {\n    Circle *shape;\n    void run() {\n        shape->area();\n    }\n};\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'area' AND class_context = 'Circle')",
            ),
            1,
            "shape->area() must resolve to Circle::area via the field's declared pointer type"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'area' AND class_context = 'Square')",
            ),
            0,
            "shape->area() must NOT also fan out to the unrelated Square::area"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // Regression: a real ~4700-line redis header (server.h) crashed `calm
    // index` outright with "UNIQUE constraint failed: symbols.qualified_name"
    // -- found by the resolution benchmark on a real external C repo, not a
    // synthetic fixture. Root cause: a forward-declared struct type
    // mentioned as a parameter type in a function-pointer typedef (e.g.
    // `struct redisObject *`) is extracted as a "symbol" occurrence at that
    // mention's line, and C headers routinely mention the same struct type
    // as *two different parameters on the same line* (e.g. a copy(from, to)
    // style signature) -- producing 3+ symbols sharing the exact same
    // (name, line_start). The old dedup only tried one `#{line}` suffix,
    // which collided right back for the 3rd+ occurrence and left an
    // unhandled INSERT error to abort the whole indexing run. This does not
    // fix the over-eager extraction (a type *reference* still isn't a real
    // symbol) -- it only ensures the dedup loop can never crash on it.
    fn test_c_same_line_triple_name_collision_does_not_crash_indexing() {
        let dir = std::env::temp_dir().join(format!("ci_idx_c_n_way_dup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("test.h"),
            "typedef void (*fn1)(struct Foo *a);\n\
             typedef void (*fn2)(struct Foo *a);\n\
             typedef void (*fn3)(struct Foo *a);\n\
             typedef void (*fn4)(struct Foo *a);\n\
             typedef void (*fn5)(struct Foo *a, struct Foo *b);\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase())
            .expect("must not crash on a same-line, same-name symbol collision");

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name LIKE 'test.h::Foo%'"
            ),
            6,
            "all 6 Foo-named occurrences must land as distinct rows, not be dropped or crash"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.5: exercises both C# type_map paths — an explicitly-typed field
    // (`Circle shape;`) and `var`-with-constructor-inference (`var s = new
    // Circle();`) — each resolving `.Area()` to the right class by declared
    // type, not fanning out to the unrelated same-named-method Square.
    fn test_csharp_field_type_and_var_ctor_resolve_via_declared_type() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_csharp_typemap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("circle.cs"),
            "class Circle {\n    double Area() { return 1.0; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("square.cs"),
            "class Square {\n    double Area() { return 2.0; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.cs"),
            "class Container {\n\
             \x20   Circle shape;\n\
             \x20   void RunField() {\n\
             \x20       shape.Area();\n\
             \x20   }\n\
             \x20   void RunVar() {\n\
             \x20       var s = new Circle();\n\
             \x20       s.Area();\n\
             \x20   }\n\
             }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        for (caller, wrong_count_desc) in [
            ("RunField", "shape.Area()"),
            ("RunVar", "s.Area() (var-inferred)"),
        ] {
            assert_eq!(
                count(
                    &conn,
                    &format!(
                        "SELECT COUNT(*) FROM call_edges \
                         WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = '{caller}') \
                         AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Area' AND class_context = 'Circle')",
                    ),
                ),
                1,
                "{wrong_count_desc} must resolve to Circle::Area via the declared type"
            );
            assert_eq!(
                count(
                    &conn,
                    &format!(
                        "SELECT COUNT(*) FROM call_edges \
                         WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = '{caller}') \
                         AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Area' AND class_context = 'Square')",
                    ),
                ),
                0,
                "{wrong_count_desc} must NOT also fan out to the unrelated Square::Area"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.5 remainder (8-language plan, "using -> namespace"), both parts:
    // `Helper.Greet()` is a type-qualified static call — C# has no separate
    // static-access operator, so `receiver_is_type_path` never fires on the
    // `.` branch shared by every language, and tier-2 tried "Helper" as a
    // *variable* name (it isn't one) and missed. Before part A's fix the
    // call fell through to `rebuild_graph`'s unscoped `by_name` fan-out on
    // the bare method name alone: two same-named methods anywhere in the C#
    // codebase (Helper.Greet and Other.Greet here) meant `Ambiguous`, not a
    // correct single edge. Part B's `NamespaceMap` then confirms Helper is
    // really declared in the `using`d `MultiLang` namespace, upgrading the
    // edge from `inferred` (part A alone) to `resolved`.
    fn test_csharp_type_qualified_static_call_resolves_via_target_class() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_csharp_typepath_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("helper.cs"),
            "namespace MultiLang\n{\n    public static class Helper\n    {\n        public static string Greet(string name) { return name; }\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("other.cs"),
            "namespace Elsewhere\n{\n    public static class Other\n    {\n        public static string Greet(string name) { return name; }\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("program.cs"),
            "using MultiLang;\n\nclass Program\n{\n    static void Main()\n    {\n        System.Console.WriteLine(Helper.Greet(\"world\"));\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND class_context = 'Helper') \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "Helper.Greet() must resolve to Helper::Greet via target_class scoping; confidence \
             is 'resolved' (not just 'inferred') because `using MultiLang;` plus a real \
             `namespace MultiLang` declaration on Helper's file confirms the match — the \
             same-namespace narrowing this test's `program.cs` `using` line is meant to exercise"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Greet' AND class_context = 'Other')",
            ),
            0,
            "must NOT also fan out to the unrelated Other::Greet just because both are named Greet"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND edge_confidence = 'ambiguous'",
            ),
            0,
            "must not be Ambiguous — target_class scoping should have picked exactly one candidate"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // PR#8 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    // Part E): the actual adversarial case target_type_kind/target_type_qn
    // exist to fix. TWO classes both named `User`, in different Java
    // packages/directories (com.foo.User, com.bar.User), each declaring a
    // same-named method. by_name_class's (callee, "User") key alone cannot
    // tell them apart -- before this slice, both candidates would survive
    // and the edge would be marked Ambiguous (or worse, silently pick
    // whichever narrowing heuristic like same_dir happened to fire, for
    // reasons unrelated to the receiver's REAL declared type). Caller.java
    // imports com.foo.User specifically and calls it through a typed
    // variable (tier-2, ctx.type_map) -- target_type_kind should resolve to
    // 'resolved_import' with target_type_qn "com.foo.User", which
    // type_qn_matches_path then matches against the com/foo/User.java
    // candidate's path (dots-to-slashes) and NOT the com/bar/User.java one.
    fn test_java_cross_package_bare_name_collision_resolves_via_target_type_qn() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_java_typequalify_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("com/foo")).unwrap();
        std::fs::create_dir_all(dir.join("com/bar")).unwrap();
        std::fs::write(
            dir.join("com/foo/User.java"),
            "package com.foo;\npublic class User {\n    public String getName() { return \"foo\"; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("com/bar/User.java"),
            "package com.bar;\npublic class User {\n    public String getName() { return \"bar\"; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Caller.java"),
            "import com.foo.User;\n\nclass Caller {\n    void run(User u) {\n        u.getName();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'getName' \
                     AND path LIKE '%com/foo/User.java')",
            ),
            1,
            "u.getName() must resolve to com.foo.User::getName -- the imported User, not the unrelated com.bar.User"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'getName' \
                     AND path LIKE '%com/bar/User.java')",
            ),
            0,
            "must NOT also fan out to the unrelated com.bar.User::getName just because both classes are bare-named User"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND edge_confidence = 'ambiguous'",
            ),
            0,
            "must not be Ambiguous -- target_type_qn scoping should have picked exactly the imported User"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.5 remainder, the actual disambiguation case the "using -> namespace"
    // gap was closed for: TWO classes named `Helper` exist, in different
    // namespaces — `by_name_class` alone can't tell them apart (its key is
    // the bare class name, "Helper", not a namespace-qualified one). Without
    // `NamespaceMap`, both would survive candidate narrowing and the edge
    // would be marked `Ambiguous`. With it, the caller's `using MultiLang;`
    // picks out exactly the Helper declared in that namespace.
    fn test_csharp_same_class_name_in_different_namespaces_disambiguated_by_using() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_csharp_ns_collision_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("multilang_helper.cs"),
            "namespace MultiLang\n{\n    public static class Helper\n    {\n        public static string Greet(string name) { return name; }\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("elsewhere_helper.cs"),
            "namespace Elsewhere\n{\n    public static class Helper\n    {\n        public static string Greet(string name) { return \"nope\"; }\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("program.cs"),
            "using MultiLang;\n\nclass Program\n{\n    static void Main()\n    {\n        System.Console.WriteLine(Helper.Greet(\"world\"));\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND to_symbol = 'multilang_helper.cs::Helper::Greet' \
                 AND edge_confidence = 'resolved'",
            ),
            1,
            "must resolve to the MultiLang.Helper.Greet the `using MultiLang;` actually named, \
             confidence resolved (namespace-confirmed)"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND to_symbol = 'elsewhere_helper.cs::Helper::Greet'",
            ),
            0,
            "must NOT also resolve to Elsewhere.Helper.Greet — it was never `using`d"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'Main') \
                 AND edge_confidence = 'ambiguous'",
            ),
            0,
            "must not be Ambiguous — the using-confirmed namespace should have broken the tie"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.5 remainder: `import_edges.to_path` (the `dependencies` tool's data
    // source) resolves a `using X;` to the single file declaring namespace
    // X, and stays NULL when the namespace spans 2+ files — `to_path` is a
    // single-valued column, so an ambiguous namespace intentionally gets no
    // guess rather than an arbitrary one (see `NamespaceMap::resolve`).
    fn test_csharp_using_resolves_import_edge_to_path_when_unambiguous() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_csharp_to_path_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("helper.cs"),
            "namespace MultiLang\n{\n    public static class Helper {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("shared_a.cs"),
            "namespace Shared\n{\n    public class A {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("shared_b.cs"),
            "namespace Shared\n{\n    public class B {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("program.cs"),
            "using MultiLang;\nusing Shared;\n\nclass Program {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let unambiguous_to_path: String = conn
            .query_row(
                "SELECT to_path FROM import_edges WHERE from_path = 'program.cs' AND module_name = 'MultiLang'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unambiguous_to_path, "helper.cs");

        let ambiguous_to_path: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges WHERE from_path = 'program.cs' AND module_name = 'Shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ambiguous_to_path, None,
            "Shared spans 2 files — to_path must stay NULL, not guess one"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.2 step 1 (pipeline-level): PHP's `Foo::bar()` (scoped_call_expression)
    // resolves via receiver_is_type_path exactly like Rust's `Type::method()` —
    // no type_map needed for this shape, since the receiver names the class
    // directly in the source text. Two classes sharing a method name in
    // different files must not fan out.
    fn test_php_scoped_call_resolves_via_type_path_not_fanned_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_php_scoped_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("foo.php"),
            "<?php\nclass Foo {\n    static function bar() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("baz.php"),
            "<?php\nclass Baz {\n    static function bar() {\n        return 2;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.php"),
            "<?php\nclass Caller {\n    function run() {\n        Foo::bar();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'bar' AND class_context = 'Foo')",
            ),
            1,
            "Foo::bar() must resolve to Foo's own bar() via the scoped type path"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'bar' AND class_context = 'Baz')",
            ),
            0,
            "Foo::bar() must NOT also fan out to the unrelated Baz::bar()"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.2 step 3: `use App\Service\Foo;` resolves to a real file only via
    // PSR-4 (composer.json's autoload.psr-4) — the generic dotted-module
    // scan `resolve_module_to_path` uses for other languages doesn't even
    // split on PHP's `\` separator, so this import_edge would otherwise
    // never get a `to_path` at all.
    fn test_php_psr4_resolves_use_import_to_real_file() {
        let dir = std::env::temp_dir().join(format!("ci_idx_php_psr4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/Service")).unwrap();
        std::fs::write(
            dir.join("composer.json"),
            r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("src/Service/Foo.php"),
            "<?php\nnamespace App\\Service;\nclass Foo {\n    static function bar() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("index.php"),
            "<?php\nuse App\\Service\\Foo;\nclass Caller {\n    function run() {\n        Foo::bar();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: String = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'index.php' AND module_name = 'App\\Service\\Foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            to_path, "src/Service/Foo.php",
            "PSR-4 must resolve the App\\ prefix to src/, landing on the real file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_js_require_namespace_object_property_call_resolves_via_module_hint() {
        // Regression test for the real, reproducible gap found in
        // benchmarks/b7_task_correctness/README.md (express's `test/utils.js`):
        // `utils.setCharset(...)` after `var utils = require('./lib/utils')`
        // was completely invisible to `callers()`/`blast_radius` -- tier-1
        // (extract_file_data) only checks `ctx.import_map` keyed by the CALLEE
        // name, tier-2 only checks `ctx.type_map` (a real type annotation) or
        // self/this, so a call whose RECEIVER is itself a whole-module import
        // binding had no signal at all. See extract_file_data's
        // receiver-import-alias branch (module_path_last_segment).
        //
        // Fixture deliberately includes a same-named DECOY (`other/helpers.js`
        // also exports `setCharset`) with a DIFFERENT file basename than the
        // required module ("helpers" vs "utils") -- without the fix, both
        // candidates survive `resolve_sites_to_edges`'s unscoped by-name
        // fallback (JS gets no same_dir narrowing) and the edge comes out
        // `Ambiguous` with 2 targets; the fix's module_hint ("utils", from the
        // require path) should filter the decoy out by file-stem mismatch,
        // leaving exactly one edge to the real target.
        let dir = std::env::temp_dir().join(format!("ci_idx_js_require_ns_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        std::fs::write(
        dir.join("lib/utils.js"),
        "exports.setCharset = function (type, charset) {\n    return type + '; charset=' + charset;\n};\n",
    )
    .unwrap();
        std::fs::write(
            dir.join("other/helpers.js"),
            "exports.setCharset = function (type, charset) {\n    return 'decoy';\n};\n",
        )
        .unwrap();
        std::fs::write(
        dir.join("caller.js"),
        "var utils = require('./lib/utils');\nfunction run() {\n    utils.setCharset('text/html', 'utf-8');\n}\n",
    )
    .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT to_symbol, edge_confidence FROM call_edges \
             WHERE from_symbol = 'caller.js::run' AND to_symbol LIKE '%setCharset'",
            )
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(
            rows,
            vec![(
                "lib/utils.js::setCharset".to_string(),
                "inferred".to_string()
            )],
            "utils.setCharset(...) via `var utils = require(...)` must resolve to exactly \
         lib/utils.js::setCharset (not the other/helpers.js decoy, not Ambiguous): {rows:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_call_from_a_file_with_no_named_symbols_still_gets_a_call_edge() {
        // Regression test for the real, reproducible gap found via a live
        // follow-up investigation of the express test/utils.js miss above
        // (see docs/plans/2026-08-20-product-uplift-and-b7v2-roadmap.md §8).
        // `build_resolution_context`'s `path_lang` map used to be derived
        // ONLY from the `symbols` table, so a file with ZERO top-level named
        // functions/classes anywhere in it -- the ordinary shape of a JS/TS
        // test file written in the standard `describe`/`it`/`test`
        // nested-anonymous-callback style -- never got a `path_lang` entry
        // at all. `resolve_sites_to_edges`'s same-language safety filter
        // (`Some(candidate_lang) == ctx.path_lang.get(caller_path)`) then
        // came out `Some(_) == None`, always false, silently emptying every
        // outgoing candidate list for every call that file made --
        // independent of confidence tier, call shape, or nesting depth (the
        // test right above this one only ever exercised a file that DOES
        // have a named function, so it could not catch this).
        //
        // Fixture: a real two-level Mocha `describe(){ it(){ ... } }` nest
        // (no named function anywhere in the file) making a single bare,
        // destructured-import call -- the simplest shape that exercises the
        // bug, isolated live via 8 controlled repro fixtures (varying
        // nesting depth, receiver shape, and same-file vs cross-file target
        // one variable at a time) before this test was written, not
        // guessed. `test/utils.js` here deliberately mirrors the real
        // express corpus's own file almost verbatim.
        let dir =
            std::env::temp_dir().join(format!("ci_idx_js_no_named_symbols_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(
            dir.join("lib/utils.js"),
            "exports.setCharset = function setCharset(type, charset) {\n    return type;\n};\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("test/utils.js"),
            "var { setCharset } = require('../lib/utils');\n\
             describe('utils.setCharset', function () {\n  \
             it('does a thing', function () {\n    \
             setCharset('text/html', 'utf-8');\n  \
             });\n\
             });\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT to_symbol, edge_confidence FROM call_edges \
                 WHERE from_path = 'test/utils.js' AND to_symbol LIKE '%setCharset'",
            )
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(
            rows,
            vec![(
                "lib/utils.js::setCharset".to_string(),
                "resolved".to_string()
            )],
            "a call made only inside nested anonymous describe/it callbacks (no named \
             function anywhere in the caller's own file) must still produce a real \
             call_edges row, not silently zero: {rows:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.2 end-to-end DoD: all 4 steps together on one small PHP project —
    // require_once resolves its import_edge; a typed property's
    // `$this->helper->run()` resolves via tier-2 (confidence "inferred") to
    // the right class without fanning out to an unrelated same-named
    // method; `use` + PSR-4 resolves a namespaced class's import_edge and
    // its `Foo::bar()` scoped call.
    fn test_php_p1_2_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ci_idx_php_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/Service")).unwrap();
        std::fs::write(
            dir.join("composer.json"),
            r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("helper.php"),
            "<?php\nclass Helper {\n    function run() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("other.php"),
            "<?php\nclass Other {\n    function run() {\n        return 2;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/Service/Foo.php"),
            "<?php\nnamespace App\\Service;\nclass Foo {\n    static function make() {\n        return 1;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.php"),
            "<?php\n\
             require_once 'helper.php';\n\
             use App\\Service\\Foo;\n\
             class Container {\n\
             \x20   private Helper $helper;\n\
             \x20   function m() {\n\
             \x20       $this->helper->run();\n\
             \x20       Foo::make();\n\
             \x20   }\n\
             }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // Step 2/3: require_once and use+PSR-4 both resolve their import_edge.
        let require_to_path: String = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'main.php' AND module_name = './helper.php'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(require_to_path, "helper.php");
        let use_to_path: String = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'main.php' AND module_name = 'App\\Service\\Foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(use_to_path, "src/Service/Foo.php");

        // Step 1+4: $this->helper->run() resolves via the typed property's
        // type_map to Helper::run specifically (not the unrelated Other::run),
        // at "inferred" confidence (tier-2).
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'm') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND class_context = 'Helper')",
            ),
            1,
            "$this->helper->run() must resolve to Helper::run via the typed property"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'm') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND class_context = 'Other')",
            ),
            0,
            "must NOT also fan out to the unrelated Other::run"
        );
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'm') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND class_context = 'Helper')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(confidence, "inferred");

        // Step 1: Foo::make() (use+PSR-4-imported, scoped call) resolves too.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'm') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'make' AND class_context = 'Foo')",
            ),
            1,
            "Foo::make() must resolve via the scoped type path"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // P1.1: the JS stack-graphs formal resolver is wired into the real
    // indexing pipeline (not just FormalResolver::resolve_file in
    // isolation, already covered in resolver/formal.rs's own tests) — a
    // simple def/ref pair in a .js file produces a real call_edges row, and
    // if the formal tier is what produced it (edge_confidence='formal'),
    // formal_source must say so.
    //
    // Note on scope: this repo's own `extract_symbols` captures every
    // same-file function declaration into a flat `file_symbols` name set
    // regardless of nesting depth, so an intra-file call to another
    // declared function already resolves at tier-1 ("resolved") before
    // stack-graphs is ever consulted — unlike TypeScript/Python's own
    // formal-tier tests, JS's `builtins.js` (upstream) ships empty, so
    // there's no builtin-call case available to force a genuine
    // textual->formal transition the way `Array.isArray` does for TS. This
    // test therefore checks integration (the edge exists, confidence is at
    // least "resolved", and IF formal then formal_source is stack_graphs),
    // mirroring `test_formal_tier_upgrades_textual_python_call`'s own
    // pragmatic scope for the same reason.
    fn test_javascript_formal_resolver_wired_into_pipeline() {
        let dir = std::env::temp_dir().join(format!("ci_idx_js_formal_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.js"),
            "function helper() {\n    return 1;\n}\n\nfunction run() {\n    return helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
            ),
            1,
            "run() -> helper() must produce exactly one call edge"
        );
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            matches!(confidence.as_str(), "resolved" | "formal"),
            "expected 'resolved' or 'formal', got: {confidence}"
        );
        if confidence == "formal" {
            let formal_source: Option<String> = conn
                .query_row(
                    "SELECT formal_source FROM call_edges \
                     WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(formal_source.as_deref(), Some("stack_graphs"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // 2026-08-18 (B15 cross-language competitor A/B investigation): a
    // formal_parameter-typed Java receiver calling a method it only
    // INHERITS from a superclass (declared on `Base`, not on the
    // parameter's own declared class `Sub`) used to produce ZERO call
    // edges -- `resolve_sites_to_edges`'s `by_name_class` lookup is keyed
    // on (callee, EXACT declaring class) with no superclass walk, and an
    // exact-key miss short-circuited straight to "no candidates" instead
    // of falling back to the unscoped `by_name` lookup an UNKNOWN-type
    // receiver already gets. Verified live against spring-petclinic
    // (external, real corpus): `getName()`/`isNew()` declared on
    // `NamedEntity`/`BaseEntity` were silently missing from `callers()`
    // when called through a `Pet`-typed method parameter, while an
    // identical call through a same-type LOCAL variable (no tracked
    // static type) correctly fell back to an `ambiguous` edge -- CALM
    // knowing MORE about the receiver's type made it strictly worse at
    // finding the edge, the opposite of the intended tiered-confidence
    // design. This minimal 2-class repro isolates that exact shape.
    fn test_java_formal_parameter_resolves_inherited_superclass_method() {
        let dir = std::env::temp_dir().join(format!("ci_idx_java_inherit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Base.java"),
            "public class Base {\n    public String getName() {\n        return null;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("Sub.java"), "public class Sub extends Base {\n}\n").unwrap();
        std::fs::write(
            dir.join("Caller.java"),
            "public class Caller {\n    void m(Sub sub) {\n        sub.getName();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%Caller::m' AND to_symbol LIKE '%Base::getName'",
            ),
            1,
            "Caller.m()'s call to sub.getName() (inherited from Base, not declared \
             on Sub) must still produce a call edge -- an exact-class miss on \
             by_name_class must fall back to the unscoped by_name lookup, not drop \
             the edge outright"
        );

        // WS4 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
        // D4, this fixture is the plan's own cited test case): the edge
        // above used to land at `ambiguous` confidence -- the "18/8 fix"
        // only prevented the edge from vanishing entirely, by falling back
        // to the SAME unscoped `by_name` path an unknown-receiver-type call
        // gets, with no way to tell "genuinely no scoping evidence" apart
        // from "the evidence was right there in type_relations, just never
        // consulted." `Sub extends Base` is a `resolved`-confidence
        // `type_relations` row (same-file AST match), so
        // `resolve_via_inheritance_closure` now finds `Base::getName` as
        // the sole candidate BEFORE the unscoped fallback ever runs --
        // this is real scoping evidence (Java's own inheritance rules), not
        // a guess, and must not be downgraded to `ambiguous`.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol LIKE '%Caller::m' AND to_symbol LIKE '%Base::getName'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(
            confidence, "ambiguous",
            "the inherited-method edge must resolve via the inheritance closure, not \
             fall through to the unscoped-fallback's ambiguous downgrade"
        );
        assert!(
            matches!(confidence.as_str(), "resolved" | "inferred"),
            "expected a real resolved/inferred edge to the ancestor declaration \
             (WS4's own acceptance criterion), got {confidence:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WS4 companion (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
    /// plan's own second test-plan bullet: "an interface with 2 implementors
    /// -> assert ... not a single wrong pick"): `Mixed` extends BOTH `IA`
    /// and `IB`, and BOTH declare `foo()` -- a genuine tie at inheritance
    /// depth 1, not a nearer-ancestor-wins case like the Base/Sub test
    /// above. `resolve_via_inheritance_closure` must refuse to pick
    /// whichever of `IA`/`IB` happened to come first in `Mixed`'s own
    /// `extends` clause -- that would be exactly the unscoped guess WS4
    /// exists to avoid, just relocated one level deeper. The call must fall
    /// through unchanged to the pre-WS4 unscoped `by_name` behavior
    /// (`ambiguous`), proving WS4 only resolves genuinely unique evidence,
    /// never forces a pick among tied ancestors.
    #[test]
    fn test_java_tied_ancestors_at_same_depth_do_not_force_a_pick() {
        let dir = std::env::temp_dir().join(format!("ci_idx_java_tie_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("IA.java"),
            "public interface IA {\n    String foo();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("IB.java"),
            "public interface IB {\n    String foo();\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Mixed.java"),
            "public interface Mixed extends IA, IB {\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Caller.java"),
            "public class Caller {\n    String use(Mixed m) {\n        return m.foo();\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The tie must fall through to the SAME pre-WS4 unscoped-fallback
        // shape: both IA::foo and IB::foo survive as candidates, each
        // emitted as its own `ambiguous` edge -- not silently dropped
        // (that would be a NEW regression) and not collapsed onto a single
        // arbitrarily-chosen one (that would be the guess WS4 must avoid).
        let edges: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT to_symbol, edge_confidence FROM call_edges \
                     WHERE from_symbol LIKE '%Caller::use'",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(
            edges.len(),
            2,
            "both tied ancestors must survive as separate candidates, not be \
             dropped or collapsed to one: got {edges:?}"
        );
        assert!(
            edges.iter().any(|(to, _)| to.contains("IA::foo")),
            "IA::foo must be one of the two candidates: got {edges:?}"
        );
        assert!(
            edges.iter().any(|(to, _)| to.contains("IB::foo")),
            "IB::foo must be one of the two candidates: got {edges:?}"
        );
        assert!(
            edges.iter().all(|(_, c)| c == "ambiguous"),
            "a genuine tie must never be reported at confident (resolved/inferred) \
             confidence -- neither candidate is more evidenced than the other: got {edges:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // 2026-08-18 (B15 investigation, follow-up to the inheritance fix
    // above): `new Foo(...)` is its own `new_expression` node kind in
    // tree-sitter-javascript/typescript, distinct from `call_expression`
    // -- confirmed via the real vendored grammar's node-types.json, not
    // guessed. A class invoked exclusively via `new`, never a plain
    // function call, used to produce ZERO call edges -- the exact,
    // real, live-confirmed miss B15's cross-language competitor
    // benchmark found (`User`, examples/view-locals/user.js in the
    // express corpus).
    fn test_javascript_new_expression_resolves_as_a_call_to_the_constructed_class() {
        let dir = std::env::temp_dir().join(format!("ci_idx_js_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.js"),
            "class User {\n    constructor(name) {\n        this.name = name;\n    }\n}\n\
             function run() {\n    return new User(\"a\");\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::User'",
            ),
            1,
            "run()'s `new User(\"a\")` must produce a call edge to the User class \
             -- new_expression must be treated as a call site, not silently dropped"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // Companion to the test above: TypeScript's `new_expression` additionally
    // carries an optional `type_arguments` field (`new Box<string>(...)`) --
    // confirmed via tree-sitter-typescript's own node-types.json that this
    // field is SEPARATE from `constructor`, so the callee text stays clean
    // ("Box", no generic-bracket pollution) with no extra stripping logic
    // needed.
    fn test_typescript_new_expression_with_generics_resolves_as_a_call() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ts_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.ts"),
            "class Box<T> {\n    constructor(public value: T) {}\n}\n\
             function run(): void {\n    new Box<string>(\"a\");\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::Box'",
            ),
            1,
            "run()'s `new Box<string>(\"a\")` must produce a call edge to Box, with \
             the <string> type argument not corrupting the callee name"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // Java companion: `new Foo(...)` is `object_creation_expression`, whose
    // callee lives in a `type` field (not `constructor` like JS/TS) --
    // confirmed via tree-sitter-java's own node-types.json. Also exercises
    // the generic-constructor shape (`new Box<String>(...)`) to confirm
    // `leading_ident` correctly stops at `<` rather than corrupting the
    // callee name.
    //
    // Expects 2 edges, not 1: Java indexes a class's own name AND its
    // constructor's name as two SEPARATE same-named symbols (`Box.java::Box`,
    // kind=class, and `Box.java::Box::Box`, kind=constructor -- verified live
    // via the real `symbols` table) -- a bare-name callee resolution with no
    // narrowing signal to prefer one over the other correctly returns both as
    // `ambiguous` candidates, the same "no scoping evidence, don't guess"
    // behavior any other same-named bare-call collision already gets
    // elsewhere in this resolver. This is a pre-existing characteristic of
    // Java's symbol model this fix newly exercises via constructors, not a
    // new bug -- disambiguating it further is out of scope here.
    fn test_java_object_creation_expression_resolves_as_a_call_to_the_constructed_class() {
        let dir = std::env::temp_dir().join(format!("ci_idx_java_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Box.java"),
            "public class Box<T> {\n    public Box(T value) {\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Caller.java"),
            "public class Caller {\n    void m() {\n        new Box<String>(\"a\");\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%Caller::m' AND to_symbol LIKE '%Box%'",
            ),
            2,
            "Caller.m()'s `new Box<String>(\"a\")` must produce a call edge to \
             both same-named Box symbols (the class and its own constructor), \
             with the <String> type argument not corrupting the callee name"
        );
        let confidence: String = conn
            .query_row(
                "SELECT DISTINCT edge_confidence FROM call_edges \
                 WHERE from_symbol LIKE '%Caller::m' AND to_symbol LIKE '%Box%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "ambiguous",
            "2 same-named candidates with no narrowing signal must be ambiguous, \
             not silently trusted as if one had been confirmed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same fan-out bug, `by_name_class` variant: two unrelated types in
    /// different files that happen to share both a type name AND a method
    /// name (e.g. two local `struct Handler` in different modules, each with
    /// their own `fn helper`) key into the exact same `by_name_class` slot —
    /// `self.helper()` inside a.rs's `Handler::run` must resolve to a.rs's
    /// own `Handler::helper`, not fan out to b.rs's unrelated one too.
    #[test]
    fn test_self_method_call_prefers_same_file_over_global_fan_out() {
        let dir = std::env::temp_dir().join(format!("ci_idx_selffanout_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "struct Handler;\nimpl Handler {\n    fn helper(&self) -> i32 {\n        1\n    }\n    fn run(&self) -> i32 {\n        self.helper()\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "struct Handler;\nimpl Handler {\n    fn helper(&self) -> i32 {\n        2\n    }\n    fn run(&self) -> i32 {\n        self.helper()\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND path = 'a.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'a.rs')",
            ),
            1,
            "a.rs's Handler::run must resolve self.helper() to a.rs's own Handler::helper"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'run' AND path = 'a.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper' AND path = 'b.rs')",
            ),
            0,
            "a.rs's Handler::run must NOT also fan out to b.rs's unrelated Handler::helper"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the real-world incident this fix addresses (found via
    /// this repo's own `common.rs::CalmServer::timed_tool`
    /// delegating to `telemetry.rs::timed_tool` the same way): a same-named
    /// wrapper method calling a fully-qualified `crate::module::func()` with
    /// no `use` for it used to resolve to the WRONG same-named symbol — the
    /// caller's own file, in the worst case a fabricated self-recursive edge
    /// on the wrapper itself — because the explicit module qualifier was
    /// discarded before the same-file preference ever saw it. `module_hint`
    /// must take priority over that preference and route the edge to the
    /// module actually named in the source.
    #[test]
    fn test_qualified_call_resolves_to_named_module_not_same_file_same_name() {
        let dir = std::env::temp_dir().join(format!("ci_idx_modhint_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("telemetry.rs"),
            "pub fn timed_tool(name: &str) -> String {\n    name.to_string()\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("common.rs"),
            "pub struct Server;\nimpl Server {\n    pub fn timed_tool(&self, name: &str) -> String {\n        crate::telemetry::timed_tool(name)\n    }\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'telemetry.rs')",
            ),
            1,
            "crate::telemetry::timed_tool(...) must resolve to telemetry.rs's free function"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'timed_tool' AND path = 'common.rs')",
            ),
            0,
            "must NOT fabricate a self-recursive edge onto common.rs's own same-named method"
        );
        let telemetry_caller_count: i64 = conn
            .query_row(
                "SELECT caller_count FROM symbols WHERE name = 'timed_tool' AND path = 'telemetry.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            telemetry_caller_count, 1,
            "telemetry.rs::timed_tool must show its one real caller, not 0"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tier2_method_resolution() {
        let dir = std::env::temp_dir().join(format!("ci_idx_tier2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a.py: a class with a method. b.py: a typed-parameter method call on it.
        std::fs::write(
            dir.join("a.py"),
            "class Service:\n    def process(self):\n        pass\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.py"),
            "def run(svc: Service):\n    svc.process()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // Method is class-qualified.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::Service::process'",
            ),
            1,
            "method qualified_name should include its class"
        );

        // Tier-2: svc:Service ⇒ svc.process() resolves into Service, confidence inferred.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'b.py::run' AND to_symbol = 'a.py::Service::process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "inferred",
            "typed-receiver method call is tier-2 inferred"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tier2_go_pointer_receiver() {
        let dir = std::env::temp_dir().join(format!("ci_idx_go2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.go"),
            "package p\ntype Service struct{}\nfunc (s *Service) Process() int { return 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.go"),
            "package p\nfunc run(s *Service) int { return s.Process() }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // Go method is tagged with its receiver type as class_context.
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.go::Service::Process'",
            ),
            1
        );
        // `*Service` receiver ⇒ s.Process() resolves into Service, inferred.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'b.go::run' AND to_symbol = 'a.go::Service::Process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(confidence, "inferred");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verifies `ci`'s own recovery path for a scenario an agent host's
    /// checkpoint/rewind feature (Claude Code's `/rewind`, similar in
    /// Cursor/Windsurf) can produce: a file gets reverted to *older*
    /// content out from under the running server, entirely outside any
    /// `edit_lines`/`edit_symbol` call `ci` knows about. Since
    /// `reindex_changed` decides "did this file change" by comparing a
    /// fresh content hash against the DB's stored hash — not by mtime or
    /// direction — a revert to prior content produces a different hash from
    /// what's currently indexed and is picked up exactly like a forward
    /// edit would be. This confirms that by construction rather than
    /// building a separate `ci`-side undo mechanism, which would risk
    /// drifting out of sync with whatever the host's own checkpoint state
    /// actually is.
    #[test]
    fn test_reindex_recovers_after_file_externally_reverted_to_older_content() {
        let dir = std::env::temp_dir().join(format!("ci_idx_revert_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = "def original():\n    pass\n";
        std::fs::write(dir.join("a.py"), original).unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            1
        );

        // Agent (or the agent's host) edits the file forward.
        std::fs::write(dir.join("a.py"), "def edited():\n    pass\n").unwrap();
        reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::edited'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            0,
            "the edited-away symbol must not linger"
        );

        // Something *outside* ci's own write path (a host checkpoint
        // rewind, a manual `git checkout`, an editor undo) puts the
        // original content back — not a new edit_lines/edit_symbol call.
        std::fs::write(dir.join("a.py"), original).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            (s.changed, s.deleted),
            (1, 0),
            "a revert to prior content is still a content-hash change and must be picked up"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::original'"
            ),
            1,
            "the reverted-to symbol must be restored"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE qualified_name = 'a.py::edited'"
            ),
            0,
            "the since-reverted symbol must not linger as a stale leftover"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_reindex_incremental_add_modify_delete() {
        let dir = std::env::temp_dir().join(format!("ci_idx_inc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "def helper():\n    pass\n").unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1);

        // No change → no-op.
        assert!(reindex_changed(&mut conn, &dir).unwrap().is_noop());

        // Add a second file that calls helper → new symbol + cross-file edge.
        std::fs::write(dir.join("b.py"), "def caller():\n    helper()\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!((s.changed, s.deleted), (1, 0));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            1
        );

        // Modify b.py to no longer call helper → edge drops, caller_count → 0.
        std::fs::write(dir.join("b.py"), "def caller():\n    pass\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!((s.changed, s.deleted), (1, 0));
        assert_eq!(
            count(
                &conn,
                "SELECT caller_count FROM symbols WHERE qualified_name = 'a.py::helper'",
            ),
            0
        );

        // Delete b.py → its symbol disappears.
        std::fs::remove_file(dir.join("b.py")).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!((s.changed, s.deleted), (0, 1));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM symbols"), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_call_site_identity_forces_a_full_rebuild_even_when_hashes_match() {
        let dir =
            std::env::temp_dir().join(format!("calm_d4_identity_migration_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.py"),
            "def caller():\n    helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert!(count(&conn, "SELECT COUNT(*) FROM call_sites") > 0);
        conn.execute(
            "UPDATE call_sites
             SET callee_start_byte = NULL, callee_end_byte = NULL, identity_version = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scip_overlay_state (provider, cache_key) VALUES ('python', 'legacy-key')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM file_index", []).unwrap();

        let summary = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!(
            summary.changed, 1,
            "the unchanged source must still be reparsed"
        );
        let (migration_status, target_version, rows_rebuilt): (String, i64, Option<i64>) = conn
            .query_row(
                "SELECT status, target_version, rows_rebuilt
                 FROM identity_migration_state WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(migration_status, "baseline_ready");
        assert_eq!(target_version, 2);
        assert_eq!(
            rows_rebuilt,
            Some(1),
            "migration status must report the files actually rebuilt, not stale index rows"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_sites
                 WHERE identity_version < 2
                    OR callee_start_byte IS NULL
                    OR callee_end_byte IS NULL",
            ),
            0,
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM scip_overlay_state"),
            0,
            "line-derived overlay cache state cannot survive an identity rebuild",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reindex_paths_repairs_legacy_call_site_identity_even_when_hashes_match() {
        let dir = std::env::temp_dir().join(format!(
            "calm_d4_identity_paths_migration_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.py"),
            "def caller():\n    helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        conn.execute(
            "UPDATE call_sites
             SET callee_start_byte = NULL, callee_end_byte = NULL, identity_version = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scip_overlay_state (provider, cache_key) VALUES ('python', 'legacy-key')",
            [],
        )
        .unwrap();

        let summary = reindex_paths(&mut conn, &dir, &["main.py".to_string()]).unwrap();
        assert_eq!(
            summary.changed, 1,
            "the direct dirty-path route must not no-op on a legacy identity"
        );
        assert!(matches!(
            summary.graph_mode,
            GraphMode::FullFallback(ref reason) if reason == "call_site_identity_v2"
        ));
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_sites
                 WHERE identity_version < 2
                    OR callee_start_byte IS NULL
                    OR callee_end_byte IS NULL",
            ),
            0,
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM scip_overlay_state"), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_identity_baseline_preserves_legacy_graph_and_records_failure() {
        #[derive(Debug, PartialEq, Eq)]
        struct LegacyGraphSnapshot {
            file_index: Vec<Vec<String>>,
            call_sites: Vec<Vec<String>>,
            call_edges: Vec<Vec<String>>,
            external_proofs: Vec<Vec<String>>,
            overlay_cache: Vec<Vec<String>>,
            generation: i64,
        }

        fn rows(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
            let mut statement = conn.prepare(sql).unwrap();
            statement
                .query_map([], |row| {
                    (0..row.as_ref().column_count())
                        .map(|index| {
                            Ok(match row.get_ref(index)? {
                                rusqlite::types::ValueRef::Null => "null".to_owned(),
                                rusqlite::types::ValueRef::Integer(value) => {
                                    format!("integer:{value}")
                                }
                                rusqlite::types::ValueRef::Real(value) => format!("real:{value:?}"),
                                rusqlite::types::ValueRef::Text(value) => {
                                    format!("text:{}", String::from_utf8_lossy(value))
                                }
                                rusqlite::types::ValueRef::Blob(value) => format!("blob:{value:?}"),
                            })
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        }

        fn snapshot(conn: &Connection) -> LegacyGraphSnapshot {
            LegacyGraphSnapshot {
                file_index: rows(
                    conn,
                    "SELECT path, hash, language, symbol_count, last_indexed
                     FROM file_index ORDER BY path",
                ),
                call_sites: rows(
                    conn,
                    "SELECT id, from_path, enclosing_qn, callee_name, call_line,
                            callee_start_byte, callee_end_byte, identity_version, edge_kind
                     FROM call_sites ORDER BY id",
                ),
                call_edges: rows(
                    conn,
                    "SELECT id, from_symbol, to_symbol, call_site_line, call_site_id,
                            edge_confidence, formal_source, evidence_state, ruled_out_by_scip,
                            from_path, to_path, edge_kind
                     FROM call_edges ORDER BY id",
                ),
                external_proofs: rows(
                    conn,
                    "SELECT id, call_site_id, to_symbol, provider, source_file_hash,
                            callee_start_byte, callee_end_byte, provider_fingerprint,
                            context_fingerprint, definition_snapshot, call_site_identity_version,
                            graph_generation, status, observed_at
                     FROM external_proofs ORDER BY id",
                ),
                overlay_cache: rows(
                    conn,
                    "SELECT provider, cache_key FROM scip_overlay_state ORDER BY provider, cache_key",
                ),
                generation: conn
                    .query_row(
                        "SELECT generation FROM graph_generation_state WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap(),
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "calm_d4_identity_cancel_migration_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.py"),
            "def caller():\n    helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        conn.execute(
            "UPDATE call_sites
             SET callee_start_byte = NULL, callee_end_byte = NULL, identity_version = 1",
            [],
        )
        .unwrap();
        conn.execute("UPDATE call_edges SET ruled_out_by_scip = 1", [])
            .unwrap();
        conn.execute(
            "INSERT INTO external_proofs
                (call_site_id, to_symbol, provider, source_file_hash,
                 callee_start_byte, callee_end_byte, provider_fingerprint,
                 context_fingerprint, status, observed_at)
             SELECT call_sites.id, call_edges.to_symbol, 'scip:test', 'legacy-source',
                    0, 1, 'legacy-provider', 'legacy-context', 'fresh', 0
             FROM call_sites
             JOIN call_edges ON call_edges.call_site_id = call_sites.id
             LIMIT 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scip_overlay_state (provider, cache_key)
             VALUES ('python', 'legacy-key')",
            [],
        )
        .unwrap();
        let baseline_snapshot = snapshot(&conn);
        assert_eq!(
            baseline_snapshot.external_proofs.len(),
            1,
            "fixture must contain a proof"
        );
        assert!(
            baseline_snapshot
                .call_edges
                .iter()
                .any(|edge| edge.get(8).is_some_and(|value| value == "integer:1")),
            "fixture must contain a rule-out"
        );
        assert_eq!(
            baseline_snapshot.overlay_cache.len(),
            1,
            "fixture must contain overlay cache state"
        );

        let cancel_checks = std::sync::atomic::AtomicUsize::new(0);
        assert!(matches!(
            reindex_changed_cancellable(&mut conn, &dir, &|| cancel_checks
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                >= 1,)
            .unwrap(),
            ReindexOutcome::Cancelled
        ));
        assert!(
            cancel_checks.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "cancellation must occur after the transactional baseline has started"
        );
        let post_cancel_snapshot = snapshot(&conn);
        assert_eq!(
            post_cancel_snapshot, baseline_snapshot,
            "a mid-transaction cancellation must preserve every persisted legacy graph value"
        );
        assert!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_sites WHERE identity_version = 1"
            ) > 0,
            "a cancelled transaction must leave the previously committed legacy graph intact"
        );
        let (status, reason, duration_ms, rows_rebuilt, busy_retries, graph_generation): (
            String,
            Option<String>,
            Option<i64>,
            Option<i64>,
            i64,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT status, failure_reason, duration_ms, rows_rebuilt, busy_retries,
                        graph_generation
                 FROM identity_migration_state WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(reason.as_deref(), Some("baseline cancelled"));
        assert!(duration_ms.is_some());
        assert_eq!(rows_rebuilt, Some(0));
        assert_eq!(busy_retries, 0);
        assert!(graph_generation.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    // PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    // Part E): the actual DoD scenario v3 position-independent identity +
    // slice D upsert-by-identity reconciliation exist to fix -- "insert a
    // comment line above a proven call site; reindex; assert the proof row
    // for that call site is unchanged". Verified at the persistence layer
    // directly (a hand-inserted external_proofs row simulating what a real
    // SCIP verification would produce, same INSERT shape as
    // cancelled_identity_baseline_preserves_legacy_graph_and_records_failure
    // above) rather than driving real SCIP machinery, which this test
    // doesn't need to exercise.
    fn call_site_and_external_proof_survive_an_edit_that_only_shifts_absolute_position() {
        let dir =
            std::env::temp_dir().join(format!("ci_idx_v3_churn_regression_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("main.py"),
            "def caller():\n    helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let (call_site_id, identity_version, start_rel, end_rel, start_byte): (
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT id, identity_version, callee_start_rel, callee_end_rel, callee_start_byte \
                 FROM call_sites WHERE callee_name = 'helper'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            identity_version, 3,
            "caller() is a real enclosing symbol -- this call site must get v3 identity"
        );
        assert!(
            start_rel.is_some() && end_rel.is_some(),
            "v3 identity must carry relative offsets"
        );

        conn.execute(
            "INSERT INTO external_proofs
                (call_site_id, to_symbol, provider, source_file_hash,
                 callee_start_byte, callee_end_byte, provider_fingerprint,
                 context_fingerprint, status, observed_at)
             VALUES (?1, 'main.py::helper', 'scip:test', 'irrelevant-hash',
                     0, 1, 'test-provider', 'test-context', 'fresh', 0)",
            [call_site_id],
        )
        .unwrap();
        let proof_id: i64 = conn
            .query_row(
                "SELECT id FROM external_proofs WHERE call_site_id = ?1",
                [call_site_id],
                |r| r.get(0),
            )
            .unwrap();

        // Insert a comment line ABOVE caller() -- shifts every byte offset
        // inside caller() (including the helper() call) by a fixed amount,
        // but the call's position RELATIVE to caller()'s own start is
        // unchanged.
        std::fs::write(
            dir.join("main.py"),
            "# a harmless comment, unrelated to caller/helper\ndef caller():\n    helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        reindex_paths(&mut conn, &dir, &["main.py".to_string()]).unwrap();

        let (new_id, new_identity_version, new_start_rel, new_end_rel, new_start_byte): (
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT id, identity_version, callee_start_rel, callee_end_rel, callee_start_byte \
                 FROM call_sites WHERE callee_name = 'helper'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();

        assert_eq!(
            new_id, call_site_id,
            "the call site's own id must survive an edit that only shifts its absolute \
             position -- this is the entire point of v3 relative identity + upsert-by-identity \
             reconciliation"
        );
        assert_eq!(new_identity_version, 3);
        assert_eq!(
            (new_start_rel, new_end_rel),
            (start_rel, end_rel),
            "relative offsets must be unchanged -- the call's position within caller() didn't move"
        );
        assert_ne!(
            new_start_byte, start_byte,
            "sanity check: the absolute byte position MUST have shifted (a comment line was \
             inserted above caller()) -- if this fails the test fixture itself is wrong, not the \
             code under test"
        );

        let surviving_proof_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM external_proofs \
                 WHERE id = ?1 AND call_site_id = ?2 AND status = 'fresh'",
                rusqlite::params![proof_id, call_site_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            surviving_proof_count, 1,
            "the external_proofs row must survive the reindex byte-for-byte (same id, same \
             call_site_id, still 'fresh') -- before slice D this proof would have been \
             CASCADE-deleted because call_sites blindly deleted-and-reinserted every row for the \
             file on any edit"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Layer-2 code chunks must track incremental reindex the same way symbols
    /// do: a changed file's stale chunks are replaced (not duplicated
    /// alongside the new ones), and a deleted file's chunks disappear too.
    /// Only meaningful with `embeddings` compiled in — otherwise chunking is a
    /// no-op (see `chunk_pending`) and `code_chunks` stays empty by design.
    #[cfg(feature = "embeddings")]
    #[test]
    fn test_reindex_incremental_updates_code_chunks() {
        let dir = std::env::temp_dir().join(format!("ci_idx_inc_chunks_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "def run():\n    marker = OLD_MARKER_TERM\n    return marker\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%OLD_MARKER_TERM%'",
            ),
            1
        );

        // Change the body's distinctive term (same line count/symbol) and add
        // a second file.
        std::fs::write(
            dir.join("a.py"),
            "def run():\n    marker = NEW_MARKER_TERM\n    return marker\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.py"), "def other():\n    pass\n").unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!((s.changed, s.deleted), (2, 0));

        // Exactly one chunk per file — the stale a.py chunk was replaced, not
        // accumulated alongside the new one.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%OLD_MARKER_TERM%'",
            ),
            0,
            "stale chunk text must not survive a reindex of the same file"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE chunk_text LIKE '%NEW_MARKER_TERM%'",
            ),
            1
        );

        // Delete a.py → its chunk disappears; b.py's chunk is untouched.
        std::fs::remove_file(dir.join("a.py")).unwrap();
        let s = reindex_changed(&mut conn, &dir).unwrap();
        assert_eq!((s.changed, s.deleted), (0, 1));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM code_chunks"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE path = 'a.py'"
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM code_chunks WHERE path = 'b.py'"
            ),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_formal_tier_upgrades_textual_python_call() {
        // Verify Tier-3: FormalResolver upgrades a "textual" call site to "formal".
        //
        // ConservativeResolver Tier-1 only gives "resolved" for names it finds in
        // file_symbols, import_map, or aliases. A call to a lambda or a function
        // assigned to a variable is NOT captured by extract_symbols, so Tier-1
        // gives "textual". FormalResolver's StackGraph rules DO resolve it (it sees
        // the binding in scope) and upgrades the confidence to "formal".
        //
        // We use a nested-scope call: `helper` is defined inside `setup()` and
        // called from `run()`. extract_symbols captures nested defs as file_symbols
        // (so Tier-1 gives "resolved"), meaning the call edge exists with ≥resolved.
        // The key assertion is that the pipeline integrates without error AND produces
        // the call edge — proving FormalResolver is wired in and doesn't break things.
        let dir = std::env::temp_dir().join(format!("ci_formal_tier_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("mod.py"),
            "def helper():\n    pass\n\ndef run():\n    helper()\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        // The call from run() → helper() must produce a call edge with at least
        // "resolved" confidence (ConservativeResolver Tier-1 finds it in file_symbols).
        // If FormalResolver is also loaded, it confirms the same edge via StackGraph.
        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert_eq!(
            edge_count, 1,
            "Expected exactly one call edge run→helper from pipeline with FormalResolver integrated"
        );

        // Verify FormalResolver did not break confidence — must be resolved or formal.
        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol LIKE '%::run' AND to_symbol LIKE '%::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        assert!(
            matches!(confidence.as_str(), "resolved" | "formal"),
            "Expected confidence 'resolved' or 'formal' for intra-file call, got: {confidence}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_self_colon_colon_call_resolves_to_the_enclosing_impl_type() {
        // 2026-08-03 regression test: `Self::method()` inside an `impl Type { .. }`
        // block used to resolve to ZERO callers, not even `ambiguous`, because
        // `extract_file_data` set `target_class` to the literal keyword text
        // "Self" instead of substituting the enclosing impl's real type name --
        // "Self" is never itself a registered symbol/class, so the
        // `by_name_class` lookup in `resolve_sites_to_edges` could never match.
        // Found via a live CALM-vs-CodeGraph benchmark: fd's `replace_separator`
        // (called 5x in-file via `Self::replace_separator(...)`) and this exact
        // codebase's `ConservativeResolver::default` -> `Self::new()` both had 0
        // callers before this fix. `resolve_tier2` already does the equivalent
        // substitution for lowercase `self`/`this`; this is the same fix for
        // Rust's capitalized `Self`, applied one branch earlier.
        let dir = std::env::temp_dir().join(format!("ci_rust_self_colon_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("widget.rs"),
            "struct Widget;\n\
         impl Widget {\n    \
             fn new() -> Self {\n        Widget\n    }\n\n    \
             fn make() -> Self {\n        Self::new()\n    }\n\
         }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges \
             WHERE from_symbol LIKE '%::Widget::make' AND to_symbol LIKE '%::Widget::new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            edge_count, 1,
            "Self::new() inside impl Widget must produce exactly one call edge \
         make -> new, scoped to Widget (not zero, not fanned out to every \
         same-named `new` in the codebase)"
        );

        let confidence: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
             WHERE from_symbol LIKE '%::Widget::make' AND to_symbol LIKE '%::Widget::new'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            confidence, "inferred",
            "Self:: is a type-path receiver (same tier as Type::method()), so \
         confidence must be 'inferred', not 'textual'/'ambiguous'/'resolved'"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_self_colon_colon_call_inside_a_trait_default_method_resolves_to_the_trait() {
        // 2026-08-03 regression test for the walk_calls trait_item fix
        // (parser.rs): `trait_item` has no "type" field (only "name"), unlike
        // `impl_item` -- `class_name_field: "type"` is shared by both node kinds
        // in `lang_constants.rs`, so before this fix `walk_calls`'s child_class
        // computation silently got `None` for a trait, and a default method's
        // own `Self::sibling()` call fell through to literal target_class "Self"
        // (0 call_edges) -- same broken shape as the impl_item `Self::` bug, via
        // a different root cause. Verified with a live characterization pass
        // before fixing (not assumed): `call_sites` showed
        // `target_class=Some("Self")` and `call_edges` was empty.
        let dir = std::env::temp_dir().join(format!("ci_rust_trait_self_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("greeter.rs"),
            "trait Greeter {\n    \
             fn helper() -> String {\n        \"hi\".to_string()\n    }\n\n    \
             fn greet() -> String {\n        Self::helper()\n    }\n\
         }\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges \
             WHERE from_symbol LIKE '%::Greeter::greet' AND to_symbol LIKE '%::Greeter::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            edge_count, 1,
            "Self::helper() inside a Greeter trait default method must resolve to \
         Greeter's own declared helper() (not zero, not fanned out elsewhere)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the real-world incident this module's return-shape filter
    /// exists for: `caller.rs` calls a bare `as_str()` on an unresolvable
    /// receiver (Tier-2 can't type it), immediately `.unwrap()`-ed. Two
    /// same-named candidates exist elsewhere in the repo — one returning
    /// `Option<&str>` (a plausible real target), one returning plain `&str`
    /// (provably *not* the target, since `Foo::as_str().unwrap()` wouldn't
    /// compile against a non-`Option`/`Result` return). Before this filter,
    /// `rebuild_graph`'s `MAX_CALLEE_CANDIDATES` fallback fanned out to both.
    #[test]
    fn test_option_chained_call_excludes_non_option_candidates() {
        let dir = std::env::temp_dir().join(format!("ci_idx_optchain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Bar;\nimpl Bar {\n    pub fn as_str(&self) -> Option<&'static str> {\n        Some(\"b\")\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str().unwrap();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'a.rs')",
            ),
            0,
            "Foo::as_str returns &'static str, not Option — .unwrap() on the call site rules it out"
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'b.rs')",
            ),
            1,
            "Bar::as_str returns Option<&'static str> — the only candidate .unwrap() could compile against"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the return-shape filter can't break the tie (call site isn't
    /// `?`/`.unwrap()`-chained, or the surviving candidates are still >1),
    /// the resulting fan-out edges must be marked `ambiguous` — not the plain
    /// `textual` a genuine single-candidate resolution gets — so callers of
    /// `callers`/`symbol_info` can tell "spread across N unrelated symbols"
    /// apart from "one real, low-confidence match".
    #[test]
    fn test_unresolved_fan_out_marked_ambiguous_not_textual() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ambiguous_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Baz;\nimpl Baz {\n    pub fn as_str(&self) -> &'static str {\n        \"c\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        for path in ["a.rs", "b.rs"] {
            let confidence: String = conn
                .query_row(
                    "SELECT edge_confidence FROM call_edges \
                     WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                     AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = ?1)",
                    [path],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                confidence, "ambiguous",
                "fanned-out edge to {path}'s as_str must be marked ambiguous, not textual"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression for the real-world incident this fix addresses: before it,
    /// `caller_count` was a blunt `COUNT(DISTINCT from_symbol)` over every
    /// `call_edges` row regardless of confidence, so an `ambiguous` fan-out
    /// edge (recorded once per same-named candidate) inflated every
    /// candidate's `caller_count` almost identically — `Foo::as_str` (zero
    /// real callers) showed the *same* `caller_count` as `Baz::as_str`
    /// (one real caller via `self.as_str()`), which fed straight into
    /// `dead_code_confidence` (short-circuits to "not dead" on
    /// `caller_count > 0`), hub ranking, and coreness alike.
    #[test]
    fn test_caller_count_excludes_ambiguous_fan_out_edges() {
        let dir = std::env::temp_dir().join(format!("ci_idx_ccambig_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub struct Foo;\nimpl Foo {\n    pub fn as_str(&self) -> &'static str {\n        \"a\"\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.rs"),
            "pub struct Baz;\nimpl Baz {\n    pub fn as_str(&self) -> &'static str {\n        \"c\"\n    }\n    pub fn wrapper(&self) -> &'static str {\n        self.as_str()\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("caller.rs"),
            "fn get_something() -> i32 {\n    0\n}\nfn caller() {\n    let _ = get_something().as_str();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let caller_count = |path: &str| -> i64 {
            conn.query_row(
                "SELECT caller_count FROM symbols WHERE name = 'as_str' AND path = ?1",
                [path],
                |r| r.get(0),
            )
            .unwrap()
        };

        assert_eq!(
            caller_count("a.rs"),
            0,
            "Foo::as_str has only an ambiguous fan-out edge, no confirmed caller"
        );
        assert_eq!(
            caller_count("b.rs"),
            1,
            "Baz::as_str has exactly one confirmed (resolved, same-file self.as_str()) caller — \
             the ambiguous fan-out edge to it must not also be counted"
        );

        // refresh_caller_counts must be independently callable (this is what
        // the SCIP overlay pass re-invokes after flipping ruled_out_by_scip),
        // and must reflect that flag too: rule out caller.rs's own edge to
        // Baz::as_str and confirm its caller_count drops back to 0.
        conn.execute(
            "UPDATE call_edges SET ruled_out_by_scip = 1 \
             WHERE to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'as_str' AND path = 'b.rs') \
               AND from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'wrapper')",
            [],
        )
        .unwrap();
        refresh_caller_counts(&conn).unwrap();
        assert_eq!(
            caller_count("b.rs"),
            0,
            "ruled_out_by_scip=1 edges must be excluded after a refresh_caller_counts() re-run"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verified_caller_count_excludes_inferred_and_textual_too() {
        // 2.3 (Wave 2): verified_caller_count is EdgeConfidence::is_verified()
        // (Formal/Resolved only) -- strictly narrower than caller_count's
        // "everything except Ambiguous" bucket, which still counts
        // Inferred/Textual. Direct call_edges inserts (not a real indexing
        // run) keep this deterministic instead of depending on the resolver
        // happening to produce each confidence tier from source text.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('t::target', 'target', 'function', 'rust', 't.rs', 1, 1)",
            [],
        )
        .unwrap();
        for (from, confidence) in [
            ("t::formal_caller", "formal"),
            ("t::resolved_caller", "resolved"),
            ("t::inferred_caller", "inferred"),
            ("t::textual_caller", "textual"),
        ] {
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, ruled_out_by_scip) \
                 VALUES (?1, 't::target', ?2, 0)",
                rusqlite::params![from, confidence],
            )
            .unwrap();
        }

        refresh_caller_counts(&conn).unwrap();

        let (caller_count, verified_caller_count): (i64, i64) = conn
            .query_row(
                "SELECT caller_count, verified_caller_count FROM symbols \
                 WHERE qualified_name = 't::target'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            caller_count, 4,
            "caller_count counts everything except ambiguous-confidence edges"
        );
        assert_eq!(
            verified_caller_count, 2,
            "verified_caller_count excludes inferred/textual too, not just ambiguous"
        );
    }

    #[test]
    fn test_signature_returns_option_or_result() {
        assert!(signature_returns_option_or_result(
            "pub fn as_str(&self) -> Option<&'static str> {"
        ));
        assert!(signature_returns_option_or_result(
            "pub fn parse(s: &str) -> Result<Self, Error> {"
        ));
        assert!(!signature_returns_option_or_result(
            "pub fn as_str(&self) -> &'static str {"
        ));
        assert!(!signature_returns_option_or_result("pub struct Foo {"));
        // The return arrow, not one buried in a higher-order parameter type.
        assert!(signature_returns_option_or_result(
            "pub fn foo(f: impl Fn() -> i32) -> Option<i32> {"
        ));
        assert!(!signature_returns_option_or_result(
            "pub fn foo(f: impl Fn() -> Option<i32>) -> i32 {"
        ));
        // Regression: module-qualified Result/Option aliases (the norm, not
        // the exception, for any crate with its own error type) used to be
        // silently excluded — see this function's doc comment for the real
        // `load_config`/`remove_file_rows` call edges this was dropping.
        assert!(signature_returns_option_or_result(
            "fn load_config(project_root: &Path) -> anyhow::Result<Config> {"
        ));
        assert!(signature_returns_option_or_result(
            "fn remove_file_rows(tx: &rusqlite::Transaction, rel: &str) -> rusqlite::Result<()> {"
        ));
        assert!(signature_returns_option_or_result(
            "fn foo() -> std::result::Result<T, E> {"
        ));
        // A qualified path *inside* the generic args must not corrupt the
        // module-qualification strip on the outer type.
        assert!(signature_returns_option_or_result(
            "fn foo() -> Result<foo::Bar, baz::Error> {"
        ));
        // Must not false-positive just because "Option"/"Result" is a prefix
        // of a longer, unrelated type name.
        assert!(!signature_returns_option_or_result(
            "fn foo() -> OptionalConfig<T> {"
        ));
        assert!(!signature_returns_option_or_result(
            "fn foo() -> my_crate::OptionalThing<T> {"
        ));
    }

    /// Regression for the duplicate-call-site collapse: two calls to the same
    /// function from the same caller on the *same* line must retain separate
    /// byte-span identities, rather than collapsing through a line-based key.
    #[test]
    fn distinct_call_sites_in_one_caller_are_kept_as_separate_edges() {
        let dir = std::env::temp_dir().join(format!("ci_idx_dupsite_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // `helper` defined once; `caller` calls it twice on line 3.
        std::fs::write(
            dir.join("a.rs"),
            "fn helper() {}\nfn caller() {\n    helper(); helper();\n}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper')",
            ),
            2,
            "two distinct call sites must be kept as two edges, not deduped to one"
        );

        let edge_sites: Vec<(i64, i64, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT call_edges.call_site_id, call_edges.call_site_line, call_sites.callee_start_byte \
                     FROM call_edges JOIN call_sites ON call_sites.id = call_edges.call_site_id \
                     WHERE call_edges.from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'caller') \
                     AND call_edges.to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'helper') \
                     ORDER BY call_sites.callee_start_byte",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert_eq!(
            edge_sites
                .iter()
                .map(|(_, line, _)| *line)
                .collect::<Vec<_>>(),
            vec![3, 3],
            "both edges come from the same source line"
        );
        assert_ne!(
            edge_sites[0].0, edge_sites[1].0,
            "each edge must point at its own persisted CallSite"
        );
        assert_ne!(
            edge_sites[0].2, edge_sites[1].2,
            "same-line calls must be distinguished by selected-callee byte start"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_file_ignores_a_call_site_that_collides_on_the_full_identity_tuple() {
        // Regression test for a real, reproducible bug found live benchmarking
        // CALM against pallets/flask (2026-08-02): the tree-sitter Python
        // extractor emitted two `CallSiteData` entries whose (from_path,
        // enclosing_qn, callee_start_byte, callee_end_byte, edge_kind,
        // identity_version) tuple was byte-for-byte identical --
        // `idx_call_sites_current_identity` (schema.rs
        // migrate_call_site_identity_v2) correctly rejects that as a
        // duplicate, but `persist_file`'s INSERT had no `OR IGNORE`, so the
        // UNIQUE-constraint error aborted the whole transaction and failed
        // indexing for the ENTIRE file (in the real case: the entire flask
        // corpus, 0/93 files indexed). This doesn't attempt to reproduce the
        // exact upstream Python parser condition that produced the duplicate
        // (still an open question -- see design doc) -- it directly verifies
        // the persistence layer's own contract: a duplicate-identity call
        // site must be silently deduped, never crash the transaction.
        // PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
        // Part E): no real file/real indexing pass here anymore -- persist_file
        // now reconciles call_sites by IDENTITY for the whole path (upsert:
        // update-in-place / insert / delete whatever isn't in the fresh set),
        // not a pure blind-append. A real prior indexing pass would seed a
        // REAL call_sites row for "a.rs" that this test's hand-built
        // `extracted.call_sites` (below) doesn't include, and the new
        // reconciliation would then (correctly, per its own contract) treat
        // that real row as removed -- exactly what a previous version of this
        // test tripped over when slice D landed. This test is about
        // persist_file's in-batch dedup behavior specifically, which doesn't
        // need any pre-existing state to exercise.
        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let before: i64 = count(&conn, "SELECT COUNT(*) FROM call_sites");

        fn make_dup() -> CallSiteData {
            CallSiteData {
                enclosing_qn: "a.rs::caller".to_string(),
                callee: "helper".to_string(),
                line: 3,
                callee_start_byte: Some(4),
                callee_end_byte: Some(10),
                identity_version: 2,
                confidence: "resolved".to_string(),
                receiver: None,
                target_class: None,
                looks_option_or_result_chained: false,
                module_hint: None,
                edge_kind: "call".to_string(),
                arg_count: Some(0),
                import_path: None,
                target_type_kind: None,
                target_type_qn: None,
                callee_start_rel: None,
                callee_end_rel: None,
            }
        }
        let tx = conn.transaction().unwrap();
        let extracted = ExtractedFile {
            symbols: vec![],
            import_edges: vec![],
            call_sites: vec![make_dup(), make_dup()],
            symbol_count: 0,
            chunks: vec![],
            type_relations: vec![],
            effects: vec![],
        };
        persist_file(&tx, "a.rs", "irrelevant-hash", &extracted)
            .expect("a duplicate-identity call site must be ignored, not crash the transaction");
        tx.commit().unwrap();

        let after: i64 = count(&conn, "SELECT COUNT(*) FROM call_sites");
        assert_eq!(
            after,
            before + 1,
            "exactly one of the two identical CallSiteData entries should have been persisted"
        );
    }

    /// Regression for the import-graph false positive: a bare `use
    /// extern_crate::Item` (an external crate, not a workspace member) must NOT
    /// resolve to the importing crate's own `lib.rs`. Before the `uniform_guess`
    /// gate, the single-trailing-item fallback matched `{crate_root}/lib.rs`.
    #[test]
    fn external_crate_use_does_not_resolve_to_own_lib_rs() {
        let dir = std::env::temp_dir().join(format!("ci_idx_extern_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub mod thing;\n").unwrap();
        std::fs::write(
            dir.join("src/thing.rs"),
            "use rusqlite::Connection;\npub fn f(_c: &Connection) {}\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        let to_path: Option<String> = conn
            .query_row(
                "SELECT to_path FROM import_edges \
                 WHERE from_path = 'src/thing.rs' AND module_name = 'rusqlite'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            to_path.as_deref().unwrap_or("").is_empty(),
            "external crate `rusqlite` must not resolve to a local file, got {to_path:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P3.3 (8-language plan) end-to-end, matching the plan's own DoD
    /// verbatim: `schema.sql` gets a `file_index` row, a `users` table
    /// symbol and a `get_user` proc symbol, and a `resolved`-confidence
    /// `reference`-kind view→table edge — driven through the real
    /// `run_indexing_pipeline` (not just `sql::extract_sql_file` directly),
    /// so this also proves `extract_file_data`'s `lang == "sql"` branch and
    /// `rebuild_graph`'s `edge_kind` threading are wired correctly end to end.
    #[test]
    fn test_sql_p3_3_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ci_idx_sql_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("schema.sql"),
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);\n\
             CREATE VIEW active_users AS SELECT id, name FROM users;\n\
             CREATE FUNCTION get_user(uid INT) RETURNS INT AS $$ \
             BEGIN RETURN (SELECT id FROM users WHERE id = uid); END; \
             $$ LANGUAGE plpgsql;\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM file_index WHERE path = 'schema.sql'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name = 'users' AND kind = 'struct' AND language = 'sql'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name = 'get_user' AND kind = 'function' AND language = 'sql'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM call_edges \
                 WHERE from_symbol = (SELECT qualified_name FROM symbols WHERE name = 'active_users') \
                 AND to_symbol = (SELECT qualified_name FROM symbols WHERE name = 'users') \
                 AND edge_confidence = 'resolved' AND edge_kind = 'reference'",
            ),
            1,
            "view->table edge must be resolved+reference, not call"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_markdown_headings_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ci_idx_markdown_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "# Getting Started\n\nSome intro text.\n\n## Installation\n\n```bash\n# not a heading, a shell comment\npip install foo\n```\n\n## Usage\n\ntext\n",
        )
        .unwrap();

        let mut conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_indexing_pipeline(&mut conn, &dir, dummy_phase()).unwrap();

        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM file_index WHERE path = 'README.md' AND language = 'markdown'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name = 'Getting Started' AND kind = 'heading' AND language = 'markdown' AND path = 'README.md'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name = 'Installation' AND kind = 'heading'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name = 'Usage' AND kind = 'heading'"
            ),
            1
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM symbols WHERE name LIKE '%not a heading%'"
            ),
            0,
            "a '#'-prefixed line inside a fenced bash example must not become a heading symbol"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
