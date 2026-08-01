//! One row of the LSP provider table — mirrors `scip::provider::ScipProvider`
//! (`scip/provider.rs:46-81`) in spirit, but is deliberately smaller: an LSP
//! server has no batch output file and no cache key the way a SCIP indexer
//! does (`run_lsp_overlay` resolves each call site live against a persistent
//! session instead of ingesting a cached `.scip` dump), so this table only
//! needs to answer three questions per language: which `file_index.language`
//! values does this provider cover, how do we find its binary, and where does
//! its diagnostic stats sidecar live.

use std::path::{Path, PathBuf};

use crate::lsp::client::LspClientProfile;
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
    /// Deterministic argv used to identify the executable that served a proof.
    pub version_args: &'static [&'static str],
    /// Workspace-root-relative inputs that can alter target resolution even
    /// when the CallSite source bytes do not change.
    pub context_inputs: &'static [&'static str],
    /// Explicit launch/initialize contract, included in the provider profile
    /// rather than inherited from a language-agnostic client default.
    pub client_profile: LspClientProfile,
    /// `.calm/<this>` diagnostic stats sidecar — kept distinct per provider so
    /// a second language's on-demand refresh can't clobber another's result.
    /// `RUST_ANALYZER` keeps the pre-existing unqualified `lsp-stats.json`
    /// name so an existing checkout's Rust stats aren't orphaned.
    pub stats_file_name: &'static str,
}

pub const RUST_ANALYZER: LspProvider = LspProvider {
    name: "rust-analyzer",
    langs: &["rust"],
    resolve_binary: crate::scip::runner::resolve_binary,
    version_args: &["--version"],
    context_inputs: &[
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
    ],
    client_profile: LspClientProfile {
        server_args: &[],
        language_id: rust_language_id,
        initialization_options_json: Some("{}"),
        include_workspace_folder: true,
    },
    stats_file_name: "lsp-stats.json",
};

pub const GOPLS: LspProvider = LspProvider {
    name: "gopls",
    langs: &["go"],
    resolve_binary: gopls_resolve_binary,
    version_args: &["version"],
    context_inputs: &["go.mod", "go.sum", "go.work"],
    client_profile: LspClientProfile {
        server_args: &[],
        language_id: go_language_id,
        initialization_options_json: Some("{}"),
        include_workspace_folder: true,
    },
    stats_file_name: "lsp-gopls-stats.json",
};

pub const CLANGD: LspProvider = LspProvider {
    name: "clangd",
    langs: &["c", "cpp"],
    resolve_binary: clangd_resolve_binary,
    version_args: &["--version"],
    context_inputs: &["compile_commands.json", "CMakeLists.txt"],
    client_profile: LspClientProfile {
        server_args: &[],
        language_id: c_family_language_id,
        initialization_options_json: Some("{}"),
        include_workspace_folder: true,
    },
    stats_file_name: "lsp-clangd-stats.json",
};

/// Runtime evidence exposed to MCP callers. `fixture-tested` is a qualification
/// of CALM's deterministic protocol/profile harness; it is deliberately never
/// upgraded to `nightly-verified` locally because that requires a hosted run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspProviderRuntimeStatus {
    pub provider: String,
    pub support_level: String,
    pub binary: Option<String>,
    pub version: Option<String>,
    pub profile_fingerprint: String,
    pub context_fingerprint: String,
    pub candidate_count: usize,
    pub run_status: String,
    pub reason: Option<String>,
}

/// Canonical identity of the reviewed client contract, excluding the resolved
/// binary and its version. It is shared by status reporting and proof
/// provenance so an argv/initialization/workspace-folder change cannot leave
/// an older LSP proof looking current.
pub(crate) fn profile_fingerprint(provider: &LspProvider) -> String {
    let language_ids = provider
        .langs
        .iter()
        .map(|language| {
            let probe_path = match *language {
                "rust" => "profile.rs",
                "go" => "profile.go",
                "c" => "profile.c",
                "cpp" => "profile.cpp",
                _ => "profile.unknown",
            };
            format!(
                "{language}:{}",
                (provider.client_profile.language_id)(Path::new(probe_path))
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    // JSON preserves the boundary between argv elements. Delimiter-joining
    // would collide, for example, ["--version", "verbose"] with one argument
    // containing that delimiter, letting a different client invocation reuse a
    // prior proof fingerprint.
    crate::indexer::pipeline::hash_content(
        &serde_json::json!({
            "name": provider.name,
            "version_args": provider.version_args,
            "context_inputs": provider.context_inputs,
            "server_args": provider.client_profile.server_args,
            "initialization_options_json": provider.client_profile.initialization_options_json,
            "include_workspace_folder": provider.client_profile.include_workspace_folder,
            "language_ids": language_ids,
        })
        .to_string(),
    )
}

/// Full provider provenance persisted with an LSP proof. The executable path
/// and probe output are intentionally part of this fingerprint in addition to
/// the static client profile above.
pub(crate) fn proof_provider_fingerprint(
    provider: &LspProvider,
    binary: &Path,
    version: &str,
) -> String {
    crate::indexer::pipeline::hash_content(&format!(
        "profile={}\nbinary={}\nversion={version}",
        profile_fingerprint(provider),
        binary.display(),
    ))
}

pub fn runtime_status(
    provider: &LspProvider,
    cfg: &crate::config::LspConfig,
    root: &Path,
    run_status: &str,
    candidate_count: usize,
) -> LspProviderRuntimeStatus {
    let profile_fingerprint = profile_fingerprint(provider);
    let context_fingerprint = resolution_context_fingerprint(root, provider.context_inputs);
    let binary = (provider.resolve_binary)(cfg.binary.as_deref(), root);
    let Some(binary) = binary else {
        return LspProviderRuntimeStatus {
            provider: provider.name.to_owned(),
            support_level: "fixture-tested".into(),
            binary: None,
            version: None,
            profile_fingerprint,
            context_fingerprint,
            candidate_count,
            run_status: run_status.to_owned(),
            reason: Some("binary_unavailable".into()),
        };
    };
    let version = std::process::Command::new(&binary)
        .args(provider.version_args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .trim()
            .to_owned()
        })
        .filter(|version| !version.is_empty());
    let reason = version.is_none().then(|| "version_probe_failed".to_owned());
    LspProviderRuntimeStatus {
        provider: provider.name.to_owned(),
        support_level: "fixture-tested".into(),
        binary: Some(binary.display().to_string()),
        version,
        profile_fingerprint,
        context_fingerprint,
        candidate_count,
        run_status: run_status.to_owned(),
        reason,
    }
}

fn resolution_context_fingerprint(root: &Path, inputs: &[&str]) -> String {
    let entries = inputs
        .iter()
        .map(|input| {
            let path = Path::new(input);
            let value = if path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                "<rejected-path>".to_owned()
            } else {
                std::fs::read_to_string(root.join(path))
                    .map(|text| crate::indexer::pipeline::hash_content(&text))
                    .unwrap_or_else(|_| "<missing>".into())
            };
            format!("{input}@{value}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    crate::indexer::pipeline::hash_content(&entries)
}

fn rust_language_id(_: &Path) -> String {
    "rust".to_string()
}
fn go_language_id(_: &Path) -> String {
    "go".to_string()
}
fn c_family_language_id(path: &Path) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("c" | "h") => "c",
        _ => "cpp",
    }
    .to_string()
}

/// PATH, then `$GOBIN`, then `~/go/bin` — same search shape as
/// `scip::runner::go_resolve_binary`, different binary name (`gopls`, the
/// persistent LSP server a user may install separately, not `scip-go`'s
/// one-shot batch indexer). Uses `gopls_runs`
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

    #[test]
    fn providers_declare_version_probe_and_resolution_context_inputs() {
        assert_eq!(RUST_ANALYZER.version_args, &["--version"]);
        assert!(RUST_ANALYZER.context_inputs.contains(&"Cargo.toml"));
        assert!(RUST_ANALYZER.context_inputs.contains(&"Cargo.lock"));
        assert_eq!(GOPLS.version_args, &["version"]);
        assert!(GOPLS.context_inputs.contains(&"go.mod"));
        assert!(CLANGD.context_inputs.contains(&"compile_commands.json"));
        assert_eq!(
            (RUST_ANALYZER.client_profile.language_id)(Path::new("x.rs")),
            "rust"
        );
        assert_eq!((GOPLS.client_profile.language_id)(Path::new("x.go")), "go");
        assert_eq!(
            (CLANGD.client_profile.language_id)(Path::new("x.cpp")),
            "cpp"
        );
        assert!(RUST_ANALYZER.client_profile.include_workspace_folder);
    }

    #[test]
    fn runtime_status_profile_fingerprint_tracks_workspace_folder_behavior() {
        fn unavailable(_: Option<&str>, _: &Path) -> Option<PathBuf> {
            None
        }
        let changed_profile = LspProvider {
            name: RUST_ANALYZER.name,
            langs: RUST_ANALYZER.langs,
            resolve_binary: unavailable,
            version_args: RUST_ANALYZER.version_args,
            context_inputs: RUST_ANALYZER.context_inputs,
            client_profile: LspClientProfile {
                server_args: RUST_ANALYZER.client_profile.server_args,
                language_id: RUST_ANALYZER.client_profile.language_id,
                initialization_options_json: RUST_ANALYZER
                    .client_profile
                    .initialization_options_json,
                include_workspace_folder: false,
            },
            stats_file_name: RUST_ANALYZER.stats_file_name,
        };
        let root = tempfile::tempdir().unwrap();
        let baseline = runtime_status(
            &LspProvider {
                resolve_binary: unavailable,
                ..RUST_ANALYZER
            },
            &crate::config::LspConfig::default(),
            root.path(),
            "not_run",
            0,
        );
        let changed = runtime_status(
            &changed_profile,
            &crate::config::LspConfig::default(),
            root.path(),
            "not_run",
            0,
        );
        assert_ne!(
            baseline.profile_fingerprint, changed.profile_fingerprint,
            "a workspace-folder protocol change must invalidate the provider profile"
        );
    }

    #[test]
    fn proof_provider_fingerprint_tracks_binary_version_argv_and_client_profile() {
        let baseline = proof_provider_fingerprint(
            &RUST_ANALYZER,
            Path::new("/opt/bin/rust-analyzer"),
            "rust-analyzer 1.0",
        );
        assert_ne!(
            baseline,
            proof_provider_fingerprint(
                &RUST_ANALYZER,
                Path::new("/opt/bin/rust-analyzer-next"),
                "rust-analyzer 1.0",
            ),
            "the resolved executable is proof provenance"
        );
        assert_ne!(
            baseline,
            proof_provider_fingerprint(
                &RUST_ANALYZER,
                Path::new("/opt/bin/rust-analyzer"),
                "rust-analyzer 2.0",
            ),
            "the probed version is proof provenance"
        );
        let changed_argv = LspProvider {
            version_args: &["--version", "--verbose"],
            ..RUST_ANALYZER
        };
        assert_ne!(
            baseline,
            proof_provider_fingerprint(
                &changed_argv,
                Path::new("/opt/bin/rust-analyzer"),
                "rust-analyzer 1.0",
            ),
            "version-probe argv is part of the profile"
        );
        let changed_profile = LspProvider {
            client_profile: LspClientProfile {
                server_args: RUST_ANALYZER.client_profile.server_args,
                language_id: RUST_ANALYZER.client_profile.language_id,
                initialization_options_json: RUST_ANALYZER
                    .client_profile
                    .initialization_options_json,
                include_workspace_folder: false,
            },
            ..RUST_ANALYZER
        };
        assert_ne!(
            baseline,
            proof_provider_fingerprint(
                &changed_profile,
                Path::new("/opt/bin/rust-analyzer"),
                "rust-analyzer 1.0",
            ),
            "client protocol profile is part of proof provenance"
        );
        let split_argv = LspProvider {
            version_args: &["--version", "verbose"],
            ..RUST_ANALYZER
        };
        let separator_in_argv = LspProvider {
            version_args: &["--version\u{1f}verbose"],
            ..RUST_ANALYZER
        };
        assert_ne!(
            profile_fingerprint(&split_argv),
            profile_fingerprint(&separator_in_argv),
            "distinct argv arrays must not collide when an argument contains a separator byte"
        );
    }

    #[test]
    fn runtime_status_never_claims_a_probe_when_the_binary_is_unavailable() {
        fn unavailable(_: Option<&str>, _: &Path) -> Option<PathBuf> {
            None
        }
        let provider = LspProvider {
            name: "unavailable-test",
            langs: &["rust"],
            resolve_binary: unavailable,
            version_args: &["--version"],
            context_inputs: &[],
            client_profile: RUST_ANALYZER.client_profile,
            stats_file_name: "unused.json",
        };
        let status = runtime_status(
            &provider,
            &crate::config::LspConfig::default(),
            Path::new("."),
            "unavailable",
            0,
        );
        assert_eq!(status.support_level, "fixture-tested");
        assert!(status.binary.is_none());
        assert!(status.version.is_none());
        assert_eq!(status.reason.as_deref(), Some("binary_unavailable"));
    }
}
