//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 4): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). The resolver's central reconciliation pass -- narrows every
//! call site's candidate set down to final `CallEdge`s (or an
//! `AmbiguityGroup` row when overflow). Move-only -- no logic changed, only
//! relocated. This is the largest, highest-blast-radius slice in the whole
//! split (`resolve_sites_to_edges` alone is ~620 lines and is the single
//! most-edited function in the repo's recent history -- WS3/WS5/PR#6 all
//! landed here), so it was read and diffed against the pre-move source in
//! full before this move, not sampled.
//!
//! `ResolutionCtx`/`SymbolCandidate`/`CallSiteRow` stay defined in
//! `pipeline.rs` itself (not moved) -- they're still used by
//! `build_resolution_context`/`resolve_via_inheritance_closure` (slice 3,
//! `pipeline/context.rs`) and by `rebuild_graph`/`incremental_graph_update`
//! (slice 6, not yet extracted), so moving them here would just relocate the
//! same "shared type" problem rather than solve it. `resolve_sites_to_edges`/
//! `insert_ambiguity_groups_batch` are `pub(super)` (not plain private)
//! since Rust module privacy is "visible in the defining module and its
//! descendants" -- `pipeline.rs` is this module's ANCESTOR, not a
//! descendant, so plain `fn` would be invisible to it (verified via
//! `callers()` before this move: both are called by
//! `rebuild_graph`/`incremental_graph_update`, still in pipeline.rs).
//! `AmbiguityGroup` is `pub(super)` too -- `rebuild_graph`/
//! `incremental_graph_update` hold a `Vec<AmbiguityGroup>` value (from
//! `resolve_sites_to_edges`'s return type) even though they never read its
//! fields, and Rust requires the type itself to be nameable/visible at that
//! use site. `resolve_via_inheritance_closure`/`signature_returns_option_or_
//! result`/`MAX_CALLEE_CANDIDATES` are pulled in via `super::` the same way
//! -- a child module sees whatever is in scope (definition OR a plain `use`)
//! in its ancestor, pub or not.

use rayon::prelude::*;
use std::collections::HashSet;
use std::path::Path;

use crate::indexer::edges::CallEdge;
use crate::types::EdgeConfidence;

use super::{
    CallSiteRow, MAX_CALLEE_CANDIDATES, ResolutionCtx, SymbolCandidate,
    resolve_via_inheritance_closure, signature_returns_option_or_result,
};

/// PR#8 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part
/// E): does `path` (a `by_name_class` candidate's file path) plausibly
/// belong to the type `target_type_qn` refers to, per `target_type_kind`'s
/// variant? A pure narrowing predicate -- `resolve_sites_to_edges` only
/// ever uses this to prefer a SUBSET of an already-name-matched candidate
/// list, exactly like `import_path`'s/`module_hint`'s own file-stem checks
/// (never a source of new candidates), so a false negative here just falls
/// through to the unscoped bare-name behavior that predates PR#8 -- never a
/// wrong resolution, only a missed narrowing opportunity.
///
/// `"resolved_same_file"`: `target_type_qn` is the caller's own file path
/// (self/this/Rust's `Self::`, set in `extract_file_data`) -- exact match
/// only, the strongest signal this function has (the receiver's type IS
/// the enclosing class, guaranteed declared in that exact file).
///
/// `"resolved_import"`: `target_type_qn` is the RAW, unmodified import
/// text the receiver's bare type name was bound to in the caller's file
/// (e.g. Java `import com.foo.User;` -> `"com.foo.User"`, JS `import
/// {User} from '../models/user'` -> `"../models/user"`). Two heuristics,
/// tried in order, both fail-open:
/// 1. Dots-to-slashes: `"com.foo.User"` -> `"com/foo/User"` -- matches
///    Java/Kotlin/Python's directory-mirrors-package convention when
///    `path` contains that translated form (with or without a trailing
///    file extension after the last segment).
/// 2. Raw substring: covers already-slash-delimited specifiers (JS/TS/Go
///    relative or package-style imports) where no dot-to-slash
///    translation is meaningful -- strips a leading `./`/`../` first so a
///    relative import matches regardless of how many directories up it
///    climbs relative to the (different) `by_name_class` candidate path.
///
/// Every other `target_type_kind` value (`"unresolved"`, unrecognized, or
/// absent) never matches -- `resolve_sites_to_edges`'s caller only invokes
/// this when both `target_type_kind`/`target_type_qn` are `Some`, so this
/// arm is defensive, not reachable in practice.
fn type_qn_matches_path(kind: &str, qn: &str, path: &str) -> bool {
    match kind {
        "resolved_same_file" => path == qn,
        "resolved_import" => {
            let dotted_to_slashed = qn.replace('.', "/");
            let stripped = qn.trim_start_matches("../").trim_start_matches("./");
            path.contains(&dotted_to_slashed) || (!stripped.is_empty() && path.contains(stripped))
        }
        _ => false,
    }
}

/// Narrow every call site in `sites` against `ctx` and produce the final,
/// deduped `CallEdge` list — a pure function of `(ctx, sites)` with no DB
/// access (the caller owns the DELETE/INSERT scope, letting `rebuild_graph`
/// clear all edges and `incremental_graph_update`, Phase B T4, clear only
/// `from_path IN delta_paths`, per plan D1/D4). Shared unchanged by both, so
/// resolution logic can never diverge between full and incremental runs.
///
/// `sites` order matters: `seen_pairs` dedup below keeps the FIRST site's
/// line/confidence per (caller, callee) pair ("first call site wins"), so
/// callers MUST load `sites` in a stable, explicit order (`ORDER BY id`) —
/// an unordered `SELECT` relies on SQLite's incidental full-table-scan
/// (rowid) order, which silently breaks the moment a `WHERE` clause makes
/// the query planner prefer an index instead (exactly what incremental's
/// delta-scoped load does) — see Phase B plan A-3.
///
/// One row per call site whose surviving candidate set, after every scoping
/// filter above, still exceeded `MAX_CALLEE_CANDIDATES` (WS3,
/// docs/plans/2026-08-18-context-intelligence-upgrade-plan.md) — the shape
/// that previously vanished as a silent zero-edge site with no trace
/// anywhere. `candidate_group_key` is the raw `callee_name` text (not a
/// resolved qualified name — by definition none of the candidates were
/// ever picked), so it's a grouping key for "sites stuck on this same
/// ambiguous name," not an identity. `candidates` (PR#6) is the real
/// identity: the actual (qualified_name, path) surviving-candidate SET, so
/// `callers()`/`reference_impact` can match a queried symbol precisely
/// instead of via the bare `candidate_group_key` (which let an unrelated
/// same-named symbol in another language/module inherit this caveat).
pub(super) struct AmbiguityGroup {
    call_site_id: i64,
    from_path: String,
    candidate_group_key: String,
    candidate_count: usize,
    // PR#6: the surviving candidate SET (qualified_name, path) pairs, persisted
    // so callers()/reference_impact can match by real identity instead of the
    // bare candidate_group_key above -- see ambiguity_group_candidates in
    // db/schema.rs for why this kills the cross-language bare-name collision.
    candidates: Vec<(String, String)>,
    reason: String,
}

pub(super) fn resolve_sites_to_edges(
    ctx: &ResolutionCtx,
    sites: &[CallSiteRow],
) -> (Vec<CallEdge>, Vec<AmbiguityGroup>) {
    // One edge per (call site, callee, kind). Distinct calls on the same line
    // remain distinct because their selected-callee byte spans identify
    // different `call_sites` rows.
    // Confidence is the resolver's verdict recorded at extraction time. A tier-2
    // call (target_class set) resolves the method within that class only.
    //
    // Candidate lookup (HashMap reads against `ctx.by_name`/`ctx.by_name_class`) is pure
    // CPU work independent per site, so it runs in parallel; the dedup merge
    // below stays sequential, walking sites in their original order so the
    // "first call site wins" line/confidence attribution is unchanged.
    //
    // Same-file preference: an unqualified call whose bare name ALSO has a
    // matching definition in the caller's own file resolves to that
    // definition, not to unrelated same-named symbols elsewhere — this is
    // Rust's own scoping, not a heuristic. Without it, every same-named
    // candidate anywhere in the repo gets an edge (private per-file helpers —
    // test fixtures, local `fn new`/`run`/`setup_db` — fan out to every other
    // file sharing that name), inflating blast_radius/caller_count for the
    // most common private-helper pattern. This mirrors the fix already
    // applied to the `receiver_is_type_path` branch above (which skips tier-1
    // entirely to dodge the same fan-out for `Type::method()` calls) — that
    // fix never covered this general by-name/by-name-class path, so the bug
    // survived here. Only fan out globally when nothing in-file matches, and
    // even then only up to MAX_CALLEE_CANDIDATES.
    // `bool` (2nd tuple field) = this site's target list was narrowed down to
    // a single candidate by the C# same-namespace check below, confirmed
    // against a real `namespace` declaration (not just a heuristic) — the
    // second loop upgrades such a site's confidence to `resolved` on that
    // signal.
    // `bool` (3rd tuple field, `weak_receiver_fallback`) — root-caused via
    // benchmarks/b2_call_graph_quality (2026-08-12): inferred/resolved/
    // textual-tier precision had collapsed to exactly 0.0 in CI, traced to
    // call sites like `some_hashmap.get(k)?` (receiver of an external/std
    // type this indexer never modeled) resolving with high confidence to
    // this repo's own unrelated `txn::get`/`telemetry::write` — the ONLY
    // same-named LOCAL symbol — purely because none of the scoping checks
    // above (same_file/same_dir/same_namespace/module_hint/arity) apply to
    // free functions vs. `.`-receiver method calls differently: a bare
    // free-function name genuinely must be in scope (same-module or `use`)
    // for real Rust to compile, but a `recv.method()` call's true target
    // depends entirely on `recv`'s type. "There happens to be exactly one
    // same-named function anywhere in the repo" is near-zero evidence for
    // what an unknown-typed receiver's method resolves to — set true only
    // when the site had a `.`-receiver AND `target_class` is still `None`
    // (this closure's own `by_name`/`by_name_class` match at the top: a
    // `Type::path()` call or a tier-2-typed `.`-receiver already came
    // through `by_name_class` with a real class name — that's positive
    // evidence, not a blind guess, even if it also falls through to this
    // same unscoped-candidates fallback below with nothing further to
    // narrow by) and it survived purely on the last, unscoped
    // `t.len() <= MAX_CALLEE_CANDIDATES` fallback with none of the positive
    // scoping signals above it firing. The second loop below downgrades such
    // an edge to `Ambiguous` instead of trusting its extraction-time
    // confidence. Free-function calls (`receiver: None`) are unaffected —
    // Rust's real name-resolution rules make that fallback more justified
    // for them, and this is a targeted fix for the specific false-positive
    // shape found, not a rewrite of the whole heuristic.
    // (candidate (qualified_name, path) pairs, namespace_confirmed, weak_receiver,
    // overflow_candidate_count, preferred_subset) per site -- factored out
    // purely for clippy::type_complexity, see the doc comment above for what
    // each field means. 4th field (WS3, docs/plans/2026-08-18-context-
    // intelligence-upgrade-plan.md): `Some(n)` ONLY on the one branch reached
    // with real candidates that exceeded MAX_CALLEE_CANDIDATES (see that
    // branch's own comment) -- every other branch (including the other,
    // genuinely-empty Vec::new() returns) leaves this `None`. Lets the second
    // loop below tell "real candidates existed, just too many to trust one"
    // apart from "nothing matched at all", which an empty Vec alone can't
    // distinguish. 5th field (WS5, same plan doc, D5): `Some(subset)` ONLY on
    // the `same_dir` branch when it found a match that is a STRICT subset of
    // `t` (i.e. real out-of-directory alternates survive too) -- every other
    // branch leaves this `None`, meaning "no ranking signal, every candidate
    // is rank 0" (the second loop's default). Lets the second loop assign
    // `candidate_rank = 0` to the preferred (same-dir) subset and `1` to
    // every other surviving candidate, instead of the old behavior of
    // silently discarding them.
    // PR#6: 4th field now carries the actual overflow candidate SET (not just
    // its length) so ambiguity_groups can persist real (qualified_name, path)
    // members instead of only a count -- callers()/reference_impact then match
    // a queried symbol by identity, not by the group's bare candidate_group_key.
    type CandidateResult = (
        Vec<(String, String)>,
        bool,
        bool,
        Option<Vec<(String, String)>>,
        Option<HashSet<(String, String)>>,
    );
    let candidates: Vec<CandidateResult> = sites
        .par_iter()
        .map(
            |(
                _,
                from_path,
                _,
                callee,
                _,
                _,
                _,
                _,
                _,
                receiver,
                target_class,
                looks_option_or_result_chained,
                module_hint,
                _,
                arg_count,
                import_path,
                target_type_kind,
                target_type_qn,
            )| {
                // Inheritance fallback (2026-08-18, B15 investigation): an
                // exact-class miss here does NOT always mean "no candidates
                // exist" -- `by_name_class` only indexes each method under
                // the class that DECLARES it, with no superclass walk, so a
                // receiver whose STATIC type is known (`target_class`
                // populated from a tracked binding -- Java's
                // `formal_parameter`, Go's `parameter_declaration`, etc.)
                // but who calls a method it only INHERITS (declared on an
                // ancestor, not on `cls` itself) used to hit this exact-key
                // miss and return zero candidates -- dropping the edge
                // entirely, with NO fallback to the unscoped `by_name` path
                // the unknown-receiver-type branch already gets. Verified
                // live via a minimal 2-class repro (a `formal_parameter`
                // calling an inherited method silently produced NO edge --
                // not even `ambiguous` -- while the identical call through a
                // local variable of the same runtime type correctly fell
                // back to `ambiguous`) and confirmed as the root cause of 3
                // of CALM's 4 real misses in B15's spring-petclinic sample
                // (`getName`/`isNew`, both declared on an ancestor of the
                // receiver's declared class, not on the declared class
                // itself).
                //
                // Gated on `ctx.by_name.contains_key(cls)` -- `cls` itself
                // must be a symbol this project actually declares (a class/
                // struct/interface, or anything else sharing that exact
                // name) before falling back, so a call through a receiver
                // typed as an UNMODELED external/stdlib type (Rust's
                // `HashMap::new()` with no `HashMap` in this project at all)
                // keeps the original "no candidates" behavior instead of
                // wrongly fanning out project-wide -- caught live by
                // `test_type_path_call_resolves_scoped_not_fanned_out`
                // regressing when the fallback was first tried unguarded.
                // `exact_class_matched` tracks which branch fired so the
                // `weak_receiver` computation below can still mark this
                // fallback's edges `ambiguous` (the receiver's exact class
                // is known, but not that it actually declares this method)
                // instead of trusting them at whatever confidence the
                // exact-class path would otherwise imply.
                //
                // WS4 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
                // D4): before falling all the way through to the unscoped
                // `by_name` path, try `resolve_via_inheritance_closure` --
                // `cls` genuinely inheriting/implementing `callee` from an
                // ancestor (confidently-resolved evidence, see that
                // function's own doc comment) is real scoping, not a guess,
                // and must NOT be marked `exact_class_matched: false` (which
                // downstream demotes to `weak_receiver`/`Ambiguous` the same
                // as a truly unscoped fallback would be).
                let (targets, exact_class_matched): (Option<Vec<SymbolCandidate>>, bool) =
                    match target_class {
                        Some(cls) => match ctx.by_name_class.get(&(callee.clone(), cls.clone())) {
                            Some(t) => {
                                // PR#8 (docs/plans/2026-08-19-evidence-
                                // architecture-execution-plan.md Part E):
                                // disambiguate a bare-name collision --
                                // by_name_class's (callee, cls) key can't
                                // by itself tell two same-named classes in
                                // different packages/files apart (both set
                                // target_class: Some("User")). When there's
                                // more than one candidate AND this call
                                // site's target_type_kind/qn narrowed to a
                                // real qualifier, prefer the subset whose
                                // path plausibly matches it. Fail-open: an
                                // empty match (heuristic missed, or every
                                // candidate is equally (im)plausible) keeps
                                // the full unscoped set, identical to
                                // pre-PR#8 behavior -- never a regression,
                                // same posture as every other narrowing
                                // filter in this function.
                                if t.len() > 1
                                    && let Some(kind) = target_type_kind
                                    && let Some(qn) = target_type_qn
                                {
                                    let qualified: Vec<(String, String, String)> = t
                                        .iter()
                                        .filter(|(_, p, _)| type_qn_matches_path(kind, qn, p))
                                        .cloned()
                                        .collect();
                                    if qualified.is_empty() {
                                        (Some(t.clone()), true)
                                    } else {
                                        (Some(qualified), true)
                                    }
                                } else {
                                    (Some(t.clone()), true)
                                }
                            }
                            None if ctx.by_name.contains_key(cls) => {
                                match resolve_via_inheritance_closure(ctx, callee, cls) {
                                    Some(ancestor_hit) => (Some(ancestor_hit), true),
                                    None => (ctx.by_name.get(callee).cloned(), false),
                                }
                            }
                            None => (None, false),
                        },
                        None => (ctx.by_name.get(callee).cloned(), false),
                    };
                let Some(t) = targets else {
                    return (Vec::new(), false, false, None, None);
                };
                let t = &t;
                // Same-language filter: a call site can only ever resolve to a
                // symbol written in the same language as its caller — a
                // cross-language name collision (e.g. a Rust `foo` incidentally
                // matching a Python `foo` elsewhere in the repo) is never a
                // real call edge no matter how well the name/class matches.
                // Applied first, before every other candidate-narrowing
                // heuristic below, since this one is a hard correctness
                // constraint rather than a preference — the rest of this
                // function is unchanged from here on, just operating on the
                // now same-language-only, language-stripped candidate list.
                let caller_lang = ctx.path_lang.get(from_path);
                let same_lang: Vec<(String, String)> = t
                    .iter()
                    .filter(|(_, _, lang)| Some(lang) == caller_lang)
                    .map(|(qn, path, _)| (qn.clone(), path.clone()))
                    .collect();
                if same_lang.is_empty() {
                    return (Vec::new(), false, false, None, None);
                }
                let t = &same_lang;
                // Return-shape exclusion: `foo.bar()?`/`foo.bar().unwrap()` can only
                // compile if `bar`'s return type is `Option`/`Result` — so a candidate
                // whose own signature returns neither is *provably* not this call's
                // real target, not just an unlikely one. Only filters when the site
                // shows the signal at all; otherwise every existing candidate stays,
                // unchanged from before this filter existed.
                let filtered: Vec<(String, String)>;
                let t: &Vec<(String, String)> = if *looks_option_or_result_chained {
                    filtered = t
                        .iter()
                        .filter(|(qn, _)| {
                            ctx.sig_by_qn
                                .get(qn)
                                .is_some_and(|sig| signature_returns_option_or_result(sig))
                        })
                        .cloned()
                        .collect();
                    &filtered
                } else {
                    t
                };
                if t.is_empty() {
                    return (Vec::new(), false, false, None, None);
                }
                // B3/A' arity gate (Tier B audit; extended to Go 2026-07-29
                // self-audit "A'" pass): Elixir's `greet/1` and `greet/2` are
                // different clauses, not overloads of one symbol; Go's same-
                // named functions across different packages can also collide
                // on a bare name -- either way, a bare-name candidate list can
                // hold multiple real, distinct declarations at once
                // (same_file's own match below would otherwise still
                // conflate them when they live in the caller's file), so this
                // narrows by each candidate's OWN declared arity
                // (`ctx.arity_by_qn`, from a real def/defp or func/method
                // declaration, not a guess) before same_file/same_dir/
                // same_namespace even run. Comparison is variadic-aware: a Go
                // candidate whose last param is `...T` accepts its minimum
                // arity or MORE (never fewer), so `n >= min_arity`, not `==` --
                // Elixir has no variadic-arg concept in the `def`/`defp` shape
                // this indexer covers, so its own candidates are always
                // `is_variadic: false` and keep the original exact-match
                // behavior unchanged. Exactly 1 survivor is real declaration-
                // verified evidence — same standing as same_namespace's
                // C#-only confirmation below — so it short-circuits straight
                // to `resolved`. Fail-open (keep `t` unchanged) when arity is
                // unknown at either end, or narrowing would leave nothing:
                // absence of a match is never proof the true candidate isn't
                // in `t` (e.g. a multi-clause function this pass doesn't
                // fully model, or a language not yet arity-verified at all).
                // Guarded on `target_class.is_none()`: when `target_class` IS
                // set, `t` already came from `ctx.by_name_class` (a real
                // receiver-type match, e.g. Go's `s.Process()` with `s`
                // statically typed `*Service`) -- a tier-2 resolution whose
                // own, already-correct confidence ("inferred") this gate must
                // not silently overwrite by "confirming" a single survivor
                // that target_class narrowing already produced for an
                // unrelated reason. Caught live by
                // `test_tier2_go_pointer_receiver` regressing to "resolved"
                // before this guard existed -- same class of bug
                // `receiver_is_type_path`'s tier-1 skip (see this same
                // function's earlier comment) was written to prevent.
                let arity_narrowed: Vec<(String, String)>;
                let t: &Vec<(String, String)> = if target_class.is_none()
                    && matches!(caller_lang.map(String::as_str), Some("elixir" | "go"))
                    && let Some(n) = arg_count
                {
                    let narrowed: Vec<(String, String)> = t
                        .iter()
                        .filter(|(qn, _)| {
                            ctx.arity_by_qn
                                .get(qn)
                                .is_some_and(|&(min_arity, variadic)| {
                                    if variadic {
                                        *n >= min_arity
                                    } else {
                                        *n == min_arity
                                    }
                                })
                        })
                        .cloned()
                        .collect();
                    if narrowed.len() == 1 {
                        return (narrowed, true, false, None, None);
                    } else if narrowed.is_empty() {
                        t
                    } else {
                        arity_narrowed = narrowed;
                        &arity_narrowed
                    }
                } else {
                    t
                };
                // WS2 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
                // D1): import-binding preference. `import_path` is
                // `resolve_tier1`'s `ResolveResult::resolved_path` — the actual
                // import target text for a bare call whose callee name matched
                // `ctx.import_map` (`from foo import bar` -> `Some("foo")`).
                // This is REAL import-binding evidence (a language-level fact
                // from the source's own import statement), not a convention or
                // guess — checked even before `module_hint` below, which is
                // only ever inferred from a lowercase `::`-qualified call's own
                // text. Narrows the same-language candidate list to whichever
                // file's stem matches the import target's last path segment
                // (same matching helper the whole-module require/import branch
                // in `extract_file_data` already uses for the
                // receiver-is-a-module-alias case). Fail-open: an import target
                // matching NO current candidate falls through to the same
                // module_hint/same_file/same_dir/... chain unchanged, never
                // removes a real candidate that simply isn't corroborated by
                // this signal — same posture as every other narrowing pass
                // here. Previously this evidence was computed and immediately
                // discarded (D1 in the upgrade plan); this is the fix.
                if let Some(path) = import_path
                    && let Some(seg) = crate::indexer::parser::module_path_last_segment(path)
                {
                    let imported: Vec<_> = t
                        .iter()
                        .filter(|(_, p)| {
                            Path::new(p.as_str())
                                .file_stem()
                                .is_some_and(|stem| stem == seg.as_str())
                        })
                        .cloned()
                        .collect();
                    if !imported.is_empty() {
                        return (imported, false, false, None, None);
                    }
                }
                // Module-qualifier preference: `crate::telemetry::timed_tool()`
                // carries an explicit, unambiguous module segment in the source
                // text (see `parser::module_hint_of`) — stronger evidence than
                // incidental file collocation, so it's checked *before* the
                // same-file-as-caller fallback below. Without this, a call
                // site like that one — whose bare callee name also happens to
                // match a same-named symbol in the caller's OWN file (e.g. a
                // same-named wrapper method delegating to the free function it
                // wraps) — silently resolved to that unrelated same-file
                // symbol instead of the module actually named in the source,
                // in the worst case fabricating a self-recursive edge.
                if let Some(hint) = module_hint {
                    let hinted: Vec<_> = t
                        .iter()
                        .filter(|(_, p)| {
                            Path::new(p.as_str())
                                .file_stem()
                                .is_some_and(|stem| stem == hint.as_str())
                        })
                        .cloned()
                        .collect();
                    if !hinted.is_empty() {
                        return (hinted, false, false, None, None);
                    }
                }
                let same_file: Vec<_> = t.iter().filter(|(_, p)| p == from_path).cloned().collect();
                // Tier-1.5 same-directory preference (8-language plan P1.3,
                // V1 — no schema change): checked only when same_file found
                // nothing, and only for go/java/c/cpp. A directory is a much
                // stronger scoping signal than "anywhere in the repo" for
                // these: Go's compilation unit literally IS the directory
                // (package = dir); a Java package commonly maps 1:1 onto a
                // directory even without a build-tool classpath on the
                // classpath to consult; C/C++ headers and their .c/.cpp
                // implementation conventionally live alongside each other.
                // Rust/Python/JS/TS are deliberately excluded — they already
                // resolve unqualified calls correctly via import_map/type_map
                // at extraction time, so widening this to them would just be
                // a second, redundant (and potentially wrong) narrowing pass.
                let same_dir = || -> Option<Vec<(String, String)>> {
                    if !matches!(
                        caller_lang.map(String::as_str),
                        Some("go" | "java" | "c" | "cpp")
                    ) {
                        return None;
                    }
                    let caller_dir = Path::new(from_path).parent();
                    let dir_matches: Vec<_> = t
                        .iter()
                        .filter(|(_, p)| Path::new(p.as_str()).parent() == caller_dir)
                        .cloned()
                        .collect();
                    (!dir_matches.is_empty()).then_some(dir_matches)
                };
                // Same-namespace preference (8-language plan P1.5's "using ->
                // namespace" remainder), C#-only: a `Type.Method()` call
                // (part A above sets `target_class = receiver` for these)
                // whose bare class name collides across namespaces can be
                // disambiguated by which namespace(s) this caller's `using`
                // directives actually bring into scope — real evidence from
                // `NamespaceMap` (built from `namespace` declarations, not a
                // directory convention), so a narrowing to exactly one
                // candidate here also upgrades confidence to `resolved`
                // below (see the `bool` in this closure's return type).
                let same_namespace = || -> Option<Vec<(String, String)>> {
                    if caller_lang.map(String::as_str) != Some("csharp") {
                        return None;
                    }
                    let usings = ctx.caller_usings.get(from_path)?;
                    let ns_matches: Vec<_> = t
                        .iter()
                        .filter(|(_, p)| usings.iter().any(|ns| ctx.namespace_map.contains(ns, p)))
                        .cloned()
                        .collect();
                    (!ns_matches.is_empty()).then_some(ns_matches)
                };
                // `weak_receiver` applies uniformly across same_file/
                // same_dir/the final catch-all below: it's never about WHICH
                // narrowing branch produced a match, only whether this call
                // site is a `.`-receiver whose type target_class never
                // resolved. "Same file" genuinely IS Rust's own scoping for
                // a bare FREE-FUNCTION call (`receiver: None`) — but for a
                // METHOD call, a file that collocates both an unrelated
                // local `impl SomeEnum { fn as_str() }` and a call to e.g.
                // `some_string.as_str()` (the stdlib's, never in `ctx` at
                // all) is exactly as coincidental as the global fallback
                // below, just scoped to one file instead of the whole repo.
                // Verified live (2026-08-12): crates/calm-server/src/tools/
                // edit.rs's own `.as_str()` call on a `String` field (edit.rs
                // ALSO defines `GateRequirement::as_str`) and crates/
                // calm-core/src/txn.rs's `.as_str()` on a `TxState` variant
                // (txn.rs ALSO defines `TxState::as_str`) both fanned out to
                // the wrong same-file `as_str` this way — part of the same
                // B2 precision collapse the final catch-all's
                // `weak_receiver_fallback` already fixed, just reached via
                // the same_file branch instead of the unscoped one. A
                // `Type::path()` call or a tier-2-typed `.`-receiver
                // (target_class: Some(..), already scoped via
                // `ctx.by_name_class` above) keeps full trust here even when
                // it ALSO happens to match same_file/same_dir — that's a
                // real, specific class already known, not a blind guess.
                // `self`/`this` are excluded from the weak signal even when
                // target_class stayed None (tier-2's self/this handling
                // doesn't fire for every shape): the receiver's real type IS
                // by construction the enclosing impl/class, so same_file
                // finding a method there is strong evidence, not a
                // coincidence — same standing as same_file already has for
                // free functions. Caught live by
                // test_caller_count_excludes_ambiguous_fan_out_edges'
                // `self.as_str()` regressing to `ambiguous` before this
                // exclusion existed.
                // `!exact_class_matched` (not `target_class.is_none()`) so the
                // new inheritance-fallback branch above -- receiver class
                // known, but the exact-class lookup itself missed -- is
                // treated with the same "weak evidence" caution as a
                // genuinely unknown-type receiver, not trusted as if the
                // exact-class path had actually confirmed it. Equivalent to
                // the old `target_class.is_none()` check whenever that
                // fallback never fires (exact_class_matched is only ever
                // false when target_class was already None in that case).
                let weak_receiver = receiver.is_some()
                    && !exact_class_matched
                    && !matches!(receiver.as_deref(), Some("self" | "this"));
                if !same_file.is_empty() {
                    (same_file, false, weak_receiver, None, None)
                } else if let Some(dir_matches) = same_dir() {
                    // WS5 (docs/plans/2026-08-18-context-intelligence-upgrade-
                    // plan.md, D5) scoping note, found DURING implementation,
                    // not assumed from the plan's prose: the plan's own D5
                    // root-cause text calls directory "a convention, not a
                    // scoping rule" for "Java/C/C++" -- but that is only true
                    // for C/C++. An UNQUALIFIED same-package Go call, or a
                    // same-package (no-import) Java type reference, is
                    // language-enforced scoping every bit as real as
                    // `same_file` -- Go's compiler and Java's own name-
                    // resolution rules make an out-of-package/out-of-import
                    // candidate structurally IMPOSSIBLE as this call's real
                    // target, not merely unlikely. Confirmed live: relaxing
                    // this for Go/Java broke
                    // `test_go_same_directory_call_resolves_not_fanned_out` /
                    // `test_java_same_package_call_resolves_not_fanned_out`
                    // (both assert a same-directory match must NOT co-surface
                    // an unrelated other-package same-named symbol -- correct
                    // behavior, backed by real language rules, not a bug).
                    // C/C++ have no package/namespace-to-directory
                    // correspondence AT ALL (the preprocessor doesn't care
                    // where a header/impl physically lives), so "true target
                    // in a sibling directory, decoy locally" (this plan's own
                    // flagship WS5 fixture, `E_same_dir_decoy_vs_true_target`,
                    // is a C fixture) is a REAL, common shape there with no
                    // language rule to rule it out -- unlike Go/Java, where
                    // the identical-looking shape can't actually occur for a
                    // bare/unqualified reference. Only C/C++ get the
                    // non-destructive ranker below; Go/Java keep the
                    // pre-WS5 hard-filter behavior, unchanged.
                    let same_dir_is_real_scoping =
                        matches!(caller_lang.map(String::as_str), Some("go" | "java"));
                    if same_dir_is_real_scoping || dir_matches.len() == t.len() {
                        // Either this call site's language makes an
                        // out-of-directory target structurally impossible
                        // (Go/Java), or every surviving candidate already
                        // lives in the caller's directory (no out-of-
                        // directory alternate to preserve either way) --
                        // unchanged from the pre-WS5 filter behavior.
                        (dir_matches, false, weak_receiver, None, None)
                    } else {
                        // C/C++ only, reached here: a real out-of-directory
                        // alternate exists and directory carries no language-
                        // level scoping meaning for these -- keep the FULL
                        // surviving set (`t.clone()`, not `dir_matches`) so
                        // the true target, if it lives elsewhere, survives
                        // as a candidate, just ranked lower than the same-
                        // directory preference (`preferred`, consumed by the
                        // second loop below to set `candidate_rank`). No new
                        // confidence rule is needed here: the second loop's
                        // existing `targets.len() > 1 => Ambiguous` already
                        // downgrades this correctly the moment a real
                        // alternate is kept (previously it never saw more
                        // than 1 survivor here because the alternates had
                        // already been dropped) -- matching WS5's "all still
                        // ambiguous-tier, not a clean single target"
                        // requirement exactly.
                        let preferred: HashSet<(String, String)> =
                            dir_matches.into_iter().collect();
                        (t.clone(), false, weak_receiver, None, Some(preferred))
                    }
                } else if let Some(ns_matches) = same_namespace() {
                    let confirmed = ns_matches.len() == 1;
                    (ns_matches, confirmed, false, None, None)
                } else if t.len() <= MAX_CALLEE_CANDIDATES {
                    // Reached with NONE of the positive scoping signals above
                    // firing — see this function's top doc comment and the
                    // `weak_receiver` comment just above. Caught live by
                    // test_type_path_call_resolves_scoped_not_fanned_out and
                    // test_tier2_method_resolution regressing to `ambiguous`
                    // before the target_class.is_none() half of this guard
                    // existed.
                    (t.clone(), false, weak_receiver, None, None)
                } else {
                    // WS3 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
                    // D3): unlike every branch above, this one is reached with
                    // REAL candidates that existed (`t` is non-empty, just over
                    // MAX_CALLEE_CANDIDATES) -- "too many to trust any one" is a
                    // different fact than "none exist at all" (the other empty-Vec
                    // returns above/below this closure). Threading `Some(t.len())`
                    // here (and only here) lets the second loop below distinguish
                    // this specific case and record it as an ambiguity_groups row
                    // instead of silent zero-edge dropping -- previously
                    // indistinguishable from a genuinely unresolved site.
                    (Vec::new(), false, false, Some(t.clone()), None)
                }
            },
        )
        .collect();

    let mut edges: Vec<CallEdge> = Vec::new();
    let mut ambiguity_groups: Vec<AmbiguityGroup> = Vec::new();
    let mut seen_pairs: HashSet<(i64, String, String)> = HashSet::new();
    for (
        (
            call_site_id,
            from_path,
            enc_qn,
            callee,
            line,
            _,
            _,
            _,
            confidence,
            _receiver,
            _target_class,
            _,
            _,
            edge_kind,
            _,
            _,
            _,
            _,
        ),
        (targets, namespace_confirmed, weak_receiver_fallback, overflow_count, preferred_subset),
    ) in sites.iter().zip(candidates.iter())
    {
        if let Some(overflow_candidates) = overflow_count {
            ambiguity_groups.push(AmbiguityGroup {
                call_site_id: *call_site_id,
                from_path: from_path.clone(),
                candidate_group_key: callee.clone(),
                candidate_count: overflow_candidates.len(),
                candidates: overflow_candidates.clone(),
                reason: "unscoped_candidates_exceeded_max_callee_candidates".to_string(),
            });
        }
        // >1 surviving candidate means this call site's edge is duplicated
        // across multiple distinct symbols with nothing left to break the
        // tie — mark it `Ambiguous` regardless of which branch produced it,
        // rather than let it masquerade as an ordinary single-target edge at
        // its originally recorded confidence (which was computed per call
        // site, not per final-candidate-count). Same treatment for a single
        // survivor that only cleared the unscoped by-name fallback with a
        // receiver of unknown type (`weak_receiver_fallback`) — see this
        // function's top doc comment; that's "no other candidate left after
        // exclusion", not "this candidate was actually identified".
        let effective_confidence = if targets.len() > 1 {
            EdgeConfidence::Ambiguous.as_str()
        } else if *namespace_confirmed {
            EdgeConfidence::Resolved.as_str()
        } else if *weak_receiver_fallback {
            EdgeConfidence::Ambiguous.as_str()
        } else {
            confidence.as_str()
        };
        for (to_qn, to_path) in targets {
            if !seen_pairs.insert((*call_site_id, to_qn.clone(), edge_kind.clone())) {
                continue;
            }
            // WS5 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md,
            // D5): `preferred_subset` is `Some` only from the `same_dir`
            // branch when a real out-of-directory alternate survived
            // alongside it (see that branch's own comment) -- membership in
            // it means "same directory as the caller", rank 0. Every other
            // candidate (including every candidate when `preferred_subset`
            // is `None`, i.e. no ranking signal at all) is rank 0 too,
            // UNLESS it lost the `same_dir` tie-break, in which case it's
            // rank 1 -- an ordinal, not a score.
            let candidate_rank: i64 = match preferred_subset {
                Some(preferred) if !preferred.contains(&(to_qn.clone(), to_path.clone())) => 1,
                _ => 0,
            };
            edges.push(CallEdge {
                from_symbol: enc_qn.clone(),
                to_symbol: to_qn.clone(),
                call_site_line: line.map(|l| l as i32),
                call_site_id: Some(*call_site_id),
                edge_confidence: effective_confidence.to_string(),
                from_path: Some(from_path.clone()),
                to_path: Some(to_path.clone()),
                edge_kind: edge_kind.clone(),
                candidate_rank,
            });
        }
    }
    (edges, ambiguity_groups)
}

/// `ambiguity_groups` mirrors `call_edges`: the caller owns the DELETE scope
/// (full sweep in `rebuild_graph`, `from_path`-scoped in
/// `incremental_graph_update`) so this is a pure insert, matching
/// `insert_call_edges_batch`'s own contract.
pub(super) fn insert_ambiguity_groups_batch(
    tx: &rusqlite::Transaction,
    groups: &[AmbiguityGroup],
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO ambiguity_groups (call_site_id, from_path, candidate_group_key, candidate_count, reason) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    // PR#6: persist the real (qualified_name, path) candidate members alongside
    // the parent row so callers()/reference_impact can match by identity --
    // see ambiguity_group_candidates in db/schema.rs.
    let mut candidate_stmt = tx.prepare(
        "INSERT INTO ambiguity_group_candidates (group_id, candidate_qn, candidate_path) \
         VALUES (?1, ?2, ?3)",
    )?;
    for g in groups {
        stmt.execute(rusqlite::params![
            g.call_site_id,
            g.from_path,
            g.candidate_group_key,
            g.candidate_count as i64,
            g.reason,
        ])?;
        let group_id = tx.last_insert_rowid();
        for (qn, path) in &g.candidates {
            candidate_stmt.execute(rusqlite::params![group_id, qn, path])?;
        }
    }
    Ok(())
}
