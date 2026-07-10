//! LSP resolve-time overlay: upgrades `ambiguous`/`textual` `call_edges` to
//! `formal` by asking a live rust-analyzer session (over stdio, LSP protocol)
//! `textDocument/definition` for each unresolved call site — the interactive
//! counterpart to `scip::run_overlay`'s one-shot batch dump.
//!
//! Scope honesty (2026-07-10 measurement, self-repo): after a fresh SCIP
//! pass, only ~12% of Rust call edges remain below `formal` (772 candidates
//! of ~6300), and batch SCIP and this overlay query the *same* analysis
//! engine (rust-analyzer), so the expected yield here is modest — this is a
//! supplementary evidence layer behind the explicit `lsp_refresh` tool, not
//! a replacement for the SCIP overlay. See ADR-0004's 2026-07-10 update for
//! why it ships for Rust anyway (protocol plumbing for future languages that
//! have an LSP server but no SCIP indexer: ruby, kotlin, swift, ...).
//!
//! Depends on the `scip-overlay` feature (see `Cargo.toml`): binary
//! discovery (`scip::runner::resolve_binary`) and location→symbol resolution
//! (`scip::ingest::resolve_unique_symbol_at_filtered`) are shared with the
//! SCIP overlay rather than duplicated.

pub mod client;
pub mod overlay;

pub use overlay::{LspIngestStats, refresh, run_lsp_overlay};
