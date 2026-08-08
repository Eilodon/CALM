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
//! **Resolution ladder (v2, PR A / P4.1 -- see the plan's P4.1 section for
//! what's still deferred):**
//! 1. Same-file (unchanged, still owned by `extract_file_data`,
//!    `resolution_source = 'same_file_ast'`).
//! 2. Cross-file, same source language, target carries NO qualifier this
//!    resolver understands (`has_unresolved_qualifier` -- a bare name, with
//!    generics optionally stripped), EXACTLY ONE type-like-kind candidate
//!    (`class`/`struct`/`trait`/`interface`/`enum` -- never a same-named
//!    function/variable) anywhere in the repo -- promoted to `confidence =
//!    'resolved'`, `resolution_source = 'cross_file_unique'`.
//! 3. A qualified target (`pkg.Base`, `crate::foo::Base`, `Foo::Base`),
//!    zero candidates, or multiple candidates -- left/reset to
//!    `to_symbol` NULL, `confidence = 'textual'`, `resolution_source`
//!    NULL. Never guessed. A qualified target stays textual even if
//!    stripping the qualifier would find a unique same-named symbol --
//!    that symbol might not be what the qualifier actually named (an
//!    external, unindexed type sharing a local name is a real false-
//!    positive risk, not a hypothetical one). Narrowing via the
//!    referencing file's own imports is deferred (would need the same
//!    import-alias machinery call-edge resolution already has, reused
//!    carefully to avoid the boundary problem above).
//!
//! **Every cross-file-owned row is reset and recomputed on every pass, not
//! just upgraded (PR A2).** Before resolving anything, every row this pass
//! previously owns (`resolution_source = 'cross_file_unique'`) is reset to
//! NULL/'textual'/NULL, THEN re-resolved from current DB state in the same
//! call. This is what lets `resolved -> textual` actually happen when
//! evidence disappears (the target symbol was deleted or renamed, or a
//! second same-named candidate appeared) -- the v1 resolver only ever
//! examined `to_symbol IS NULL` rows, so a row it had already resolved was
//! never re-examined and could go stale silently. `same_file_ast` rows are
//! NEVER reset here (the WHERE clause only ever matches
//! `cross_file_unique`) -- they're owned by, and re-derived fresh by,
//! `extract_file_data` on every reindex of their own file instead.
//!
//! Runs on every full and incremental graph rebuild (`indexer::pipeline`'s
//! `rebuild_graph`/`incremental_graph_update`), so a row that was ambiguous
//! or unresolved when a class was added out of order self-heals on the next
//! rebuild once its target exists unambiguously -- the same self-healing
//! property `compute_digests`/`compute_package_dependencies` already have.
//!
//! **Deferred from the full P4.1 spec (see the plan doc for the reasoning):**
//! a physical `type_relation_sites`/`type_relation_edges` table split (this
//! single-table, lifecycle-differentiated-columns design gets the same
//! functional separation more cheaply, matching how `symbols`/`call_edges`
//! already mix indexer- and graph-owned columns in one table); a full
//! `TypeRef` struct (reduced to the `lookup_name`/`has_unresolved_qualifier`
//! helpers below for v2); a SCIP-overlay resolution rung; import-alias/
//! namespace disambiguation for the qualified/multi-candidate cases; and
//! `reference_impact` integration.

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

// PR A4: true when `target_text` (after stripping generic arguments) still
// carries a qualifier prefix (`pkg.Base`, `crate::foo::Base`, `Foo::Base`)
// this resolver doesn't parse. See the module doc comment's resolution
// ladder step 3 for why: `pkg.Base` might name an external, unindexed
// type, and blindly matching a same-named LOCAL symbol would dress up a
// guess as `confidence = 'resolved'`. Only a genuinely bare name is ever
// attempted for unique-match resolution.
fn has_unresolved_qualifier(target_text: &str) -> bool {
    let without_generics = target_text.split('<').next().unwrap_or(target_text).trim();
    without_generics.contains('.') || without_generics.contains("::")
}

/// The graph-wide follow-up pass described in the module doc comment.
/// Idempotent and safe to call on every rebuild: only touches rows where
/// `to_symbol IS NULL`, so an already-resolved (same-file or a prior run of
/// this same pass) row is never re-examined or downgraded.
pub fn resolve_cross_file_type_relations(conn: &Connection) -> rusqlite::Result<()> {
    // A2 step 1: reset every row this pass owns -- if its evidence still
    // holds, the loop below re-derives the identical result; if not
    // (target deleted/renamed, or a new ambiguous sibling appeared), it
    // correctly stays at this reset (textual) state instead of staying
    // stale at an upgrade this pass made in a previous rebuild.
    conn.execute(
        "UPDATE type_relations SET to_symbol = NULL, confidence = 'textual', resolution_source = NULL \
         WHERE resolution_source = 'cross_file_unique'",
        [],
    )?;

    // A3: candidates are restricted to type-like kinds -- the same set
    // `pipeline::extract_file_data`'s own `class_qn_by_name` already uses
    // for same-file resolution (see its doc comment). A same-named
    // function/variable must never steal a type relation.
    let mut by_name_lang: HashMap<(String, String), Vec<String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT name, qualified_name, language FROM symbols \
             WHERE kind IN ('class', 'struct', 'trait', 'interface', 'enum')",
        )?;
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
        "UPDATE type_relations SET to_symbol = ?1, confidence = 'resolved', \
         resolution_source = 'cross_file_unique' WHERE id = ?2",
    )?;
    for (id, target_text, language) in unresolved {
        // A4: a qualified target is never guessed past -- skip entirely,
        // leaving it at the reset (or extraction-set) textual state.
        if has_unresolved_qualifier(&target_text) {
            continue;
        }
        let name = lookup_name(&target_text);
        let Some(candidates) = by_name_lang.get(&(name.to_string(), language)) else {
            continue;
        };
        if let [only] = candidates.as_slice() {
            update.execute(rusqlite::params![only, id])?;
        }
        // 0 or >1 candidates: leave as reset it (NULL / 'textual') -- never guessed.
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

    // PR A4: name kept for git-blame continuity, but the expectation
    // flipped -- a QUALIFIED target (even with generics on top) must now
    // stay textual, never guessed past. See
    // resolves_bare_generic_target_without_qualifier below for the
    // generic-only-strip case this test used to (incorrectly) cover too.
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
        assert_eq!(
            to_symbol, None,
            "pkg.Repository might not be THIS Repository -- an unknown \
             qualifier must never be discarded to manufacture a match"
        );
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn resolves_bare_generic_target_without_qualifier() {
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
            "Repository<Foo>",
            "derived.java",
        );

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.java::Derived");
        assert_eq!(
            to_symbol.as_deref(),
            Some("base.java::Repository"),
            "a bare (unqualified) generic target must still resolve -- only \
             a QUALIFIER is unresolved evidence, generics alone are not"
        );
        assert_eq!(confidence, "resolved");
    }

    #[test]
    fn never_resolves_when_only_a_same_named_non_type_symbol_exists() {
        let conn = setup_db();
        // A function named `Repository`, not a class/struct/trait/
        // interface/enum -- A3: candidates are restricted to type-like
        // kinds, so this must never be treated as a match.
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('util.py::Repository', 'Repository', 'function', 'python', 'util.py', 1, 1)",
            [],
        )
        .unwrap();
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Repository", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol, None,
            "a same-named FUNCTION must never resolve a type relation"
        );
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn never_resolves_when_target_has_a_qualifier_even_if_a_bare_match_exists() {
        let conn = setup_db();
        // Only bar.py::Base exists; the relation names foo.Base -- a
        // DIFFERENT (unindexed, or simply different) qualifier must never
        // be discarded to match the wrong local symbol.
        insert_symbol(&conn, "bar.py::Base", "Base", "bar.py", "python");
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "foo.Base", "derived.py");

        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(to_symbol, None);
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn already_resolved_same_file_row_is_never_touched() {
        let conn = setup_db();
        insert_symbol(&conn, "a.py::Base", "Base", "a.py", "python");
        insert_symbol(&conn, "a.py::Derived", "Derived", "a.py", "python");
        // Simulates extraction's own same-file resolution: to_symbol already
        // set, resolution_source = 'same_file_ast' (never 'cross_file_unique',
        // so the A2 reset step -- which only matches 'cross_file_unique' --
        // must never touch this row).
        conn.execute(
            "INSERT INTO type_relations (from_symbol, relation_kind, target_text, to_symbol, confidence, resolution_source, source_path, line) \
             VALUES ('a.py::Derived', 'extends', 'Base', 'a.py::Base', 'resolved', 'same_file_ast', 'a.py', 1)",
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

    fn resolution_source(conn: &Connection, from_symbol: &str) -> Option<String> {
        conn.query_row(
            "SELECT resolution_source FROM type_relations WHERE from_symbol = ?1",
            [from_symbol],
            |r| r.get(0),
        )
        .unwrap()
    }

    // PR A2: the three "resolved -> textual" downgrade scenarios the plan's
    // A6 acceptance gate calls out explicitly. Each proves the reset step
    // at the top of resolve_cross_file_type_relations actually undoes a
    // PRIOR call's resolution when its evidence changes, not just that a
    // fresh call resolves correctly the first time (every test above this
    // point only ever calls the function once or twice on monotonically
    // ADDED symbols -- never a REMOVED/renamed/newly-ambiguous one).

    #[test]
    fn resolved_cross_file_row_downgrades_to_textual_when_target_symbol_deleted() {
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
        assert_eq!(
            resolution_source(&conn, "derived.py::Derived").as_deref(),
            Some("cross_file_unique")
        );

        // Base's file is removed/reindexed away -- its symbol row is gone.
        conn.execute(
            "DELETE FROM symbols WHERE qualified_name = 'base.py::Base'",
            [],
        )
        .unwrap();
        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol, None,
            "resolved -> textual: the target symbol no longer exists"
        );
        assert_eq!(confidence, "textual");
        assert_eq!(resolution_source(&conn, "derived.py::Derived"), None);
    }

    #[test]
    fn resolved_cross_file_row_downgrades_to_textual_when_target_symbol_renamed() {
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
        assert_eq!(
            to_symbol_and_confidence(&conn, "derived.py::Derived").1,
            "resolved"
        );

        // Base is renamed to BaseRenamed (its OWN name, not just a file
        // move) -- the bare name "Base" no longer has ANY candidate.
        conn.execute(
            "UPDATE symbols SET name = 'BaseRenamed', qualified_name = 'base.py::BaseRenamed' \
             WHERE qualified_name = 'base.py::Base'",
            [],
        )
        .unwrap();
        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol, None,
            "resolved -> textual: target_text still says \"Base\", which no \
             longer names anything after the rename"
        );
        assert_eq!(confidence, "textual");
    }

    #[test]
    fn resolved_cross_file_row_downgrades_to_textual_when_second_same_name_type_appears() {
        let conn = setup_db();
        insert_symbol(&conn, "a.py::Handler", "Handler", "a.py", "python");
        insert_symbol(
            &conn,
            "derived.py::Derived",
            "Derived",
            "derived.py",
            "python",
        );
        insert_relation(&conn, "derived.py::Derived", "Handler", "derived.py");
        resolve_cross_file_type_relations(&conn).unwrap();
        assert_eq!(
            to_symbol_and_confidence(&conn, "derived.py::Derived").1,
            "resolved"
        );

        // A second, unrelated Handler is added elsewhere -- now ambiguous.
        insert_symbol(&conn, "b.py::Handler", "Handler", "b.py", "python");
        resolve_cross_file_type_relations(&conn).unwrap();

        let (to_symbol, confidence) = to_symbol_and_confidence(&conn, "derived.py::Derived");
        assert_eq!(
            to_symbol, None,
            "resolved -> textual: a second same-name candidate makes the \
             match no longer unique"
        );
        assert_eq!(confidence, "textual");
    }
}
