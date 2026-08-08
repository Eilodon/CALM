//! Cross-file resolution for Tier 1 type relations (P4,
//! docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md).
//!
//! `indexer::semantic_facts::extract_type_relations_from_tree` (called from
//! `indexer::pipeline::extract_file_data`, per-file, at reparse time) only
//! ever resolves `to_symbol` for a target defined in the SAME file --
//! deliberately, since a single file's parse has no visibility into the rest
//! of the repo. This module is the graph-wide follow-up pass: given every
//! `type_relations` row extraction left unresolved (`to_symbol IS NULL`),
//! look up its `target_text` against the WHOLE repo's symbol table and
//! upgrade it when the match is unambiguous.
//!
//! Deliberately does NOT reuse `indexer::pipeline`'s private `ResolutionCtx`
//! (built for call-edge resolution, with call-specific fields like
//! `by_name_class`/arity/`caller_usings`) -- this module builds its own
//! minimal name index directly from `symbols`, matching how every other
//! `graph::` module (`coreness`, `digest`, `package_deps`) is self-contained
//! and takes only a `Connection`, not indexer-internal state. A few dozen
//! lines of index-building duplicated across two call sites is a small,
//! honest cost for keeping the indexer/graph boundary this codebase already
//! enforces elsewhere.
//!
//! **Resolution ladder (v1 -- see the plan's P4 section for what's deferred):**
//! 1. Same-file (unchanged, still owned by `extract_file_data`).
//! 2. Cross-file, same source language, EXACTLY ONE bare-name match anywhere
//!    in the repo -- promoted to `confidence = 'resolved'`.
//! 3. Zero or multiple candidates -- left exactly as extraction set it
//!    (`to_symbol` NULL, `confidence = 'textual'`). Never guessed. A
//!    same-language multi-candidate case (two classes named `Handler` in
//!    different packages, say) stays textual rather than picking one --
//!    narrowing via the referencing file's own imports is deferred (would
//!    need the same import-alias machinery call-edge resolution already
//!    has, reused carefully to avoid the boundary problem above).
//!
//! Runs on every full and incremental graph rebuild (`indexer::pipeline`'s
//! `rebuild_graph`/`incremental_graph_update`), so a row that was ambiguous
//! or unresolved when a class was added out of order self-heals on the next
//! rebuild once its target exists unambiguously -- the same self-healing
//! property `compute_digests`/`compute_package_dependencies` already have.
//!
//! **Deferred from the full P4 spec (see the plan doc for the reasoning):**
//! a physical `type_relation_sites`/`type_relation_edges` table split (this
//! single-table, lifecycle-differentiated-columns design gets the same
//! functional separation more cheaply, matching how `symbols`/`call_edges`
//! already mix indexer- and graph-owned columns in one table); a full
//! `TypeRef` struct (reduced to the `lookup_name` helper below for v1); a
//! SCIP-overlay resolution rung; import-alias/namespace disambiguation for
//! the multi-candidate case; and `reference_impact` integration.

use rusqlite::Connection;
use std::collections::HashMap;

/// Strips generic type arguments (`Base<T>` -> `Base`) and a qualifier
/// prefix (`pkg.Base` -> `Base`) from a raw `type_relations.target_text`,
/// leaving the bare name a repo-wide symbol lookup can match against.
/// Never fabricates a value: if `target_text` is already bare, it's
/// returned unchanged.
fn lookup_name(target_text: &str) -> &str {
    let without_generics = target_text.split('<').next().unwrap_or(target_text).trim();
    without_generics
        .rsplit('.')
        .next()
        .unwrap_or(without_generics)
}

/// The graph-wide follow-up pass described in the module doc comment.
/// Idempotent and safe to call on every rebuild: only touches rows where
/// `to_symbol IS NULL`, so an already-resolved (same-file or a prior run of
/// this same pass) row is never re-examined or downgraded.
pub fn resolve_cross_file_type_relations(conn: &Connection) -> rusqlite::Result<()> {
    let mut by_name_lang: HashMap<(String, String), Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT name, qualified_name, language FROM symbols")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            let (name, qn, language) = row;
            by_name_lang.entry((name, language)).or_default().push(qn);
        }
    }

    let unresolved: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT tr.id, tr.target_text, s.language \
             FROM type_relations tr \
             JOIN symbols s ON s.qualified_name = tr.from_symbol \
             WHERE tr.to_symbol IS NULL",
        )?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut update = conn.prepare(
        "UPDATE type_relations SET to_symbol = ?1, confidence = 'resolved' WHERE id = ?2",
    )?;
    for (id, target_text, language) in unresolved {
        let name = lookup_name(&target_text);
        let Some(candidates) = by_name_lang.get(&(name.to_string(), language)) else {
            continue;
        };
        if let [only] = candidates.as_slice() {
            update.execute(rusqlite::params![only, id])?;
        }
        // 0 or >1 candidates: leave as extraction set it (NULL / 'textual') -- never guessed.
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_name_strips_generics_and_qualifier() {
        assert_eq!(lookup_name("Base"), "Base");
        assert_eq!(lookup_name("pkg.Base"), "Base");
        assert_eq!(lookup_name("Base<T>"), "Base");
        assert_eq!(lookup_name("pkg.Base<T>"), "Base");
        assert_eq!(lookup_name("a.b.Base"), "Base");
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn
    }

    fn insert_symbol(conn: &Connection, qn: &str, name: &str, path: &str, language: &str) {
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES (?1, ?2, 'class', ?3, ?4, 1, 1)",
            rusqlite::params![qn, name, language, path],
        )
        .unwrap();
    }

    fn insert_relation(conn: &Connection, from_symbol: &str, target_text: &str, path: &str) {
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, confidence, source_path, line) \
             VALUES (?1, 'extends', ?2, 'textual', ?3, 1)",
            rusqlite::params![from_symbol, target_text, path],
        )
        .unwrap();
    }

    fn to_symbol_and_confidence(conn: &Connection, from_symbol: &str) -> (Option<String>, String) {
        conn.query_row(
            "SELECT to_symbol, confidence FROM type_relations WHERE from_symbol = ?1",
            [from_symbol],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn resolves_unique_cross_file_same_language_match() {
        let conn = setup_db();
        insert_symbol(&conn, "base.py::Base", "Base", "base.py", "python");
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Base", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(to_symbol.as_deref(), Some("base.py::Base"));
        assert_eq!(confidence, "resolved");
    }

    #[test]
    fn stays_textual_when_multiple_same_language_candidates_exist() {
        let conn = setup_db();
        insert_symbol(&conn, "a.py::Handler", "Handler", "a.py", "python");
        insert_symbol(&conn, "b.py::Handler", "Handler", "b.py", "python");
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Handler", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(to_symbol, None, "ambiguous match must never be guessed");
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn stays_textual_when_target_is_a_different_language() {
        let conn = setup_db();
        // A same-named class exists, but in a DIFFERENT language -- must
        // never cross-attribute (e.g. a Python Base and an unrelated Java
        // Base sharing a name is a coincidence, not the same type).
        insert_symbol(&conn, "Base.java::Base", "Base", "Base.java", "java");
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Base", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(to_symbol, None);
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn resolves_generic_and_qualified_target_text() {
        let conn = setup_db();
        insert_symbol(
            &conn,
            "base.java::Repository",
            "Repository",
            "base.java",
            "java",
        );
        insert_symbol(
            &conn,
            "derived.java::Derived",
            "Derived",
            "derived.java",
            "java",
        );
        insert_relation(
            &conn,
            "derived.java::Derived",
            "pkg.Repository<Foo>",
            "derived.java",
        );

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.java::Derived");
        assert_eq!(to_symbol.as_deref(), Some("base.java::Repository"));
        assert_eq!(confidence, "resolved");
    }

    #[test]
    fn already_resolved_same_file_row_is_never_touched() {
        let conn = setup_db();
        insert_symbol(&conn, "a.py::Base", "Base", "a.py", "python");
        insert_symbol(&conn, "a.py::Derived", "Derived", "a.py", "python");
        // Simulates extraction's own same-file resolution: to_symbol already set.
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, to_symbol, confidence, source_path, line) \
             VALUES ('a.py::Derived', 'extends', 'Base', 'a.py::Base', 'resolved', 'a.py', 1)",
            [],
        )
        .unwrap();
        // A same-named decoy elsewhere, in the same language -- if this pass
        // incorrectly re-examined already-resolved rows, it would still
        // resolve correctly here by luck (unique match); the real point of
        // this test is that it doesn't even query rows with to_symbol set,
        // which the next assertion on an ambiguous decoy setup proves.
        insert_symbol(
            &conn,
            "elsewhere.py::Base",
            "Base",
            "elsewhere.py",
            "python",
        );

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "a.py::Derived");
        assert_eq!(
            to_symbol.as_deref(),
            Some("a.py::Base"),
            "same-file resolution must be left exactly as extraction set it"
        );
        assert_eq!(confidence, "resolved");
    }

    #[test]
    fn self_heals_on_a_later_rebuild_once_the_target_exists() {
        let conn = setup_db();
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Base", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();
        let (to_symbol, _) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol, None,
            "target doesn't exist yet -- must stay unresolved"
        );

        // Base is added in a later file/rebuild.
        insert_symbol(&conn, "base.py::Base", "Base", "base.py", "python");
        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol.as_deref(),
            Some("base.py::Base"),
            "a subsequent rebuild must resolve the row once its target exists"
        );
        assert_eq!(confidence, "resolved");
    }
}
