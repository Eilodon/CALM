//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 2): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). Per-file parsing/resolution (`extract_file_data`, Tier-0/1/2/3)
//! and its DB-persistence counterpart (`persist_file`). Move-only -- no logic
//! changed, only relocated. `extract_file_data`/`persist_file` are
//! `pub(super)` (not plain private) since Rust module privacy is "visible in
//! the defining module and its descendants" -- `pipeline.rs` is this module's
//! ANCESTOR, not a descendant, so plain `fn` would be invisible to it (same
//! reasoning as `discovery.rs`, slice 1). `formal_resolution_timeout_count`
//! stays `pub` and is re-exported by `pipeline.rs` at its unchanged
//! `crate::indexer::pipeline::formal_resolution_timeout_count` path (verified
//! via `callers()` before this move: one real external caller in
//! calm-server's `tools/recover.rs`). `ExtractedFile`/`CallSiteData` stay
//! defined in `pipeline.rs` (shared by other not-yet-extracted slices too) and
//! are pulled in here via `super::` -- plain private is enough for that
//! direction since a child module can always see its ancestor's private items.

use std::collections::{HashMap, HashSet};

use crate::indexer::chunker::{CodeChunk, chunk_file};
use crate::indexer::edges::{
    insert_code_chunks_batch, insert_import_edges_batch, insert_symbols_batch,
};
use crate::indexer::parser::{
    MODULE_ENCLOSING, ParsedSymbol, extract_calls_from_tree, extract_file_aliases_from_tree,
    extract_symbols_from_tree, extract_symbols_shallow, extract_type_map_from_tree, parse_tree,
};
use crate::types::EdgeConfidence;

use super::{CallSiteData, ExtractedFile};

/// Process-global counter of formal-resolution cancellations/timeouts
/// (ADR-A1) -- surfaced via `indexing_status` so a silently-cancelled
/// `resolve_file` call is no longer invisible. A cancelled resolution must
/// never be treated as equivalent to "resolved, found nothing": both used
/// to collapse to the same empty `HashSet`, so a file's call-graph edges
/// could silently flip between `formal` and `textual` confidence across
/// reindexes purely due to machine load, with zero signal to explain why.
static FORMAL_RESOLUTION_TIMEOUTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Count of formal-resolution cancellations since this process started --
/// see `FORMAL_RESOLUTION_TIMEOUTS` and `IndexingStatusOutput::
/// formal_resolution_timeouts`. A nonzero, growing count on a real repo
/// means `RESOLVE_TIMEOUT` (formal.rs) is tripping under load, silently
/// denying some files their `formal` confidence upgrade this pass -- see
/// ADR-A2 for the deterministic (machine-load-independent) follow-up.
pub fn formal_resolution_timeout_count() -> u64 {
    FORMAL_RESOLUTION_TIMEOUTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Distinguishes `resolve_file` completing with no formal edges
/// (`Ok(vec![])`) from being cancelled/timed out (`Err`) -- previously both
/// collapsed to an identical empty `HashSet` via `.unwrap_or_default()`, so
/// a file's call sites could silently flip from `formal` to `textual`
/// confidence across reindexes purely from timing jitter, with no signal to
/// tell the two cases apart (ADR-A1). `Err` still degrades to an empty set
/// here (unchanged behavior for the `formally_resolved.contains(...)` check
/// below -- tier-1/tier-2 confidence is untouched either way), but now
/// increments a counter and logs a warning instead of failing silently.
pub(super) fn formally_resolved_names(
    result: anyhow::Result<Vec<crate::resolver::formal::FormalEdge>>,
    lang: &str,
    rel: &str,
) -> HashSet<String> {
    match result {
        Ok(edges) => edges.into_iter().map(|e| e.reference_symbol).collect(),
        Err(e) => {
            FORMAL_RESOLUTION_TIMEOUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(
                "formal resolution cancelled for {rel} ({lang}): {e} -- calls in this file \
                 cannot be upgraded to `formal` confidence this pass"
            );
            HashSet::new()
        }
    }
}

/// Parse and resolve one file's symbols, imports, and call sites. No DB access —
/// safe to run concurrently across files (see [`run_indexing_pipeline`]).
///
/// `qualified_name` is `relpath::name` (`#line` suffix on intra-file collision)
/// so the UNIQUE(qualified_name) index never rejects a real symbol.
pub(super) fn extract_file_data(
    rel: &str,
    lang: &str,
    source: &str,
    entry_point_patterns: &[String],
    formal: &crate::resolver::formal::FormalResolver,
) -> ExtractedFile {
    // SQL (8-language plan P3.3) is its own standalone module, not a
    // tree-sitter grammar — its DDL vocabulary and dialect-specific
    // procedural bodies don't fit the per-node-kind-table shape every other
    // language here uses (see `indexer::sql`'s module doc comment). Handled
    // entirely before `parse_tree` even runs.
    if lang == "sql" {
        let sql_file = crate::indexer::sql::extract_sql_file(rel, source);
        let symbol_count = sql_file.symbols.len();
        let chunks = chunk_pending(source, &sql_file.symbols);
        let call_sites = sql_file
            .references
            .into_iter()
            .map(|r| CallSiteData {
                enclosing_qn: r.enclosing_qn,
                callee: r.target_name,
                line: r.line,
                callee_start_byte: Some(r.callee_start_byte as i64),
                callee_end_byte: Some(r.callee_end_byte as i64),
                identity_version: 2,
                confidence: r.confidence.as_str().to_string(),
                receiver: None,
                target_class: None,
                looks_option_or_result_chained: false,
                module_hint: None,
                edge_kind: r.edge_kind.to_string(),
                arg_count: None,
                import_path: None,
                target_type_kind: None,
                target_type_qn: None,
                callee_start_rel: None,
                callee_end_rel: None,
            })
            .collect();
        return ExtractedFile {
            symbols: sql_file.symbols,
            import_edges: Vec::new(),
            call_sites,
            symbol_count,
            chunks,
            type_relations: Vec::new(),
            effects: Vec::new(),
        };
    }

    // Markdown: same standalone-module shape as the SQL branch above, not
    // a tree-sitter grammar — dedicated fence-aware heading scan (see
    // `indexer::parser::extract_markdown_symbols`'s doc comment for why it
    // isn't routed through the shared `extract_symbols_shallow` instead).
    if lang == "markdown" {
        let symbols = crate::indexer::parser::extract_markdown_symbols(source, rel);
        let symbol_count = symbols.len();
        let chunks = chunk_pending(source, &symbols);
        return ExtractedFile {
            symbols,
            import_edges: Vec::new(),
            call_sites: Vec::new(),
            symbol_count,
            chunks,
            type_relations: Vec::new(),
            effects: Vec::new(),
        };
    }

    let Some(tree) = parse_tree(source, lang) else {
        // Tier-0.5: no tree-sitter grammar for this language — extract symbols
        // via lightweight line-scan (no calls, no imports, no resolver tiers).
        let symbols = extract_symbols_shallow(source, lang, rel);
        let symbol_count = symbols.len();
        let chunks = chunk_pending(source, &symbols);
        return ExtractedFile {
            symbols,
            import_edges: Vec::new(),
            call_sites: Vec::new(),
            symbol_count,
            chunks,
            type_relations: Vec::new(),
            effects: Vec::new(),
        };
    };

    let mut syms = extract_symbols_from_tree(&tree, source, lang, rel);
    let mut seen: HashSet<String> = HashSet::new();
    for s in &mut syms {
        s.path = rel.to_string();
        // Methods are qualified by their class so two classes' `run` don't collide.
        s.qualified_name = match &s.class_context {
            Some(cls) => format!("{}::{}::{}", rel, cls, s.name),
            None => format!("{}::{}", rel, s.name),
        };
        if !seen.insert(s.qualified_name.clone()) {
            // More than 2 symbols can share (name, line_start) -- e.g. a C
            // function-pointer typedef mentioning the same forward-declared
            // struct type as two different parameters on one line (`struct
            // redisObject *fromkey, struct redisObject *tokey`), which this
            // extractor (over-eagerly, but not fixed here) treats as two
            // `redisObject` symbol occurrences at the identical line. A
            // single `#{line}` suffix collides right back in that case, and
            // an unhandled INSERT there previously hard-crashed the entire
            // `calm index` run (found indexing a real ~4700-line C header,
            // not a synthetic fixture). Loop until genuinely unique instead.
            let base = format!("{}#{}", s.qualified_name, s.line_start);
            let mut candidate = base.clone();
            let mut suffix = 2;
            while !seen.insert(candidate.clone()) {
                candidate = format!("{}#{}", base, suffix);
                suffix += 1;
            }
            s.qualified_name = candidate;
        }
        // Defense-in-depth: same kind-gate as `walk_symbols`'s
        // `detect_entry_point` call (`parser.rs`) — a struct/enum/const can
        // never be a genuine entry point no matter what a user-configured
        // `entry_points` pattern matches against. This branch is inert on
        // CALM's own repo today (`Config::default().entry_points` is empty
        // and no config.json overrides it), but stays correct the moment a
        // project configures a non-empty pattern list.
        if !s.is_entry_point
            && matches!(
                s.kind,
                crate::types::SymbolKind::Function | crate::types::SymbolKind::Method
            )
            // Exact match against either the bare NAME (for simple
            // conventions like "main"/"serve") or the full `qualified_name`
            // (the user-facing escape hatch for pinning one exact symbol,
            // e.g. `"a.py::custom_entry"` — see
            // test_entry_points_config_escape_hatch) — never `.contains()`
            // substring match: that would let a bare-name pattern like
            // "cli" hit every symbol under any `*cli*` path (e.g.
            // `crates/calm-cli/`), or "run" hit every symbol in any
            // `runner.rs`/`*_runner.rs` file, regardless of the symbol's own
            // name.
            && entry_point_patterns
                .iter()
                .any(|p| p == &s.name || p == &s.qualified_name)
        {
            s.is_entry_point = true;
        }
    }

    // (bare name, line_start) → qualified_name, for attributing call sites.
    let qn_by_loc: HashMap<(String, usize), String> = syms
        .iter()
        .map(|s| ((s.name.clone(), s.line_start), s.qualified_name.clone()))
        .collect();
    // PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
    // Part E): qualified_name → start_byte, so each call site below can
    // compute its identity RELATIVE to its own enclosing symbol's start
    // (position-independent v3) instead of absolute-within-file (v2).
    // Only real symbols land here -- the MODULE_ENCLOSING synthetic qn a
    // top-level call gets (see the loop below) is never a key in `syms`,
    // so those calls correctly fall back to v2 (no enclosing symbol to be
    // relative to).
    let qn_to_start_byte: HashMap<String, usize> = syms
        .iter()
        .map(|s| (s.qualified_name.clone(), s.start_byte))
        .collect();
    let file_symbols: HashSet<String> = syms.iter().map(|s| s.name.clone()).collect();
    let symbol_count = syms.len();

    // Imports → import_edges (to_path resolved later, globally) + import_map.
    let imports = crate::indexer::imports::extract_imports_from_tree(&tree, source, lang);
    let mut import_map: HashMap<String, String> = HashMap::new();
    let mut import_edges = Vec::with_capacity(imports.len());
    for imp in &imports {
        let symbols_used =
            serde_json::to_string(&imp.imported_names).unwrap_or_else(|_| "[]".to_string());
        import_edges.push(crate::indexer::edges::ImportEdge {
            from_path: rel.to_string(),
            to_path: None, // resolved later, globally — see resolve_import_targets
            module_name: imp.module_name.clone(),
            symbols_used,
        });
        for n in &imp.imported_names {
            import_map
                .entry(n.clone())
                .or_insert_with(|| imp.module_name.clone());
        }
    }

    // Full resolver context: file symbols + imports + type annotations.
    let ctx = crate::resolver::FileContext {
        file_symbols,
        import_map,
        type_map: extract_type_map_from_tree(&tree, source, lang),
    };
    let resolver = crate::resolver::conservative::ConservativeResolver::new();
    let aliases = extract_file_aliases_from_tree(&tree, source, lang, &ctx);

    // Tier-3: formal scope resolution via StackGraph rules.
    // For languages with stack-graphs support (currently Python), build the set of
    // reference symbol names that StackGraph confirms have a definition in scope
    // within this file. Used below to upgrade "textual"/"inferred" call sites to
    // "formal" — a higher-confidence tier than heuristic type inference.
    // Falls back to empty on unsupported languages or parse errors (non-fatal).
    let formally_resolved: HashSet<String> = if formal.has_language(lang) {
        formally_resolved_names(formal.resolve_file(lang, rel, source), lang, rel)
    } else {
        HashSet::new()
    };

    // Calls → call_sites. Tier-1 (conservative resolver): file symbol / import /
    // alias → "resolved", else "textual". Tier-2: a still-textual *method* call
    // whose receiver type is inferable (self/this → enclosing class, or a typed
    // variable) becomes "inferred" with a target_class for the rebuild to match.
    // Tier-3: formal StackGraph resolution upgrades "textual"/"inferred" to "formal".
    let calls = extract_calls_from_tree(&tree, source, lang);
    let mut call_sites = Vec::with_capacity(calls.len());
    for c in &calls {
        // UPGRADE_PLAN.md FIX1: a call whose enclosing_name/enclosing_line
        // doesn't map to any indexed symbol is normally dropped (top-level
        // calls have no caller symbol -- see extract_calls' docstring) --
        // EXCEPT when enclosing_name is the MODULE_ENCLOSING sentinel
        // (parser.rs), in which case synthesize a per-file pseudo
        // qualified_name directly rather than doing a qn_by_loc lookup that
        // could never hit (line 0 is never a real symbol's line_start).
        // This string is used ONLY as a call_edges.from_symbol value below --
        // never inserted into `symbols`, so symbol counts/search/hotspots/
        // hub/coreness are all unaffected.
        let enc_qn = qn_by_loc
            .get(&(c.enclosing_name.clone(), c.enclosing_line))
            .cloned()
            .or_else(|| {
                (c.enclosing_name == MODULE_ENCLOSING).then(|| format!("{rel}::{MODULE_ENCLOSING}"))
            });
        if let Some(enc_qn) = enc_qn {
            // PR#9 (docs/plans/2026-08-19-evidence-architecture-execution-
            // plan.md Part E): position-independent identity. `Some` only
            // when `enc_qn` is a REAL symbol (`qn_to_start_byte` has it) --
            // the `MODULE_ENCLOSING` synthetic qn a top-level call gets
            // (this loop's own comment above) is never a key there, so
            // those calls correctly stay at v2 (no enclosing symbol to be
            // relative to). The `>=` guard is defensive, not expected to
            // ever fail for a genuinely-enclosed call (tree-sitter
            // guarantees a node's byte range is nested inside its parent's),
            // but fails open to v2 rather than panicking or wrapping on
            // unsigned underflow if some future language's `enclosing_line`
            // attribution ever turns out to be looser than that.
            let (callee_start_rel, callee_end_rel, identity_version): (
                Option<i64>,
                Option<i64>,
                i64,
            ) = match qn_to_start_byte.get(&enc_qn) {
                Some(&enclosing_start) if c.callee_start_byte >= enclosing_start => (
                    Some((c.callee_start_byte - enclosing_start) as i64),
                    Some((c.callee_end_byte - enclosing_start) as i64),
                    3,
                ),
                _ => (None, None, 2),
            };
            let mut confidence;
            let mut target_class: Option<String> = None;
            let mut module_hint = c.module_hint.clone();
            // WS2: set only by the tier-1 `else` branch below (a real
            // `ctx.import_map` hit) — every other branch (type-path receiver,
            // tier-2, C# static-access, whole-module require/import) leaves
            // this `None`, same "narrowing filter only, never a guess" posture
            // as `module_hint`.
            let mut import_path: Option<String> = None;

            if c.receiver_is_type_path
                && let Some(receiver) = &c.receiver
            {
                // `Type::method()` names its type directly and unambiguously
                // in the source text — this takes priority over tier-1
                // *before* it even runs, not just when tier-1 comes up
                // textual. Tier-1's `file_symbols`/`import_map` check
                // matches on the bare callee name alone (`"new"`), with no
                // idea a receiver type was named at all, so it would happily
                // "resolve" e.g. `Vec::new()` against this file's own
                // unrelated `SomeStruct::new` just because both are named
                // "new" — same file, same bare name, wrong symbol entirely.
                // Skipping tier-1 here (rather than only overriding it when
                // textual) is what actually closes that gap. And if nothing
                // in the codebase has this exact type, "unresolved" is the
                // correct answer — no fallback to the unscoped global
                // by_name match, which is the fan-out bug this prevents.
                //
                // 2026-08-03 fix: Rust's `Self::method()` shorthand hit this
                // exact "nothing in the codebase has this exact type" case
                // every single time — `receiver` is the literal keyword text
                // "Self", which is never itself a registered symbol/class
                // name, so `by_name_class` in `resolve_sites_to_edges` could
                // never match it and the call site silently resolved to ZERO
                // edges (not even `ambiguous`). Verified real via the
                // CALM-vs-CodeGraph benchmark (fd's `replace_separator`,
                // called 5x in-file via `Self::replace_separator(...)`, and
                // this exact codebase's `ConservativeResolver::default`'s
                // `Self::new()` call, both 0 callers before this fix).
                // `resolve_tier2` below already substitutes the enclosing
                // class for lowercase `self`/`this` — this mirrors that same
                // substitution for Rust's capitalized `Self`, using
                // `c.enclosing_class` (already correctly extracted via Rust's
                // `class_name_field: "type"` on `impl_item`, see
                // `lang_constants.rs`) instead of the literal keyword text.
                // Falls back to the literal "Self" (the prior behavior) only
                // when there's no enclosing class at all — not valid Rust for
                // a real `Self::` call, so this can't regress a working case.
                let effective_receiver = if lang == "rust" && receiver == "Self" {
                    c.enclosing_class
                        .clone()
                        .unwrap_or_else(|| receiver.clone())
                } else {
                    receiver.clone()
                };
                confidence = EdgeConfidence::Inferred;
                target_class = Some(effective_receiver);
            } else {
                let tier1 = resolver.resolve_tier1(&c.callee, &ctx, &aliases);
                confidence = tier1.confidence;
                import_path = tier1.resolved_path;
                if confidence == EdgeConfidence::Textual
                    && let Some(receiver) = &c.receiver
                    && let Some(cls) =
                        resolver.resolve_tier2(receiver, &ctx, c.enclosing_class.as_deref())
                {
                    confidence = EdgeConfidence::Inferred;
                    target_class = Some(cls);
                } else if confidence == EdgeConfidence::Textual
                    && lang == "csharp"
                    && let Some(receiver) = &c.receiver
                    && crate::indexer::parser::is_type_like(receiver)
                {
                    // C# has no separate static-access operator (`.` covers
                    // both `helper.Greet()` and `Helper.Greet()`), so
                    // `receiver_is_type_path` — set only on the `::` branch
                    // of `split_receiver_callee`, shared by every language —
                    // never fires here, and tier-2 just tried `receiver` as
                    // a *variable* name (it isn't one) and missed. Without
                    // this, `Helper.Greet()` fell through to `rebuild_graph`'s
                    // unscoped `by_name` fan-out on the bare method name
                    // alone — silently wrong (or `Ambiguous`) the moment two
                    // same-named methods exist anywhere in the C# codebase.
                    // Scoped to csharp only (a `lang` string check, not a
                    // change to the shared `is_type_like`/
                    // `split_receiver_callee` used by every other language)
                    // to keep this a zero-blast-radius fix elsewhere — see
                    // the 8-language plan's P1.5 for the equivalent Java gap
                    // this does NOT fix (out of scope here).
                    // `rebuild_graph`'s same-namespace narrowing (8-language
                    // plan P1.5's "using -> namespace" remainder) may still
                    // upgrade this to `Resolved` once it can confirm
                    // `receiver` is declared in one of this file's active
                    // `using` namespaces — that needs the whole-project
                    // `NamespaceMap`, unavailable here (extract_file_data
                    // runs per-file, in parallel, before it's built).
                    confidence = EdgeConfidence::Inferred;
                    target_class = Some(receiver.clone());
                } else if confidence == EdgeConfidence::Textual
                    && module_hint.is_none()
                    && let Some(receiver) = &c.receiver
                    && let Some(module_path) = ctx.import_map.get(receiver)
                    && let Some(seg) = crate::indexer::parser::module_path_last_segment(module_path)
                {
                    // Whole-module require/import binding (`var utils =
                    // require('../lib/utils')`, Python `import os`): tier-1
                    // above only checks `ctx.import_map` keyed by the
                    // CALLEE name (`setCharset`), and tier-2 only checks
                    // `ctx.type_map` (a real type annotation) or self/this —
                    // neither ever asks "is the RECEIVER itself a name this
                    // file imported as a whole module?", so a call like
                    // `utils.setCharset(...)` fell all the way through to
                    // Textual with no receiver-derived signal at all, unlike
                    // the destructured-import sibling call `setCharset(...)`
                    // (bare, resolves fine via tier-1's own import_map
                    // check). Reuses the exact same `module_hint` file-stem
                    // filter `resolve_sites_to_edges` already applies for
                    // Rust's `crate::module::function()` (module_hint_of
                    // above) -- a pure NARROWING filter over the
                    // already-name-matched `ctx.by_name` candidate list,
                    // fail-open when the hint matches nothing (identical to
                    // today's behavior), never a source of new candidates on
                    // its own. Found live via B7 (benchmarks/
                    // b7_task_correctness/README.md): express's
                    // `test/utils.js` calls `utils.setCharset(...)` after
                    // `var utils = require('../lib/utils')` -- completely
                    // absent from `callers()`/`blast_radius` before this fix.
                    confidence = EdgeConfidence::Inferred;
                    module_hint = Some(seg);
                }
            }
            // PR#8 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md
            // Part E): qualify target_class instead of leaving it a bare
            // string -- root cause of the P0-shaped bug this fixes: two
            // classes named e.g. "User" in different files/packages both
            // set target_class: Some("User"), and reconcile.rs's
            // by_name_class lookup keys purely on that bare name, unable to
            // tell them apart. Computed uniformly for target_class from ANY
            // of the three branches above (type-path receiver, tier-2,
            // C# static-access) -- the disambiguation signal doesn't depend
            // on which branch produced the bare name.
            let (target_type_kind, target_type_qn): (Option<String>, Option<String>) =
                match target_class.as_deref() {
                    None => (None, None),
                    Some(cls) => {
                        if c.enclosing_class.as_deref() == Some(cls) {
                            // self/this (tier-2 substitutes enclosing_class
                            // for these) or Rust's Self:: (receiver_is_type_path
                            // branch above, same substitution) -- the
                            // receiver's type IS the enclosing class by
                            // construction, guaranteed declared in THIS
                            // file. Stronger, cheaper signal than an
                            // import-table lookup: this file's own path IS
                            // the qualification, matched EXACTLY (not a
                            // heuristic) against a by_name_class candidate's
                            // own `path` at reconcile time.
                            (
                                Some("resolved_same_file".to_string()),
                                Some(rel.to_string()),
                            )
                        } else if let Some(module_path) = ctx.import_map.get(cls) {
                            // The bare receiver-type name was itself imported
                            // from a specific module in THIS file. Stores the
                            // RAW import text unmodified (e.g. Java `import
                            // com.foo.User;` -> "com.foo.User",
                            // imported_names bound "User" -> module_name
                            // "com.foo.User" INCLUDES the class name itself
                            // as its trailing segment -- see
                            // parse_java_import) -- deliberately NOT reduced
                            // via `module_path_last_segment` the way
                            // import_path/module_hint are on the callee-name
                            // axis: that reduction throws away exactly the
                            // package-qualifying prefix this field exists to
                            // preserve (module_path_last_segment("com.foo.
                            // User") == "User", i.e. right back to the bare
                            // class name PR#8 is trying to disambiguate --
                            // caught live while designing the slice-3
                            // matching logic, before it shipped, by tracing
                            // parse_java_import's actual module_name shape).
                            // resolve_sites_to_edges (slice 3) resolves this
                            // raw text against a by_name_class candidate's
                            // `path` with a best-effort dots-to-slashes
                            // substring heuristic, fail-open exactly like
                            // every other narrowing filter in that function.
                            (
                                Some("resolved_import".to_string()),
                                Some(module_path.clone()),
                            )
                        } else {
                            // Not self/this/Self::, not found in this file's
                            // own import table. Could be a local (same-file)
                            // class not going through self/this, a
                            // same-package sibling never explicitly imported
                            // (Java/Go implicit-package visibility), or a
                            // genuinely external/stdlib type -- none
                            // distinguishable here (extract_file_data runs
                            // per-file, in parallel, with no whole-project
                            // view). 'external' is deliberately NOT emitted
                            // yet -- would need per-language external-vs-
                            // project import syntax detection (e.g.
                            // Python's `from os import X` vs `from .models
                            // import X`, TS's bare package specifier vs
                            // relative path); real, scoped out of this
                            // slice. 'unresolved' is the honest answer, and
                            // reconcile.rs's matching logic falls through to
                            // today's unscoped by_name_class bare-name
                            // behavior unchanged whenever it sees this --
                            // never a regression from pre-PR#8 behavior.
                            (Some("unresolved".to_string()), None)
                        }
                    }
                };
            let callee = aliases.get(&c.callee).unwrap_or(&c.callee).clone();
            // Tier-3: StackGraph confirmed this callee has a definition in scope.
            // Upgrades "textual" and "inferred" but not "resolved" (already correct).
            if confidence != EdgeConfidence::Resolved && formally_resolved.contains(callee.as_str())
            {
                confidence = EdgeConfidence::Formal;
            }
            call_sites.push(CallSiteData {
                enclosing_qn: enc_qn.clone(),
                callee,
                line: c.line as i64,
                callee_start_byte: Some(c.callee_start_byte as i64),
                callee_end_byte: Some(c.callee_end_byte as i64),
                callee_start_rel,
                callee_end_rel,
                identity_version,
                confidence: confidence.as_str().to_string(),
                receiver: c.receiver.clone(),
                target_class,
                looks_option_or_result_chained: c.looks_option_or_result_chained,
                module_hint,
                edge_kind: "call".to_string(),
                arg_count: c.arg_count,
                import_path,
                target_type_kind,
                target_type_qn,
            });
        }
    }

    // Tier 1 semantic facts (2026-08-07 roadmap T1): extends/implements +
    // explicit-throw/write-field, extracted via a second lightweight walk
    // over the SAME already-parsed `tree` (see `indexer::semantic_facts`'s
    // module doc comment for exactly what's captured per language and why).
    // `class_qn_by_name`: bare name -> qualified_name for every class-like
    // symbol this file defines -- used for (a) Rust's `impl Trait for Type`
    // relation, whose `from_symbol` can't use the (name, line) exact match
    // every other language gets (an impl block never gets its own `symbols`
    // row -- see semantic_facts.rs's module doc comment), and (b) same-file
    // `to_symbol` resolution for every language's relation target text.
    let class_qn_by_name: HashMap<String, String> = syms
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                crate::types::SymbolKind::Class
                    | crate::types::SymbolKind::Struct
                    | crate::types::SymbolKind::Trait
                    | crate::types::SymbolKind::Interface
                    | crate::types::SymbolKind::Enum
            )
        })
        .map(|s| (s.name.clone(), s.qualified_name.clone()))
        .collect();

    let raw_relations =
        crate::indexer::semantic_facts::extract_type_relations_from_tree(&tree, source, lang);
    let mut type_relations = Vec::with_capacity(raw_relations.len());
    for rt in raw_relations {
        // Exact (bare_name, def_line) match first (works for Java/TS/JS/
        // Python, whose class node IS what's being walked); same-file
        // by-name fallback second (Rust's impl-block case, and a safety net
        // for anything else). Dropped -- never guessed -- if neither
        // resolves, e.g. an `impl Trait for` a type declared in another
        // file/crate (rare in idiomatic Rust, out of scope for a
        // same-file-only resolver -- see db::schema's table comment).
        let Some(from_symbol) = qn_by_loc
            .get(&(rt.class_name.clone(), rt.class_line))
            .cloned()
            .or_else(|| class_qn_by_name.get(&rt.class_name).cloned())
        else {
            continue;
        };
        // Same-file-only resolution in v1 (no cross-file global pass yet).
        // `target_text` can be a dotted/qualified name ("module.Base"),
        // which a bare-name lookup naturally won't match -- falls to
        // `textual`, not wrong.
        let to_symbol = class_qn_by_name.get(&rt.target_text).cloned();
        // PR A (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md,
        // P4.1): resolution_source labels this row as owned by extraction
        // ("same_file_ast") so graph::type_resolve's later cross-file pass
        // never resets/downgrades it -- only rows IT resolved get that
        // treatment. None (unresolved here) is left for graph::type_resolve
        // to attempt, not a claim this file's extraction failed permanently.
        let (confidence, resolution_source) = if to_symbol.is_some() {
            ("resolved", Some("same_file_ast"))
        } else {
            ("textual", None)
        };
        type_relations.push(crate::indexer::edges::TypeRelationData {
            from_symbol,
            relation_kind: rt.relation_kind,
            target_text: rt.target_text,
            to_symbol,
            confidence,
            resolution_source,
            source_path: rel.to_string(),
            line: rt.class_line as i64,
        });
    }

    let raw_effects =
        crate::indexer::semantic_facts::extract_effects_from_tree(&tree, source, lang);
    let mut effects = Vec::with_capacity(raw_effects.len());
    for re in raw_effects {
        // Same exact (bare_name, def_line) resolution call_sites' own
        // `enc_qn` already uses above -- the enclosing-function tracking in
        // `semantic_facts::walk_effects` mirrors `walk_calls` exactly, so
        // this is guaranteed to hit the same qualified_name a call site
        // inside the same function would resolve to.
        let Some(symbol_qn) = qn_by_loc
            .get(&(re.enclosing_name.clone(), re.enclosing_line))
            .cloned()
        else {
            continue;
        };
        effects.push(crate::indexer::edges::SymbolEffectData {
            symbol_qn,
            effect_kind: re.effect_kind,
            target_text: re.target_text,
            target_confidence: re.target_confidence,
            source_path: rel.to_string(),
            line: re.line as i64,
        });
    }

    let chunks = chunk_pending(source, &syms);

    ExtractedFile {
        symbols: syms,
        import_edges,
        call_sites,
        symbol_count,
        chunks,
        type_relations,
        effects,
    }
}

/// Chunk `source` for Layer-2 semantic search — only when the `embeddings`
/// feature is compiled in, since chunks are otherwise never embedded or
/// queried. `embedding::ENABLED` is a `const bool`, so the disabled branch is
/// eliminated at compile time rather than costing a runtime check.
fn chunk_pending(source: &str, symbols: &[ParsedSymbol]) -> Vec<CodeChunk> {
    if crate::embedding::ENABLED {
        chunk_file(source, symbols)
    } else {
        Vec::new()
    }
}

/// Persist one file's already-extracted symbols, imports, call sites, and
/// Layer-2 code chunks. Pure DB I/O — call sequentially against a single
/// transaction, after all files have been extracted (possibly in parallel).
pub(super) fn persist_file(
    tx: &rusqlite::Transaction,
    rel: &str,
    file_hash: &str,
    extracted: &ExtractedFile,
) -> rusqlite::Result<()> {
    insert_symbols_batch(tx, &extracted.symbols)?;
    insert_import_edges_batch(tx, &extracted.import_edges)?;
    // `OR IGNORE`, not a plain INSERT: `idx_call_sites_current_identity` (schema.rs
    // migrate_call_site_identity_v2) enforces one row per (from_path, enclosing_qn,
    // callee_start_byte, callee_end_byte, edge_kind, identity_version) -- a real,
    // reproducible tree-sitter extractor duplicate for that exact tuple (observed
    // live indexing pallets/flask: a UNIQUE-constraint error here aborted the whole
    // transaction, failing the ENTIRE file's index, not just this one row) must not
    // be allowed to take down indexing for every other file in the batch. Every
    // sibling INSERT into an identity-constrained table (`call_edges`, `import_edges`
    // -- see indexer/edges.rs) already uses this exact `OR IGNORE` idiom; this one
    // was the one place still using a bare INSERT against a constraint added after
    // it was written. Skips are counted and surfaced via `tracing::debug!` below --
    // fail-soft, not silently swallowed.
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO call_sites (from_path, enclosing_qn, callee_name, call_line, callee_start_byte, callee_end_byte, identity_version, confidence, receiver, target_class, looks_option_or_result_chained, module_hint, edge_kind, arg_count, import_path, target_type_kind, target_type_qn, callee_start_rel, callee_end_rel) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
    )?;
    let mut skipped = 0u32;
    for c in &extracted.call_sites {
        let inserted = stmt.execute(rusqlite::params![
            rel,
            c.enclosing_qn,
            c.callee,
            c.line,
            c.callee_start_byte,
            c.callee_end_byte,
            c.identity_version,
            c.confidence,
            c.receiver,
            c.target_class,
            c.looks_option_or_result_chained as i64,
            c.module_hint,
            c.edge_kind,
            c.arg_count,
            c.import_path,
            c.target_type_kind,
            c.target_type_qn,
            c.callee_start_rel,
            c.callee_end_rel,
        ])?;
        if inserted == 0 {
            skipped += 1;
        }
    }
    if skipped > 0 {
        tracing::debug!(
            "persist_file({rel}): {skipped} duplicate call_sites row(s) (identical from_path/enclosing_qn/callee_start_byte/callee_end_byte/edge_kind/identity_version) skipped by OR IGNORE"
        );
    }
    insert_code_chunks_batch(tx, rel, file_hash, &extracted.chunks)?;
    crate::indexer::edges::insert_type_relations_batch(tx, &extracted.type_relations)?;
    crate::indexer::edges::insert_symbol_effects_batch(tx, &extracted.effects)?;
    Ok(())
}
