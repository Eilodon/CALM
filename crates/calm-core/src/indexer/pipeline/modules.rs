//! PR#7 (docs/plans/2026-08-19-evidence-architecture-execution-plan.md Part E,
//! Wave 1 slice 5): behavior-preserving extraction from `pipeline.rs` (issue
//! #67 hotspot). Import-target resolution: mapping an `import_edges.
//! module_name` string to an indexed file path, across every language's own
//! module conventions (Rust crate map, JS/TS relative + NodeNext-ESM,
//! Python packages + sys.path, PHP PSR-4, C# namespaces, JVM packages, Go
//! stdlib/package-dir). Move-only -- no logic changed, only relocated.
//!
//! `resolve_import_targets` is `pub(super)` (not plain private) since Rust
//! module privacy is "visible in the defining module and its descendants" --
//! `pipeline.rs` is this module's ANCESTOR, not a descendant, so plain `fn`
//! would be invisible to it (verified via `callers()` before this move: it's
//! called by `rebuild_graph`/`incremental_graph_update`, both still in
//! pipeline.rs). The other nine functions here
//! (resolve_rust_module/resolve_candidates/resolve_module_to_path/
//! strip_js_emit_extension/own_module_dir/python_package_root/parent_of/
//! join_rel/normalize_rel) are called only by each other within this same
//! cluster (verified via grep before the move: none of their names appear
//! anywhere else in pipeline.rs), so they stay plain private.
//! `ResolutionMaps` stays defined in `pipeline.rs` (already `pub`, not
//! moved) -- pulled in via `super::`.

use rayon::prelude::*;
use std::collections::HashSet;

use super::ResolutionMaps;

/// Best-effort resolution of `import_edges.to_path` against indexed files, so the
/// `dependencies` tool's `imported_by` direction works for in-project imports.
/// External modules (no matching file) keep `to_path = NULL`.
pub(super) fn resolve_import_targets(
    tx: &rusqlite::Transaction,
    maps: &ResolutionMaps,
) -> rusqlite::Result<()> {
    let known: HashSet<String> = {
        let mut stmt = tx.prepare("SELECT path FROM file_index")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    };
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare("SELECT id, from_path, module_name FROM import_edges")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // Candidate-path resolution is pure CPU work against a shared, read-only
    // `known` set, so it runs in parallel; the UPDATE loop stays sequential.
    let targets: Vec<Option<String>> = rows
        .par_iter()
        .map(|(_, from_path, module)| resolve_module_to_path(from_path, module, &known, maps))
        .collect();

    let mut ustmt = tx.prepare("UPDATE import_edges SET to_path = ?1 WHERE id = ?2")?;
    for ((id, _, _), target) in rows.iter().zip(targets.iter()) {
        if let Some(target) = target {
            ustmt.execute(rusqlite::params![target, id])?;
        }
    }
    Ok(())
}
/// Resolve a Rust `use` module path to an indexed file, using the workspace
/// crate map. Handles `crate::`, `self::`, an external crate-name prefix, and a
/// best-effort `super::`. Returns `None` for paths that leave the workspace
/// (std, third-party crates) — those correctly keep `to_path = NULL`.
///
/// `super::` is ambiguous between two real Rust module layouts: the older
/// `foo/mod.rs`-per-directory convention (climbing one filesystem directory
/// per `super` is correct) and the modern 2018-edition `foo.rs` + `foo/`
/// sibling-submodule convention, where files inside `foo/` (e.g.
/// `tools/common.rs` and `tools/guardrails.rs`, both submodules of `tools`)
/// are already siblings of each other — so a single `super` hop from one to
/// reach the other resolves *within the same directory*, not one level up.
/// For a single `super`, the same-directory hypothesis is tried first (it's
/// the dominant modern convention, and the one this very codebase uses),
/// falling back to the climbed-directory interpretation only if that misses.
/// A miss on both falls back to `None`, never a wrong edge — see
/// `resolve_candidates`'s `allow_root_fallback` for the guarantee this
/// depends on.
fn resolve_rust_module(
    from_path: &str,
    module: &str,
    crate_map: &crate::indexer::crate_map::CrateMap,
    known: &HashSet<String>,
) -> Option<String> {
    let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let from_dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    // (base directory to resolve the remaining segments under, remaining
    // segments, whether a single trailing segment may fall back to base_dir's
    // OWN `.rs`/`mod.rs`/`lib.rs` file — only sound when base_dir is a
    // *verified* crate/module root, never a `super`/`self`-climbed ancestor).
    let (base_dir, rest, allow_root_fallback): (String, &[&str], bool) = match segs[0] {
        "crate" => {
            let (_, root) = crate_map.crate_of_file(from_path)?;
            (root.to_string(), &segs[1..], true)
        }
        "self" => (from_dir.clone(), &segs[1..], false),
        "super" => {
            let mut dir = from_dir.clone();
            let mut i = 0;
            while i < segs.len() && segs[i] == "super" {
                dir = parent_of(&dir);
                i += 1;
            }
            if i == 1 {
                // `super::x` at the top level of this file: one module up is
                // the file's own directory.
                if let Some(hit) = resolve_candidates(&from_dir, &segs[1..], false, known) {
                    return Some(hit);
                }
                // The same `use super::x;` written *inside* an inline `mod`
                // block (overwhelmingly `#[cfg(test)] mod tests`) means one
                // level up from **that block** — i.e. this file's own module,
                // whose directory is `foo/` for `foo.rs`. The branch above
                // never looks there (it only ever sees `foo.rs`'s parent), so
                // every `use super::sibling;` inside a test module used to
                // resolve to nothing. `mod.rs`/`lib.rs`/`main.rs` already own
                // their directory and are covered above, so `own_module_dir`
                // returns `None` for them. Tried second so a genuine
                // top-level `super::x` still wins when both could match.
                if let Some(own) = own_module_dir(from_path)
                    && let Some(hit) = resolve_candidates(&own, &segs[1..], false, known)
                {
                    return Some(hit);
                }
            }
            (dir, &segs[i..], false)
        }
        other => {
            // Rust's "uniform paths": an unprefixed leading segment in a `use`
            // is looked up as an external crate name first; if it isn't one,
            // the path is implicitly relative to the *importing file's own*
            // crate root (e.g. `use engine::Engine;` inside that crate's own
            // `lib.rs` means the same as `use crate::engine::Engine;`).
            match crate_map.root_of(&other.replace('-', "_")) {
                Some(root) => (root.to_string(), &segs[1..], true),
                None => {
                    // ...and "in scope" includes a module declared in *this
                    // same file* — the `pub mod overlay; pub use overlay::X;`
                    // re-export façade. That module's files live under the
                    // importing file's own module directory, which the crate-
                    // root fallback below never looks at (it only matches a
                    // module sitting directly at the crate root). Tried first
                    // because uniform paths give an in-scope module priority
                    // over a same-named item at the root.
                    let own = own_module_dir(from_path).unwrap_or_else(|| from_dir.clone());
                    if let Some(hit) = resolve_candidates(&own, &segs, false, known) {
                        return Some(hit);
                    }
                    let (_, root) = crate_map.crate_of_file(from_path)?;
                    (root.to_string(), segs.as_slice(), false)
                }
            }
        }
    };

    resolve_candidates(&base_dir, rest, allow_root_fallback, known)
}

/// Try resolving `rest` (module segments after the `crate`/`self`/`super`/
/// external-crate prefix has been stripped) under `base_dir` against the set
/// of indexed files (`known`). Tries the full remaining path and, for item
/// imports (`use a::b::Item`), its parent directory — plus `mod.rs`/`lib.rs`
/// directory-index conventions.
///
/// `allow_root_fallback` additionally permits a single trailing segment
/// (`use crate::Item`) to match `base_dir`'s own `.rs`/`mod.rs`/`lib.rs` file
/// directly — a genuine re-export-at-the-root pattern, but only sound when
/// `base_dir` is a *verified* crate/module root (the `crate::` branch and the
/// named-external-crate case, both backed by `CrateMap`). It must stay
/// `false` for `super`/`self`, where `base_dir` is merely a climbed filesystem
/// ancestor with no such guarantee: enabling it there previously let a
/// `super::sibling` import spuriously match the *crate's own* `lib.rs` — a
/// confidently wrong `to_path` — whenever the climbed ancestor directory
/// happened to coincide with the crate root, instead of the honest `None`
/// this function is documented to fall back to on a genuine miss.
fn resolve_candidates(
    base_dir: &str,
    rest: &[&str],
    allow_root_fallback: bool,
    known: &HashSet<String>,
) -> Option<String> {
    let joined = rest.join("/");
    let mut bases: Vec<String> = Vec::new();
    if joined.is_empty() {
        bases.push(base_dir.to_string());
    } else {
        bases.push(join_rel(base_dir, &joined));
        if let Some((parent, _)) = joined.rsplit_once('/') {
            bases.push(join_rel(base_dir, parent));
        } else if allow_root_fallback {
            bases.push(base_dir.to_string());
        }
    }

    for base in &bases {
        let base = base.trim_start_matches('/');
        for cand in [
            format!("{base}.rs"),
            format!("{base}/mod.rs"),
            format!("{base}/lib.rs"),
        ] {
            if known.contains(&cand) {
                return Some(cand);
            }
        }
        if known.contains(base) {
            return Some(base.to_string());
        }
    }
    None
}

/// Map a module/path string to an indexed file path, trying the conventions of
/// all six languages (dotted, scoped, and JS-relative) plus common index files.
fn resolve_module_to_path(
    from_path: &str,
    module: &str,
    known: &HashSet<String>,
    maps: &ResolutionMaps,
) -> Option<String> {
    let m = module.trim().trim_matches(|c| c == '"' || c == '\'');
    if m.is_empty() {
        return None;
    }
    // Rust: use the crate-map-aware resolver first; fall through to the generic
    // convention scan only if it finds nothing (keeps single-crate repos working
    // even when the crate map is empty).
    if from_path.ends_with(".rs")
        && let Some(hit) = resolve_rust_module(from_path, m, &maps.crate_map, known)
    {
        return Some(hit);
    }

    // PHP: a `use App\Service\Foo;`-style backslash-separated namespace path
    // needs PSR-4 (composer.json's `autoload.psr-4` prefix→dir table) to
    // resolve at all — PHP namespaces don't reliably mirror directory
    // structure the way Go packages do, so the generic dotted-module scan
    // below (which doesn't even split on `\`) can't find these. Falls
    // through to that generic scan (a harmless no-op for a `\`-containing
    // module) if PSR-4 is empty (no composer.json) or the prefix doesn't
    // match anything.
    if from_path.ends_with(".php")
        && m.contains('\\')
        && let Some(hit) = maps.psr4.resolve(m)
        && known.contains(&hit)
    {
        return Some(hit);
    }

    // C#: `using MultiLang;` names a namespace directly (no PSR-4-style
    // prefix→dir table needed — `csharp_namespace::NamespaceMap` already
    // read every real `namespace` declaration). Only resolves when exactly
    // one file declares that namespace — a namespace legitimately spanning
    // several files has no single correct `to_path` (single-valued column,
    // see `NamespaceMap::resolve`'s doc comment), so it's left `None` rather
    // than guessing one of them.
    if from_path.ends_with(".cs")
        && let Some(hit) = maps.namespace_map.resolve(m)
        && known.contains(hit)
    {
        return Some(hit.to_string());
    }

    // JVM: `import a.b.C;` names package `a.b` plus type `C`. Maven/Gradle
    // put sources under `src/main/java/<package path>`, which the generic
    // project-root/`src/` scan below never finds, so every Java/Kotlin/Groovy
    // import used to stay NULL. The package declaration inside each file is
    // the layout-agnostic answer, and the only one that can separate a
    // first-party import from a JDK one here -- Maven's own tree contains
    // directories literally named `java`/`org`/`com`.
    if matches!(
        std::path::Path::new(from_path)
            .extension()
            .and_then(|e| e.to_str()),
        Some("java" | "kt" | "kts" | "groovy" | "scala")
    ) {
        // A JVM import is always a package-qualified name; the generic scan
        // below can only mis-bind it (`java.io.Serializable` -> some local
        // `Serializable.java`), so this is the sole authority for these files.
        return maps
            .jvm
            .resolve_type(m)
            .filter(|hit| known.contains(*hit))
            .map(str::to_string);
    }

    // Go: the standard library owns exactly the import paths whose first
    // element has no dot; everything else is domain-qualified. Without that
    // rule `import "errors"` binds to whatever `errors.go` the project has --
    // measured on gin, where `errors`/`path`/`context` ALL mis-resolved to
    // gin's own files while gin's real module-path imports resolved to
    // nothing. Both halves are fixed here.
    if from_path.ends_with(".go") {
        if maps.go.is_stdlib(m) {
            return None;
        }
        if let Some(dir) = maps.go.package_dir(m) {
            // A Go package is a directory: any indexed `.go` file inside it
            // identifies the package. Deterministic pick so repeated indexes
            // agree, since `import_edges.to_path` holds a single target.
            let prefix = if dir.is_empty() {
                String::new()
            } else {
                format!("{dir}/")
            };
            let mut in_pkg: Vec<&str> = known
                .iter()
                .map(String::as_str)
                .filter(|p| {
                    p.ends_with(".go") && p.starts_with(&prefix) && !p[prefix.len()..].contains('/')
                })
                .collect();
            in_pkg.sort_unstable();
            return in_pkg.first().map(|p| p.to_string());
        }
        // Domain-qualified but outside this module: a third-party dependency.
        return None;
    }

    // Build candidate base paths (without extension), forward-slash normalised.
    let mut bases: Vec<String> = Vec::new();
    let from_dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    if m.starts_with("./") || m.starts_with("../") {
        // JS/TS relative import, resolved against the importing file's directory.
        bases.push(normalize_rel(&from_dir, m));
        // TS/NodeNext-ESM convention: source imports a sibling `.ts` module
        // using the *compiled-output* extension (`./foo.js` referring to
        // `foo.ts` on disk) — required by `"moduleResolution": "node16"` /
        // `"nodenext"` / `"bundler"` since the emitted JS must contain a
        // specifier that resolves at runtime. The exact-match candidate
        // above only ever finds a *real* `.js` file; without this second,
        // extension-stripped candidate the EXTS loop below can only append
        // more extensions onto the specifier's own `.js` suffix (producing
        // nonsense like `foo.js.ts`) and never tries the real `foo.ts`.
        if let Some(stripped) = strip_js_emit_extension(m) {
            bases.push(normalize_rel(&from_dir, stripped));
        }
    } else if let Some(stripped) = m.strip_prefix('.') {
        // Python relative import: leading dots climb packages.
        let ups = m.len() - m.trim_start_matches('.').len();
        let tail = stripped.trim_start_matches('.').replace('.', "/");
        let mut dir = from_dir.clone();
        for _ in 1..ups {
            dir = parent_of(&dir);
        }
        bases.push(if tail.is_empty() {
            dir
        } else {
            join_rel(&dir, &tail)
        });
    } else {
        // Absolute/dotted/scoped module, relative to project root.
        let norm = m.replace("::", "/").replace('.', "/");
        let norm = norm
            .trim_start_matches("crate/")
            .trim_start_matches("self/")
            .trim_start_matches("super/")
            .to_string();
        // Python: an absolute `import a.b` / `from a.b import x` resolves
        // against the `sys.path` entries in effect for the *importing* file,
        // not against the project root. The nearest such entry is the file's
        // own package root; after that, any `__file__`-anchored
        // `sys.path.insert(...)` the file performs itself. Both are tried
        // ahead of the project-root/`src/` guesses below, which only ever
        // happen to be right for a module sitting at the repo root — without
        // them an ordinary intra-package import (`from pkg.helper import x`)
        // never resolved at all.
        if from_path.ends_with(".py") {
            bases.push(join_rel(&python_package_root(from_path, known), &norm));
            for root in maps.pysys.roots_for(from_path) {
                bases.push(join_rel(root, &norm));
            }
        }
        // The full path, and — for item imports like `use a::b::Item` — its parent.
        // Also try a conventional `src/` source root.
        bases.push(norm.clone());
        if let Some((parent, _)) = norm.rsplit_once('/') {
            bases.push(parent.to_string());
            bases.push(format!("src/{parent}"));
        }
        bases.push(format!("src/{norm}"));
    }

    const EXTS: &[&str] = &[".py", ".rs", ".go", ".ts", ".tsx", ".js", ".jsx", ".java"];
    const INDEX_SUFFIXES: &[&str] = &[
        "/__init__.py",
        "/mod.rs",
        "/index.ts",
        "/index.tsx",
        "/index.js",
    ];
    for base in &bases {
        let base = base.trim_start_matches("./");
        if known.contains(base) {
            return Some(base.to_string());
        }
        for e in EXTS {
            let c = format!("{base}{e}");
            if known.contains(&c) {
                return Some(c);
            }
        }
        for s in INDEX_SUFFIXES {
            let c = format!("{base}{s}");
            if known.contains(&c) {
                return Some(c);
            }
        }
    }
    None
}
/// Strips a compiled/emitted JS-family extension (`.mjs`/`.cjs`/`.jsx`/`.js`)
/// from a relative import specifier, or `None` if it doesn't end in one.
fn strip_js_emit_extension(m: &str) -> Option<&str> {
    for ext in [".mjs", ".cjs", ".jsx", ".js"] {
        if let Some(s) = m.strip_suffix(ext) {
            return Some(s);
        }
    }
    None
}

/// The directory a Rust file's own module owns — where its submodule files
/// live: `a/b/foo.rs` → `a/b/foo`. `None` for the three file names that already
/// *are* their containing directory's module (`mod.rs`/`lib.rs`/`main.rs`), for
/// which the caller's plain parent-directory candidate is already correct.
fn own_module_dir(from_path: &str) -> Option<String> {
    let path = std::path::Path::new(from_path);
    let stem = path.file_stem()?.to_str()?;
    if matches!(stem, "mod" | "lib" | "main") {
        return None;
    }
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    Some(join_rel(&dir, stem))
}

/// The `sys.path` entry a Python file's own package hangs off: walk out of the
/// `__init__.py` chain the file sits in (`pkg/sub/mod.py`, with both
/// `pkg/__init__.py` and `pkg/sub/__init__.py` present, → the directory holding
/// `pkg`). A file in no package is its own root — exactly what `python foo.py`
/// puts on `sys.path[0]`. PEP 420 namespace packages (no `__init__.py`) stop
/// the walk early, which is the conservative direction: a shorter root only
/// ever yields fewer candidates, never a wrong one.
fn python_package_root(from_path: &str, known: &HashSet<String>) -> String {
    let mut dir = std::path::Path::new(from_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    while !dir.is_empty() && known.contains(&format!("{dir}/__init__.py")) {
        dir = parent_of(&dir);
    }
    dir
}

fn parent_of(dir: &str) -> String {
    std::path::Path::new(dir)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

fn join_rel(dir: &str, tail: &str) -> String {
    if dir.is_empty() {
        tail.to_string()
    } else {
        format!("{dir}/{tail}")
    }
}

/// Resolve `./`, `../` and `.` components of a JS-style relative path against a base dir.
fn normalize_rel(base_dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}
