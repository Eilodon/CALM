//! One row of the LSP provider table — mirrors `scip::provider::ScipProvider`
//! (`scip/provider.rs:46-81`) in spirit, but is deliberately smaller: an LSP
//! server has no batch output file and no cache key the way a SCIP indexer
//! does (`run_lsp_overlay` resolves each call site live against a persistent
//! session instead of ingesting a cached `.scip` dump), so this table only
//! needs to answer three questions per language: which `file_index.language`
//! values does this provider cover, how do we find its binary, and where
//! does its `MinInterval`-policy sidecar live.

use std::path::{Path, PathBuf};

use crate::scip::runner::{binary_runs, dirs_home};

/// One row of the provider table.
pub struct LspProvider {
    /// Display name for log lines, e.g. `"rust-analyzer"`.
    pub name: &'static str,
    /// `file_index.language` values this provider resolves candidate call
    /// edges for — gates `has_any_lang_files` (skip spawning a server for a
    /// project with none of these files) and filters `load_candidate_edges`
    /// (a gopls session must never be asked to open a `.rs` file). A
    /// provider spanning more than one value (`CLANGD` covers `c` and
    /// `cpp`) follows `scip::provider::TYPESCRIPT`'s precedent for the same
    /// shape.
    pub langs: &'static [&'static str],
    /// Locate a usable binary: explicit override first, then this server's
    /// own PATH/toolchain probe.
    pub resolve_binary: fn(Option<&str>, &Path) -> Option<PathBuf>,
    /// `.calm/<this>` sidecar for `RefreshPolicy::MinInterval` last-run
    /// tracking — kept distinct per provider so a second language's overlay
    /// can't clobber another's timestamp (`scip::provider::ScipProvider::
    /// cache_file_name`'s exact reasoning). `RUST_ANALYZER` keeps the
    /// pre-existing unqualified `lsp-stats.json` name so an existing
    /// checkout's Rust stats aren't orphaned by this generalization.
    pub stats_file_name: &'static str,
}

pub const RUST_ANALYZER: LspProvider = LspProvider {
    name: "rust-analyzer",
    langs: &["rust"],
    resolve_binary: crate::scip::runner::resolve_binary,
    stats_file_name: "lsp-stats.json",
};

pub const GOPLS: LspProvider = LspProvider {
    name: "gopls",
    langs: &["go"],
    resolve_binary: gopls_resolve_binary,
    stats_file_name: "lsp-gopls-stats.json",
};

pub const CLANGD: LspProvider = LspProvider {
    name: "clangd",
    langs: &["c", "cpp"],
    resolve_binary: clangd_resolve_binary,
    stats_file_name: "lsp-clangd-stats.json",
};

/// PATH, then `$GOBIN`, then `~/go/bin` — same search shape as
/// `scip::runner::go_resolve_binary`, different binary name (`gopls`, the
/// persistent LSP server `go install golang.org/x/tools/gopls@latest`
/// installs, not `scip-go`'s one-shot batch indexer). Uses `gopls_runs`
/// (its own `version` SUBCOMMAND, not the shared `binary_runs`'s `--version`
/// FLAG) — confirmed live (2026-07-11): `gopls --version` errors with exit
/// 2 ("flag provided but not defined: -version") and prints its own help
/// text to stdout instead, so `binary_runs` would have silently reported a
/// real, working gopls as "not found." Same failure class already
/// documented for PHP's `php_binary_runs` in `scip::runner`.
fn gopls_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("gopls")); // PATH lookup via which-style probe
    if let Some(gobin) = std::env::var_os("GOBIN") {
        candidates.push(PathBuf::from(gobin).join("gopls"));
    }
    if let Some(home) = dirs_home() {
        candidates.push(home.join("go").join("bin").join("gopls"));
    }
    candidates.into_iter().find(|c| gopls_runs(c))
}

/// `gopls version` (subcommand) — see `gopls_resolve_binary`'s doc comment
/// for why this can't reuse the shared `binary_runs` (`--version` flag).
fn gopls_runs(path: &Path) -> bool {
    std::process::Command::new(path)
        .arg("version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// PATH (`clangd`), then Debian/Ubuntu's unaliased versioned package names.
/// Confirmed live (2026-07-11, Ubuntu 24.04 `noble`): `apt-get install
/// clangd` pulls in `clangd-18` as a dependency and the `clangd` metapackage
/// itself provides the `/usr/bin/clangd` alternative via
/// `update-alternatives` — but a bare `clangd` on `PATH` isn't guaranteed on
/// every distro/install method, so the versioned fallback stays as a safety
/// net rather than an assumption.
fn clangd_resolve_binary(override_bin: Option<&str>, _root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(b) = override_bin {
        candidates.push(PathBuf::from(b));
    }
    candidates.push(PathBuf::from("clangd")); // PATH lookup
    for v in ["20", "19", "18", "17", "16", "15", "14"] {
        candidates.push(PathBuf::from(format!("clangd-{v}")));
    }
    candidates.into_iter().find(|c| binary_runs(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gopls_resolve_binary_returns_none_without_a_real_binary() {
        assert!(gopls_resolve_binary(Some("/no/such/gopls-binary"), Path::new(".")).is_none());
    }

    #[test]
    fn clangd_resolve_binary_returns_none_without_a_real_binary() {
        assert!(clangd_resolve_binary(Some("/no/such/clangd-binary"), Path::new(".")).is_none());
    }

    #[test]
    fn providers_cover_the_langs_they_claim_and_nothing_ambiguous() {
        // Cheap sanity lock: a future edit that accidentally overlaps two
        // providers' `langs` would silently double-route candidate edges.
        let all: Vec<&str> = [RUST_ANALYZER.langs, GOPLS.langs, CLANGD.langs]
            .into_iter()
            .flatten()
            .copied()
            .collect();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "provider langs must not overlap");
    }
}
