//! Tier 2 semantic fact (2026-08-07 roadmap,
//! docs/plans/2026-08-07-pecorino-adoption-roadmap.md T2): Architecture
//! Digest — a deterministic, factual (NEVER LLM-generated) per-symbol
//! summary of its confirmed callees, type relations (T1), effects (T1),
//! and whether it participates in a call cycle.
//!
//! # Scope decisions (deliberate, not oversights — see the roadmap's own
//! critique of this exact design before committing to it)
//!
//! **Full recompute every rebuild, no incremental invalidation.** This
//! mirrors `graph::coreness`/`graph::churn`/`graph::hub` EXACTLY — all
//! three are already "pure functions of current DB state" recomputed
//! unconditionally on every `rebuild_graph`/`incremental_graph_update`
//! pass (see `indexer::pipeline`'s own "Step 7 ... identical N global
//! metric passes" comment). The original roadmap sketch proposed a
//! content-hash-based selective-invalidation scheme (`input_digest`,
//! propagated child-digest hashes, generation fencing on COMMIT) — that
//! adds a whole new class of staleness/propagation-order bugs for a
//! dataset this cheap to just recompute wholesale (O(symbols + confirmed
//! edges), same order coreness already pays every rebuild). Revisit only
//! if this measurably shows up in reindex latency at a much larger scale
//! than this repo's own ~5400 symbols / ~13000 edges.
//!
//! **No recursive child-digest embedding, so no topological/SCC-ordered
//! computation is needed either.** A digest only ever names a callee's
//! bare NAME + short role tags (its own `name_tokens`, already computed
//! by the indexer for every symbol) — never that callee's own digest
//! text. This is the roadmap's own explicit anti-vocabulary-bleed rule
//! ("propagate only callee name + short role tags, not raw child digest").
//! One consequence: every symbol's digest is computable independently of
//! every other symbol's digest, in any order — so Tarjan SCC here is used
//! ONLY to answer one boolean per symbol ("do you participate in a call
//! cycle"), never to produce a computation order.
//!
//! **No generation-fencing staleness check.** Because every rebuild
//! recomputes every digest unconditionally (DELETE all + re-INSERT all,
//! same posture `coreness`'s `UPDATE symbols SET coreness = 0` reset
//! already takes), a PRESENT row is always current as of the last
//! successful rebuild — there is no code path that could leave a stale
//! row behind a fresher graph. `graph_generation` is stored purely as an
//! observability breadcrumb, never compared for correctness. The only
//! real state `understand`'s `architecture_digest` field distinguishes is
//! "row exists" vs "no row" (symbol not digestable, or digest computation
//! hasn't run yet on a freshly migrated DB) — see `db::schema`'s
//! `symbol_digests` table comment.
//!
//! **Confidence-filtered, unlike pecorino's HCGS (which treats every
//! edge/every cycle-breaking choice as equally trustworthy).** "Calls:"
//! only lists `Formal`/`Resolved` callees; a separate, clearly-labeled
//! "Possibly calls:" line lists `Inferred` callees. `Textual`/`Ambiguous`/
//! `Unresolved` edges are excluded from the digest entirely (too noisy to
//! be worth surfacing as a fact, even a hedged one).

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::Serialize;

use crate::types::EdgeConfidence;

/// Max confirmed / possible callees rendered before truncating (keeps a
/// hub symbol's digest bounded — see the roadmap's own "vocabulary bleed"
/// concern). `facts_json` still carries `truncated: true` when more exist,
/// so nothing is silently dropped without a trace.
const MAX_CALLEES_SHOWN: usize = 8;
const MAX_POSSIBLE_CALLEES_SHOWN: usize = 5;
const MAX_EFFECTS_SHOWN: usize = 8;
const MAX_ROLE_TAGS: usize = 2;

/// Iterative Tarjan's SCC over the CONFIRMED call graph only
/// (`Formal`/`Resolved`, `rank() >= EdgeConfidence::Resolved.rank()`) —
/// deliberately stricter than `coreness`'s "confirmed" bucket (which also
/// includes `Inferred`/`Textual`), since a digest claiming "this function
/// is recursive" is a stronger assertion than a structural degree count.
/// Iterative (explicit work-list, not real recursion) so a pathologically
/// deep call chain can't blow the stack — same caution `parser::parse_tree`
/// already takes against adversarial input via `PARSE_TIMEOUT_MICROS`.
///
/// Returns the set of qualified_names that are in a nontrivial SCC
/// (more than one member) OR have a direct self-loop (`A calls A`). Both
/// mean the same thing for a reader -- you can't understand this symbol
/// without also reading its cycle-mates -- so they're not distinguished
/// in the output.
pub fn compute_recursive_symbols(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let mut recursive: HashSet<String> = HashSet::new();

    // `ruled_out_by_scip = 0`: mirrors `coreness::compute_coreness` — a
    // SCIP-disproven edge must not be able to fabricate a cycle.
    let mut stmt = conn.prepare(
        "SELECT from_symbol, to_symbol, edge_confidence FROM call_edges WHERE ruled_out_by_scip = 0",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (from, to, confidence) = row?;
        let is_confirmed = EdgeConfidence::parse(&confidence)
            .map(|c| c.rank() >= EdgeConfidence::Resolved.rank())
            .unwrap_or(false);
        if !is_confirmed {
            continue;
        }
        if from == to {
            recursive.insert(from);
            continue;
        }
        adj.entry(from).or_default().push(to);
    }

    let start_nodes: Vec<String> = adj.keys().cloned().collect();
    let mut indices: HashMap<String, usize> = HashMap::new();
    let mut lowlink: HashMap<String, usize> = HashMap::new();
    let mut on_stack: HashSet<String> = HashSet::new();
    let mut scc_stack: Vec<String> = Vec::new();
    let mut next_index = 0usize;
    let empty: Vec<String> = Vec::new();

    for start in &start_nodes {
        if indices.contains_key(start) {
            continue;
        }
        indices.insert(start.clone(), next_index);
        lowlink.insert(start.clone(), next_index);
        next_index += 1;
        scc_stack.push(start.clone());
        on_stack.insert(start.clone());
        // (node, next-child-index-to-visit) — an explicit work-list
        // simulating the recursive DFS's call stack.
        let mut call_stack: Vec<(String, usize)> = vec![(start.clone(), 0)];

        while let Some((node, child_idx)) = call_stack.last().cloned() {
            let children = adj.get(&node).unwrap_or(&empty);
            if child_idx < children.len() {
                call_stack.last_mut().unwrap().1 += 1;
                let child = children[child_idx].clone();
                if !indices.contains_key(&child) {
                    indices.insert(child.clone(), next_index);
                    lowlink.insert(child.clone(), next_index);
                    next_index += 1;
                    scc_stack.push(child.clone());
                    on_stack.insert(child.clone());
                    call_stack.push((child, 0));
                } else if on_stack.contains(&child) {
                    let child_index = indices[&child];
                    let node_lowlink = lowlink[&node];
                    if child_index < node_lowlink {
                        lowlink.insert(node.clone(), child_index);
                    }
                }
                // else: already visited and not on the stack -- a cross
                // edge into a finished SCC, no lowlink update needed.
            } else {
                call_stack.pop();
                if let Some((parent, _)) = call_stack.last() {
                    let node_lowlink = lowlink[&node];
                    let parent_lowlink = lowlink[parent];
                    if node_lowlink < parent_lowlink {
                        lowlink.insert(parent.clone(), node_lowlink);
                    }
                }
                if lowlink[&node] == indices[&node] {
                    let mut component: Vec<String> = Vec::new();
                    loop {
                        let w = scc_stack.pop().expect("SCC stack underflow");
                        on_stack.remove(&w);
                        let is_root = w == node;
                        component.push(w);
                        if is_root {
                            break;
                        }
                    }
                    if component.len() > 1 {
                        recursive.extend(component);
                    }
                }
            }
        }
    }

    Ok(recursive)
}

#[derive(Serialize, Clone)]
pub struct CalleeFact {
    pub name: String,
    pub role_tags: Vec<String>,
}

#[derive(Serialize, Clone)]
pub struct TypeRelationFact {
    pub relation_kind: String,
    pub target_text: String,
    // PR B (docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md,
    // Digest Epistemic Integrity): carried through from type_relations so
    // render_digest can distinguish a confirmed base/interface from one
    // extraction/graph::type_resolve never resolved -- never silently
    // upgrade a textual relation into unhedged prose.
    pub to_symbol: Option<String>,
    pub confidence: String,
}

#[derive(Serialize, Clone)]
pub struct EffectFact {
    pub effect_kind: String,
    pub target_text: String,
    // PR B: carried through from symbol_effects (P3a's event/target
    // confidence split) -- dropping these between T1 and T2 was exactly
    // the kind of epistemic-metadata loss this plan's own principle
    // rejects. event_confidence is currently always "exact" (every
    // extraction site fires only on a real syntactic event); target_confidence
    // is "none" for a bare raise or an uncertain reference (raise e, raise
    // factory()) -- see semantic_facts.rs.
    pub event_confidence: String,
    pub target_confidence: String,
    // PR B3: repeated identical (effect_kind, target_text, *_confidence)
    // facts on the same symbol are deduped to ONE EffectFact before the
    // MAX_EFFECTS_SHOWN budget cap, so e.g. 5 identical self.cache = ...
    // writes across a function don't crowd out other distinct facts --
    // occurrences preserves the count instead of just dropping it.
    pub occurrences: usize,
}

/// Everything a digest is rendered from — kept separate from rendering
/// itself so `render_digest` is a pure, independently-unit-testable
/// function with no DB dependency (same split T1's `semantic_facts.rs`
/// extraction-vs-resolution already uses).
#[derive(Serialize, Clone)]
pub struct DigestFacts {
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub complexity: i64,
    pub confirmed_callees: Vec<CalleeFact>,
    pub possible_callees: Vec<CalleeFact>,
    pub type_relations: Vec<TypeRelationFact>,
    pub effects: Vec<EffectFact>,
    pub recursive_component: bool,
    pub truncated: bool,
}

/// Deterministic, factual text rendering — NEVER an LLM call. Every line
/// is a direct restatement of a `DigestFacts` field; nothing here
/// "interprets" or "summarizes" beyond simple compression (capping list
/// length), so an agent reading it is reading compressed ground truth,
/// not a guess.
pub fn render_digest(facts: &DigestFacts) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("{} {}.", facts.kind, facts.name));
    if !facts.signature.is_empty() {
        parts.push(format!("Signature: {}", facts.signature));
    }
    if !facts.confirmed_callees.is_empty() {
        let callees: Vec<String> = facts.confirmed_callees.iter().map(format_callee).collect();
        parts.push(format!("Calls: {}.", callees.join(", ")));
    }
    if !facts.possible_callees.is_empty() {
        let callees: Vec<String> = facts.possible_callees.iter().map(format_callee).collect();
        parts.push(format!("Possibly calls: {}.", callees.join(", ")));
    }
    // PR B5: resolved vs textual type relations get separate lines --
    // "Extends: X." is only ever printed for a CONFIRMED (resolved)
    // relation; an unresolved one is hedged, never upgraded into unhedged
    // prose just because it's the only relation of its kind.
    let extends_resolved: Vec<&str> = facts
        .type_relations
        .iter()
        .filter(|r| r.relation_kind == "extends" && r.confidence == "resolved")
        .map(|r| r.target_text.as_str())
        .collect();
    if !extends_resolved.is_empty() {
        parts.push(format!("Extends: {}.", extends_resolved.join(", ")));
    }
    let extends_textual: Vec<&str> = facts
        .type_relations
        .iter()
        .filter(|r| r.relation_kind == "extends" && r.confidence != "resolved")
        .map(|r| r.target_text.as_str())
        .collect();
    if !extends_textual.is_empty() {
        parts.push(format!(
            "Possibly extends (unresolved): {}.",
            extends_textual.join(", ")
        ));
    }
    let implements_resolved: Vec<&str> = facts
        .type_relations
        .iter()
        .filter(|r| r.relation_kind == "implements" && r.confidence == "resolved")
        .map(|r| r.target_text.as_str())
        .collect();
    if !implements_resolved.is_empty() {
        parts.push(format!("Implements: {}.", implements_resolved.join(", ")));
    }
    let implements_textual: Vec<&str> = facts
        .type_relations
        .iter()
        .filter(|r| r.relation_kind == "implements" && r.confidence != "resolved")
        .map(|r| r.target_text.as_str())
        .collect();
    if !implements_textual.is_empty() {
        parts.push(format!(
            "Possibly implements (unresolved): {}.",
            implements_textual.join(", ")
        ));
    }
    let writes: Vec<String> = facts
        .effects
        .iter()
        .filter(|e| e.effect_kind == "write_field")
        .map(format_effect_target)
        .collect();
    if !writes.is_empty() {
        parts.push(format!("Writes: {}.", writes.join(", ")));
    }
    // PR B2: a throw's target is only ever printed unhedged when
    // target_confidence is "exact" -- `raise e`/`raise factory()` (target
    // text present but not provably an exception TYPE from AST alone) get
    // a separate hedged line, and a bare `raise` (no target text at all)
    // becomes its own contentless fact, never "Throws: ." (an empty name
    // joined into the confident list).
    let throws: Vec<String> = facts
        .effects
        .iter()
        .filter(|e| {
            e.effect_kind == "explicit_throw"
                && e.target_confidence == "exact"
                && !e.target_text.is_empty()
        })
        .map(format_effect_target)
        .collect();
    if !throws.is_empty() {
        parts.push(format!("Throws: {}.", throws.join(", ")));
    }
    let possible_throws: Vec<String> = facts
        .effects
        .iter()
        .filter(|e| {
            e.effect_kind == "explicit_throw"
                && e.target_confidence != "exact"
                && !e.target_text.is_empty()
        })
        .map(format_effect_target)
        .collect();
    if !possible_throws.is_empty() {
        parts.push(format!(
            "Possibly raises (target unresolved): {}.",
            possible_throws.join(", ")
        ));
    }
    if facts
        .effects
        .iter()
        .any(|e| e.effect_kind == "explicit_throw" && e.target_text.is_empty())
    {
        parts.push("Reraises an exception.".to_string());
    }
    if facts.complexity > 1 {
        parts.push(format!("Complexity: {}.", facts.complexity));
    }
    if facts.recursive_component {
        parts.push("Participates in a call cycle.".to_string());
    }
    parts.join(" ")
}

fn format_callee(c: &CalleeFact) -> String {
    if c.role_tags.is_empty() {
        c.name.clone()
    } else {
        format!("{} [{}]", c.name, c.role_tags.join(" "))
    }
}

// PR B3: renders occurrences > 1 as a "(xN)" suffix so a repeated fact (5
// identical writes to the same field) is visible as repeated, not silently
// collapsed into indistinguishable-from-once.
fn format_effect_target(e: &EffectFact) -> String {
    if e.occurrences > 1 {
        format!("{} (x{})", e.target_text, e.occurrences)
    } else {
        e.target_text.clone()
    }
}

/// Bumped whenever `compute_digests`'s rendering/rollup logic changes what a
/// digest contains (facts included, role-tag derivation, truncation), OR
/// whenever another graph-rebuild-time derivation pass whose output feeds
/// digests changes its semantics (PR A / P4.1: `graph::type_resolve` now
/// resets and recomputes cross-file type-relation resolution on every pass
/// instead of only ever upgrading it -- a `to_symbol`/`confidence` an
/// already-indexed install computed under the old logic needs the same
/// graph-only recompute this version bump already exists to trigger).
/// Folded into `InputCatalog::index_input_snapshot`'s `context_material`
/// bucket (`indexer::refresh`) alongside `PACKAGE_GRAPH_VERSION` -- both are
/// fully recomputed by every `rebuild_graph`/`incremental_graph_update`
/// call, so a `Context`-class drift (graph rebuild, no reparse) is
/// sufficient to pick up a bump here, unlike `SOURCE_EXTRACTION_VERSION`
/// which needs a full reparse.
/// See docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md P1.
///
/// A change here is verified by
/// `derived_artifact_versions::graph_derivation_fixture_is_pinned_to_its_version`
/// (crates/calm-core/tests/derived_artifact_versions.rs) -- bump this AND
/// that test's expected hash together, in the same commit, never one alone.
// PR B (Digest Epistemic Integrity, same plan section as PR A above): this
// bump covers compute_digests's own rendering-logic change directly --
// confidence-hedged type-relation/throw lines, dedup-with-occurrences for
// effects, canonical type-relation ordering. The exact case this const's
// own doc comment above was written for.
pub const GRAPH_DERIVATION_VERSION: i64 = 4;

/// Recompute every digestable symbol's `symbol_digests` row from current
/// DB state — see the module doc comment for why this is a full
/// DELETE-then-reinsert, not selective invalidation. Call sites: mirrors
/// `coreness`/`hub`/`churn`'s own placement in `rebuild_graph`/
/// `incremental_graph_update`.
pub fn compute_digests(conn: &Connection) -> rusqlite::Result<()> {
    let started = std::time::Instant::now();
    let recursive = compute_recursive_symbols(conn)?;

    let graph_generation: i64 = conn
        .query_row(
            "SELECT generation FROM graph_generation_state WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Only kinds with meaningful call/effect facts -- a markdown Heading
    // or a bare type alias has nothing to summarize.
    let mut stmt = conn.prepare(
        "SELECT qualified_name, name, kind, signature, cyclomatic_complexity \
         FROM symbols WHERE kind IN ('function', 'method', 'class', 'struct', 'trait', 'interface', 'constructor')",
    )?;
    struct Row {
        qn: String,
        name: String,
        kind: String,
        signature: String,
        complexity: i64,
    }
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok(Row {
                qn: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                signature: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                complexity: r.get::<_, Option<i64>>(4)?.unwrap_or(1),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // name -> name_tokens, for role-tagging CALLEES (a callee's role tags
    // come from ITS OWN name_tokens, not the caller's -- see module doc
    // comment on why this is the whole propagation mechanism, not a
    // recursive digest embed). Bare name (not qualified_name), matching
    // how `call_edges.to_symbol` is keyed.
    let mut name_tokens_by_qn: HashMap<String, String> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT qualified_name, name_tokens FROM symbols")?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        for row in iter {
            let (qn, tokens) = row?;
            name_tokens_by_qn.insert(qn, tokens);
        }
    }

    // from_symbol -> [(to_symbol, confidence)], confirmed(Formal/Resolved)
    // and possible(Inferred) only -- Textual/Ambiguous/Unresolved excluded
    // entirely, per the module doc comment.
    let mut callees_by_from: HashMap<String, Vec<(String, EdgeConfidence)>> = HashMap::new();
    let mut edges_considered = 0usize;
    {
        let mut stmt = conn.prepare(
            "SELECT from_symbol, to_symbol, edge_confidence FROM call_edges \
             WHERE ruled_out_by_scip = 0",
        )?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in iter {
            let (from, to, confidence) = row?;
            let Some(conf) = EdgeConfidence::parse(&confidence) else {
                continue;
            };
            if conf.rank() >= EdgeConfidence::Inferred.rank() {
                edges_considered += 1;
                callees_by_from.entry(from).or_default().push((to, conf));
            }
        }
    }

    let mut type_relations_by_from: HashMap<String, Vec<TypeRelationFact>> = HashMap::new();
    {
        // PR B5: to_symbol/confidence carried through so render_digest can
        // hedge an unresolved relation instead of presenting it as fact.
        let mut stmt = conn.prepare(
            "SELECT from_symbol, relation_kind, target_text, to_symbol, confidence \
             FROM type_relations",
        )?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in iter {
            let (from, kind, target, to_symbol, confidence) = row?;
            type_relations_by_from
                .entry(from)
                .or_default()
                .push(TypeRelationFact {
                    relation_kind: kind,
                    target_text: target,
                    to_symbol,
                    confidence,
                });
        }
    }

    // PR B1/B3: event_confidence/target_confidence carried through (never
    // drop epistemic metadata between T1 and T2); repeated identical
    // (effect_kind, target_text, *_confidence) facts on the same symbol are
    // deduped to ONE EffectFact with an occurrence count, in first-seen
    // (line) order, BEFORE the MAX_EFFECTS_SHOWN truncation below runs --
    // so e.g. 5 identical `self.cache = ...` writes don't crowd the budget.
    // (effect_kind, target_text, event_confidence, target_confidence) ->
    // index into that symbol's effects_by_symbol Vec, for O(1) dedup lookup.
    type EffectDedupKey = (String, String, String, String);
    let mut effects_by_symbol: HashMap<String, Vec<EffectFact>> = HashMap::new();
    let mut effect_index_by_symbol: HashMap<String, HashMap<EffectDedupKey, usize>> =
        HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT symbol_qn, effect_kind, target_text, event_confidence, target_confidence \
             FROM symbol_effects ORDER BY line",
        )?;
        let iter = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        for row in iter {
            let (qn, kind, target, event_confidence, target_confidence) = row?;
            let key = (
                kind.clone(),
                target.clone(),
                event_confidence.clone(),
                target_confidence.clone(),
            );
            let facts = effects_by_symbol.entry(qn.clone()).or_default();
            let index = effect_index_by_symbol.entry(qn).or_default();
            if let Some(&i) = index.get(&key) {
                facts[i].occurrences += 1;
            } else {
                index.insert(key, facts.len());
                facts.push(EffectFact {
                    effect_kind: kind,
                    target_text: target,
                    event_confidence,
                    target_confidence,
                    occurrences: 1,
                });
            }
        }
    }

    conn.execute("DELETE FROM symbol_digests", [])?;
    let mut insert_stmt = conn.prepare(
        "INSERT INTO symbol_digests (symbol_qn, facts_json, rendered_text, recursive_component, graph_generation, truncated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;

    for row in &rows {
        let raw_callees = callees_by_from.get(&row.qn).cloned().unwrap_or_default();
        let mut confirmed: Vec<CalleeFact> = raw_callees
            .iter()
            .filter(|(_, c)| c.rank() >= EdgeConfidence::Resolved.rank())
            .map(|(to, _)| CalleeFact {
                name: to.clone(),
                role_tags: role_tags_for(&name_tokens_by_qn, to),
            })
            .collect();
        confirmed.sort_by(|a, b| a.name.cmp(&b.name));
        confirmed.dedup_by(|a, b| a.name == b.name);
        let confirmed_truncated = confirmed.len() > MAX_CALLEES_SHOWN;
        confirmed.truncate(MAX_CALLEES_SHOWN);

        let mut possible: Vec<CalleeFact> = raw_callees
            .iter()
            .filter(|(_, c)| *c == EdgeConfidence::Inferred)
            .map(|(to, _)| CalleeFact {
                name: to.clone(),
                role_tags: role_tags_for(&name_tokens_by_qn, to),
            })
            .collect();
        possible.sort_by(|a, b| a.name.cmp(&b.name));
        possible.dedup_by(|a, b| a.name == b.name);
        let possible_truncated = possible.len() > MAX_POSSIBLE_CALLEES_SHOWN;
        possible.truncate(MAX_POSSIBLE_CALLEES_SHOWN);

        // PR B4: canonical sort + dedup before render -- output must be
        // stable regardless of DB row-return order (which HashMap grouping
        // + SQLite don't guarantee canonical across rebuilds).
        let mut type_relations = type_relations_by_from
            .get(&row.qn)
            .cloned()
            .unwrap_or_default();
        type_relations.sort_by(|a, b| {
            (a.relation_kind.as_str(), a.target_text.as_str())
                .cmp(&(b.relation_kind.as_str(), b.target_text.as_str()))
        });
        type_relations
            .dedup_by(|a, b| a.relation_kind == b.relation_kind && a.target_text == b.target_text);

        let mut effects = effects_by_symbol.get(&row.qn).cloned().unwrap_or_default();
        let effects_truncated = effects.len() > MAX_EFFECTS_SHOWN;
        effects.truncate(MAX_EFFECTS_SHOWN);

        let truncated = confirmed_truncated || possible_truncated || effects_truncated;

        let facts = DigestFacts {
            name: row.name.clone(),
            kind: row.kind.clone(),
            signature: row.signature.clone(),
            complexity: row.complexity,
            confirmed_callees: confirmed,
            possible_callees: possible,
            type_relations,
            effects,
            recursive_component: recursive.contains(&row.qn),
            truncated,
        };
        let rendered = render_digest(&facts);
        let facts_json = serde_json::to_string(&facts).unwrap_or_else(|_| "{}".to_string());

        insert_stmt.execute(rusqlite::params![
            row.qn,
            facts_json,
            rendered,
            facts.recursive_component as i64,
            graph_generation,
            facts.truncated as i64,
        ])?;
    }

    // Observability for the "full recompute every rebuild" call this module's
    // doc comment makes -- "revisit only if this measurably shows up in
    // reindex latency" needs a number to actually notice that happening.
    tracing::info!(
        symbols = rows.len(),
        edges_considered,
        recursive_symbols = recursive.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "compute_digests: full recompute"
    );

    Ok(())
}

fn role_tags_for(name_tokens_by_qn: &HashMap<String, String>, callee_qn: &str) -> Vec<String> {
    name_tokens_by_qn
        .get(callee_qn)
        .map(|tokens| {
            tokens
                .split_whitespace()
                .take(MAX_ROLE_TAGS)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn insert_edge(conn: &Connection, from: &str, to: &str, confidence: &str) {
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, ruled_out_by_scip) VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![from, to, confidence],
        )
        .unwrap();
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn no_cycle_chain_is_not_recursive() {
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "b", "c", "resolved");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty(), "{recursive:?}");
    }

    #[test]
    fn three_cycle_is_recursive() {
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "b", "c", "resolved");
        insert_edge(&conn, "c", "a", "resolved");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert_eq!(
            recursive,
            HashSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn self_loop_is_recursive() {
        let conn = test_conn();
        insert_edge(&conn, "a", "a", "formal");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert_eq!(recursive, HashSet::from(["a".to_string()]));
    }

    #[test]
    fn diamond_is_not_recursive() {
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "a", "c", "resolved");
        insert_edge(&conn, "b", "d", "resolved");
        insert_edge(&conn, "c", "d", "resolved");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty(), "{recursive:?}");
    }

    #[test]
    fn two_disjoint_cycles_both_detected_independently() {
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "resolved");
        insert_edge(&conn, "b", "a", "resolved");
        insert_edge(&conn, "x", "y", "resolved");
        insert_edge(&conn, "y", "x", "resolved");
        insert_edge(&conn, "a", "x", "resolved"); // cross-edge, must not merge the two SCCs
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert_eq!(
            recursive,
            HashSet::from([
                "a".to_string(),
                "b".to_string(),
                "x".to_string(),
                "y".to_string()
            ])
        );
    }

    #[test]
    fn textual_confidence_edges_do_not_count_toward_cycles() {
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "textual");
        insert_edge(&conn, "b", "a", "textual");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(
            recursive.is_empty(),
            "textual-confidence edges must not be trusted enough to fabricate a cycle: {recursive:?}"
        );
    }

    #[test]
    fn inferred_confidence_alone_does_not_count_toward_cycles() {
        // Digest's recursive-component bar is Resolved+ (stricter than
        // coreness's Inferred+) -- an Inferred-only cycle must not count.
        let conn = test_conn();
        insert_edge(&conn, "a", "b", "inferred");
        insert_edge(&conn, "b", "a", "inferred");
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty(), "{recursive:?}");
    }

    #[test]
    fn ruled_out_by_scip_edge_is_excluded() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, ruled_out_by_scip) VALUES ('a', 'b', 'resolved', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO call_edges (from_symbol, to_symbol, edge_confidence, ruled_out_by_scip) VALUES ('b', 'a', 'resolved', 0)",
            [],
        )
        .unwrap();
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty(), "{recursive:?}");
    }

    #[test]
    fn empty_graph_is_fine() {
        let conn = test_conn();
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty());
    }

    #[test]
    fn long_chain_does_not_blow_the_stack() {
        // Exercises the ITERATIVE nature of the Tarjan implementation --
        // a real recursive DFS at this depth risks a stack overflow.
        let conn = test_conn();
        let n = 20_000;
        for i in 0..n {
            insert_edge(&conn, &format!("n{i}"), &format!("n{}", i + 1), "resolved");
        }
        let recursive = compute_recursive_symbols(&conn).unwrap();
        assert!(recursive.is_empty(), "a pure chain has no cycles");
    }

    #[test]
    fn render_digest_is_factual_and_compact() {
        let facts = DigestFacts {
            name: "refresh_session".to_string(),
            kind: "method".to_string(),
            signature: "fn refresh_session(token, store) -> User".to_string(),
            complexity: 7,
            confirmed_callees: vec![
                CalleeFact {
                    name: "verify_token".to_string(),
                    role_tags: vec!["verify".to_string(), "token".to_string()],
                },
                CalleeFact {
                    name: "load_session".to_string(),
                    role_tags: vec![],
                },
            ],
            possible_callees: vec![CalleeFact {
                name: "emit_event".to_string(),
                role_tags: vec!["emit".to_string()],
            }],
            type_relations: vec![TypeRelationFact {
                relation_kind: "implements".to_string(),
                target_text: "SessionRefresher".to_string(),
                to_symbol: Some("session.py::SessionRefresher".to_string()),
                confidence: "resolved".to_string(),
            }],
            effects: vec![
                EffectFact {
                    effect_kind: "write_field".to_string(),
                    target_text: "last_refresh".to_string(),
                    event_confidence: "exact".to_string(),
                    target_confidence: "exact".to_string(),
                    occurrences: 1,
                },
                EffectFact {
                    effect_kind: "explicit_throw".to_string(),
                    target_text: "ExpiredToken".to_string(),
                    event_confidence: "exact".to_string(),
                    target_confidence: "exact".to_string(),
                    occurrences: 1,
                },
            ],
            recursive_component: false,
            truncated: false,
        };
        let text = render_digest(&facts);
        assert_eq!(
            text,
            "method refresh_session. Signature: fn refresh_session(token, store) -> User \
             Calls: verify_token [verify token], load_session. \
             Possibly calls: emit_event [emit]. \
             Implements: SessionRefresher. Writes: last_refresh. Throws: ExpiredToken. Complexity: 7."
        );
    }

    #[test]
    fn render_digest_minimal_symbol_has_no_empty_sections() {
        let facts = DigestFacts {
            name: "helper".to_string(),
            kind: "function".to_string(),
            signature: String::new(),
            complexity: 1,
            confirmed_callees: vec![],
            possible_callees: vec![],
            type_relations: vec![],
            effects: vec![],
            recursive_component: false,
            truncated: false,
        };
        assert_eq!(render_digest(&facts), "function helper.");
    }

    #[test]
    fn render_digest_shows_recursive_marker() {
        let facts = DigestFacts {
            name: "walk".to_string(),
            kind: "function".to_string(),
            signature: String::new(),
            complexity: 3,
            confirmed_callees: vec![],
            possible_callees: vec![],
            type_relations: vec![],
            effects: vec![],
            recursive_component: true,
            truncated: false,
        };
        assert!(render_digest(&facts).contains("Participates in a call cycle."));
    }

    #[test]
    fn compute_digests_end_to_end_reflects_call_graph_and_t1_facts() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, name_tokens, cyclomatic_complexity) \
             VALUES ('a.py::Foo::m', 'm', 'method', 'python', 'a.py', 1, 2, 'def m(self):', 'm', 5)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, name_tokens, cyclomatic_complexity) \
             VALUES ('a.py::Foo::helper', 'helper', 'method', 'python', 'a.py', 4, 5, 'def helper(self):', 'helper', 1)",
            [],
        ).unwrap();
        insert_edge(&conn, "a.py::Foo::m", "a.py::Foo::helper", "resolved");
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, confidence, source_path, line) \
             VALUES ('a.py::Foo::m', 'implements', 'SomeIface', 'textual', 'a.py', 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO symbol_effects (symbol_qn, effect_kind, target_text, source_path, line) \
             VALUES ('a.py::Foo::m', 'write_field', 'x', 'a.py', 1)",
            [],
        )
        .unwrap();

        compute_digests(&conn).unwrap();

        let rendered: String = conn
            .query_row(
                "SELECT rendered_text FROM symbol_digests WHERE symbol_qn = 'a.py::Foo::m'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(rendered.contains("Calls: a.py::Foo::helper"), "{rendered}");
        assert!(rendered.contains("Writes: x"), "{rendered}");

        let recursive: i64 = conn
            .query_row(
                "SELECT recursive_component FROM symbol_digests WHERE symbol_qn = 'a.py::Foo::m'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recursive, 0);
    }

    #[test]
    fn compute_digests_full_recompute_removes_deleted_symbols() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, name_tokens) \
             VALUES ('a.py::f', 'f', 'function', 'python', 'a.py', 1, 2, 'f')",
            [],
        ).unwrap();
        compute_digests(&conn).unwrap();
        let count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_digests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 1);

        conn.execute("DELETE FROM symbols WHERE qualified_name = 'a.py::f'", [])
            .unwrap();
        compute_digests(&conn).unwrap();
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbol_digests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count_after, 0,
            "a deleted symbol's stale digest row must not survive a full recompute"
        );
    }

    // PR B2: bare `raise` (no target text) and an uncertain reference
    // (`raise e`) must never be presented as confirmed exception facts.
    #[test]
    fn render_digest_hedges_uncertain_and_bare_throws() {
        let facts = DigestFacts {
            name: "handler".to_string(),
            kind: "function".to_string(),
            signature: String::new(),
            complexity: 1,
            confirmed_callees: vec![],
            possible_callees: vec![],
            type_relations: vec![],
            effects: vec![
                EffectFact {
                    effect_kind: "explicit_throw".to_string(),
                    target_text: String::new(),
                    event_confidence: "exact".to_string(),
                    target_confidence: "none".to_string(),
                    occurrences: 1,
                },
                EffectFact {
                    effect_kind: "explicit_throw".to_string(),
                    target_text: "e".to_string(),
                    event_confidence: "exact".to_string(),
                    target_confidence: "none".to_string(),
                    occurrences: 1,
                },
            ],
            recursive_component: false,
            truncated: false,
        };
        let text = render_digest(&facts);
        assert!(
            !text.contains("Throws: ."),
            "bare raise must never render as an empty confident throw: {text}"
        );
        assert!(
            !text.contains("Throws: e."),
            "an uncertain target must never be presented as a confirmed exception type: {text}"
        );
        assert!(text.contains("Reraises an exception."), "{text}");
        assert!(
            text.contains("Possibly raises (target unresolved): e."),
            "{text}"
        );
    }

    // PR B5: an unresolved (textual) type relation must never render as
    // unhedged "Extends:"/"Implements:" prose.
    #[test]
    fn render_digest_hedges_unresolved_type_relations() {
        let facts = DigestFacts {
            name: "Derived".to_string(),
            kind: "class".to_string(),
            signature: String::new(),
            complexity: 1,
            confirmed_callees: vec![],
            possible_callees: vec![],
            type_relations: vec![TypeRelationFact {
                relation_kind: "extends".to_string(),
                target_text: "Base".to_string(),
                to_symbol: None,
                confidence: "textual".to_string(),
            }],
            effects: vec![],
            recursive_component: false,
            truncated: false,
        };
        let text = render_digest(&facts);
        assert!(!text.contains("Extends: Base."), "{text}");
        assert!(
            text.contains("Possibly extends (unresolved): Base."),
            "{text}"
        );
    }

    // PR B3: an EffectFact with occurrences > 1 renders with a count
    // suffix, so a repeated fact stays visibly repeated after dedup.
    #[test]
    fn render_digest_shows_occurrence_count_for_repeated_writes() {
        let facts = DigestFacts {
            name: "save".to_string(),
            kind: "method".to_string(),
            signature: String::new(),
            complexity: 1,
            confirmed_callees: vec![],
            possible_callees: vec![],
            type_relations: vec![],
            effects: vec![EffectFact {
                effect_kind: "write_field".to_string(),
                target_text: "cache".to_string(),
                event_confidence: "exact".to_string(),
                target_confidence: "exact".to_string(),
                occurrences: 5,
            }],
            recursive_component: false,
            truncated: false,
        };
        assert!(render_digest(&facts).contains("Writes: cache (x5)."));
    }

    // PR B3/B4, end-to-end through compute_digests (not just render_digest
    // against a hand-built DigestFacts): 5 identical writes on the same
    // line-varying symbol_effects rows must dedupe to one fact with an
    // occurrence count, and two type_relations rows inserted out of
    // alphabetical order must render sorted -- canonical output must not
    // depend on DB insertion/iteration order.
    #[test]
    fn compute_digests_dedupes_repeated_effects_and_canonicalizes_type_relations() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, name_tokens) \
             VALUES ('a.py::Foo::save', 'save', 'method', 'python', 'a.py', 1, 10, 'save')",
            [],
        )
        .unwrap();
        for line in 1..=5 {
            conn.execute(
                "INSERT INTO symbol_effects (symbol_qn, effect_kind, target_text, source_path, line) \
                 VALUES ('a.py::Foo::save', 'write_field', 'cache', 'a.py', ?1)",
                [line],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, confidence, source_path, line) \
             VALUES ('a.py::Foo::save', 'implements', 'Zeta', 'textual', 'a.py', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, confidence, source_path, line) \
             VALUES ('a.py::Foo::save', 'implements', 'Alpha', 'textual', 'a.py', 2)",
            [],
        )
        .unwrap();

        compute_digests(&conn).unwrap();

        let rendered: String = conn
            .query_row(
                "SELECT rendered_text FROM symbol_digests WHERE symbol_qn = 'a.py::Foo::save'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            rendered.contains("Writes: cache (x5)."),
            "5 identical writes must dedupe to one fact with an occurrence count: {rendered}"
        );
        assert!(
            rendered.contains("Possibly implements (unresolved): Alpha, Zeta."),
            "type relations must render sorted (Alpha before Zeta) regardless of \
             insertion order: {rendered}"
        );
    }
}
