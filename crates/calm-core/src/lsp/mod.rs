//! LSP resolve-time overlay: upgrades `ambiguous`/`textual` `call_edges` to
//! `formal` by asking a live LSP session (over stdio) `textDocument/
//! definition` for each unresolved call site — the interactive counterpart
//! to `scip::run_overlay`'s one-shot batch dump.
//!
//! Table-driven over `provider::LspProvider` (D.0, 2026-07-11) — today's
//! providers: rust-analyzer (rust), gopls (go), clangd (c/cpp). Scope
//! honesty (2026-07-10 measurement, self-repo, rust-analyzer only): after a
//! fresh SCIP pass, only ~12% of Rust call edges remain below `formal` (772
//! candidates of ~6300), and batch SCIP and this overlay query the *same*
//! analysis engine for Rust, so the expected yield there is modest — this is
//! a supplementary evidence layer behind the explicit `lsp_refresh` tool,
//! not a replacement for the SCIP overlay. `clangd` has no live-verified
//! SCIP counterpart in this codebase (`scip-clang` is scaffolded but never
//! live-verified — see `scip::provider::CLANG`'s doc comment), so for C/C++
//! this overlay is the only formal-tier path with any live verification.
//! `gopls` does have one (`scip::provider::GO`, live-verified) — for Go this
//! overlay is a supplementary path, not the only one.
//!
//! Depends on the `scip-overlay` feature (see `Cargo.toml`): binary
//! discovery helpers (`scip::runner::binary_runs`/`dirs_home`) and
//! location→symbol resolution (`scip::ingest::resolve_unique_symbol_at_filtered`)
//! are shared with the SCIP overlay rather than duplicated.

pub mod client;
pub mod overlay;
pub mod provider;

pub use overlay::{LspIngestStats, run_lsp_overlay};

use std::path::Path;

use rusqlite::Connection;

/// Manually refresh one or every LSP provider right now, bypassing
/// `cfg.policy`'s automatic-run gate (`force: true` — see `run_lsp_overlay`)
/// — the entry point behind the `lsp_refresh` MCP tool. Mirrors
/// `scip::refresh_language`'s shape exactly. `lang`: `None`/`Some("all")`
/// runs every provider in the table, in this fixed order;
/// `Some("rust"|"go"|"c")` runs just that one. An unrecognized `lang` is an
/// `Err`, not a silent no-op — this is an explicit user request, not an
/// auto-detect probe.
pub fn refresh_language(
    conn: &Connection,
    root: &Path,
    config: &crate::config::Config,
    lang: Option<&str>,
) -> anyhow::Result<Vec<(String, LspIngestStats)>> {
    let all = ["rust", "go", "c"];
    let want: &[&str] = match lang {
        None | Some("all") => &all,
        Some(l) if all.contains(&l) => std::slice::from_ref(
            all.iter()
                .find(|x| **x == l)
                .expect("just checked contains"),
        ),
        Some(other) => {
            anyhow::bail!("unknown LSP provider {other:?} — expected one of: rust, go, c, all")
        }
    };
    let mut out = Vec::with_capacity(want.len());
    for lang in want {
        let stats = match *lang {
            "rust" => {
                run_lsp_overlay(conn, root, &provider::RUST_ANALYZER, &config.rust.lsp, true)?
            }
            "go" => run_lsp_overlay(conn, root, &provider::GOPLS, &config.go.lsp, true)?,
            "c" => run_lsp_overlay(conn, root, &provider::CLANGD, &config.clang.lsp, true)?,
            _ => unreachable!("want is filtered to `all` above"),
        };
        out.push((lang.to_string(), stats));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_language_rejects_an_unknown_provider_name() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let config = crate::config::Config::default();
        let err = refresh_language(&conn, Path::new("."), &config, Some("bogus"))
            .expect_err("unknown provider must be an Err, not a silent no-op");
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn refresh_language_all_runs_every_provider_as_a_noop_with_no_matching_files() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let config = crate::config::Config::default();
        let results = refresh_language(&conn, Path::new("."), &config, None).unwrap();
        let langs: Vec<&str> = results.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(langs, vec!["rust", "go", "c"]);
        for (_, stats) in &results {
            assert_eq!(*stats, LspIngestStats::default());
        }
    }
}
