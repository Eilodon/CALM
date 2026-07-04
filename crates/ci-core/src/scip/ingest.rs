//! Upgrade existing Rust call edges to `formal` confidence using SCIP evidence.
//! ADDITIVE ONLY: never inserts, deletes, or downgrades an `edge_confidence`
//! rank (ADR-0004 §3). A second, orthogonal marking pass (`ruled_out_by_scip`)
//! also runs here — it never touches `edge_confidence` either, only a separate
//! boolean column, so the ADR-0004 §3 invariant (existing rank is never
//! downgraded) holds for both passes; see that column's doc comment in
//! `db/schema.rs` for what it means and how query-side consumes it.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use super::parse::ScipOccurrence;

/// Outcome of one `ingest_occurrences` pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestStats {
    /// Edges upgraded to `formal` confidence this run (SCIP confirmed this
    /// exact `(from_path, call_site_line) -> to_symbol` edge).
    pub upgraded: usize,
    /// Ambiguous fan-out siblings marked `ruled_out_by_scip` this run — SCIP
    /// decisively answered "what does this call site resolve to" and it
    /// wasn't this candidate. See `mark_ruled_out_siblings`.
    pub ruled_out: usize,
}

/// Match SCIP occurrences against existing call edges and upgrade the confidence
/// of each corroborated edge to `formal`, then run `mark_ruled_out_siblings`.
///
/// Matching (conservative): a call edge `(from_path, call_site_line) -> to_symbol`
/// is corroborated when there is a non-local SCIP reference at
/// `(from_path, call_site_line)` whose definition occurrence lands on the same
/// file+line as `to_symbol`'s declaration.
pub fn ingest_occurrences(
    conn: &Connection,
    occ: &[ScipOccurrence],
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
    // site — any confidence, including already-`formal`, since the
    // ruled-out pass below needs to see a `formal` sibling even when it was
    // upgraded on a previous run (and so is no longer itself a candidate).
    let rows: Vec<(i64, String, i64, String, i64, String, bool)> = {
        let mut stmt = conn.prepare(
            "SELECT ce.id, ce.from_path, ce.call_site_line, s.path, s.line_start, \
                    ce.edge_confidence, ce.ruled_out_by_scip \
             FROM call_edges ce \
             JOIN symbols s ON s.qualified_name = ce.to_symbol \
             WHERE ce.call_site_line IS NOT NULL \
               AND ce.from_path IS NOT NULL",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)? != 0,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut to_upgrade: Vec<i64> = Vec::new();
    for (id, from_path, call_line, def_path, def_line, confidence, _) in &rows {
        if confidence == "formal" {
            continue;
        }
        let key = (from_path.as_str(), *call_line as usize);
        if let Some(targets) = ref_targets.get(&key)
            && targets
                .iter()
                .any(|(f, l)| *f == def_path.as_str() && *l == *def_line as usize)
        {
            to_upgrade.push(*id);
        }
    }
    {
        let mut stmt =
            conn.prepare("UPDATE call_edges SET edge_confidence = 'formal' WHERE id = ?1")?;
        for id in &to_upgrade {
            stmt.execute([id])?;
        }
    }

    let upgraded_this_run: HashSet<i64> = to_upgrade.iter().copied().collect();
    let to_rule_out = mark_ruled_out_siblings(conn, &rows, &ref_targets, &upgraded_this_run)?;

    Ok(IngestStats {
        upgraded: to_upgrade.len(),
        ruled_out: to_rule_out,
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
/// One fan-out group member: `(id, def_path, def_line, is_formal, already_ruled_out)`.
type GroupMember<'a> = (i64, &'a str, i64, bool, bool);

fn mark_ruled_out_siblings(
    conn: &Connection,
    rows: &[(i64, String, i64, String, i64, String, bool)],
    ref_targets: &HashMap<(&str, usize), Vec<(&str, usize)>>,
    upgraded_this_run: &HashSet<i64>,
) -> rusqlite::Result<usize> {
    let mut groups: HashMap<(&str, i64), Vec<GroupMember>> = HashMap::new();
    for (id, from_path, call_line, def_path, def_line, confidence, ruled_out) in rows {
        let is_formal = confidence == "formal" || upgraded_this_run.contains(id);
        groups
            .entry((from_path.as_str(), *call_line))
            .or_default()
            .push((*id, def_path.as_str(), *def_line, is_formal, *ruled_out));
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
        let stats = ingest_occurrences(&conn, &occ).unwrap();
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
}
