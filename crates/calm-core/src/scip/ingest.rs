//! Interpret parsed SCIP occurrences using exact CallSite provenance. D4 only
//! lets evidence alter the call graph when its byte span and source hash match
//! the current CallSite; legacy line-keyed occurrences remain observation-only.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;

use super::parse::ScipOccurrence;

type ExactCallSiteKey = (String, i64, i64, String);
type DefinitionSite = (String, i64);
/// One joined `call_edges`/`call_sites`/`file_index`/`symbols` row used by
/// both the upgrade pass and `exact_sibling_rule_out_ids`: (edge id,
/// call_site id, exact CallSite key, target def path, target def line,
/// edge_confidence, formal_source, ruled_out_by_scip).
type CallEdgeRow = (
    i64,
    i64,
    ExactCallSiteKey,
    String,
    i64,
    String,
    Option<String>,
    bool,
);

/// Outcome of one `ingest_occurrences` pass.
///
/// Exact SCIP ingestion outcome. Legacy line-keyed SCIP evidence leaves every
/// mutation count at zero.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct IngestStats {
    /// Existing call edges upgraded by exact SCIP evidence.
    pub upgraded: usize,
    /// Ambiguous sibling edges ruled out at an exact matching CallSite.
    pub ruled_out: usize,
    /// Formal edges inserted for an exact matching CallSite.
    pub inserted: usize,
    /// Fraction of exact SCIP call-site references that matched CALM state.
    pub match_rate: f64,
    /// Occurrences whose primary span came from a guessed (fallback, not
    /// self-declared) encoding, where the alternate raw-UTF-8
    /// interpretation ALSO independently resolved to a distinct, real
    /// CallSite — two plausible-but-different targets, so neither was used
    /// to upgrade, rule out, or insert anything. See
    /// `parse::ScipOccurrence::guessed_alt_byte_range`.
    pub ambiguous: usize,
    /// `true` when this ENTIRE pass was discarded because the graph
    /// mutated between when the caller captured `graph_generation`
    /// (`ExternalProofContext::at_graph_generation`) and when this
    /// function's fence checked it against `graph_generation_state` --
    /// every other field is forced to its zero/default value in that case,
    /// since no DB row was touched. Distinguishes "discarded, stale" from
    /// "ran cleanly and genuinely found nothing" (both otherwise look like
    /// an all-zero `IngestStats`). Callers MUST NOT persist a cache key /
    /// "this input has been seen" marker when this is `true` -- doing so
    /// makes the discarded evidence silently unrecoverable, because the
    /// next run would then skip re-invoking the indexer for the same
    /// input entirely (PATTERN-DEBT scip-cache-commits-discarded-generation,
    /// fixed 2026-08-06: `run_overlay_for_with_catalog`/
    /// `run_go_workspace_overlay_with_catalog` both now check this before
    /// calling `state::write_state`).
    pub discarded_stale_generation: bool,
}

/// Provider inputs that make an exact external result auditable.  Callers that
/// cannot supply these inputs may still ingest observations, but cannot create
/// a fresh durable proof record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalProofContext {
    pub provider: String,
    pub provider_fingerprint: String,
    pub context_fingerprint: String,
    /// Graph baseline observed before the external provider began work.  `None`
    /// keeps compatibility-only observation callers non-durable.
    pub graph_generation: Option<i64>,
}

impl ExternalProofContext {
    pub fn new(
        provider: impl Into<String>,
        provider_fingerprint: impl Into<String>,
        context_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            provider_fingerprint: provider_fingerprint.into(),
            context_fingerprint: context_fingerprint.into(),
            graph_generation: None,
        }
    }

    pub fn at_graph_generation(mut self, graph_generation: i64) -> Self {
        self.graph_generation = Some(graph_generation);
        self
    }

    pub fn has_same_provenance_as(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.provider_fingerprint == other.provider_fingerprint
            && self.context_fingerprint == other.context_fingerprint
    }
}

/// D3's historic SCIP-versus-Stack Graphs disagreement counter. Exact D4
/// evidence has separate provenance, so this counter remains scoped to the
/// legacy regression oracle until its semantics are upgraded deliberately.
static SCIP_STACK_GRAPHS_OVERRIDES: AtomicU64 = AtomicU64::new(0);

/// Count of historic line-keyed SCIP-versus-Stack Graphs overrides exercised
/// by the D3 regression oracle.
pub fn scip_stack_graphs_override_count() -> u64 {
    SCIP_STACK_GRAPHS_OVERRIDES.load(Ordering::Relaxed)
}

/// Records one legacy SCIP-vs-stack_graphs disagreement: bumps the process-wide
/// counter and logs the call site + target so it can be traced back to a
/// specific edge without re-querying the DB. `ruled_out` distinguishes the
/// two call sites this fires from: `false` = `ingest_occurrences`'s upgrade
/// loop (a `stack_graphs` edge got reconfirmed to `formal_source = 'scip'`),
/// `true` = `mark_ruled_out_siblings` (a `stack_graphs`-sourced sibling lost
/// to a SCIP-backed one in an ambiguous fan-out group).
#[cfg(test)]
fn record_scip_stack_graphs_override(
    from_path: &str,
    call_line: i64,
    def_path: &str,
    def_line: i64,
    ruled_out: bool,
) {
    SCIP_STACK_GRAPHS_OVERRIDES.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(
        from_path,
        call_line,
        def_path,
        def_line,
        ruled_out,
        "SCIP overrode a stack_graphs formal verdict"
    );
}

/// One existing `call_edges` row with a known call site, joined to its
/// target's declaration site.
#[cfg(test)]
struct EdgeRow {
    id: i64,
    from_path: String,
    call_line: i64,
    def_path: String,
    def_line: i64,
    confidence: String,
    ruled_out: bool,
    formal_source: Option<String>,
}

/// Use SCIP only when its source range and source hash identify one current
/// CallSite byte span exactly. Legacy line-only occurrences remain inert.
pub fn ingest_occurrences(
    conn: &Connection,
    occ: &[ScipOccurrence],
    insert_missing: bool,
) -> rusqlite::Result<IngestStats> {
    ingest_occurrences_with_proof_context(conn, occ, insert_missing, None)
}

/// Exact SCIP ingestion with the provider/context fingerprints required to
/// persist a D4 proof. `None` deliberately leaves the result unverified for
/// compatibility with observation-only callers and narrowly scoped unit tests.
pub fn ingest_occurrences_with_proof_context(
    conn: &Connection,
    occ: &[ScipOccurrence],
    insert_missing: bool,
    proof_context: Option<&ExternalProofContext>,
) -> rusqlite::Result<IngestStats> {
    if let Some(expected_generation) = proof_context.and_then(|context| context.graph_generation) {
        let current_generation: i64 = conn.query_row(
            "SELECT generation FROM graph_generation_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current_generation != expected_generation {
            // Tagged (not a bare `default()`) so callers can tell "discarded,
            // stale" apart from "ran cleanly and found nothing" -- see
            // `IngestStats::discarded_stale_generation`'s doc comment.
            return Ok(IngestStats {
                discarded_stale_generation: true,
                ..Default::default()
            });
        }
    }
    // Moniker -> declaration location is still sufficient for identifying the
    // callee symbol. The call-site proof below is intentionally stronger: a
    // reference participates only with its exact byte span and source hash.
    let mut def_of: HashMap<&str, DefinitionSite> = HashMap::new();
    for occurrence in occ {
        if occurrence.is_def && !occurrence.is_local {
            def_of.insert(
                occurrence.symbol.as_str(),
                (occurrence.file.clone(), occurrence.line as i64),
            );
        }
    }

    let mut ref_targets: HashMap<ExactCallSiteKey, Vec<DefinitionSite>> = HashMap::new();
    // Parallel to `ref_targets`: for a `Fallback`-encoded occurrence whose
    // alternate raw-UTF-8 interpretation disagreed with its primary guess
    // (`guessed_alt_byte_range`), this holds the alternate span's own key —
    // checked below against real `call_sites` rows for ambiguity.
    let mut alt_key_of: HashMap<ExactCallSiteKey, ExactCallSiteKey> = HashMap::new();
    for occurrence in occ {
        let (Some(start_byte), Some(end_byte), Some(source_file_hash)) = (
            occurrence.start_byte,
            occurrence.end_byte,
            occurrence.source_file_hash.as_deref(),
        ) else {
            continue;
        };
        let primary_key: ExactCallSiteKey = (
            occurrence.file.clone(),
            start_byte as i64,
            end_byte as i64,
            source_file_hash.to_string(),
        );
        if let Some((alt_start, alt_end)) = occurrence.guessed_alt_byte_range {
            alt_key_of.insert(
                primary_key.clone(),
                (
                    occurrence.file.clone(),
                    alt_start as i64,
                    alt_end as i64,
                    source_file_hash.to_string(),
                ),
            );
        }
        if !occurrence.is_def
            && !occurrence.is_local
            && let Some(definition) = def_of.get(occurrence.symbol.as_str())
        {
            ref_targets
                .entry(primary_key)
                .or_default()
                .push(definition.clone());
        }
    }

    let rows: Vec<CallEdgeRow> = {
        let mut stmt = conn.prepare(
            "SELECT ce.id, ce.call_site_id, cs.from_path, cs.callee_start_byte,
                    cs.callee_end_byte, fi.hash, s.path, s.line_start, ce.edge_confidence,
                    ce.formal_source, ce.ruled_out_by_scip
             FROM call_edges ce
             JOIN call_sites cs ON cs.id = ce.call_site_id
             JOIN file_index fi ON fi.path = cs.from_path
             JOIN symbols s ON s.qualified_name = ce.to_symbol
             WHERE ce.call_site_id IS NOT NULL
               AND cs.identity_version >= 2
               AND cs.callee_start_byte IS NOT NULL
               AND cs.callee_end_byte IS NOT NULL",
        )?;
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                (row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?),
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get::<_, i64>(10)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?
    };

    // Defense-in-depth (VHEATM Tier-2 §1 Lớp 2): a `Fallback`-encoded
    // occurrence whose primary AND alternate spans both independently
    // resolve to distinct, real CallSite rows can't be trusted — reject it
    // outright (neither span upgrades, rules out, or inserts anything)
    // rather than guessing which one is right.
    let row_keys: HashSet<ExactCallSiteKey> =
        rows.iter().map(|(_, _, key, ..)| key.clone()).collect();
    let ambiguous_keys: Vec<ExactCallSiteKey> = alt_key_of
        .iter()
        .filter(|(primary, alt)| {
            *alt != *primary && row_keys.contains(*primary) && row_keys.contains(*alt)
        })
        .map(|(primary, _)| primary.clone())
        .collect();
    for key in &ambiguous_keys {
        ref_targets.remove(key);
    }
    let ambiguous = ambiguous_keys.len();

    let mut to_upgrade = Vec::new();
    let mut newly_upgraded = 0;
    let mut satisfied = HashSet::new();
    for (edge_id, _, key, def_path, def_line, confidence, formal_source, _) in &rows {
        let agrees = ref_targets.get(key).is_some_and(|targets| {
            targets
                .iter()
                .any(|(path, line)| path == def_path && *line == *def_line)
        });
        if !agrees {
            continue;
        }
        satisfied.insert(key.clone());
        if confidence == "formal" && formal_source.as_deref() == Some("scip") {
            continue;
        }
        if confidence != "formal" {
            newly_upgraded += 1;
        }
        to_upgrade.push(*edge_id);
    }

    let updated_edge_ids = {
        let mut update = conn.prepare(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'scip',
                 evidence_state = CASE WHEN ?2 != 0 THEN 'fresh' ELSE 'unverified' END
             WHERE id = ?1",
        )?;
        let mut changed = Vec::new();
        for edge_id in to_upgrade {
            if update.execute(rusqlite::params![
                edge_id,
                i64::from(proof_context.is_some())
            ])? > 0
            {
                changed.push(edge_id);
            }
        }
        changed
    };
    if let Some(context) = proof_context {
        for edge_id in updated_edge_ids {
            record_external_proof_for_edge(conn, context, edge_id, "scip")?;
        }
    }

    let to_rule_out = exact_sibling_rule_out_ids(&rows, &ref_targets);
    let mut rule_out = conn.prepare("UPDATE call_edges SET ruled_out_by_scip = 1 WHERE id = ?1")?;
    for edge_id in &to_rule_out {
        rule_out.execute([edge_id])?;
    }

    let inserted = if insert_missing {
        insert_missing_exact_edges(conn, &ref_targets, &mut satisfied, proof_context)?
    } else {
        0
    };

    Ok(IngestStats {
        upgraded: newly_upgraded,
        ruled_out: to_rule_out.len(),
        inserted,
        match_rate: if ref_targets.is_empty() {
            0.0
        } else {
            satisfied.len() as f64 / ref_targets.len() as f64
        },
        ambiguous,
        discarded_stale_generation: false,
    })
}

fn exact_sibling_rule_out_ids(
    rows: &[CallEdgeRow],
    ref_targets: &HashMap<ExactCallSiteKey, Vec<DefinitionSite>>,
) -> Vec<i64> {
    let mut groups: HashMap<i64, Vec<_>> = HashMap::new();
    for row in rows {
        if ref_targets.contains_key(&row.2) {
            groups.entry(row.1).or_default().push(row);
        }
    }

    let mut ruled_out = Vec::new();
    for members in groups.into_values() {
        if members.len() < 2 {
            continue;
        }
        for (edge_id, _, key, def_path, def_line, _, _, already_ruled_out) in members {
            let matches_scip = ref_targets.get(key).is_some_and(|targets| {
                targets
                    .iter()
                    .any(|(path, line)| path == def_path && *line == *def_line)
            });
            if !matches_scip && !*already_ruled_out {
                ruled_out.push(*edge_id);
            }
        }
    }
    ruled_out
}

fn insert_missing_exact_edges(
    conn: &Connection,
    ref_targets: &HashMap<ExactCallSiteKey, Vec<DefinitionSite>>,
    satisfied: &mut HashSet<ExactCallSiteKey>,
    proof_context: Option<&ExternalProofContext>,
) -> rusqlite::Result<usize> {
    let mut inserted = 0;
    let mut insert = conn.prepare(
        "INSERT OR IGNORE INTO call_edges
            (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path,
             to_path, edge_kind, formal_source, evidence_state, ruled_out_by_scip)
         VALUES (?1, ?2, ?3, ?4, 'formal', ?5, ?6, 'call', 'scip',
                 CASE WHEN ?7 != 0 THEN 'fresh' ELSE 'unverified' END, 0)",
    )?;

    for (key, targets) in ref_targets {
        let Some((call_site_id, enclosing_qn, call_line)) = exact_current_call_site(conn, key)?
        else {
            continue;
        };
        for (def_path, def_line) in targets {
            let Some(to_symbol) = resolve_unique_symbol_at(conn, def_path, *def_line)? else {
                continue;
            };
            // WS7 (evidence reconciliation, fixture I / D8): the static
            // resolver may have already resolved this EXACT call site to a
            // DIFFERENT target at a confident tier via a real language rule --
            // e.g. Python's file-symbol-over-import priority, where a later
            // same-scope `def name` shadows `from x import name`, so `name()`
            // never calls the import. SCIP is a strong authority but not an
            // infallible one: some SCIP indexers follow the import binding and
            // miss that shadowing. It must NOT silently ADD a competing
            // `formal` edge contradicting a confident static resolution -- that
            // is a genuine, live false_confidence_rate data point (a top-tier
            // edge to a target the language never actually calls). Skip the
            // insert on conflict; the correct static edge stays.
            if has_conflicting_confident_static_edge(conn, call_site_id, &to_symbol)? {
                continue;
            }
            let added = insert.execute(rusqlite::params![
                enclosing_qn,
                to_symbol,
                call_line,
                call_site_id,
                &key.0,
                def_path,
                i64::from(proof_context.is_some()),
            ])?;
            if added > 0 {
                inserted += added;
                satisfied.insert(key.clone());
                if let Some(context) = proof_context {
                    let edge_id = conn.query_row(
                        "SELECT id FROM call_edges
                         WHERE call_site_id = ?1 AND to_symbol = ?2 AND edge_kind = 'call'",
                        rusqlite::params![call_site_id, to_symbol],
                        |row| row.get(0),
                    )?;
                    record_external_proof_for_edge(conn, context, edge_id, "scip")?;
                }
            }
        }
    }
    Ok(inserted)
}

/// WS7 reconciliation guard for `insert_missing_exact_edges`: true when this
/// call site already carries a CONFIDENT STATIC edge to a target OTHER than the
/// one SCIP wants to add. "Confident static" = `resolved` (tier-1 language-rule
/// resolution) or a non-SCIP `formal` (e.g. stack-graphs) edge -- deliberately
/// NOT `ambiguous`/`textual`/`inferred`, which the overlay is SUPPOSED to
/// override (that is the whole point of SCIP disambiguating fan-out). A SCIP
/// proof contradicting a confident static resolution is a conflict, not a new
/// target: adding it would manufacture a false-confidence edge (D8).
fn has_conflicting_confident_static_edge(
    conn: &Connection,
    call_site_id: i64,
    scip_target: &str,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM call_edges
             WHERE call_site_id = ?1
               AND to_symbol != ?2
               AND ruled_out_by_scip = 0
               AND ( edge_confidence = 'resolved'
                  OR (edge_confidence = 'formal'
                      AND (formal_source IS NULL OR formal_source != 'scip')) )
         )",
        rusqlite::params![call_site_id, scip_target],
        |row| row.get::<_, i64>(0),
    )
    .map(|exists| exists != 0)
}

/// Persist evidence only after the graph row itself was accepted.  The SELECT
/// rechecks the current CallSite/span/file snapshot, so a stale or deleted
/// edge cannot manufacture a proof record by id alone.
pub(crate) fn record_external_proof_for_edge(
    conn: &Connection,
    context: &ExternalProofContext,
    edge_id: i64,
    expected_formal_source: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO external_proofs
            (call_site_id, to_symbol, provider, source_file_hash, callee_start_byte,
             callee_end_byte, provider_fingerprint, context_fingerprint, graph_generation,
             call_site_identity_version, definition_snapshot, status, observed_at)
         SELECT ce.call_site_id, ce.to_symbol, ?1, fi.hash, cs.callee_start_byte,
                 cs.callee_end_byte, ?2, ?3, graph.generation, cs.identity_version,
                 printf('%s:%d:%d:%s', COALESCE(def_file.hash, ''), def.line_start, def.line_end, def.signature),
                 'fresh', unixepoch('now')
         FROM call_edges ce
         JOIN call_sites cs ON cs.id = ce.call_site_id
         JOIN file_index fi ON fi.path = cs.from_path
         JOIN symbols def ON def.qualified_name = ce.to_symbol
         LEFT JOIN file_index def_file ON def_file.path = def.path
         JOIN graph_generation_state graph ON graph.id = 1
         WHERE ce.id = ?4
            AND ce.formal_source = ?5
            AND cs.identity_version >= 2
            AND cs.callee_start_byte IS NOT NULL
            AND cs.callee_end_byte IS NOT NULL
            AND (?6 IS NULL OR graph.generation = ?6)
          ON CONFLICT(call_site_id, to_symbol, provider) DO UPDATE SET
              source_file_hash = excluded.source_file_hash,
              callee_start_byte = excluded.callee_start_byte,
              callee_end_byte = excluded.callee_end_byte,
              provider_fingerprint = excluded.provider_fingerprint,
              context_fingerprint = excluded.context_fingerprint,
              graph_generation = excluded.graph_generation,
              call_site_identity_version = excluded.call_site_identity_version,
              definition_snapshot = excluded.definition_snapshot,
              status = 'fresh',
             observed_at = excluded.observed_at,
             failure_reason = NULL",
        rusqlite::params![
            context.provider,
            context.provider_fingerprint,
            context.context_fingerprint,
            edge_id,
            expected_formal_source,
            context.graph_generation,
        ],
    )?;
    Ok(())
}

/// Finds one current call site only when its source hash proves the SCIP range
/// was measured against the same bytes CALM parsed. The partial unique index
/// makes a duplicate impossible for new rows; treating any legacy corruption
/// as ambiguous still keeps this path fail-closed.
fn exact_current_call_site(
    conn: &Connection,
    key: &ExactCallSiteKey,
) -> rusqlite::Result<Option<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT cs.id, cs.enclosing_qn, cs.call_line
         FROM call_sites cs
         JOIN file_index fi ON fi.path = cs.from_path
         WHERE cs.from_path = ?1
           AND cs.callee_start_byte = ?2
           AND cs.callee_end_byte = ?3
           AND fi.hash = ?4
           AND cs.identity_version >= 2
           AND cs.edge_kind = 'call'",
    )?;
    let matches = stmt
        .query_map(rusqlite::params![&key.0, key.1, key.2, &key.3], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((matches.len() == 1)
        .then(|| matches.into_iter().next())
        .flatten())
}

/// Retains D3's line-keyed behavior exclusively as a regression oracle for
/// tests. Production code must use [`ingest_occurrences`], which mutates only
/// when D4 has exact CallSite byte-span and source-hash identity.
#[cfg(test)]
fn ingest_line_keyed_occurrences_for_regression(
    conn: &Connection,
    occ: &[ScipOccurrence],
    insert_missing: bool,
) -> rusqlite::Result<IngestStats> {
    // moniker -> (def_file, def_line)
    let mut def_of: HashMap<&str, (&str, usize)> = HashMap::new();
    for o in occ {
        if o.is_def && !o.is_local {
            def_of.insert(o.symbol.as_str(), (o.file.as_str(), o.line));
        }
    }
    // (ref_file, ref_line) -> set of def sites it points to
    let mut ref_targets: HashMap<(&str, usize), Vec<(&str, usize)>> = HashMap::new();
    for o in occ {
        if !o.is_def
            && !o.is_local
            && let Some(&def) = def_of.get(o.symbol.as_str())
        {
            ref_targets
                .entry((o.file.as_str(), o.line))
                .or_default()
                .push(def);
        }
    }

    // Load every edge with a known call site, joined to its target's decl
    // site — any confidence, including already-`formal`, since the override
    // check below and the ruled-out pass both need to see a `formal` sibling
    // even when it was upgraded on a previous run.
    let rows: Vec<EdgeRow> = {
        let mut stmt = conn.prepare(
            "SELECT ce.id, ce.from_path, ce.call_site_line, s.path, s.line_start, \
                    ce.edge_confidence, ce.ruled_out_by_scip, ce.formal_source \
             FROM call_edges ce \
             JOIN symbols s ON s.qualified_name = ce.to_symbol \
             WHERE ce.call_site_line IS NOT NULL \
               AND ce.from_path IS NOT NULL",
        )?;
        stmt.query_map([], |r| {
            Ok(EdgeRow {
                id: r.get(0)?,
                from_path: r.get(1)?,
                call_line: r.get(2)?,
                def_path: r.get(3)?,
                def_line: r.get(4)?,
                confidence: r.get(5)?,
                ruled_out: r.get::<_, i64>(6)? != 0,
                formal_source: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut to_upgrade: Vec<i64> = Vec::new();
    let mut newly_upgraded_count = 0usize;
    // Owned (not borrowed) so it can hold keys from both `rows` (fresh
    // `String`s read from the DB) and `ref_targets` (borrowed from `occ`) —
    // two unrelated lifetimes that a borrowed-key set couldn't unify.
    let mut satisfied_sites: HashSet<(String, usize)> = HashSet::new();
    for row in &rows {
        let key = (row.from_path.as_str(), row.call_line as usize);
        let scip_agrees = ref_targets.get(&key).is_some_and(|targets| {
            targets
                .iter()
                .any(|(f, l)| *f == row.def_path.as_str() && *l == row.def_line as usize)
        });
        if scip_agrees {
            satisfied_sites.insert((row.from_path.clone(), row.call_line as usize));
        }
        if row.confidence == "formal" && row.formal_source.as_deref() == Some("scip") {
            continue; // already the strongest possible evidence — never re-litigated
        }
        if scip_agrees {
            if row.confidence == "formal"
                && matches!(row.formal_source.as_deref(), None | Some("stack_graphs"))
            {
                // D3: about to reconfirm/override a stack_graphs (or
                // unattributed) formal verdict with SCIP's exact evidence --
                // a real disagreement between the two formal-tier sources.
                record_scip_stack_graphs_override(
                    &row.from_path,
                    row.call_line,
                    &row.def_path,
                    row.def_line,
                    false,
                );
            }
            to_upgrade.push(row.id);
            if row.confidence != "formal" {
                newly_upgraded_count += 1;
            }
        }
    }
    {
        // Sets `formal_source = 'scip'` unconditionally for every id here —
        // correct whether this is a fresh upgrade (was textual/inferred/
        // ambiguous) or a reconfirmation of a `stack_graphs`-sourced formal
        // edge SCIP's exact match agrees with (P0.3: SCIP is allowed to
        // confirm/override `stack_graphs`, never the reverse).
        let mut stmt = conn.prepare(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'scip' \
             WHERE id = ?1",
        )?;
        for id in &to_upgrade {
            stmt.execute([id])?;
        }
    }

    let upgraded_this_run: HashSet<i64> = to_upgrade.iter().copied().collect();
    let to_rule_out = mark_ruled_out_siblings(conn, &rows, &ref_targets, &upgraded_this_run)?;

    let inserted = if insert_missing {
        insert_missing_edges(conn, &rows, &ref_targets, &mut satisfied_sites)?
    } else {
        0
    };

    let match_rate = if ref_targets.is_empty() {
        0.0
    } else {
        satisfied_sites.len() as f64 / ref_targets.len() as f64
    };

    Ok(IngestStats {
        upgraded: newly_upgraded_count,
        ruled_out: to_rule_out,
        inserted,
        match_rate,
        // Legacy line-keyed path has no per-provider encoding guess to
        // second-guess (see `parse::ScipOccurrence::guessed_alt_byte_range`'s
        // doc comment) — nothing to flag as ambiguous.
        ambiguous: 0,
        // This legacy path never checks `graph_generation_state` at all —
        // it can't discard for staleness in the first place.
        discarded_stale_generation: false,
    })
}

/// Second, orthogonal marking pass: for every `(from_path, call_site_line)`
/// group with more than one candidate edge (the ambiguous-fan-out shape —
/// same call site, one row per same-named symbol tree-sitter couldn't
/// disambiguate), mark every non-`formal` member `ruled_out_by_scip` once
/// SCIP has *decisively* answered what that exact call site resolves to:
///
/// - one sibling in the group is (now, or already) `formal` — the others are
///   therefore proven wrong, since one call site has exactly one true target; or
/// - SCIP resolved a reference at that site to a definition outside every
///   member's declaration site entirely (e.g. every candidate is a same-named
///   *project* method but the real receiver is `String`/`serde_json::Value`/
///   another external type) — every member is proven wrong.
///
/// Groups with no decisive evidence either way (no SCIP reference recorded at
/// that site at all, or SCIP's reference doesn't resolve at all — e.g.
/// unstable/generic code rust-analyzer itself couldn't type-check) are left
/// untouched: absence of evidence is not evidence of wrongness. Never touches
/// `edge_confidence` — see the module doc comment.
///
/// A `formal` member sourced from `stack_graphs` (or an unattributed
/// pre-migration row) is treated as formal for this pass UNLESS this run's
/// SCIP evidence has an opinion about that exact call site and disagrees —
/// in that case it's demoted to non-formal for this computation alone (never
/// touching its `edge_confidence`), so a sibling with better evidence can win
/// the group and this one can be ruled out instead (P0.3: SCIP overriding a
/// `stack_graphs`-sourced formal edge).
/// One fan-out group member: `(id, def_path, def_line, is_formal, already_ruled_out)`.
/// `was_stack_graphs_formal` (D3, appended last) is `true` when this row's
/// ORIGINAL `edge_confidence == "formal"` AND `formal_source` was
/// `None`/`"stack_graphs"` -- a genuine stack-graphs (or unattributed)
/// formal verdict this pass might rule out, distinct from an ordinary
/// non-formal sibling losing a fan-out.
#[cfg(test)]
type GroupMember<'a> = (i64, &'a str, i64, bool, bool, bool);

#[cfg(test)]
fn mark_ruled_out_siblings(
    conn: &Connection,
    rows: &[EdgeRow],
    ref_targets: &HashMap<(&str, usize), Vec<(&str, usize)>>,
    upgraded_this_run: &HashSet<i64>,
) -> rusqlite::Result<usize> {
    let mut groups: HashMap<(&str, i64), Vec<GroupMember>> = HashMap::new();
    for row in rows {
        let key = (row.from_path.as_str(), row.call_line as usize);
        let scip_agrees = ref_targets.get(&key).is_some_and(|targets| {
            targets
                .iter()
                .any(|(f, l)| *f == row.def_path.as_str() && *l == row.def_line as usize)
        });
        let scip_has_opinion = ref_targets.contains_key(&key);
        let is_formal = if upgraded_this_run.contains(&row.id)
            || (row.confidence == "formal" && row.formal_source.as_deref() == Some("scip"))
        {
            true
        } else if row.confidence == "formal" {
            !scip_has_opinion || scip_agrees
        } else {
            false
        };
        let was_stack_graphs_formal = row.confidence == "formal"
            && matches!(row.formal_source.as_deref(), None | Some("stack_graphs"));
        groups
            .entry((row.from_path.as_str(), row.call_line))
            .or_default()
            .push((
                row.id,
                row.def_path.as_str(),
                row.def_line,
                is_formal,
                row.ruled_out,
                was_stack_graphs_formal,
            ));
    }

    let mut to_rule_out: Vec<i64> = Vec::new();
    for ((from_path, call_line), members) in &groups {
        if members.len() < 2 {
            continue; // not a fan-out group — nothing to declutter
        }
        let has_formal_member = members.iter().any(|(_, _, _, is_formal, _, _)| *is_formal);
        let key = (*from_path, *call_line as usize);
        let scip_points_outside_group = ref_targets.get(&key).is_some_and(|targets| {
            targets.iter().all(|(f, l)| {
                !members
                    .iter()
                    .any(|(_, def_path, def_line, ..)| f == def_path && *l == *def_line as usize)
            })
        });
        if !has_formal_member && !scip_points_outside_group {
            continue; // no decisive evidence for this group yet
        }
        for (id, def_path, def_line, is_formal, already_ruled_out, was_stack_graphs_formal) in
            members
        {
            if !*is_formal && !*already_ruled_out {
                to_rule_out.push(*id);
                if *was_stack_graphs_formal {
                    // D3: this sibling held a stack_graphs (or unattributed)
                    // formal verdict and SCIP just proved it wrong -- a real
                    // disagreement, not an ordinary fan-out loser.
                    record_scip_stack_graphs_override(
                        from_path, *call_line, def_path, *def_line, true,
                    );
                }
            }
        }
    }

    let mut stmt = conn.prepare("UPDATE call_edges SET ruled_out_by_scip = 1 WHERE id = ?1")?;
    for id in &to_rule_out {
        stmt.execute([id])?;
    }
    Ok(to_rule_out.len())
}

/// Gated insert: for every `(from_path, call_line) -> (def_path, def_line)`
/// SCIP resolved this run that no existing `call_edges` row represents at
/// all (checked against `rows`, i.e. every confidence tier — a
/// lower-confidence row already covering this exact target just needs the
/// upgrade pass above, not a duplicate insert), insert a new
/// `formal`/`formal_source: 'scip'` edge — but only when:
/// - the call site is a real syntactic call tree-sitter itself extracted (a
///   `call_sites` row at that exact `(from_path, call_line)`, with a single
///   unambiguous `enclosing_qn` — this is what keeps a mere SCIP type
///   reference or field access from fabricating a bogus "call" edge out of
///   nothing, since neither is ever recorded in `call_sites`);
/// - the resolved definition maps to exactly one `symbols` row (narrowest
///   enclosing range at `(def_path, def_line)`; zero matches or a tie for
///   narrowest is treated as unresolvable, not guessed at).
///
/// This closes the plan's "MAX_CALLEE_CANDIDATES cap" gap: a call site whose
/// bare callee name fans out to more than 20 same-named candidates repo-wide
/// gets ZERO `call_edges` rows from `rebuild_graph` (not even a
/// low-confidence one) — without this, no amount of a perfect `.scip` file
/// could ever put a `formal` edge there, since the upgrade pass above only
/// ever touches a *pre-existing* row.
#[cfg(test)]
fn insert_missing_edges(
    conn: &Connection,
    rows: &[EdgeRow],
    ref_targets: &HashMap<(&str, usize), Vec<(&str, usize)>>,
    satisfied_sites: &mut HashSet<(String, usize)>,
) -> rusqlite::Result<usize> {
    // Every (site, target) pair already represented by *some* existing edge
    // — at any confidence — so the upgrade pass (not this one) is the one
    // that handles it.
    let mut already_represented: HashSet<(&str, usize, &str, i64)> = HashSet::new();
    for row in rows {
        already_represented.insert((
            row.from_path.as_str(),
            row.call_line as usize,
            row.def_path.as_str(),
            row.def_line,
        ));
    }

    let mut inserted = 0usize;
    // OR IGNORE + the UNIQUE index on call_edges (db::schema
    // dedup_edges_and_add_unique_indexes) are what actually keep this pass
    // idempotent across overlay runs. `already_represented` is keyed on the
    // target's `symbols.line_start` (from the JOIN in `ingest_occurrences`),
    // but the re-resolved SCIP def-occurrence line often differs from it, so
    // the in-memory check below can fail to recognize an edge inserted on a
    // prior run. The constraint backstops that miss: a re-insert becomes a
    // no-op instead of the duplicate that once inflated caller counts ~19x.
    let mut insert_stmt = conn.prepare(
        "INSERT OR IGNORE INTO call_edges \
            (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path, \
             formal_source, ruled_out_by_scip) \
         VALUES (?1, ?2, ?3, 'formal', ?4, ?5, 'scip', 0)",
    )?;
    // A pre-existing non-formal edge at this exact call site, pointing to a
    // DIFFERENT target than the formal edge we just inserted, is a proven
    // tree-sitter mistake: SCIP had an opinion about this site (it's the
    // reason we're inserting at all) and picked a different definition.
    // `mark_ruled_out_siblings` above only declutters fan-out GROUPS
    // (`members.len() >= 2` at the time it runs, before any insert here) —
    // a site where the stale edge was the sole occupant slips through it
    // and would otherwise stay `ruled_out_by_scip = 0` (still served by
    // every caller/callee/edit_context query) forever. Ruling it out here,
    // gated on a real insert actually happening, closes that gap without
    // ever leaving a call site edge-less.
    let mut rule_out_stale_stmt = conn.prepare(
        "UPDATE call_edges SET ruled_out_by_scip = 1 \
         WHERE from_path = ?1 AND call_site_line = ?2 \
           AND edge_confidence != 'formal' AND ruled_out_by_scip = 0 AND to_path != ?3",
    )?;
    for (&(from_path, call_line), targets) in ref_targets {
        for &(def_path, def_line) in targets {
            if already_represented.contains(&(from_path, call_line, def_path, def_line as i64)) {
                satisfied_sites.insert((from_path.to_string(), call_line));
                continue;
            }
            let Some(enc_qn) = enclosing_qn_at(conn, from_path, call_line as i64)? else {
                continue;
            };
            let Some(to_qn) = resolve_unique_symbol_at(conn, def_path, def_line as i64)? else {
                continue;
            };
            // Count only rows the constraint actually accepted — an IGNOREd
            // duplicate returns 0, keeping `inserted`/telemetry honest.
            let n = insert_stmt.execute(rusqlite::params![
                enc_qn,
                to_qn,
                call_line as i64,
                from_path,
                def_path,
            ])?;
            already_represented.insert((from_path, call_line, def_path, def_line as i64));
            satisfied_sites.insert((from_path.to_string(), call_line));
            if n > 0 {
                rule_out_stale_stmt.execute(rusqlite::params![
                    from_path,
                    call_line as i64,
                    def_path
                ])?;
            }
            inserted += n;
        }
    }
    Ok(inserted)
}

/// The single `call_sites.enclosing_qn` recorded for a real syntactic call at
/// `(path, line)`, or `None` when there's no such call site at all, or more
/// than one *distinct* enclosing symbol claims that exact line (shouldn't
/// happen for well-formed source — "ambiguous, skip" beats guessing).
#[cfg(test)]
fn enclosing_qn_at(conn: &Connection, path: &str, line: i64) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT enclosing_qn FROM call_sites WHERE from_path = ?1 AND call_line = ?2",
    )?;
    let mut names: Vec<String> = stmt
        .query_map(rusqlite::params![path, line], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if names.len() == 1 { names.pop() } else { None })
}

/// The one `symbols` row whose `[line_start, line_end]` range at `path`
/// narrowly encloses `line` — narrowest wins over an outer enclosing symbol
/// (e.g. a method's own range over its containing `impl`/class); a tie for
/// narrowest or zero matches is unresolvable and returns `None` rather than
/// guessing.
fn resolve_unique_symbol_at(
    conn: &Connection,
    path: &str,
    line: i64,
) -> rusqlite::Result<Option<String>> {
    resolve_unique_symbol_at_filtered(conn, path, line, false)
}

/// Narrowest-span-wins location→symbol resolution, shared with the LSP
/// overlay (`crate::lsp::overlay`), which passes `exclude_headings: true`
/// because markdown ATX headings are indexed as symbols but are never call
/// targets — SCIP's own callers only ever hand this Rust/Go/... source
/// locations, where no heading rows exist, so `false` preserves their exact
/// pre-existing behavior. A tie for narrowest span returns `None` (genuinely
/// ambiguous — stay conservative) for both callers.
// The optional LSP overlay calls this helper; the default build does not.
#[allow(dead_code)]
pub(crate) fn resolve_unique_symbol_at_filtered(
    conn: &Connection,
    path: &str,
    line: i64,
    exclude_headings: bool,
) -> rusqlite::Result<Option<String>> {
    let sql = if exclude_headings {
        "SELECT qualified_name, line_start, line_end FROM symbols \
         WHERE path = ?1 AND line_start <= ?2 AND line_end >= ?2 AND kind != 'heading'"
    } else {
        "SELECT qualified_name, line_start, line_end FROM symbols \
         WHERE path = ?1 AND line_start <= ?2 AND line_end >= ?2"
    };
    let mut stmt = conn.prepare(sql)?;
    let candidates: Vec<(String, i64, i64)> = stmt
        .query_map(rusqlite::params![path, line], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let min_span = candidates.iter().map(|(_, s, e)| e - s).min().unwrap();
    let mut narrowest = candidates.into_iter().filter(|(_, s, e)| e - s == min_span);
    let first = narrowest.next();
    if narrowest.next().is_some() {
        return Ok(None); // tie for narrowest — genuinely ambiguous
    }
    Ok(first.map(|(qn, ..)| qn))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// D3's line-keyed regression oracle intentionally uses occurrences that
    /// lack D4's byte-span proof. Keeping that fixture shape local prevents
    /// legacy tests from accidentally becoming evidence for production ingest.
    #[derive(Clone)]
    struct LegacyScipOccurrence {
        file: String,
        line: usize,
        symbol: String,
        is_def: bool,
        is_local: bool,
    }

    type ScipOccurrence = LegacyScipOccurrence;

    fn production_occurrences(
        occurrences: &[ScipOccurrence],
    ) -> Vec<crate::scip::parse::ScipOccurrence> {
        occurrences
            .iter()
            .map(|occurrence| crate::scip::parse::ScipOccurrence {
                file: occurrence.file.clone(),
                line: occurrence.line,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: occurrence.symbol.clone(),
                is_def: occurrence.is_def,
                is_local: occurrence.is_local,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Unresolved,
                guessed_alt_byte_range: None,
            })
            .collect()
    }

    // Keep D3's focused mutation cases as a regression oracle without giving
    // the production ingest API permission to mutate call graph rows.
    fn ingest_occurrences(
        conn: &Connection,
        occ: &[ScipOccurrence],
        insert_missing: bool,
    ) -> rusqlite::Result<IngestStats> {
        let legacy_occurrences = production_occurrences(occ);
        super::ingest_line_keyed_occurrences_for_regression(
            conn,
            &legacy_occurrences,
            insert_missing,
        )
    }

    fn db_with_one_textual_edge() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('core/src/engine.rs::Engine::start','start','method','rust','core/src/engine.rs',6,8);
             INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path)
             VALUES ('app/src/main.rs::main','core/src/engine.rs::Engine::start',5,'textual','app/src/main.rs','core/src/engine.rs');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn exact_span_reference_upgrades_only_its_matching_same_line_call_site() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust',
                     'core/src/engine.rs', 6, 8);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('app/src/main.rs', 'fresh-source', 'rust', 1, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES
                ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 4, 9, 2, 'call'),
                ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 12, 17, 2, 'call');",
        )
        .unwrap();
        let site_ids: Vec<i64> = conn
            .prepare("SELECT id FROM call_sites ORDER BY callee_start_byte")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for site_id in &site_ids {
            conn.execute(
                "INSERT INTO call_edges
                    (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
                 VALUES ('app/src/main.rs::main', 'core/src/engine.rs::Engine::start', 5, ?1,
                         'textual', 'app/src/main.rs', 'core/src/engine.rs', 'call')",
                [site_id],
            )
            .unwrap();
        }

        let occ = vec![
            crate::scip::parse::ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                start_byte: Some(4),
                end_byte: Some(9),
                source_file_hash: Some("fresh-source".into()),
                symbol: "M".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
        ];

        let proof_context =
            super::ExternalProofContext::new("scip:test", "test-binary", "test-context");
        let stats =
            super::ingest_occurrences_with_proof_context(&conn, &occ, false, Some(&proof_context))
                .unwrap();
        assert_eq!(stats.upgraded, 1);
        let confidence: Vec<String> = conn
            .prepare(
                "SELECT edge_confidence FROM call_edges
                 ORDER BY call_site_id",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(confidence, vec!["formal", "textual"]);
        let evidence_states: Vec<String> = conn
            .prepare("SELECT evidence_state FROM call_edges ORDER BY call_site_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(evidence_states, vec!["fresh", "unverified"]);
        let proof: (String, String, String, i64, i64, String) = conn
            .query_row(
                "SELECT provider, provider_fingerprint, context_fingerprint,
                        graph_generation, call_site_identity_version, definition_snapshot
                 FROM external_proofs",
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
        assert_eq!(proof.0, "scip:test");
        assert_eq!(proof.1, "test-binary");
        assert_eq!(proof.2, "test-context");
        assert_eq!(proof.3, 0);
        assert_eq!(proof.4, 2);
        assert!(
            !proof.5.is_empty(),
            "a durable proof must snapshot the target definition"
        );

        conn.execute(
            "UPDATE call_edges SET edge_confidence = 'textual', formal_source = NULL",
            [],
        )
        .unwrap();
        let mut stale_occurrences = occ;
        stale_occurrences[1].source_file_hash = Some("stale-source".into());
        let stale_stats = super::ingest_occurrences(&conn, &stale_occurrences, false).unwrap();
        assert_eq!(stale_stats.upgraded, 0);
        let formal_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE edge_confidence = 'formal'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            formal_count, 0,
            "a range derived from different source bytes must not mutate the graph"
        );
    }

    #[test]
    fn ambiguous_guessed_encoding_upgrades_neither_candidate() {
        // Two real call sites at the same line, same callee — mirrors the
        // fixture above, but here ONE occurrence's primary span matches the
        // first call site while its `guessed_alt_byte_range` (defense-in-
        // depth for a fallback-guessed encoding, VHEATM Tier-2 §1 Lớp 2)
        // independently matches the second, distinct call site. Two
        // plausible-but-different targets means neither should be trusted.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust',
                     'core/src/engine.rs', 6, 8);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('app/src/main.rs', 'fresh-source', 'rust', 1, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES
                ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 4, 9, 2, 'call'),
                ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 12, 17, 2, 'call');",
        )
        .unwrap();
        let site_ids: Vec<i64> = conn
            .prepare("SELECT id FROM call_sites ORDER BY callee_start_byte")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for site_id in &site_ids {
            conn.execute(
                "INSERT INTO call_edges
                    (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
                 VALUES ('app/src/main.rs::main', 'core/src/engine.rs::Engine::start', 5, ?1,
                         'textual', 'app/src/main.rs', 'core/src/engine.rs', 'call')",
                [site_id],
            )
            .unwrap();
        }

        let occ = vec![
            crate::scip::parse::ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                start_byte: Some(4),
                end_byte: Some(9),
                source_file_hash: Some("fresh-source".into()),
                symbol: "M".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Fallback,
                guessed_alt_byte_range: Some((12, 17)),
            },
        ];

        let stats = super::ingest_occurrences(&conn, &occ, false).unwrap();
        assert_eq!(
            stats.upgraded, 0,
            "ambiguous evidence must not upgrade either candidate"
        );
        assert_eq!(stats.ambiguous, 1);
        let confidence: Vec<String> = conn
            .prepare("SELECT edge_confidence FROM call_edges ORDER BY call_site_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            confidence,
            vec!["textual".to_string(), "textual".to_string()],
            "neither edge should be touched"
        );
    }

    #[test]
    fn same_line_non_call_reference_cannot_formalize_or_rule_out_a_call() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES
                ('a.rs::A::start', 'start', 'method', 'rust', 'a.rs', 1, 1),
                ('b.rs::B::start', 'start', 'method', 'rust', 'b.rs', 1, 1);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('app/src/main.rs', 'fresh-source', 'rust', 1, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 4, 9, 2, 'call');
             INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
             VALUES
                ('app/src/main.rs::main', 'a.rs::A::start', 5, 1, 'ambiguous', 'app/src/main.rs', 'a.rs', 'call'),
                ('app/src/main.rs::main', 'b.rs::B::start', 5, 1, 'ambiguous', 'app/src/main.rs', 'b.rs', 'call');",
        )
        .unwrap();
        let occurrences = vec![
            crate::scip::parse::ScipOccurrence {
                file: "b.rs".into(),
                line: 1,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                // This is another `start` reference on the same line, not the
                // call site's callee token at byte range 4..9.
                start_byte: Some(12),
                end_byte: Some(17),
                source_file_hash: Some("fresh-source".into()),
                symbol: "M".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
        ];

        let stats = super::ingest_occurrences(&conn, &occurrences, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 0);
        assert_eq!(stats.inserted, 0);
        let rows: Vec<(String, String, i64)> = conn
            .prepare(
                "SELECT to_symbol, edge_confidence, ruled_out_by_scip
                 FROM call_edges ORDER BY to_symbol",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a.rs::A::start".into(), "ambiguous".into(), 0),
                ("b.rs::B::start".into(), "ambiguous".into(), 0),
            ]
        );
        let proof_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM external_proofs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(proof_count, 0);
    }

    #[test]
    fn exact_span_reference_inserts_for_an_uncandidated_call_site() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust',
                     'core/src/engine.rs', 6, 8);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('app/src/main.rs', 'fresh-source', 'rust', 1, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 4, 9, 2, 'call');",
        )
        .unwrap();
        let occ = vec![
            crate::scip::parse::ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                start_byte: Some(4),
                end_byte: Some(9),
                source_file_hash: Some("fresh-source".into()),
                symbol: "M".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
        ];

        let stats = super::ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.inserted, 1);
        let (site_id, confidence, source): (i64, String, String) = conn
            .query_row(
                "SELECT call_site_id, edge_confidence, formal_source FROM call_edges",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            site_id,
            conn.query_row("SELECT id FROM call_sites", [], |row| row.get::<_, i64>(0))
                .unwrap()
        );
        assert_eq!(confidence, "formal");
        assert_eq!(source, "scip");
    }

    #[test]
    fn scip_does_not_add_formal_edge_conflicting_with_confident_static_resolution() {
        // WS7 / fixture I (D8): `from external import name; def name(): ...;
        // name()`. Python's file-symbol-over-import priority correctly resolves
        // the call to the LOCAL `name` at `resolved`. scip-python follows the
        // import binding and reports external.py::name -- but real Python
        // semantics never call it (the later same-scope def shadows the
        // import). The overlay must NOT insert a competing `formal` edge that
        // contradicts the confident static resolution (verified live 2026-08-19:
        // it used to, producing a top-tier false_confidence edge).
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES
                ('caller.py::name', 'name', 'function', 'python', 'caller.py', 4, 5),
                ('external.py::name', 'name', 'function', 'python', 'external.py', 1, 2);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('caller.py', 'fresh-source', 'python', 2, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('caller.py', 'caller.py::use', 'name', 9, 4, 9, 2, 'call');
             INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence,
                 from_path, to_path, edge_kind)
             VALUES ('caller.py::use', 'caller.py::name', 9, 1, 'resolved',
                     'caller.py', 'caller.py', 'call');",
        )
        .unwrap();
        let occ = vec![
            crate::scip::parse::ScipOccurrence {
                file: "external.py".into(),
                line: 1,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "N".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "caller.py".into(),
                line: 9,
                start_byte: Some(4),
                end_byte: Some(9),
                source_file_hash: Some("fresh-source".into()),
                symbol: "N".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
        ];

        let stats = super::ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            stats.inserted, 0,
            "scip must not insert a competing formal edge conflicting with the \
             confident static (resolved) resolution of the same call site"
        );
        let edges: Vec<(String, String, i64)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT to_symbol, edge_confidence, ruled_out_by_scip \
                     FROM call_edges ORDER BY to_symbol",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            edges,
            vec![("caller.py::name".to_string(), "resolved".to_string(), 0)],
            "only the correct local edge should survive, un-ruled-out: {edges:?}"
        );
    }

    #[test]
    fn exact_span_reference_rules_out_only_other_targets_of_the_same_call_site() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end)
             VALUES
                ('a.rs::A::start', 'start', 'method', 'rust', 'a.rs', 1, 1),
                ('b.rs::B::start', 'start', 'method', 'rust', 'b.rs', 1, 1);
             INSERT INTO file_index (path, hash, language, symbol_count, last_indexed)
             VALUES ('app/src/main.rs', 'fresh-source', 'rust', 1, 0);
             INSERT INTO call_sites
                (from_path, enclosing_qn, callee_name, call_line, callee_start_byte,
                 callee_end_byte, identity_version, edge_kind)
             VALUES ('app/src/main.rs', 'app/src/main.rs::main', 'start', 5, 4, 9, 2, 'call');
             INSERT INTO call_edges
                (from_symbol, to_symbol, call_site_line, call_site_id, edge_confidence, from_path, to_path, edge_kind)
             VALUES
                ('app/src/main.rs::main', 'a.rs::A::start', 5, 1, 'ambiguous', 'app/src/main.rs', 'a.rs', 'call'),
                ('app/src/main.rs::main', 'b.rs::B::start', 5, 1, 'ambiguous', 'app/src/main.rs', 'b.rs', 'call');",
        )
        .unwrap();
        let occ = vec![
            crate::scip::parse::ScipOccurrence {
                file: "b.rs".into(),
                line: 1,
                start_byte: None,
                end_byte: None,
                source_file_hash: None,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
            crate::scip::parse::ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                start_byte: Some(4),
                end_byte: Some(9),
                source_file_hash: Some("fresh-source".into()),
                symbol: "M".into(),
                is_def: false,
                is_local: false,
                encoding_provenance: crate::scip::parse::EncodingProvenance::Declared,
                guessed_alt_byte_range: None,
            },
        ];

        let stats = super::ingest_occurrences(&conn, &occ, false).unwrap();
        assert_eq!(stats.upgraded, 1);
        assert_eq!(stats.ruled_out, 1);
        let rows: Vec<(String, String, i64)> = conn
            .prepare(
                "SELECT to_symbol, edge_confidence, ruled_out_by_scip
                 FROM call_edges ORDER BY to_symbol",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a.rs::A::start".into(), "ambiguous".into(), 1),
                ("b.rs::B::start".into(), "formal".into(), 0),
            ]
        );
    }

    #[test]
    fn upgrades_matching_edge_to_formal() {
        let conn = db_with_one_textual_edge();
        let occ = vec![
            // def of start() at engine.rs line 6
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            // ref at the call site (main.rs line 5) pointing to the same moniker
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 1);
        assert_eq!(
            stats.ruled_out, 0,
            "lone edge has no fan-out sibling to rule out"
        );
        let conf: String = conn
            .query_row("SELECT edge_confidence FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(conf, "formal");
    }

    #[test]
    fn never_downgrades_or_inserts() {
        let conn = db_with_one_textual_edge();
        conn.execute("UPDATE call_edges SET edge_confidence = 'resolved'", [])
            .unwrap();
        // Occurrences that match nothing must leave the edge and count untouched.
        let occ = vec![ScipOccurrence {
            file: "zzz.rs".into(),
            line: 99,
            symbol: "X".into(),
            is_def: false,
            is_local: false,
        }];
        let exact_occurrences = production_occurrences(&occ);
        let stats = super::ingest_occurrences(&conn, &exact_occurrences, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 0);
        let (conf, cnt): (String, i64) = conn
            .query_row(
                "SELECT edge_confidence, (SELECT COUNT(*) FROM call_edges) FROM call_edges",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(conf, "resolved");
        assert_eq!(cnt, 1);
    }

    /// A lone `ambiguous` edge (no sibling at the same call site) is never
    /// ruled out even when SCIP's reference there resolves to something else
    /// entirely — with only one candidate, there's no fan-out noise to
    /// declutter, and ADR-0004 §3 says never remove/hide the only edge a
    /// caller/callee query would otherwise return for that site.
    #[test]
    fn lone_edge_is_never_ruled_out_even_when_scip_disagrees() {
        let conn = db_with_one_textual_edge();
        conn.execute("UPDATE call_edges SET edge_confidence = 'ambiguous'", [])
            .unwrap();
        // SCIP resolves the call site to a def elsewhere (not Engine::start).
        let occ = vec![
            ScipOccurrence {
                file: "std/string.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 0);
        let ruled_out: bool = conn
            .query_row("SELECT ruled_out_by_scip FROM call_edges", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|v| v != 0)
            .unwrap();
        assert!(!ruled_out);
    }

    fn db_with_ambiguous_fan_out(targets: &[(&str, &str, i64)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        for (qname, path, line) in targets {
            conn.execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
                 VALUES (?1, 'as_str', 'method', 'rust', ?2, ?3, ?3)",
                rusqlite::params![qname, path, line],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path) \
                 VALUES ('app/src/main.rs::main', ?1, 5, 'ambiguous', 'app/src/main.rs', ?2)",
                rusqlite::params![qname, path],
            )
            .unwrap();
        }
        conn
    }

    /// Ambiguous fan-out (same bare method name, several candidate symbols,
    /// one row each) — once SCIP confirms ONE candidate is the real target
    /// (upgraded to `formal`), every sibling at that exact call site is
    /// proven wrong (a call site has exactly one true target) and gets
    /// `ruled_out_by_scip`, without touching their `edge_confidence`.
    #[test]
    fn confirming_one_fan_out_candidate_rules_out_its_siblings() {
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
            ("c.rs::C::as_str", "c.rs", 1),
        ]);
        let occ = vec![
            ScipOccurrence {
                file: "b.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 1);
        assert_eq!(stats.ruled_out, 2);
        let mut stmt = conn
            .prepare("SELECT to_symbol, edge_confidence, ruled_out_by_scip FROM call_edges ORDER BY to_symbol")
            .unwrap();
        let rows: Vec<(String, String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(
            rows,
            vec![
                ("a.rs::A::as_str".to_string(), "ambiguous".to_string(), 1),
                ("b.rs::B::as_str".to_string(), "formal".to_string(), 0),
                ("c.rs::C::as_str".to_string(), "ambiguous".to_string(), 1),
            ]
        );
    }

    /// Ambiguous fan-out where SCIP resolves the call site to a definition
    /// entirely outside the candidate set (e.g. every syntactic candidate is
    /// a same-named *project* method but the real receiver is an external
    /// type like `String`) — no candidate gets `formal` (none is right), but
    /// every one is proven wrong and gets `ruled_out_by_scip`.
    #[test]
    fn fan_out_ruled_out_entirely_when_scip_resolves_outside_the_group() {
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        let occ = vec![
            ScipOccurrence {
                file: "std/string.rs".into(),
                line: 42,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 2);
        let mut stmt = conn
            .prepare("SELECT edge_confidence, ruled_out_by_scip FROM call_edges")
            .unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            rows.iter()
                .all(|(conf, ruled_out)| conf == "ambiguous" && *ruled_out == 1)
        );
    }

    /// A fan-out group with no SCIP evidence at all for that call site (e.g.
    /// unreachable in the SCIP dump, or `rust-analyzer` itself couldn't
    /// type-check that expression) is left completely untouched — absence of
    /// evidence is not evidence of wrongness.
    #[test]
    fn fan_out_untouched_when_scip_has_no_reference_at_the_site() {
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        let occ = vec![]; // no SCIP occurrences at all
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 0);
        let ruled_out: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE ruled_out_by_scip = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ruled_out, 0);
    }

    fn db_with_call_site(
        from_path: &str,
        enclosing_qn: &str,
        call_line: i64,
        callee_name: &str,
    ) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO call_sites (from_path, enclosing_qn, callee_name, call_line) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![from_path, enclosing_qn, callee_name, call_line],
        )
        .unwrap();
        conn
    }

    /// The MAX_CALLEE_CANDIDATES-cap gap gated-insert exists for: a real
    /// syntactic call (`call_sites` row) whose candidate selection dropped it
    /// entirely — `rebuild_graph` produced ZERO `call_edges` rows for it, so
    /// the upgrade pass above has nothing to touch. SCIP's exact evidence
    /// should still be enough to insert the correct edge from scratch.
    #[test]
    fn inserts_edge_for_uncandidated_call_site() {
        let conn = db_with_call_site("app/src/main.rs", "app/src/main.rs::main", 5, "start");
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust', \
                     'core/src/engine.rs', 6, 8)",
            [],
        )
        .unwrap();
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.match_rate, 1.0);
        let (from_symbol, to_symbol, confidence, formal_source): (
            String,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT from_symbol, to_symbol, edge_confidence, formal_source FROM call_edges",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(from_symbol, "app/src/main.rs::main");
        assert_eq!(to_symbol, "core/src/engine.rs::Engine::start");
        assert_eq!(confidence, "formal");
        assert_eq!(formal_source.as_deref(), Some("scip"));
    }

    #[test]
    // B2 (2026-07-28 benchmark root-cause): a stale non-formal edge at a call
    // site is a LONE occupant there (only one call_edges row) at the time
    // `mark_ruled_out_siblings` runs, since that pass runs BEFORE this
    // function — `lone_edge_is_never_ruled_out_even_when_scip_disagrees`
    // above locks in that a lone edge is deliberately left alone by that
    // earlier pass. If SCIP then resolves the site to a genuinely different,
    // previously-unrepresented target, this function inserts the correct
    // formal edge but — before this fix — left the stale wrong edge at
    // `ruled_out_by_scip = 0`, still served by every caller/callee/
    // edit_context query. Measured live on 4 real OSS repos: 15-437 such
    // edges per repo, 100% pointing to a target other than their formal
    // sibling.
    fn insert_replaces_stale_wrong_sibling_edge_that_rule_out_missed() {
        let conn = db_with_call_site("app/src/main.rs", "app/src/main.rs::main", 5, "start");
        conn.execute_batch(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust', 'core/src/engine.rs', 6, 8); \
             INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('other/src/wrong.rs::Wrong::start', 'start', 'method', 'rust', 'other/src/wrong.rs', 3, 5); \
             INSERT INTO call_edges (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path) \
             VALUES ('app/src/main.rs::main', 'other/src/wrong.rs::Wrong::start', 5, 'ambiguous', 'app/src/main.rs', 'other/src/wrong.rs');",
        )
        .unwrap();

        // SCIP resolves the call site to the CORRECT target — a def with no
        // pre-existing edge representing it at all, exactly the shape
        // `insert_missing_edges` handles.
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            stats.inserted, 1,
            "correct target should get a new formal edge"
        );

        let (confidence, ruled_out): (String, i64) = conn
            .query_row(
                "SELECT edge_confidence, ruled_out_by_scip FROM call_edges \
                 WHERE to_symbol = 'other/src/wrong.rs::Wrong::start'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            confidence, "ambiguous",
            "edge_confidence itself is left untouched"
        );
        assert_eq!(
            ruled_out, 1,
            "stale wrong edge must be ruled out now that a formal replacement exists at the same site"
        );

        let (to_symbol, new_confidence, new_ruled_out): (String, String, i64) = conn
            .query_row(
                "SELECT to_symbol, edge_confidence, ruled_out_by_scip FROM call_edges \
                 WHERE to_symbol = 'core/src/engine.rs::Engine::start'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(to_symbol, "core/src/engine.rs::Engine::start");
        assert_eq!(new_confidence, "formal");
        assert_eq!(new_ruled_out, 0);
    }

    /// `insert_missing: false` (the config off-switch) skips the insert gate
    /// entirely, even when every other condition for a successful insert
    /// (real call_sites row, uniquely-resolved def symbol) is met.
    #[test]
    fn insert_missing_false_skips_the_insert_gate_entirely() {
        let conn = db_with_call_site("app/src/main.rs", "app/src/main.rs::main", 5, "start");
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust', \
                     'core/src/engine.rs', 6, 8)",
            [],
        )
        .unwrap();
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, false).unwrap();
        assert_eq!(stats.inserted, 0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    /// SCIP resolves the call site's reference just fine, but the definition
    /// site doesn't correspond to any known `symbols` row (e.g. a file CALM's
    /// own indexer doesn't parse, or a stale/mismatched path) — there's
    /// nothing to name the new edge's `to_symbol` after, so no insert happens
    /// rather than guessing or inventing a placeholder.
    #[test]
    fn stale_proof_context_is_rejected_before_ingest() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "UPDATE graph_generation_state SET generation = 1 WHERE id = 1",
            [],
        )
        .unwrap();

        let context = super::ExternalProofContext::new("scip:test", "provider", "context")
            .at_graph_generation(0);
        let stats =
            super::ingest_occurrences_with_proof_context(&conn, &[], true, Some(&context)).unwrap();
        // PATTERN-DEBT scip-cache-commits-discarded-generation: a discarded
        // pass must be tagged, not silently indistinguishable from a clean
        // "ran and genuinely found nothing" pass -- callers rely on this to
        // decide whether it's safe to persist a cache key.
        assert!(
            stats.discarded_stale_generation,
            "a graph-generation mismatch must set discarded_stale_generation"
        );
        assert_eq!(
            stats,
            super::IngestStats {
                discarded_stale_generation: true,
                ..Default::default()
            },
            "every other field must still be zero -- no DB row may be touched when the \
             generation fence rejects the pass"
        );
    }

    #[test]
    fn no_insert_when_def_unknown_symbol() {
        let conn = db_with_call_site("app/src/main.rs", "app/src/main.rs::main", 5, "start");
        // No `symbols` row at core/src/engine.rs:6 at all.
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.match_rate, 0.0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    /// SCIP resolves the reference and the definition maps to a real
    /// `symbols` row, but there's no `call_sites` row at all for that exact
    /// `(from_path, call_line)` — tree-sitter never recorded this as a call in
    /// the first place (e.g. it's a type reference or field access SCIP
    /// indexed but isn't a call expression at all). Without a `call_sites`
    /// row to name the enclosing symbol from, no edge is fabricated.
    #[test]
    fn no_insert_when_enclosing_missing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust', \
                     'core/src/engine.rs', 6, 8)",
            [],
        )
        .unwrap();
        // No call_sites row at app/src/main.rs:5 at all.
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.inserted, 0);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    /// An ambiguous fan-out where one candidate already got marked `formal`
    /// by the (weaker, per-file name-set) `stack_graphs` heuristic on a
    /// previous index run — but it's actually the wrong target for this call
    /// site. SCIP's exact (file,line) evidence names a *different* sibling as
    /// the real target: that sibling gets upgraded to `formal`/`'scip'`, and
    /// the stale `stack_graphs` pick gets `ruled_out_by_scip` (its
    /// `edge_confidence` itself is never downgraded — ADR-0004 §3).
    /// Serializes every test that asserts an exact before/after delta on the
    /// process-global `SCIP_STACK_GRAPHS_OVERRIDES` counter (D3, 2026-07-30
    /// stack-graphs-demotion-lever) -- cargo test's default parallel
    /// execution would otherwise let this test and the 4 counter-specific
    /// ones below interleave, since ALL of them exercise
    /// `mark_ruled_out_siblings`/`ingest_occurrences`'s upgrade loop against
    /// the SAME real static. A `>= before + 1` assertion would dodge this
    /// but weaken the exact-count guarantee those tests exist to enforce
    /// (catching double-counting, the bug audit-design found in the
    /// original spec draft) -- serializing is the correct fix, not a looser
    /// assertion. `unwrap_or_else` recovers from lock poisoning so one
    /// failing test in the group doesn't cascade-fail the rest.
    static COUNTER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn p0_observation_only_leaves_scip_proof_out_of_call_graph() {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let overrides_before = scip_stack_graphs_override_count();
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        conn.execute(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'stack_graphs' \
             WHERE to_symbol = 'b.rs::B::as_str'",
            [],
        )
        .unwrap();
        let occ = vec![
            ScipOccurrence {
                file: "a.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];

        let exact_occurrences = production_occurrences(&occ);
        let stats = super::ingest_occurrences(&conn, &exact_occurrences, true).unwrap();
        assert_eq!(stats.upgraded, 0);
        assert_eq!(stats.ruled_out, 0);
        assert_eq!(stats.inserted, 0);
        assert_eq!(scip_stack_graphs_override_count(), overrides_before);
        let rows: Vec<(String, String, Option<String>, i64)> = conn
            .prepare(
                "SELECT to_symbol, edge_confidence, formal_source, ruled_out_by_scip \
                 FROM call_edges ORDER BY to_symbol",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a.rs::A::as_str".into(), "ambiguous".into(), None, 0),
                (
                    "b.rs::B::as_str".into(),
                    "formal".into(),
                    Some("stack_graphs".into()),
                    0,
                ),
            ]
        );

        let insertion_conn =
            db_with_call_site("app/src/main.rs", "app/src/main.rs::main", 5, "start");
        insertion_conn
            .execute(
                "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
                 VALUES ('core/src/engine.rs::Engine::start', 'start', 'method', 'rust', \
                         'core/src/engine.rs', 6, 8)",
                [],
            )
            .unwrap();
        let insert_occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "I".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "I".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let exact_insert_occurrences = production_occurrences(&insert_occ);
        let insert_stats =
            super::ingest_occurrences(&insertion_conn, &exact_insert_occurrences, true).unwrap();
        assert_eq!(insert_stats.inserted, 0);
        let edge_count: i64 = insertion_conn
            .query_row("SELECT COUNT(*) FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(edge_count, 0);
    }

    #[test]
    fn scip_overrides_stack_graphs_target() {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        conn.execute(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'stack_graphs' \
             WHERE to_symbol = 'b.rs::B::as_str'",
            [],
        )
        .unwrap();
        // SCIP's exact evidence says the real target is a.rs::A::as_str.
        let occ = vec![
            ScipOccurrence {
                file: "a.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            stats.upgraded, 1,
            "a.rs::A::as_str is a real tier change (ambiguous -> formal)"
        );
        assert_eq!(
            stats.ruled_out, 1,
            "the stale stack_graphs pick is ruled out, not downgraded"
        );
        let mut stmt = conn
            .prepare(
                "SELECT to_symbol, edge_confidence, formal_source, ruled_out_by_scip \
                 FROM call_edges ORDER BY to_symbol",
            )
            .unwrap();
        let rows: Vec<(String, String, Option<String>, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "a.rs::A::as_str".to_string(),
                    "formal".to_string(),
                    Some("scip".to_string()),
                    0
                ),
                (
                    "b.rs::B::as_str".to_string(),
                    "formal".to_string(),
                    Some("stack_graphs".to_string()),
                    1
                ),
            ]
        );
    }

    /// D3 positive case (upgrade-loop path): a `formal`/`'stack_graphs'`
    /// edge that SCIP reconfirms/overrides to `'scip'` MUST increment the
    /// counter -- this is a real disagreement between the two formal-tier
    /// sources.
    #[test]
    fn scip_stack_graphs_override_counter_increments_on_real_override() {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = db_with_one_textual_edge();
        conn.execute(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'stack_graphs'",
            [],
        )
        .unwrap();
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let before = scip_stack_graphs_override_count();
        ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            scip_stack_graphs_override_count(),
            before + 1,
            "SCIP overriding a stack_graphs verdict must count as exactly 1 disagreement"
        );
        let formal_source: Option<String> = conn
            .query_row("SELECT formal_source FROM call_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(formal_source.as_deref(), Some("scip"));
    }

    /// D3 negative case (upgrade-loop path, catches the exact bug audit-design
    /// found in the original spec draft): a fresh upgrade from `textual` --
    /// stack-graphs never had an opinion on this edge -- must NOT increment
    /// the counter. Without this test, an implementation that increments on
    /// every `to_upgrade` push (not filtered by prior `formal_source`) would
    /// still pass every other test while overcounting in production.
    #[test]
    fn scip_stack_graphs_override_counter_unchanged_on_fresh_upgrade() {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = db_with_one_textual_edge();
        let occ = vec![
            ScipOccurrence {
                file: "core/src/engine.rs".into(),
                line: 6,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let before = scip_stack_graphs_override_count();
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(stats.upgraded, 1, "sanity: the edge really did upgrade");
        assert_eq!(
            scip_stack_graphs_override_count(),
            before,
            "a textual -> formal/scip upgrade has nothing to do with stack-graphs and must not count"
        );
    }

    /// D3 positive case (ruled-out-siblings path): a `formal`/`'stack_graphs'`
    /// sibling losing a fan-out to SCIP's exact evidence MUST increment the
    /// counter -- same fixture as `scip_overrides_stack_graphs_target` above.
    #[test]
    fn scip_stack_graphs_override_counter_increments_on_ruled_out_sibling() {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        conn.execute(
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'stack_graphs' \
             WHERE to_symbol = 'b.rs::B::as_str'",
            [],
        )
        .unwrap();
        let occ = vec![
            ScipOccurrence {
                file: "a.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let before = scip_stack_graphs_override_count();
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            stats.ruled_out, 1,
            "sanity: the stale stack_graphs pick really was ruled out"
        );
        assert_eq!(
            scip_stack_graphs_override_count(),
            before + 1,
            "ruling out a stack_graphs-sourced sibling is a real disagreement"
        );
    }

    /// D3 negative case (ruled-out-siblings path): a fan-out group where the
    /// losing sibling was plain `ambiguous` (never `stack_graphs`-formal) must
    /// NOT increment the counter -- losing a fan-out to a stronger candidate
    /// is unrelated to stack-graphs when stack-graphs never had a verdict on
    /// that sibling in the first place.
    #[test]
    fn scip_stack_graphs_override_counter_unchanged_when_ruled_out_sibling_was_never_stack_graphs()
    {
        let _guard = COUNTER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = db_with_ambiguous_fan_out(&[
            ("a.rs::A::as_str", "a.rs", 1),
            ("b.rs::B::as_str", "b.rs", 1),
        ]);
        // Both siblings start plain `ambiguous` -- neither was ever formal.
        let occ = vec![
            ScipOccurrence {
                file: "a.rs".into(),
                line: 1,
                symbol: "M".into(),
                is_def: true,
                is_local: false,
            },
            ScipOccurrence {
                file: "app/src/main.rs".into(),
                line: 5,
                symbol: "M".into(),
                is_def: false,
                is_local: false,
            },
        ];
        let before = scip_stack_graphs_override_count();
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
        assert_eq!(
            stats.ruled_out, 1,
            "sanity: b.rs::B::as_str still loses the fan-out"
        );
        assert_eq!(
            scip_stack_graphs_override_count(),
            before,
            "the ruled-out sibling was never stack_graphs-formal, so this isn't a disagreement"
        );
    }
}
