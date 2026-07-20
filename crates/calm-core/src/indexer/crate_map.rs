use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Maps a workspace's crate names to their source-root directories, so Rust
/// `use other_crate::Item` and `use crate::mod::Item` imports resolve to real
/// indexed files. Built once per index pass.
#[derive(Debug, Default, Clone)]
pub struct CrateMap {
    /// normalized crate name (`-` → `_`) → src-root dir, project-root-relative,
    /// forward-slashed, no trailing slash (e.g. `"crates/calm-core/src"`).
    roots: HashMap<String, String>,
}

impl CrateMap {
    /// Build from `cargo metadata --no-deps` when `cargo` is available; otherwise
    /// fall back to scanning `Cargo.toml` files. Never fails — an empty map just
    /// means cross-crate resolution degrades to today's behavior.
    pub fn build(project_root: &Path) -> Self {
        Self::from_cargo_metadata(project_root)
            .unwrap_or_else(|| Self::from_toml_scan(project_root))
    }

    pub fn root_of(&self, crate_name: &str) -> Option<&str> {
        self.roots.get(crate_name).map(String::as_str)
    }

    /// The (crate name, src-root) that owns `rel_path` — longest src-root prefix.
    pub fn crate_of_file(&self, rel_path: &str) -> Option<(&str, &str)> {
        self.roots
            .iter()
            .filter(|(_, root)| {
                rel_path == root.as_str() || rel_path.starts_with(&format!("{root}/"))
            })
            .max_by_key(|(_, root)| root.len())
            .map(|(name, root)| (name.as_str(), root.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    fn from_cargo_metadata(project_root: &Path) -> Option<Self> {
        let out = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(project_root)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let mut roots = HashMap::new();
        for pkg in json.get("packages")?.as_array()? {
            let name = pkg.get("name")?.as_str()?.replace('-', "_");
            // The lib target (fallback: any target) gives the crate's root file.
            let targets = pkg.get("targets")?.as_array()?;
            let root_file = targets
                .iter()
                .find(|t| {
                    t.get("kind")
                        .and_then(|k| k.as_array())
                        .is_some_and(|ks| ks.iter().any(|k| k == "lib"))
                })
                .or_else(|| targets.first())
                .and_then(|t| t.get("src_path"))
                .and_then(|p| p.as_str())?;
            if let Some(rel) = rel_src_dir(project_root, root_file) {
                roots.insert(name, rel);
            }
        }
        if roots.is_empty() {
            None
        } else {
            Some(Self { roots })
        }
    }

    fn from_toml_scan(project_root: &Path) -> Self {
        let mut roots = HashMap::new();
        for entry in crate::walk::build_walker(project_root, &[], false) {
            let Ok(entry) = entry else { continue };
            if entry.file_name() != "Cargo.toml" {
                continue;
            }
            let path = entry.path();
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(doc) = text.parse::<toml::Value>() else {
                continue;
            };
            let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            else {
                continue; // virtual/workspace-only manifest
            };
            // Convention: <manifest_dir>/src is the crate root unless [lib] path overrides.
            let manifest_dir = path.parent().unwrap_or(project_root);
            let lib_path = doc
                .get("lib")
                .and_then(|l| l.get("path"))
                .and_then(|p| p.as_str());
            let src_dir = match lib_path {
                Some(p) => manifest_dir.join(p).parent().map(Path::to_path_buf),
                None => Some(manifest_dir.join("src")),
            };
            if let Some(dir) = src_dir
                && let Some(rel) = rel_dir(project_root, &dir)
            {
                roots.insert(name.replace('-', "_"), rel);
            }
        }
        Self { roots }
    }
}

/// Project-root-relative, forward-slashed parent dir of an absolute src file path.
fn rel_src_dir(project_root: &Path, abs_file: &str) -> Option<String> {
    let parent = Path::new(abs_file).parent()?;
    rel_dir(project_root, parent)
}

fn rel_dir(project_root: &Path, abs_dir: &Path) -> Option<String> {
    let rel = abs_dir.strip_prefix(project_root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust_workspace")
    }

    #[test]
    fn maps_crate_names_to_src_roots() {
        let m = CrateMap::build(&fixture());
        // `-` in the package name is normalized to `_` (matches the identifier
        // used in `use demo_core::...`).
        assert_eq!(m.root_of("demo_core"), Some("core/src"));
        assert_eq!(m.root_of("demo_app"), Some("app/src"));
    }

    #[test]
    fn resolves_owning_crate_of_a_file() {
        let m = CrateMap::build(&fixture());
        let (name, root) = m.crate_of_file("core/src/engine.rs").unwrap();
        assert_eq!(name, "demo_core");
        assert_eq!(root, "core/src");
    }
}
