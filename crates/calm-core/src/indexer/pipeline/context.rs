//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 3): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). Resolution-context construction + inheritance-closure
//! lookup, consumed by `resolve_sites_to_edges` (not yet extracted -- slice
//! 4). Move-only -- no logic changed, only relocated.
//!
//! `ResolutionCtx`/`SymbolCandidate` stay defined in `pipeline.rs` itself
//! (not moved here) because `resolve_sites_to_edges` still lives there and
//! reads `ResolutionCtx`'s fields directly -- same "shared type stays at the
//! ancestor until every consumer has moved" pattern already used for
//! `ExtractedFile`/`CallSiteData` in slices 1-2. `build_resolution_context`/
//! `resolve_via_inheritance_closure` are `pub(super)` (not plain private)
//! since Rust module privacy is "visible in the defining module and its
//! descendants" -- `pipeline.rs` is this module's ANCESTOR, not a
//! descendant, so plain `fn` would be invisible to it (verified via
//! `callers()` before this move: build_resolution_context is called by
//! rebuild_graph/incremental_graph_update, resolve_via_inheritance_closure by
//! resolve_sites_to_edges -- all three still in pipeline.rs).
//! `build_inheritance_closure` and `MAX_INHERITANCE_DEPTH` stay plain
//! private: both are used only inside this module (by
//! `build_resolution_context`, moved here in the same slice).

use std::collections::{HashMap, HashSet};

use super::{ResolutionCtx, SymbolCandidate};

/// Build the candidate-lookup tables `resolve_sites_to_edges` narrows
/// against: `by_name`/`by_name_class` (bare-name and `Type::method` lookup),
/// `sig_by_qn` (return-shape filter), `path_lang` (same-language filter),
/// `caller_usings` (C# same-namespace filter). One global `SELECT` over
/// `symbols`/`import_edges` — same cost whether the caller is about to
/// resolve every site (`rebuild_graph`) or only a delta
/// (`incremental_graph_update`, Phase B plan §3 step 5: resolution inputs
/// like a candidate's own signature can change even when the *site* calling
/// it didn't, so the lookup tables themselves are never scoped to the delta).
pub(super) fn build_resolution_context<'a>(
    tx: &rusqlite::Transaction,
    namespace_map: &'a crate::indexer::csharp_namespace::NamespaceMap,
) -> rusqlite::Result<ResolutionCtx<'a>> {
    // name → [(qn, path, language)] for tier-1; (name, class) → [(qn, path,
    // language)] for tier-2. `language` rides along so a call site can never
    // resolve to a same-named symbol written in a different language (see
    // `path_lang` and the same-language filter below) — a bare-name/textual
    // match across languages is never a real call, just an incidental name
    // collision (e.g. a Rust `new` and a Python `new` sharing `by_name`).
    let mut by_name: HashMap<String, Vec<SymbolCandidate>> = HashMap::new();
    let mut by_name_class: HashMap<(String, String), Vec<SymbolCandidate>> = HashMap::new();
    // qualified_name → signature, so the `MAX_CALLEE_CANDIDATES` fallback below
    // can tell whether a candidate's return type could possibly be
    // `Option`/`Result` — see `looks_option_or_result_chained`'s doc comment.
    let mut sig_by_qn: HashMap<String, String> = HashMap::new();
    // path → language, ONE ENTRY PER INDEXED FILE -- bugfix (found via B7's
    // real express/test/utils.js miss): this used to be derived only from
    // `symbols` (populated inside that table's own loop below), so a file
    // with ZERO top-level named functions/classes anywhere in it -- the
    // ordinary shape of a JS/TS test file written with `describe`/`it`/
    // `test` nested-anonymous-callback style -- never got an entry at all.
    // The same-language safety filter in `resolve_sites_to_edges`
    // (`Some(candidate_lang) == ctx.path_lang.get(caller_path)`) then came
    // out `Some(_) == None`, always false, silently emptying every outgoing
    // candidate list for every call that file made -- not a resolution-
    // confidence gap, a total blind spot, independent of call shape or
    // nesting depth (root-caused live via 8 controlled repro fixtures).
    // `file_index` already records `(path, language)` for every indexed
    // file regardless of symbol count -- the exact same `language_for_
    // extension(ext)` value `extract_file_data` itself was called with (see
    // `driver.rs`'s `upsert_file_index` call sites) -- so it's the correct,
    // already-existing source of truth, not a new concept.
    let mut path_lang: HashMap<String, String> = HashMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT path, language FROM file_index WHERE language IS NOT NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (path, language) in rows {
            path_lang.insert(path, language);
        }
    }
    // qualified_name → (declared arity, is_variadic) -- Elixir/Go for now --
    // feeds the arity gate below (B3/A').
    let mut arity_by_qn: HashMap<String, (i64, bool)> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT name, qualified_name, path, class_context, signature, language, arity, arity_variadic FROM symbols",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, bool>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, qn, path, cls, sig, language, arity, variadic) in rows {
            by_name.entry(name.clone()).or_default().push((
                qn.clone(),
                path.clone(),
                language.clone(),
            ));
            if let Some(c) = cls {
                by_name_class
                    .entry((name, c))
                    .or_default()
                    .push((qn.clone(), path, language));
            }
            if let Some(a) = arity {
                arity_by_qn.insert(qn.clone(), (a, variadic));
            }
            sig_by_qn.insert(qn, sig);
        }
    }

    // C# `using X;` directives, per caller file — feeds the same-namespace
    // candidate narrowing below (8-language plan P1.5's "using -> namespace"
    // remainder). `import_edges` is already populated by this point (this
    // function runs after every file in the current batch is parsed and
    // persisted), so no extra parse pass is needed; filtering to `.cs` keeps
    // this cheap and skips rows other languages' imports could never match
    // anyway (`NamespaceMap` only ever knows about C# namespaces).
    let mut caller_usings: HashMap<String, HashSet<String>> = HashMap::new();
    if !namespace_map.is_empty() {
        let mut stmt = tx.prepare(
            "SELECT from_path, module_name FROM import_edges WHERE from_path LIKE '%.cs'",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (from_path, module_name) in rows {
            caller_usings
                .entry(from_path)
                .or_default()
                .insert(module_name);
        }
    }

    let inheritance_closure = build_inheritance_closure(tx)?;

    Ok(ResolutionCtx {
        by_name,
        by_name_class,
        sig_by_qn,
        path_lang,
        caller_usings,
        namespace_map,
        arity_by_qn,
        inheritance_closure,
    })
}

const MAX_INHERITANCE_DEPTH: usize = 12;

/// WS4 (docs/plans/2026-08-18-context-intelligence-upgrade-plan.md, D4):
/// class/interface qualified_name -> its transitive `extends`/`implements`
/// ancestors, closest-first (multi-source BFS), cycle-safe (a `visited` set
/// per start node), bounded to `MAX_INHERITANCE_DEPTH` hops so a pathological
/// or accidentally-cyclic relation chain can never loop or blow up cost.
///
/// **Hard gate, non-negotiable (post-WS2 review):** only `type_relations`
/// rows whose OWN `confidence` is `resolved` (or, once formal type
/// resolution exists, `formal`) ever feed this closure -- NEVER `textual`.
/// `extract_file_data`/`graph::type_resolve::resolve_cross_file_type_relations`
/// already stamp each relation with its own confidence (`resolved` for a
/// real same-file or unambiguous cross-file class match, `textual` when the
/// target text didn't resolve to any known symbol -- see
/// `edges::TypeRelationData`). A `textual` ancestor relation is exactly the
/// same class of weak evidence `resolve_sites_to_edges` already refuses to
/// trust for a call (`weak_receiver`) -- feeding it here would fix Law 2 on
/// the call graph while silently reintroducing the identical violation one
/// layer up, through the type graph. Depends on
/// `graph::type_resolve::resolve_cross_file_type_relations` having already
/// run this pass (both `rebuild_graph` and `incremental_graph_update` call
/// it before `build_resolution_context` specifically so this closure never
/// sees last-pass-stale `to_symbol`/`confidence` values).
fn build_inheritance_closure(
    tx: &rusqlite::Transaction,
) -> rusqlite::Result<HashMap<String, Vec<Vec<String>>>> {
    // `target_class` (this closure's own lookup key -- see
    // `resolve_via_inheritance_closure`'s call site) is always a BARE class
    // name: `resolve_tier2`/the Rust `effective_receiver`/etc. paths that set
    // it never had a qualified_name available to use, the same limitation
    // `ctx.by_name_class`'s own `(name, class)` key already has. But
    // `type_relations.from_symbol`/`to_symbol` are real qualified names. This
    // closure is therefore built keyed by BARE name, translated from the
    // qualified `type_relations` rows through a `symbols` reverse lookup --
    // inheriting the exact same "two classes named the same thing in
    // different files collide" limitation `by_name_class` already accepts,
    // not a new one WS4 introduces.
    let qn_to_name: HashMap<String, String> = {
        let mut stmt = tx.prepare("SELECT qualified_name, name FROM symbols")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?
    };
    let direct_qn: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT from_symbol, to_symbol FROM type_relations \
             WHERE relation_kind IN ('extends', 'implements') AND confidence = 'resolved' \
             AND to_symbol IS NOT NULL",
        )?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut direct_parents: HashMap<String, Vec<String>> = HashMap::new();
    for (child_qn, parent_qn) in direct_qn {
        let (Some(child), Some(parent)) = (qn_to_name.get(&child_qn), qn_to_name.get(&parent_qn))
        else {
            continue;
        };
        direct_parents
            .entry(child.clone())
            .or_default()
            .push(parent.clone());
    }

    let mut closure: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    for start in direct_parents.keys() {
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(start.clone());
        let mut levels: Vec<Vec<String>> = Vec::new();
        let mut frontier: Vec<String> = vec![start.clone()];
        let mut depth = 0;
        while !frontier.is_empty() && depth < MAX_INHERITANCE_DEPTH {
            let mut next: Vec<String> = Vec::new();
            for node in &frontier {
                if let Some(parents) = direct_parents.get(node) {
                    for p in parents {
                        if visited.insert(p.clone()) {
                            next.push(p.clone());
                        }
                    }
                }
            }
            if !next.is_empty() {
                levels.push(next.clone());
            }
            frontier = next;
            depth += 1;
        }
        closure.insert(start.clone(), levels);
    }
    Ok(closure)
}

/// WS4: called only when `cls`'s own `by_name_class` entry for `callee`
/// missed but `cls` itself is a real project symbol (the exact gate the
/// caller below already applies before reaching here). Walks
/// `ctx.inheritance_closure`'s LEVELS nearest-first (mirroring real single-
/// inheritance/interface method-resolution order -- a nearer ancestor's own
/// declaration is what a real call actually reaches) and, at the first level
/// with ANY declaring ancestor, unions every match across every ancestor AT
/// THAT LEVEL. Confident only when that union is a singleton: e.g.
/// `interface Mixed extends IA, IB` where both `IA` and `IB` declare the
/// same method name is a genuine tie at depth 1 -- picking whichever
/// happened to come first in source order would be exactly the kind of
/// unscoped guess this function exists to avoid, so a >1 union at the
/// nearest declaring level stops here rather than either guessing or
/// incorrectly continuing past it to a farther, spuriously-unique level.
/// Returns `None` (fall through unchanged to the existing unscoped `by_name`
/// fallback) on no hit anywhere in the closure, or a tied hit at the
/// nearest declaring level -- WS4 deliberately defers the interface/multi-
/// implementor "legitimate polymorphism" case (the plan's own future
/// `polymorphic` EdgeConfidence variant) rather than guessing a single
/// wrong pick here.
pub(super) fn resolve_via_inheritance_closure(
    ctx: &ResolutionCtx,
    callee: &str,
    cls: &str,
) -> Option<Vec<SymbolCandidate>> {
    let levels = ctx.inheritance_closure.get(cls)?;
    for level in levels {
        let mut hits: Vec<SymbolCandidate> = Vec::new();
        for ancestor in level {
            if let Some(t) = ctx
                .by_name_class
                .get(&(callee.to_string(), ancestor.clone()))
            {
                for cand in t {
                    if !hits.contains(cand) {
                        hits.push(cand.clone());
                    }
                }
            }
        }
        if !hits.is_empty() {
            return (hits.len() == 1).then_some(hits);
        }
    }
    None
}
