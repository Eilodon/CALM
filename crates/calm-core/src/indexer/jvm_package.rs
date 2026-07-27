//! JVM package scan: reads every Java/Kotlin/Groovy/Scala file's `package`
//! declaration and records which files declare which package, so `import
//! a.b.C;` can be resolved against a real declaration instead of a build-layout
//! guess — the same "read real files, build an in-memory map" pattern as
//! `crate_map::CrateMap`, `psr4::Psr4Map` and `csharp_namespace::NamespaceMap`.
//!
//! Why this exists at all: `resolve_module_to_path`'s generic dotted-module
//! scan only ever tries the project root and a `src/` prefix, but Maven and
//! Gradle put sources under `src/main/java/<package path>`. Every Java import
//! therefore stayed `to_path = NULL` — measured 0 of 22 genuinely first-party
//! imports resolved on spring-petclinic, with Kotlin (14/14) and Groovy
//! (459/459) equally blank. `dependencies`' `imported_by` direction and the
//! `[[boundaries]]` fitness gate both read that column.
//!
//! Why the package declaration rather than a hardcoded `src/main/java` prefix:
//! the declaration is layout-agnostic, so it also covers Gradle custom source
//! sets, Bazel, and plain `javac` trees. It is also the only signal that can
//! tell a first-party import from a JDK one here — Maven's own layout contains
//! directories literally named `java`, `org` and `com`, the exact root segments
//! of `java.io.*` and `org.springframework.*`, so no directory-name heuristic
//! can separate them.
//!
//! Deliberately a line scan, not a tree-sitter walk (unlike `csharp_namespace`):
//! a `package` declaration is a single leading line in all four languages, and
//! a line scan keeps working when a grammar isn't compiled in — Scala and
//! Groovy are opt-in `lang-*` features, so a grammar-based reader would
//! silently return nothing for them in a default build.

use std::collections::HashMap;
use std::path::Path;

/// Extensions whose files carry a JVM-style `package` declaration.
const JVM_EXTENSIONS: &[&str] = &["java", "kt", "kts", "groovy", "scala"];

/// A `package` declaration is always in a file's header; bounding the scan
/// keeps a stray `package` inside a long multi-line string from being read as
/// one, and keeps the cost off large generated sources.
const HEADER_SCAN_LINES: usize = 200;

#[derive(Clone, Default)]
pub struct JvmPackageMap {
    /// package (dotted, exactly as written) -> every project-root-relative,
    /// forward-slashed file declaring it. Deduped; one package legitimately
    /// spans many files, so this is not the 1:1 relationship `CrateMap` has.
    files_by_package: HashMap<String, Vec<String>>,
}

impl JvmPackageMap {
    /// Never fails — an empty map just means these upgrades are skipped, the
    /// same silent-degrade philosophy as `CrateMap`/`Psr4Map`/`NamespaceMap`.
    pub fn build(project_root: &Path) -> Self {
        let mut files_by_package: HashMap<String, Vec<String>> = HashMap::new();
        for entry in crate::walk::build_walker(project_root, &[], false) {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let is_jvm = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| JVM_EXTENSIONS.contains(&e));
            if !is_jvm {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(project_root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            for pkg in packages_declared_in(&source) {
                let files = files_by_package.entry(pkg).or_default();
                if !files.contains(&rel) {
                    files.push(rel.clone());
                }
            }
        }
        Self { files_by_package }
    }

    pub fn is_empty(&self) -> bool {
        self.files_by_package.is_empty()
    }

    /// Resolve a fully-qualified type reference (`org.example.app.model.Person`)
    /// to the file declaring it: split off the last segment as the type name,
    /// treat the rest as the package, and take the file in that package whose
    /// stem matches the type.
    ///
    /// Falls back to treating the LAST TWO segments as `Type.member`, which is
    /// what a Java static import (`import static a.b.C.method;`) looks like.
    ///
    /// Returns `None` for a wildcard (`a.b.*`) and for a package that no file
    /// declares — `import_edges.to_path` is single-valued, so an import naming
    /// a package rather than one type has no single correct target and is left
    /// unresolved rather than guessed at, the same rule `NamespaceMap::resolve`
    /// follows.
    pub fn resolve_type(&self, dotted: &str) -> Option<&str> {
        self.lookup(dotted).or_else(|| {
            // `a.b.C.method` -> package `a.b`, type `C`.
            let (head, _member) = dotted.rsplit_once('.')?;
            self.lookup(head)
        })
    }

    fn lookup(&self, dotted: &str) -> Option<&str> {
        let (package, type_name) = dotted.rsplit_once('.')?;
        if type_name.is_empty() || type_name == "*" {
            return None;
        }
        self.files_by_package.get(package)?.iter().find_map(|f| {
            let stem = Path::new(f).file_stem().and_then(|s| s.to_str())?;
            (stem == type_name).then_some(f.as_str())
        })
    }
}

/// Every `package` declaration in `source`'s header, in order.
///
/// Java/Kotlin/Groovy allow exactly one; Scala allows several (including the
/// chained `package a.b` + `package c` form, which really means `a.b.c`).
/// Each is recorded as written rather than reassembled — a file is then
/// findable under any of the prefixes it declares, which is over-inclusive in
/// exactly the direction that costs nothing here: `lookup` still requires the
/// file's own stem to match the imported type name.
fn packages_declared_in(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines().take(HEADER_SCAN_LINES) {
        let Some(rest) = line.trim_start().strip_prefix("package ") else {
            continue;
        };
        let name: String = rest
            .trim()
            .trim_end_matches(';')
            .trim()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '$')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("calm_jvmpkg_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn resolves_a_type_under_a_maven_source_root() {
        let d = tmp("maven");
        write(
            &d,
            "src/main/java/org/example/model/Person.java",
            "package org.example.model;\npublic class Person {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(
            m.resolve_type("org.example.model.Person"),
            Some("src/main/java/org/example/model/Person.java")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn does_not_resolve_a_package_no_file_declares() {
        let d = tmp("jdk");
        write(
            &d,
            "src/main/java/org/example/model/Person.java",
            "package org.example.model;\npublic class Person {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(m.resolve_type("java.io.Serializable"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The package is declared but the type is not — a same-named class in a
    /// DIFFERENT package must not be substituted.
    #[test]
    fn does_not_substitute_a_same_named_type_from_another_package() {
        let d = tmp("othertype");
        write(
            &d,
            "src/main/java/org/example/a/Thing.java",
            "package org.example.a;\npublic class Thing {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(m.resolve_type("org.example.b.Thing"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wildcard_import_has_no_single_target() {
        let d = tmp("wildcard");
        write(
            &d,
            "src/main/java/org/example/model/Person.java",
            "package org.example.model;\npublic class Person {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(m.resolve_type("org.example.model.*"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `import static a.b.C.method;` names a member, not a type — the fallback
    /// drops the trailing member and still finds `C`.
    #[test]
    fn static_import_resolves_to_the_declaring_type() {
        let d = tmp("static");
        write(
            &d,
            "src/main/java/org/example/util/Assertions.java",
            "package org.example.util;\npublic class Assertions {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(
            m.resolve_type("org.example.util.Assertions.assertThat"),
            Some("src/main/java/org/example/util/Assertions.java")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn covers_kotlin_and_groovy_without_a_trailing_semicolon() {
        let d = tmp("kt");
        write(
            &d,
            "src/main/kotlin/org/example/Widget.kt",
            "package org.example\n\nclass Widget\n",
        );
        write(
            &d,
            "src/main/groovy/org/example/Gadget.groovy",
            "package org.example\n\nclass Gadget {}\n",
        );
        let m = JvmPackageMap::build(&d);
        assert_eq!(
            m.resolve_type("org.example.Widget"),
            Some("src/main/kotlin/org/example/Widget.kt")
        );
        assert_eq!(
            m.resolve_type("org.example.Gadget"),
            Some("src/main/groovy/org/example/Gadget.groovy")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_jvm_files_yields_empty_map() {
        let d = tmp("empty");
        write(&d, "main.py", "def f():\n    pass\n");
        assert!(JvmPackageMap::build(&d).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
