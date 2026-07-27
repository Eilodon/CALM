//! Go module path, read from `go.mod`, plus the stdlib rule that depends on it.
//!
//! Go import resolution was measured exactly backwards on the real gin corpus:
//! every one of gin's OWN module-path imports
//! (`github.com/gin-gonic/gin/internal/bytesconv`, ...) stayed `to_path = NULL`,
//! while `errors`, `path`, `context` and `path/filepath` — all standard library
//! — "resolved" to gin's own same-named `errors.go`/`path.go`/`context.go`.
//! So 100% of what it resolved was wrong and 100% of what was genuinely
//! first-party was missed.
//!
//! Both halves come from one fact Go states as a language rule: an import
//! path's first element is a domain name (it contains a dot) for everything
//! outside the standard library, and the standard library alone gets the
//! dotless names. The module's own path, declared in `go.mod`, is the prefix
//! that turns a first-party import into a directory in the tree.

use std::path::Path;

#[derive(Clone, Default)]
pub struct GoModule {
    /// The `module <path>` line from `go.mod` at the project root, if any.
    module_path: Option<String>,
}

impl GoModule {
    /// Never fails — no `go.mod` (or an unreadable one) just leaves every Go
    /// import unresolved rather than mis-resolved, the same silent-degrade
    /// philosophy as the other per-ecosystem maps in this module.
    pub fn build(project_root: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(project_root.join("go.mod")) else {
            return Self::default();
        };
        let module_path = text.lines().find_map(|l| {
            let rest = l.trim_start().strip_prefix("module ")?;
            let name = rest.split("//").next().unwrap_or(rest).trim();
            (!name.is_empty()).then(|| name.to_string())
        });
        Self { module_path }
    }

    /// Is `spec` an import of the Go standard library?
    ///
    /// The rule is Go's own: outside the standard library every import path
    /// begins with a domain (`github.com/...`, `example.com/...`), so a
    /// dotless first element means stdlib. The module's own path is checked
    /// first because a module MAY legally be declared without a dot (`module
    /// myproject`, common in local-only code) — without that check its own
    /// packages would be misread as stdlib.
    pub fn is_stdlib(&self, spec: &str) -> bool {
        if self.owns(spec) {
            return false;
        }
        let first = spec.split('/').next().unwrap_or(spec);
        !first.is_empty() && !first.contains('.')
    }

    /// Does `spec` name a package inside this module? Exact module path, or
    /// the module path followed by `/`.
    pub fn owns(&self, spec: &str) -> bool {
        let Some(m) = self.module_path.as_deref() else {
            return false;
        };
        spec == m || spec.strip_prefix(m).is_some_and(|r| r.starts_with('/'))
    }

    /// The in-repo directory a first-party import names, e.g. for module
    /// `example.com/proj`, `example.com/proj/helper` -> `helper`. `None` when
    /// `spec` isn't inside this module; empty string for the module root.
    pub fn package_dir(&self, spec: &str) -> Option<String> {
        let m = self.module_path.as_deref()?;
        if spec == m {
            return Some(String::new());
        }
        spec.strip_prefix(m)
            .and_then(|r| r.strip_prefix('/'))
            .map(|r| r.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str, gomod: Option<&str>) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("calm_gomod_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        if let Some(body) = gomod {
            std::fs::write(d.join("go.mod"), body).unwrap();
        }
        d
    }

    #[test]
    fn dotless_first_element_is_stdlib() {
        let d = tmp("std", Some("module example.com/proj\n\ngo 1.21\n"));
        let g = GoModule::build(&d);
        assert!(g.is_stdlib("errors"));
        assert!(g.is_stdlib("net/http"));
        assert!(g.is_stdlib("path/filepath"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn domain_qualified_import_is_not_stdlib() {
        let d = tmp("domain", Some("module example.com/proj\n\ngo 1.21\n"));
        let g = GoModule::build(&d);
        assert!(!g.is_stdlib("github.com/stretchr/testify"));
        assert!(!g.is_stdlib("example.com/proj/helper"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A module may legally be declared without a dot; its own packages must
    /// not then be mistaken for stdlib.
    #[test]
    fn a_dotless_module_owns_its_own_packages() {
        let d = tmp("dotless", Some("module myproject\n\ngo 1.21\n"));
        let g = GoModule::build(&d);
        assert!(!g.is_stdlib("myproject/helper"));
        assert!(g.is_stdlib("errors"));
        assert_eq!(g.package_dir("myproject/helper").as_deref(), Some("helper"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn package_dir_strips_the_module_prefix() {
        let d = tmp("dir", Some("module example.com/proj\n"));
        let g = GoModule::build(&d);
        assert_eq!(
            g.package_dir("example.com/proj/internal/x").as_deref(),
            Some("internal/x")
        );
        assert_eq!(g.package_dir("example.com/proj").as_deref(), Some(""));
        assert_eq!(g.package_dir("github.com/other/pkg"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A module path sharing a prefix must not be treated as ours:
    /// `example.com/projector` is not inside `example.com/proj`.
    #[test]
    fn a_prefix_collision_is_not_ownership() {
        let d = tmp("prefix", Some("module example.com/proj\n"));
        let g = GoModule::build(&d);
        assert!(!g.owns("example.com/projector"));
        assert_eq!(g.package_dir("example.com/projector"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_go_mod_resolves_nothing_rather_than_guessing() {
        let d = tmp("nomod", None);
        let g = GoModule::build(&d);
        assert!(!g.owns("anything"));
        assert_eq!(g.package_dir("anything"), None);
        // Still knows the language rule even without a module path.
        assert!(g.is_stdlib("errors"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ignores_a_trailing_comment_on_the_module_line() {
        let d = tmp("comment", Some("module example.com/proj // the thing\n"));
        let g = GoModule::build(&d);
        assert!(g.owns("example.com/proj/x"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
