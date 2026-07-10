//! Upgrade existing Rust call edges to `formal` confidence using SCIP evidence,
//! and (gated) insert new ones for a call site tree-sitter's own candidate
//! selection never produced any row for at all. ADDITIVE in the ADR-0004 §3
//! sense that follows: an existing `edge_confidence` rank is never
//! downgraded, and `mark_ruled_out_siblings`'s `ruled_out_by_scip` flag is an
//! orthogonal, separate column that never touches it either — see that
//! column's doc comment in `db/schema.rs`. The one place this module
//! deliberately overrides a *prior verdict* rather than only adding to it:
//! `formal_source` (also documented in `db/schema.rs`) lets an exact
//! (file,line) SCIP match override a `formal` edge that got there via the
//! weaker, per-file-name-set `stack_graphs` heuristic — never one that got
//! there via a prior SCIP match of its own.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::parse::ScipOccurrence;

/// Outcome of one `ingest_occurrences` pass.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct IngestStats {
    /// Edges whose `edge_confidence` actually changed to `formal` this run
    /// (SCIP confirmed this exact `(from_path, call_site_line) -> to_symbol`
    /// edge). Does not count a `formal`-already edge merely reconfirmed with
    /// `formal_source = 'scip'` (no confidence-tier change) — see
    /// `formal_source`'s doc comment in `db/schema.rs`.
    pub upgraded: usize,
    /// Ambiguous fan-out siblings marked `ruled_out_by_scip` this run — SCIP
    /// decisively answered "what does this call site resolve to" and it
    /// wasn't this candidate. See `mark_ruled_out_siblings`.
    pub ruled_out: usize,
    /// New `call_edges` rows inserted this run for a call site SCIP resolved
    /// but that had no existing row representing that exact target at all —
    /// gated by `insert_missing` (config: `rust.scip.insert_missing`,
    /// default auto-on). See `insert_missing_edges`.
    pub inserted: usize,
    /// Fraction (0.0-1.0) of distinct SCIP-resolved call sites — a
    /// `(from_path, call_line)` with at least one non-local reference whose
    /// definition this same dump also recorded — that ended this run
    /// represented by at least one `formal` `call_edges` row (already
    /// formal, upgraded, or newly inserted). `0.0` when there were no
    /// SCIP-resolved call sites at all to measure against. A low value on an
    /// otherwise-healthy `.scip` file is the diagnostic signal the 8-language
    /// plan's risk list calls out: paths likely aren't rebased correctly for
    /// wherever this indexer actually ran (see `parse::parse_index`'s
    /// `rebase_prefix`).
    pub match_rate: f64,
}

/// One existing `call_edges` row with a known call site, joined to its
/// target's declaration site.
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

/// Match SCIP occurrences against existing call edges and upgrade the
/// confidence of each corroborated edge to `formal` (or, for one already
/// `formal` via `stack_graphs`, reconfirm its provenance to `'scip'`),
/// gated-insert new edges for SCIP-resolved call sites with no existing row
/// for that exact target (see `insert_missing_edges`), then run
/// `mark_ruled_out_siblings`.
///
/// Matching (conservative): a call edge `(from_path, call_site_line) -> to_symbol`
/// is corroborated when there is a non-local SCIP reference at
/// `(from_path, call_site_line)` whose definition occurrence lands on the same
/// file+line as `to_symbol`'s declaration.
pub fn ingest_occurrences(
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
type GroupMember<'a> = (i64, &'a str, i64, bool, bool);

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
        groups
            .entry((row.from_path.as_str(), row.call_line))
            .or_default()
            .push((
                row.id,
                row.def_path.as_str(),
                row.def_line,
                is_formal,
                row.ruled_out,
            ));
    }

    let mut to_rule_out: Vec<i64> = Vec::new();
    for ((from_path, call_line), members) in &groups {
        if members.len() < 2 {
            continue; // not a fan-out group — nothing to declutter
        }
        let has_formal_member = members.iter().any(|(.., is_formal, _)| *is_formal);
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
        for (id, _, _, is_formal, already_ruled_out) in members {
            if !*is_formal && !*already_ruled_out {
                to_rule_out.push(*id);
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
    let mut insert_stmt = conn.prepare(
        "INSERT INTO call_edges \
            (from_symbol, to_symbol, call_site_line, edge_confidence, from_path, to_path, \
             formal_source, ruled_out_by_scip) \
         VALUES (?1, ?2, ?3, 'formal', ?4, ?5, 'scip', 0)",
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
            insert_stmt.execute(rusqlite::params![
                enc_qn,
                to_qn,
                call_line as i64,
                from_path,
                def_path,
            ])?;
            already_represented.insert((from_path, call_line, def_path, def_line as i64));
            satisfied_sites.insert((from_path.to_string(), call_line));
            inserted += 1;
        }
    }
    Ok(inserted)
}

/// The single `call_sites.enclosing_qn` recorded for a real syntactic call at
/// `(path, line)`, or `None` when there's no such call site at all, or more
/// than one *distinct* enclosing symbol claims that exact line (shouldn't
/// happen for well-formed source — "ambiguous, skip" beats guessing).
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
        let stats = ingest_occurrences(&conn, &occ, true).unwrap();
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
    #[test]
    fn scip_overrides_stack_graphs_target() {
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
}
