//! Standalone SQL indexer (8-language plan P3.3). Deliberately NOT wired
//! into `lang_constants`'s per-node-kind tables the way every tree-sitter
//! language is (see `pipeline::extract_file_data`'s early `lang == "sql"`
//! branch, which bypasses `parse_tree` entirely for this language) — SQL's
//! DDL vocabulary and its procedural-body dialects (T-SQL `BEGIN...END`,
//! Postgres `$$...$$`/plpgsql, MySQL) don't fit that shape the way a
//! general-purpose language's grammar does.
//!
//! Dep: `sqlparser` (crates.io name for the `apache/datafusion-sqlparser-rs`
//! project the plan names by its GitHub repo) — used only for what
//! `GenericDialect` parses reliably: top-level `CREATE TABLE/VIEW/INDEX/
//! FUNCTION/TRIGGER` headers. Verified experimentally (not assumed) against
//! this crate: MySQL/T-SQL-style `CREATE PROCEDURE ... BEGIN ... END`
//! (without `AS`) fails to parse under `GenericDialect`; Postgres-style
//! `CREATE FUNCTION ... AS $$ ... $$ LANGUAGE plpgsql` parses fine,
//! dollar-quoted body included verbatim. A statement `sqlparser` can't parse
//! falls back to a regex header scan (`classify_statement_by_regex`) so
//! indexing never loses that statement's symbol outright — and every
//! statement, parsed or not, is regex-scanned for `FROM`/`JOIN`/`CALL`/
//! `EXEC[UTE]` references (`scan_references`) rather than walked
//! structurally, since a procedure/function body's *contents* are exactly
//! the part dialects disagree on most. This mirrors the plan's own
//! robustness note (dbt/Jinja templating similarly can't be parsed as SQL at
//! all) — the same fallback mechanism handles both cases uniformly.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use sqlparser::ast::Statement;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::graph::tokenize::tokenize_identifier;
use crate::indexer::parser::ParsedSymbol;
use crate::types::{EdgeConfidence, SymbolKind};

/// One reference this file's SQL makes to another named object: a view/proc
/// `SELECT`-ing `FROM`/`JOIN`-ing a table, or a proc/trigger invoking another
/// proc via `CALL`/`EXEC[UTE]`. Kept as this module's own public type
/// (rather than reaching into `pipeline::CallSiteData`, which is private)
/// so `sql.rs` stays independently testable; `pipeline::extract_file_data`
/// converts these 1:1 into `CallSiteData` values.
pub struct SqlReference {
    pub enclosing_qn: String,
    pub target_name: String,
    pub line: i64,
    pub confidence: EdgeConfidence,
    /// `"reference"` for FROM/JOIN (this object reads another), `"call"` for
    /// CALL/EXEC(UTE) (this object invokes another) — see
    /// `call_edges.edge_kind`'s migration comment in `db::schema`.
    pub edge_kind: &'static str,
}

pub struct SqlFile {
    pub symbols: Vec<ParsedSymbol>,
    pub references: Vec<SqlReference>,
}

/// Parse `source` (one `.sql` file at relative path `rel`) into symbols
/// (`CREATE TABLE`/`VIEW`/`MATERIALIZED VIEW`/`PROCEDURE`/`FUNCTION`/
/// `TRIGGER`/`INDEX`) and the references those objects make to each other.
/// Never fails outright: a statement neither `sqlparser` nor the regex
/// fallback can classify contributes 0 symbols, not an error — same
/// no-external-tool-or-grammar-available-yet, degrade-don't-crash posture as
/// the rest of this indexer (ADR-0004 §2).
///
/// Confidence: a reference whose target is *also* defined somewhere in this
/// same file is `Resolved` (mirrors `ConservativeResolver::resolve_tier1`'s
/// same-file-membership rule every other language already gets); a
/// cross-file reference stays `Textual` and relies on `rebuild_graph`'s
/// existing global by-name fallback for an actual edge — exactly the
/// established "edge exists, confidence label just doesn't claim more than
/// bare-name matching earned" pattern already used for e.g. PHP's
/// `require`/`include` (plan §4/P1.2).
pub fn extract_sql_file(rel: &str, source: &str) -> SqlFile {
    let dialect = GenericDialect {};
    let mut symbols: Vec<ParsedSymbol> = Vec::new();
    let mut references: Vec<SqlReference> = Vec::new();
    let mut seen_qn: HashSet<String> = HashSet::new();

    for (stmt_text, start_line) in split_statements(source) {
        if stmt_text.is_empty() {
            continue;
        }
        let parsed: Option<Statement> = Parser::parse_sql(&dialect, &stmt_text)
            .ok()
            .and_then(|mut stmts| (stmts.len() == 1).then(|| stmts.remove(0)));

        let Some((kind, name)) = parsed
            .as_ref()
            .and_then(classify_statement)
            .or_else(|| classify_statement_by_regex(&stmt_text))
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }

        let qn = unique_qualified_name(rel, &name, start_line, &mut seen_qn);
        let line_end = start_line + stmt_text.matches('\n').count();
        symbols.push(ParsedSymbol {
            qualified_name: qn.clone(),
            name: name.clone(),
            kind,
            language: "sql".to_string(),
            path: rel.to_string(),
            line_start: start_line,
            line_end,
            signature: first_line(&stmt_text),
            docstring: String::new(),
            name_tokens: tokenize_identifier(&name),
            is_entry_point: false,
            is_test: false,
            class_context: None,
            complexity: 1,
        });
        references.extend(scan_references(&stmt_text, &qn, start_line));
    }

    // Same-file membership → Resolved (see doc comment above). Computed as a
    // post-pass, not inline in the loop above, because a forward reference
    // (a proc defined before the table it selects FROM, common when tables
    // come last in a migration file) must still resolve — the full symbol
    // set for this file is only known once every statement has been walked.
    let file_symbol_names: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    for r in &mut references {
        if file_symbol_names.contains(r.target_name.as_str()) {
            r.confidence = EdgeConfidence::Resolved;
        }
    }

    SqlFile {
        symbols,
        references,
    }
}

/// Classify an already-`sqlparser`-parsed statement into `(SymbolKind, name)`
/// — `None` for any statement kind this indexer doesn't turn into a symbol
/// (`SELECT`/`INSERT`/`ALTER`/... at the top level).
fn classify_statement(stmt: &Statement) -> Option<(SymbolKind, String)> {
    match stmt {
        Statement::CreateTable(t) => Some((SymbolKind::Struct, last_segment(&t.name.to_string()))),
        Statement::CreateView(v) => Some((SymbolKind::Struct, last_segment(&v.name.to_string()))),
        Statement::CreateIndex(i) => i
            .name
            .as_ref()
            .map(|n| (SymbolKind::Struct, last_segment(&n.to_string()))),
        Statement::CreateFunction(f) => {
            Some((SymbolKind::Function, last_segment(&f.name.to_string())))
        }
        Statement::CreateTrigger(t) => {
            Some((SymbolKind::Function, last_segment(&t.name.to_string())))
        }
        _ => None,
    }
}

/// Fallback for a statement `sqlparser`'s `GenericDialect` can't parse at
/// all (vendor-specific procedural syntax, dbt/Jinja templating around a
/// `CREATE` header) — a plain header regex, so this indexer's coverage of
/// "does this file declare an object named X" never depends on a specific
/// dialect's procedural-body grammar being supported.
fn classify_statement_by_regex(stmt_text: &str) -> Option<(SymbolKind, String)> {
    let caps = create_header_regex().captures(stmt_text)?;
    let kind_word = caps.get(1)?.as_str().to_ascii_uppercase();
    let name = last_segment(caps.get(2)?.as_str());
    let kind =
        if kind_word.contains("TABLE") || kind_word.contains("VIEW") || kind_word.contains("INDEX")
        {
            SymbolKind::Struct
        } else if kind_word.contains("PROCEDURE")
            || kind_word.contains("FUNCTION")
            || kind_word.contains("TRIGGER")
        {
            SymbolKind::Function
        } else {
            return None;
        };
    Some((kind, name))
}

fn create_header_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?is)^\s*CREATE\s+(?:OR\s+REPLACE\s+|OR\s+ALTER\s+)?(?:UNIQUE\s+)?",
            r"(TABLE|MATERIALIZED\s+VIEW|VIEW|PROCEDURE|FUNCTION|TRIGGER|INDEX)\s+",
            r"(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][A-Za-z0-9_.]*)",
        ))
        .unwrap()
    })
}

/// Scan one statement's raw text for `FROM`/`JOIN` (a read reference) and
/// `CALL`/`EXEC[UTE][ PROCEDURE|FUNCTION]` (an invocation) targets. Regex
/// over raw text rather than walking `sqlparser`'s typed `Query`/body AST —
/// see the module doc comment for why a structural walk doesn't generalize
/// across dialects here the way it does for the header.
fn scan_references(stmt_text: &str, enclosing_qn: &str, start_line: usize) -> Vec<SqlReference> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for caps in reference_regex().captures_iter(stmt_text) {
        let keyword = caps.get(1).unwrap().as_str().to_ascii_uppercase();
        let target = last_segment(caps.get(2).unwrap().as_str());
        if target.is_empty() || target.eq_ignore_ascii_case(enclosing_qn) {
            continue;
        }
        let edge_kind = if keyword == "FROM" || keyword == "JOIN" {
            "reference"
        } else {
            "call"
        };
        let key = format!("{edge_kind}:{target}");
        if !seen.insert(key) {
            continue;
        }
        let offset = caps.get(0).unwrap().start();
        let line = start_line as i64 + stmt_text[..offset].matches('\n').count() as i64;
        out.push(SqlReference {
            enclosing_qn: enclosing_qn.to_string(),
            target_name: target,
            line,
            confidence: EdgeConfidence::Textual,
            edge_kind,
        });
    }
    out
}

fn reference_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)\b(FROM|JOIN|CALL|EXEC(?:UTE)?)\b(?:\s+(?:PROCEDURE|FUNCTION))?\s+",
            r"([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?)",
        ))
        .unwrap()
    })
}

/// Last `.`-separated segment of a possibly schema-qualified name
/// (`public.users` → `users`), with common quoting stripped (`"users"`,
/// `` `users` ``, `[users]`). V1 simplification (noted in the plan): schema
/// qualification is recognized enough to not corrupt the base name, but not
/// used to disambiguate two different schemas' same-named tables — the
/// same-name-collision behavior every other language's bare-name matching
/// already has.
fn last_segment(qualified: &str) -> String {
    qualified
        .rsplit('.')
        .next()
        .unwrap_or(qualified)
        .trim_matches(|c: char| c == '"' || c == '`' || c == '[' || c == ']')
        .to_string()
}

fn first_line(stmt_text: &str) -> String {
    stmt_text
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

/// `rel::name`, with the same incrementing-suffix collision handling as
/// `pipeline::extract_file_data`'s own dedup loop (the fix for the real C
/// indexer crash found in the 8-language plan's benchmark run, §7) — two
/// `CREATE` statements for the same name at different lines (e.g. a `DROP
/// ... ; CREATE ...` pair, or a redefinition later in a long migration file)
/// must never collide into one UNIQUE-constraint-violating insert.
fn unique_qualified_name(rel: &str, name: &str, line: usize, seen: &mut HashSet<String>) -> String {
    let base = format!("{rel}::{name}");
    if seen.insert(base.clone()) {
        return base;
    }
    let mut candidate = format!("{base}#{line}");
    let mut suffix = 2;
    while !seen.insert(candidate.clone()) {
        candidate = format!("{base}#{line}#{suffix}");
        suffix += 1;
    }
    candidate
}

/// Splits `source` into individual top-level SQL statements, each paired
/// with its 1-indexed starting line. Respects `'...'`/`"..."` quoting, `--`
/// line comments, `/* */` block comments, and Postgres-style dollar-quoted
/// bodies (`$$...$$` / `$tag$...$tag$`) — a `;` inside a function body (very
/// common: `$$ SELECT ...; $$`) must never be treated as a statement
/// boundary. A trailing statement with no closing `;` is still included.
fn split_statements(source: &str) -> Vec<(String, usize)> {
    #[derive(PartialEq)]
    enum State {
        Normal,
        LineComment,
        BlockComment,
        SingleQuote,
        DoubleQuote,
        DollarQuote,
    }

    let mut out = Vec::new();
    let mut buf = String::new();
    let mut stmt_start_line = 1usize;
    let mut line = 1usize;
    let mut state = State::Normal;
    let mut dollar_tag = String::new();
    let mut chars = source.char_indices().peekable();

    while let Some((_, c)) = chars.next() {
        if buf.trim().is_empty() && !c.is_whitespace() {
            stmt_start_line = line;
        }
        buf.push(c);
        if c == '\n' {
            line += 1;
            if state == State::LineComment {
                state = State::Normal;
            }
            continue;
        }
        match state {
            State::LineComment => {}
            State::BlockComment => {
                if c == '*' && chars.peek().map(|&(_, n)| n) == Some('/') {
                    let (_, n) = chars.next().unwrap();
                    buf.push(n);
                    state = State::Normal;
                }
            }
            State::SingleQuote => {
                if c == '\'' {
                    if chars.peek().map(|&(_, n)| n) == Some('\'') {
                        let (_, n) = chars.next().unwrap();
                        buf.push(n); // escaped '' stays inside the string
                    } else {
                        state = State::Normal;
                    }
                }
            }
            State::DoubleQuote => {
                if c == '"' {
                    state = State::Normal;
                }
            }
            State::DollarQuote => {
                if c == '$' && buf.ends_with(&dollar_tag) {
                    state = State::Normal;
                }
            }
            State::Normal => match c {
                '-' if chars.peek().map(|&(_, n)| n) == Some('-') => {
                    let (_, n) = chars.next().unwrap();
                    buf.push(n);
                    state = State::LineComment;
                }
                '/' if chars.peek().map(|&(_, n)| n) == Some('*') => {
                    let (_, n) = chars.next().unwrap();
                    buf.push(n);
                    state = State::BlockComment;
                }
                '\'' => state = State::SingleQuote,
                '"' => state = State::DoubleQuote,
                '$' => {
                    // Postgres dollar-quote open tag: `$` [ident]* `$`.
                    let mut tag = String::from("$");
                    let mut lookahead = chars.clone();
                    let mut closed = false;
                    while let Some(&(_, n)) = lookahead.peek() {
                        if n == '$' {
                            tag.push('$');
                            closed = true;
                            break;
                        } else if n.is_alphanumeric() || n == '_' {
                            tag.push(n);
                            lookahead.next();
                        } else {
                            break;
                        }
                    }
                    if closed {
                        // Consume the tag body chars plus the closing `$`
                        // (the opening `$` was already pushed as `c` above).
                        for _ in 0..(tag.len() - 2) {
                            let (_, n) = chars.next().unwrap();
                            buf.push(n);
                        }
                        let (_, n) = chars.next().unwrap();
                        buf.push(n);
                        dollar_tag = tag;
                        state = State::DollarQuote;
                    }
                }
                ';' => {
                    buf.pop(); // drop the ';' just pushed at the top of the loop
                    out.push((buf.trim().to_string(), stmt_start_line));
                    buf.clear();
                }
                _ => {}
            },
        }
    }
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        out.push((trimmed.to_string(), stmt_start_line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_statements_basic_semicolon_boundaries() {
        let stmts = split_statements("CREATE TABLE a (id INT); CREATE TABLE b (id INT);");
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "CREATE TABLE a (id INT)");
        assert_eq!(stmts[1].0, "CREATE TABLE b (id INT)");
    }

    #[test]
    fn split_statements_keeps_trailing_statement_with_no_semicolon() {
        let stmts = split_statements("CREATE TABLE a (id INT)");
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].0, "CREATE TABLE a (id INT)");
    }

    /// The exact case the plan calls out by name: a semicolon *inside* a
    /// dollar-quoted function body must not be treated as a statement
    /// boundary — without this, `split_statements` would cut the body in
    /// half and hand `sqlparser` two syntactically broken halves instead of
    /// one valid `CREATE FUNCTION`.
    #[test]
    fn split_statements_respects_semicolons_inside_dollar_quoted_body() {
        let sql = "CREATE FUNCTION get_user(uid INT) RETURNS INT AS $$ \
                   SELECT id FROM users WHERE id = uid; $$ LANGUAGE sql; \
                   CREATE TABLE audit (id INT);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2, "got: {stmts:?}");
        assert!(stmts[0].0.starts_with("CREATE FUNCTION"));
        assert!(stmts[0].0.contains("SELECT id FROM users WHERE id = uid;"));
        assert!(stmts[1].0.starts_with("CREATE TABLE audit"));
    }

    #[test]
    fn split_statements_respects_semicolons_inside_single_quoted_string() {
        let sql = "CREATE TABLE a (note TEXT DEFAULT 'a; b'); CREATE TABLE b (id INT);";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].0.contains("'a; b'"));
    }

    #[test]
    fn split_statements_ignores_semicolons_in_comments() {
        let sql = "-- a comment with a semicolon; right here\nCREATE TABLE a (id INT);\n/* block ; comment */\nCREATE TABLE b (id INT);";
        let stmts = split_statements(sql);
        // The `;` inside each comment must not create a spurious 3rd/4th
        // split — exactly 2 real statements. Comment text itself is left in
        // place (this function's job is boundary-finding, not stripping),
        // so assert containment of the real statement, not a prefix match.
        assert_eq!(stmts.len(), 2, "got: {stmts:?}");
        assert!(stmts[0].0.contains("CREATE TABLE a (id INT)"));
        assert!(stmts[1].0.contains("CREATE TABLE b (id INT)"));
    }

    #[test]
    fn extract_sql_file_table_view_and_index_symbols() {
        let src = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);\n\
                   CREATE VIEW active_users AS SELECT id, name FROM users;\n\
                   CREATE INDEX idx_users_name ON users (name);";
        let f = extract_sql_file("schema.sql", src);
        let names: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"users"));
        assert!(names.contains(&"active_users"));
        assert!(names.contains(&"idx_users_name"));
        let users = f.symbols.iter().find(|s| s.name == "users").unwrap();
        assert_eq!(users.kind, SymbolKind::Struct);
    }

    /// Plan §7's DoD, verbatim: `schema.sql` → symbol `users` (table) +
    /// `get_user` (proc), view→table edge `resolved`.
    #[test]
    fn extract_sql_file_view_to_table_reference_is_resolved_same_file() {
        let src = "CREATE TABLE users (id INT PRIMARY KEY, name TEXT);\n\
                   CREATE VIEW active_users AS SELECT id, name FROM users;";
        let f = extract_sql_file("schema.sql", src);
        let r = f
            .references
            .iter()
            .find(|r| r.target_name == "users")
            .expect("view->table reference");
        assert_eq!(r.confidence, EdgeConfidence::Resolved);
        assert_eq!(r.edge_kind, "reference");
        assert!(r.enclosing_qn.ends_with("active_users"));
    }

    #[test]
    fn extract_sql_file_cross_file_reference_stays_textual() {
        // `orders` is never defined in this file — the reference still gets
        // recorded (so `rebuild_graph`'s global by-name pass can resolve it
        // against another file), but at `Textual`, not `Resolved`.
        let src = "CREATE VIEW recent_orders AS SELECT id FROM orders;";
        let f = extract_sql_file("views.sql", src);
        let r = f.references.first().unwrap();
        assert_eq!(r.target_name, "orders");
        assert_eq!(r.confidence, EdgeConfidence::Textual);
    }

    #[test]
    fn extract_sql_file_postgres_function_with_dollar_body_parses_and_finds_from() {
        let src = "CREATE TABLE users (id INT, name TEXT);\n\
                   CREATE FUNCTION get_user(uid INT) RETURNS INT AS $$ \
                   BEGIN RETURN (SELECT id FROM users WHERE id = uid); END; \
                   $$ LANGUAGE plpgsql;";
        let f = extract_sql_file("schema.sql", src);
        let get_user = f
            .symbols
            .iter()
            .find(|s| s.name == "get_user")
            .expect("get_user symbol");
        assert_eq!(get_user.kind, SymbolKind::Function);
        let r = f
            .references
            .iter()
            .find(|r| r.enclosing_qn.ends_with("get_user") && r.target_name == "users")
            .expect("get_user -> users reference");
        assert_eq!(r.confidence, EdgeConfidence::Resolved);
        assert_eq!(r.edge_kind, "reference");
    }

    /// MySQL/T-SQL-style `CREATE PROCEDURE ... BEGIN ... END` (no `AS`) is
    /// confirmed (experimentally, against this exact `sqlparser` version) to
    /// fail `GenericDialect` parsing — this is the regex-fallback path, and
    /// it must still produce a symbol, not silently drop the procedure.
    #[test]
    fn extract_sql_file_unparseable_procedure_still_yields_symbol_via_regex_fallback() {
        let src = "CREATE PROCEDURE get_user(IN uid INT)\nBEGIN\n  SELECT id, name FROM users WHERE id = uid;\nEND;";
        let f = extract_sql_file("proc.sql", src);
        let proc = f
            .symbols
            .iter()
            .find(|s| s.name == "get_user")
            .expect("get_user symbol via regex fallback");
        assert_eq!(proc.kind, SymbolKind::Function);
        // The body reference is still found even though the statement as a
        // whole never parsed as structured SQL — same regex mechanism
        // extracts both the header and the FROM reference independently.
        assert!(f.references.iter().any(|r| r.target_name == "users"));
    }

    #[test]
    fn extract_sql_file_call_reference_between_procedures() {
        let src = "CREATE FUNCTION log_event(msg TEXT) RETURNS VOID AS $$ BEGIN NULL; END; $$ LANGUAGE plpgsql;\n\
                   CREATE FUNCTION get_user(uid INT) RETURNS INT AS $$ BEGIN CALL log_event('lookup'); RETURN uid; END; $$ LANGUAGE plpgsql;";
        let f = extract_sql_file("procs.sql", src);
        let r = f
            .references
            .iter()
            .find(|r| r.enclosing_qn.ends_with("get_user") && r.target_name == "log_event")
            .expect("get_user -> log_event call reference");
        assert_eq!(r.edge_kind, "call");
        assert_eq!(r.confidence, EdgeConfidence::Resolved);
    }

    #[test]
    fn extract_sql_file_name_collision_gets_unique_qualified_name() {
        let src =
            "CREATE TABLE dup (id INT);\nDROP TABLE dup;\nCREATE TABLE dup (id INT, extra TEXT);";
        let f = extract_sql_file("migrate.sql", src);
        let dups: Vec<_> = f.symbols.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 2);
        assert_ne!(dups[0].qualified_name, dups[1].qualified_name);
    }

    #[test]
    fn extract_sql_file_ignores_non_create_statements() {
        let src = "INSERT INTO users (id, name) VALUES (1, 'a');\nSELECT * FROM users;";
        let f = extract_sql_file("data.sql", src);
        assert!(f.symbols.is_empty());
        assert!(f.references.is_empty());
    }
}
