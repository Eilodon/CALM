//! T4a Package Dependency Graph (2026-08-07 roadmap,
//! docs/plans/2026-08-07-pecorino-adoption-roadmap.md, TIER 4 first
//! stage: "package-dependency-graph (evidence-based, làm trước)").
//! Extracts DECLARED external dependencies straight from manifest files
//! (`Cargo.toml`, `package.json`, `go.mod`, `requirements.txt`,
//! `pyproject.toml`) into a `package_dependencies` table.
//!
//! # Scope (deliberate, matches the roadmap's own sequencing)
//!
//! This is single-repo scope ONLY — "what does this repo declare it
//! depends on." It is explicitly NOT the roadmap's later T4 stages:
//! federated cross-repo search fanout (needs a repo-identity concept
//! this codebase doesn't have) or a cross-repo call graph (the roadmap's
//! own words: "đừng, tới khi có evidence" — don't, until there's
//! evidence). Both stay unstarted; this module only builds the
//! foundation stage the roadmap says to do first.
//!
//! Scope cuts within this stage, all deliberate:
//! - **Java (`pom.xml`/`build.gradle`) is not parsed.** This codebase has
//!   no XML parsing dependency anywhere, and a hand-rolled regex/tag scan
//!   over real-world `pom.xml` (namespaces, `<properties>` version
//!   interpolation, profiles) risks silently misreading structure rather
//!   than just being absent — the same "prefer missing over wrong"
//!   posture `indexer::semantic_facts` already documents. Deferred, not
//!   an oversight.
//! - **Lockfiles are never read** (`Cargo.lock`/`package-lock.json`/
//!   `go.sum`/etc). This extracts DECLARED (manifest-level) dependencies
//!   — what a developer/reviewer actually reads and reasons about — not
//!   the fully resolved transitive closure, which is a different,
//!   heavier feature.
//! - **Version specs are stored as the raw declared string, never
//!   resolved or range-parsed.** `^1.2`, `~1.2`, `>=1.2,<2.0`, a git URL,
//!   a path — all pass through verbatim. Interpreting semver ranges is
//!   out of scope here.

use std::path::Path;

#[derive(Debug)]
pub struct RawPackageDependency {
    pub manifest_path: String,
    pub ecosystem: &'static str,
    pub name: String,
    pub version_spec: Option<String>,
    pub kind: &'static str,
}

/// Walks `project_root` via the shared ignore-aware walker (respects
/// `config.ignore`, `.gitignore`, and the built-in `IGNORE_DIRS` —
/// critically, this is what keeps `node_modules`'s thousands of nested
/// `package.json` files out of the result without any special-casing
/// here) looking for the five manifest filenames this module recognizes,
/// parsing each with the matching ecosystem parser below.
pub fn scan_project(project_root: &Path, ignore: &[String]) -> Vec<RawPackageDependency> {
    let mut out = Vec::new();
    for entry in crate::walk::build_walker(project_root, ignore, false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(project_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let deps = match file_name {
            "Cargo.toml" => parse_cargo_toml(&content),
            "package.json" => parse_package_json(&content),
            "go.mod" => parse_go_mod(&content),
            "requirements.txt" => parse_requirements_txt(&content),
            "pyproject.toml" => parse_pyproject_toml(&content),
            _ => continue,
        };
        out.extend(deps.into_iter().map(|mut d| {
            d.manifest_path = rel.clone();
            d
        }));
    }
    out
}

/// Bumped whenever a manifest parser's extraction rules change (a new
/// dependency kind recognized, a new manifest shape supported, a fix to what
/// counts as a version spec). Folded into `InputCatalog::index_input_snapshot`'s
/// `context_material` bucket (`indexer::refresh`) alongside
/// `GRAPH_DERIVATION_VERSION` -- package deps are recomputed by every graph
/// rebuild (`compute_package_dependencies` is called from both
/// `pipeline::rebuild_graph` and `pipeline::incremental_graph_update`), never
/// reparsed from source directly, so a `Context`-class drift is sufficient.
/// See docs/plans/2026-08-08-derived-artifact-hardening-execution-plan.md P1.
///
/// A change here is verified by
/// `derived_artifact_versions::package_graph_fixture_is_pinned_to_its_version`
/// (crates/calm-core/tests/derived_artifact_versions.rs) -- bump this AND
/// that test's expected hash together, in the same commit, never one alone.
pub const PACKAGE_GRAPH_VERSION: i64 = 1;

/// Full DELETE-then-reinsert into `package_dependencies` -- same posture
/// as `graph::digest::compute_digests` (manifests are small, re-scanning
/// on every rebuild is cheap, no selective invalidation needed). Called
/// with `ignore: &[]` from `pipeline::rebuild_graph` (built-in
/// `IGNORE_DIRS` + real `.gitignore` already keep `node_modules`/
/// `target`/`dist`/`build`/`__pycache__` out -- see `walk::build_walker`'s
/// doc comment -- a deliberate simplification over threading
/// `config.ignore` through `rebuild_graph`'s signature for a feature
/// where a user's custom ignore glob almost never targets a project's
/// OWN manifest files specifically).
pub fn compute_package_dependencies(
    conn: &rusqlite::Connection,
    project_root: &Path,
    ignore: &[String],
) -> rusqlite::Result<()> {
    let deps = scan_project(project_root, ignore);
    conn.execute("DELETE FROM package_dependencies", [])?;
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO package_dependencies \
         (manifest_path, ecosystem, dependency_name, version_spec, dependency_kind) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for d in &deps {
        stmt.execute(rusqlite::params![
            d.manifest_path,
            d.ecosystem,
            d.name,
            d.version_spec,
            d.kind,
        ])?;
    }
    Ok(())
}

fn dep(
    name: &str,
    version: Option<&str>,
    ecosystem: &'static str,
    kind: &'static str,
) -> RawPackageDependency {
    RawPackageDependency {
        manifest_path: String::new(), // filled in by scan_project
        ecosystem,
        name: name.to_string(),
        version_spec: version.map(str::to_string),
        kind,
    }
}

/// `[dependencies]`/`[dev-dependencies]`/`[build-dependencies]` — each
/// entry is either a bare version string (`serde = "1"`) or a table
/// (`serde = { version = "1", features = [...] }`, or `{ workspace =
/// true }` with no `version` key at all, in which case `version_spec` is
/// `None` rather than fabricated).
fn parse_cargo_toml(content: &str) -> Vec<RawPackageDependency> {
    let Ok(doc) = content.parse::<toml::Value>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (table_name, kind) in [
        ("dependencies", "runtime"),
        ("dev-dependencies", "dev"),
        ("build-dependencies", "build"),
    ] {
        let Some(table) = doc.get(table_name).and_then(|t| t.as_table()) else {
            continue;
        };
        for (name, value) in table {
            let version = match value {
                toml::Value::String(s) => Some(s.as_str()),
                toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()),
                _ => None,
            };
            out.push(dep(name, version, "cargo", kind));
        }
    }
    // `[workspace.dependencies]` -- the shared version pins a Cargo
    // workspace's ROOT manifest declares (member crates then reference
    // them via `dep = { workspace = true }`, already handled above with
    // `version_spec: None` since that form omits `version`). Missing this
    // table means a workspace's real dependency versions are invisible
    // for every workspace root manifest, which is common enough in real
    // Rust repos (this very codebase's own root Cargo.toml is one) that
    // it isn't a rare edge case to skip.
    if let Some(table) = doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|t| t.as_table())
    {
        for (name, value) in table {
            let version = match value {
                toml::Value::String(s) => Some(s.as_str()),
                toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()),
                _ => None,
            };
            out.push(dep(name, version, "cargo", "runtime"));
        }
    }
    out
}

/// `dependencies`/`devDependencies`/`peerDependencies`/
/// `optionalDependencies` — each a flat `{name: versionRange}` object.
fn parse_package_json(content: &str) -> Vec<RawPackageDependency> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(content) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, kind) in [
        ("dependencies", "runtime"),
        ("devDependencies", "dev"),
        ("peerDependencies", "peer"),
        ("optionalDependencies", "optional"),
    ] {
        let Some(table) = doc.get(key).and_then(|t| t.as_object()) else {
            continue;
        };
        for (name, value) in table {
            out.push(dep(name, value.as_str(), "npm", kind));
        }
    }
    out
}

/// `require module version` (single-line) or a `require ( ... )` block,
/// one module+version pair per line, optionally trailed by `// indirect`
/// (still recorded — an indirect dependency is a real declared one, just
/// not directly imported; not distinguished from direct via `kind` here
/// since go.mod has no separate dev/build dependency concept at all).
fn parse_go_mod(content: &str) -> Vec<RawPackageDependency> {
    let mut out = Vec::new();
    let mut in_require_block = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "require (" {
            in_require_block = true;
            continue;
        }
        if in_require_block && trimmed == ")" {
            in_require_block = false;
            continue;
        }
        let entry = if in_require_block {
            Some(trimmed)
        } else {
            trimmed.strip_prefix("require ").map(str::trim)
        };
        let Some(entry) = entry else { continue };
        // Strip a trailing `// indirect` (or any `//` comment) before splitting.
        let entry = entry.split("//").next().unwrap_or(entry).trim();
        let mut parts = entry.split_whitespace();
        let (Some(module), Some(version)) = (parts.next(), parts.next()) else {
            continue;
        };
        out.push(dep(module, Some(version), "go", "runtime"));
    }
    out
}

/// One requirement per non-comment, non-blank, non-option line. Splits on
/// the first version-comparison operator; a bare name (no operator) is
/// recorded with `version_spec: None`, never fabricated. `name[extras]`
/// has the bracket suffix stripped from the NAME (extras aren't a
/// separate dependency, and keeping them in `name` would make the same
/// package look like two different ones across files).
fn parse_requirements_txt(content: &str) -> Vec<RawPackageDependency> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let line = line.split('#').next().unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        let split_at = line
            .find(|c: char| "=<>!~;".contains(c))
            .unwrap_or(line.len());
        let (raw_name, rest) = line.split_at(split_at);
        let name = raw_name.split('[').next().unwrap_or(raw_name).trim();
        if name.is_empty() {
            continue;
        }
        let version = if rest.trim().is_empty() {
            None
        } else {
            Some(rest.trim())
        };
        out.push(dep(name, version, "pypi", "runtime"));
    }
    out
}

/// Two supported shapes: PEP 621 `[project] dependencies = [...]` (array
/// of PEP 508 strings, parsed the same way `parse_requirements_txt`
/// parses one line) and Poetry's `[tool.poetry.dependencies]`/
/// `[tool.poetry.group.dev.dependencies]` (tables of `name = spec`,
/// skipping the `python` key itself — a Python-version constraint, not a
/// real dependency).
fn parse_pyproject_toml(content: &str) -> Vec<RawPackageDependency> {
    let Ok(doc) = content.parse::<toml::Value>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(deps) = doc
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for entry in deps {
            if let Some(spec) = entry.as_str() {
                out.extend(parse_requirements_txt(spec));
            }
        }
    }
    let poetry_tables: [(&[&str], &'static str); 2] = [
        (&["tool", "poetry", "dependencies"], "runtime"),
        (&["tool", "poetry", "dev-dependencies"], "dev"),
    ];
    for (path, kind) in poetry_tables {
        let mut cursor = &doc;
        let mut found = true;
        for segment in path {
            match cursor.get(segment) {
                Some(v) => cursor = v,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if !found {
            continue;
        }
        let Some(table) = cursor.as_table() else {
            continue;
        };
        for (name, value) in table {
            if name == "python" {
                continue;
            }
            let version = match value {
                toml::Value::String(s) => Some(s.as_str()),
                toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()),
                _ => None,
            };
            out.push(dep(name, version, "pypi", kind));
        }
    }
    // Modern Poetry dependency groups (Poetry 1.2+):
    // `[tool.poetry.group.<name>.dependencies]`. Every group here is
    // supplementary by definition (the `main` group is `[tool.poetry.dependencies]`
    // above, never expressed as `group.main`), so all of them map to `dev` --
    // same posture as the legacy `dev-dependencies` table, and consistent
    // with this scanner only distinguishing runtime vs. non-runtime, not
    // Poetry's own free-form group names.
    if let Some(groups) = doc
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("group"))
        .and_then(|g| g.as_table())
    {
        for group in groups.values() {
            let Some(table) = group.get("dependencies").and_then(|d| d.as_table()) else {
                continue;
            };
            for (name, value) in table {
                if name == "python" {
                    continue;
                }
                let version = match value {
                    toml::Value::String(s) => Some(s.as_str()),
                    toml::Value::Table(t) => t.get("version").and_then(|v| v.as_str()),
                    _ => None,
                };
                out.push(dep(name, version, "pypi", "dev"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(deps: &[RawPackageDependency]) -> Vec<&str> {
        deps.iter().map(|d| d.name.as_str()).collect()
    }

    #[test]
    fn cargo_toml_runtime_and_dev_deps_with_table_and_string_versions() {
        let deps = parse_cargo_toml(
            r#"
[package]
name = "x"

[dependencies]
serde = "1.0"
rusqlite = { version = "0.31", features = ["bundled"] }
local-crate = { path = "../local", workspace = true }

[dev-dependencies]
tempfile = "3"
"#,
        );
        assert_eq!(
            deps.iter()
                .map(|d| (d.name.as_str(), d.version_spec.as_deref(), d.kind))
                .collect::<Vec<_>>()
                .len(),
            4
        );
        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version_spec.as_deref(), Some("1.0"));
        assert_eq!(serde.kind, "runtime");
        let rusqlite = deps.iter().find(|d| d.name == "rusqlite").unwrap();
        assert_eq!(rusqlite.version_spec.as_deref(), Some("0.31"));
        let local = deps.iter().find(|d| d.name == "local-crate").unwrap();
        assert_eq!(
            local.version_spec, None,
            "no version key present -- must not fabricate one"
        );
        let tempfile = deps.iter().find(|d| d.name == "tempfile").unwrap();
        assert_eq!(tempfile.kind, "dev");
    }

    #[test]
    fn cargo_toml_workspace_dependencies_table_is_scanned() {
        let deps = parse_cargo_toml(
            r#"
[workspace]
members = ["crates/a"]

[workspace.dependencies]
serde = "1.0"
rusqlite = { version = "0.31", features = ["bundled"] }
"#,
        );
        assert_eq!(names(&deps).len(), 2);
        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version_spec.as_deref(), Some("1.0"));
        assert_eq!(serde.kind, "runtime");
    }

    #[test]
    fn package_json_all_four_dependency_kinds() {
        let deps = parse_package_json(
            r#"{
                "dependencies": {"react": "^18.0.0"},
                "devDependencies": {"jest": "29.0.0"},
                "peerDependencies": {"react-dom": "^18.0.0"},
                "optionalDependencies": {"fsevents": "2.0.0"}
            }"#,
        );
        assert_eq!(names(&deps).len(), 4);
        let react = deps.iter().find(|d| d.name == "react").unwrap();
        assert_eq!(react.version_spec.as_deref(), Some("^18.0.0"));
        assert_eq!(react.kind, "runtime");
        assert!(deps.iter().any(|d| d.name == "jest" && d.kind == "dev"));
        assert!(
            deps.iter()
                .any(|d| d.name == "react-dom" && d.kind == "peer")
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "fsevents" && d.kind == "optional")
        );
    }

    #[test]
    fn go_mod_single_line_and_block_form() {
        let deps = parse_go_mod(
            "module example.com/x\n\ngo 1.21\n\nrequire github.com/foo/bar v1.2.3\n\nrequire (\n\tgithub.com/baz/qux v0.1.0\n\tgithub.com/indirect/pkg v2.0.0 // indirect\n)\n",
        );
        assert_eq!(
            names(&deps),
            vec![
                "github.com/foo/bar",
                "github.com/baz/qux",
                "github.com/indirect/pkg"
            ]
        );
        let indirect = deps
            .iter()
            .find(|d| d.name == "github.com/indirect/pkg")
            .unwrap();
        assert_eq!(
            indirect.version_spec.as_deref(),
            Some("v2.0.0"),
            "indirect comment must not leak into the version"
        );
    }

    #[test]
    fn requirements_txt_operators_bare_names_extras_and_comments() {
        let deps = parse_requirements_txt(
            "requests==2.31.0\nflask>=2.0,<3.0\nnumpy\nboto3[crt]==1.28.0  # pinned for X\n# a full-line comment\n\n-r other-requirements.txt\n",
        );
        assert_eq!(names(&deps), vec!["requests", "flask", "numpy", "boto3"]);
        let numpy = deps.iter().find(|d| d.name == "numpy").unwrap();
        assert_eq!(numpy.version_spec, None);
        let boto3 = deps.iter().find(|d| d.name == "boto3").unwrap();
        assert_eq!(boto3.version_spec.as_deref(), Some("==1.28.0"));
    }

    #[test]
    fn pyproject_toml_pep621_and_poetry_shapes() {
        let deps = parse_pyproject_toml(
            r#"
[project]
dependencies = ["requests>=2.0", "click"]

[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"

[tool.poetry.dev-dependencies]
pytest = "^7.0"
"#,
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "requests" && d.version_spec.as_deref() == Some(">=2.0"))
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "click" && d.version_spec.is_none())
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "fastapi" && d.kind == "runtime")
        );
        assert!(
            !deps.iter().any(|d| d.name == "python"),
            "python version constraint is not a dependency"
        );
        assert!(deps.iter().any(|d| d.name == "pytest" && d.kind == "dev"));
    }

    #[test]
    fn pyproject_toml_modern_poetry_dependency_groups() {
        let deps = parse_pyproject_toml(
            r#"
[tool.poetry.dependencies]
python = "^3.11"
fastapi = "^0.100"

[tool.poetry.group.dev.dependencies]
pytest = "^7.0"

[tool.poetry.group.docs.dependencies]
mkdocs = "^1.5"
"#,
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "fastapi" && d.kind == "runtime")
        );
        assert!(
            deps.iter().any(|d| d.name == "pytest" && d.kind == "dev"),
            "modern [tool.poetry.group.dev.dependencies] must be scanned: {deps:?}"
        );
        assert!(
            deps.iter().any(|d| d.name == "mkdocs" && d.kind == "dev"),
            "an arbitrarily-named group (docs) must still be scanned as dev: {deps:?}"
        );
        assert!(
            !deps.iter().any(|d| d.name == "python"),
            "python version constraint is not a dependency"
        );
    }

    #[test]
    fn malformed_manifests_yield_empty_not_a_panic() {
        assert!(parse_cargo_toml("not valid toml {{{").is_empty());
        assert!(parse_package_json("not json").is_empty());
        assert!(parse_pyproject_toml("not valid toml {{{").is_empty());
    }

    #[test]
    fn scan_project_walks_real_files_and_skips_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/some-pkg")).unwrap();
        std::fs::write(
            dir.path().join("node_modules/some-pkg/package.json"),
            r#"{"dependencies": {"should-not-appear": "1.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "^18.0.0"}}"#,
        )
        .unwrap();

        let deps = scan_project(dir.path(), &[]);
        assert!(
            deps.iter()
                .any(|d| d.name == "serde" && d.manifest_path == "Cargo.toml")
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "react" && d.manifest_path == "package.json")
        );
        assert!(
            !deps.iter().any(|d| d.name == "should-not-appear"),
            "node_modules must be skipped by the built-in IGNORE_DIRS walker gate: {deps:?}"
        );
    }

    #[test]
    fn compute_package_dependencies_persists_and_full_recomputes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();

        compute_package_dependencies(&conn, dir.path(), &[]).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_dependencies", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);

        // Manifest changed (dependency removed) -- next compute must not
        // leave the old row behind (full recompute, same posture as
        // graph::digest::compute_digests).
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        compute_package_dependencies(&conn, dir.path(), &[]).unwrap();
        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM package_dependencies", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            count_after, 0,
            "removed dependency's stale row must not survive a recompute"
        );
    }
}
