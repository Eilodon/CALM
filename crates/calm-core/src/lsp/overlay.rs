//! The DB-driven overlay pass itself, in three strictly separated phases:
//!
//! 1. **DB read** (caller thread, sync): load candidate edges + the source
//!    text of their files, then let go of the connection.
//! 2. **LSP session** (dedicated OS thread, its own single-threaded tokio
//!    runtime): spawn rust-analyzer, resolve each call site, return plain
//!    data. A dedicated thread — NOT an inline `block_on` — because the MCP
//!    tool that calls this already runs on the server's ambient tokio
//!    runtime, where a nested `block_on` panics ("Cannot start a runtime
//!    from within a runtime"; reproduced 2026-07-10). The thread boundary
//!    also keeps `rusqlite::Connection` (`!Sync`) entirely out of async code.
//! 3. **DB write** (caller thread, sync): re-verify each hit is still an
//!    upgradable row and apply it, counting rows actually changed — a
//!    concurrent `rebuild_graph` (`DELETE FROM call_edges` + reinsert) makes
//!    snapshot ids stale, so `to_upgrade.len()` would over-report.
//!
//! Fail-silent like `scip::run_overlay_for`: every failure mode returns
//! `Ok(LspIngestStats::default())`, leaving the graph untouched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::config::{LspConfig, RefreshPolicy};
use crate::lsp::client::{
    DefinitionOutcome, LspClient, PositionEncoding, path_to_uri, uri_to_path,
};
use crate::scip::runner::resolve_binary;

/// Bounds every individual LSP request round-trip. Live probe data
/// (2026-07-10, rust-analyzer 1.96 on the `rust_workspace` fixture): replies
/// stall up to ~4s while initial indexing runs, so this must comfortably
/// exceed that; after warm-up, replies are milliseconds.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds the whole LSP phase (spawn → last definition). ADR-0004 proposed
/// 60s for an enrichment pass; the probe showed ~5.4s cold-start on a tiny
/// fixture, so a CALM-sized repo plausibly needs 30-60s of warm-up alone —
/// 180s keeps the hard-cap guarantee without starving the first real run.
const PASS_BUDGET: Duration = Duration::from_secs(180);
/// How long the warm-up loop keeps re-asking before concluding the server
/// is as ready as it will get (see `resolve_all_on_thread`).
const WARMUP_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LspIngestStats {
    /// Edges whose row was actually changed to `formal`/`formal_source='lsp'`
    /// this run — counted from `UPDATE` rowcounts, not from resolution hits,
    /// so ids gone stale under a concurrent rebuild don't inflate it.
    pub upgraded: usize,
    /// Call sites actually queried against the live server.
    pub attempted: usize,
    /// `upgraded / attempted` (0.0 when nothing was attempted).
    pub match_rate: f64,
}

/// One `call_edges` row eligible for LSP resolution: not yet formal, not
/// already ruled out by SCIP's exact evidence (241 of 1013 otherwise-eligible
/// rows on the 2026-07-10 self-repo measurement — re-querying those wastes
/// round-trips, and a divergent answer would let LSP contradict SCIP's
/// stronger verdict), and carrying a usable call-site location.
struct CandidateEdge {
    id: i64,
    from_path: String,
    call_line: i64,
    to_symbol: String,
    to_name: String,
}

/// A resolution the LSP phase produced, pending phase-3 verification.
struct ResolvedSite {
    edge_id: i64,
    to_symbol: String,
    def_uri: lsp_types::Uri,
    def_line_zero_based: u32,
}

/// Run the LSP resolve-time overlay for Rust (`config.rust.lsp`) — the
/// `lsp_refresh` MCP tool's `force: true` entry point, and (if ever wired to
/// an automatic caller) the `force: false` one gated by `cfg.policy`.
pub fn refresh(
    conn: &Connection,
    root: &Path,
    config: &crate::config::Config,
    force: bool,
) -> anyhow::Result<LspIngestStats> {
    run_lsp_overlay(conn, root, &config.rust.lsp, force)
}

pub fn run_lsp_overlay(
    conn: &Connection,
    root: &Path,
    cfg: &LspConfig,
    force: bool,
) -> anyhow::Result<LspIngestStats> {
    if cfg.enabled == Some(false) {
        return Ok(LspIngestStats::default());
    }
    // Cheap DB check before ever probing for a binary or spawning anything —
    // same reasoning as scip::run_overlay_for's provider_has_any_files gate.
    if !has_any_rust_files(conn) {
        return Ok(LspIngestStats::default());
    }
    if !force && !policy_allows_automatic_run(cfg.policy, root) {
        tracing::info!(
            "LSP overlay: refresh policy ({}) skips this automatic run — \
             use the `lsp_refresh` MCP tool to force it",
            cfg.policy
        );
        return Ok(LspIngestStats::default());
    }
    let Some(bin) = resolve_binary(cfg.binary.as_deref(), root) else {
        if cfg.enabled == Some(true) {
            tracing::info!("LSP overlay enabled but no rust-analyzer found — skipping");
        }
        return Ok(LspIngestStats::default());
    };

    // ---- Phase 1: DB read (sync, caller thread) ----
    let rows = load_candidate_edges(conn)?;
    if rows.is_empty() {
        return Ok(LspIngestStats::default());
    }
    let mut by_file: HashMap<String, Vec<CandidateEdge>> = HashMap::new();
    for row in rows {
        by_file.entry(row.from_path.clone()).or_default().push(row);
    }
    // Read file contents up front too: phase 2 then touches nothing but its
    // own inputs, and a file that changed on disk mid-pass can't desync the
    // didOpen text from the lines we compute columns against.
    let mut files: Vec<(PathBuf, String, Vec<CandidateEdge>)> = Vec::with_capacity(by_file.len());
    for (from_path, edges) in by_file {
        let abs = root.join(&from_path);
        match std::fs::read_to_string(&abs) {
            Ok(text) => files.push((abs, text, edges)),
            Err(_) => continue, // deleted/unreadable since indexing — skip
        }
    }

    // ---- Phase 2: LSP session on a dedicated thread ----
    let root_owned = root.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("calm-lsp-overlay".into())
        .spawn(move || resolve_all_on_thread(&bin, &root_owned, files))
        .map_err(anyhow::Error::from)?;
    let (resolutions, attempted) = match handle.join() {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            tracing::warn!("LSP overlay run failed, keeping prior graph state: {e}");
            return Ok(LspIngestStats::default());
        }
        Err(_) => {
            tracing::warn!("LSP overlay thread panicked, keeping prior graph state");
            return Ok(LspIngestStats::default());
        }
    };

    // ---- Phase 3: DB write (sync, caller thread) ----
    let mut upgraded = 0usize;
    {
        let mut update = conn.prepare(
            // Re-verify the row is still in an upgradable state: a concurrent
            // rebuild_graph DELETEs+reinserts call_edges with fresh ids, so a
            // stale id must update 0 rows, and an id that survived must still
            // not be formal/ruled-out.
            "UPDATE call_edges SET edge_confidence = 'formal', formal_source = 'lsp' \
             WHERE id = ?1 AND edge_confidence IN ('ambiguous', 'textual') \
               AND formal_source IS NULL AND ruled_out_by_scip = 0",
        )?;
        for site in &resolutions {
            let Some(def_path) = uri_to_repo_path(&site.def_uri, root) else {
                continue;
            };
            // def_line from LSP is 0-indexed; symbols.line_start is 1-indexed.
            let resolved = crate::scip::ingest::resolve_unique_symbol_at_filtered(
                conn,
                &def_path,
                site.def_line_zero_based as i64 + 1,
                true, // markdown headings are never call targets
            )?;
            if resolved.as_deref() == Some(site.to_symbol.as_str()) {
                upgraded += update.execute([site.edge_id])?;
            }
        }
    }
    if upgraded > 0 {
        // Same contract as scip::run_and_refresh: caller_count (and the
        // hub/coreness/dead-code signals derived from it) counts by
        // confidence tier, so flipping ambiguous→formal changes it.
        crate::indexer::pipeline::refresh_caller_counts(conn)?;
    }

    let match_rate = if attempted == 0 {
        0.0
    } else {
        upgraded as f64 / attempted as f64
    };
    let stats = LspIngestStats {
        upgraded,
        attempted,
        match_rate,
    };
    tracing::info!(
        "LSP overlay: {} of {} attempted call sites upgraded to formal (match_rate={:.2})",
        stats.upgraded,
        stats.attempted,
        stats.match_rate
    );
    write_last_run_stats(root, &stats);
    Ok(stats)
}

/// Phase 2 entry: builds this thread's own single-threaded runtime (safe —
/// no ambient runtime exists on a fresh OS thread), applies the overall
/// pass budget, and always tears the server down before returning.
fn resolve_all_on_thread(
    bin: &Path,
    root: &Path,
    files: Vec<(PathBuf, String, Vec<CandidateEdge>)>,
) -> anyhow::Result<(Vec<ResolvedSite>, usize)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut client = LspClient::spawn(bin, root, REQUEST_TIMEOUT).await?;
        let mut resolutions = Vec::new();
        let mut attempted = 0usize;
        // Budget expiry keeps whatever resolved so far — `resolutions` and
        // `attempted` live outside the timed future.
        let run = tokio::time::timeout(
            PASS_BUDGET,
            resolve_loop(&mut client, root, &files, &mut resolutions, &mut attempted),
        )
        .await;
        client.shutdown().await; // every path, including budget expiry
        match run {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("LSP resolve loop ended early: {e}"),
            Err(_) => tracing::info!(
                "LSP overlay pass budget ({PASS_BUDGET:?}) expired — keeping partial results"
            ),
        }
        Ok((resolutions, attempted))
    })
}

async fn resolve_loop(
    client: &mut LspClient,
    root: &Path,
    files: &[(PathBuf, String, Vec<CandidateEdge>)],
    resolutions: &mut Vec<ResolvedSite>,
    attempted: &mut usize,
) -> anyhow::Result<()> {
    // Warm-up: a freshly spawned rust-analyzer answers `null` (not "please
    // retry"!) to definition requests until initial indexing settles —
    // observed live: null, null, -32801, then correct, over ~5.4s on a tiny
    // fixture. An early `null` is therefore not authoritative; keep
    // re-asking the first few sites until one resolves or the warm-up budget
    // expires, and only then trust `NotFound` answers.
    let warmup_deadline = tokio::time::Instant::now() + WARMUP_BUDGET;
    let mut warmed_up = false;

    for (abs_path, text, edges) in files {
        let Ok(uri) = path_to_uri(abs_path) else {
            continue;
        };
        if client.open_file(abs_path, &uri, text).await.is_err() {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        for edge in edges {
            // call_site_line is 1-indexed (same convention symbols.line_start
            // uses); LSP Position is 0-indexed.
            let Ok(line_idx) = usize::try_from(edge.call_line - 1) else {
                continue;
            };
            let Some(line_text) = lines.get(line_idx) else {
                continue;
            };
            // Column isn't stored in call_edges — best-effort: first
            // whole-word occurrence of the callee's short name on the line.
            // Wrong-column guesses degrade to "no upgrade" (phase 3
            // cross-checks the definition target), never a wrong upgrade.
            let Some(byte_col) = find_identifier_column(line_text, &edge.to_name) else {
                continue;
            };
            let character = match client.encoding {
                // Negotiated utf-8 (rust-analyzer's actual answer): byte offset.
                PositionEncoding::Utf8 => byte_col as u32,
                // LSP default: UTF-16 code units of the prefix.
                PositionEncoding::Utf16 => line_text[..byte_col].encode_utf16().count() as u32,
            };
            *attempted += 1;
            let mut outcome = client.definition(&uri, line_idx as u32, character).await;
            // Retry loop: -32801 is always "ask again"; during warm-up a
            // `null` is too (see above).
            loop {
                match &outcome {
                    Ok(DefinitionOutcome::Retryable) => {}
                    Ok(DefinitionOutcome::NotFound)
                        if !warmed_up && tokio::time::Instant::now() < warmup_deadline => {}
                    _ => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                outcome = client.definition(&uri, line_idx as u32, character).await;
            }
            match outcome {
                Ok(DefinitionOutcome::Resolved(def_uri, def_line)) => {
                    warmed_up = true;
                    // Only keep in-repo definitions; std/deps can't match a
                    // graph symbol anyway.
                    if uri_to_path(&def_uri).is_some_and(|p| p.starts_with(root)) {
                        resolutions.push(ResolvedSite {
                            edge_id: edge.id,
                            to_symbol: edge.to_symbol.clone(),
                            def_uri,
                            def_line_zero_based: def_line,
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    // One hard protocol error (closed pipe, timeout) ends the
                    // pass — later requests would fail identically.
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

fn load_candidate_edges(conn: &Connection) -> rusqlite::Result<Vec<CandidateEdge>> {
    let mut stmt = conn.prepare(
        "SELECT ce.id, ce.from_path, ce.call_site_line, ce.to_symbol, s.name \
         FROM call_edges ce \
         JOIN symbols s ON s.qualified_name = ce.to_symbol \
         WHERE ce.edge_confidence IN ('ambiguous', 'textual') \
           AND ce.formal_source IS NULL \
           AND ce.ruled_out_by_scip = 0 \
           AND ce.call_site_line IS NOT NULL \
           AND ce.from_path IS NOT NULL \
           AND s.kind != 'heading'",
    )?;
    stmt.query_map([], |r| {
        Ok(CandidateEdge {
            id: r.get(0)?,
            from_path: r.get(1)?,
            call_line: r.get(2)?,
            to_symbol: r.get(3)?,
            to_name: r.get(4)?,
        })
    })?
    .collect()
}

fn has_any_rust_files(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM file_index WHERE language = 'rust')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .unwrap_or(true) // fail open, same posture as scip::provider_has_any_files
}

/// First whole-word occurrence of `name` on `line`, as a BYTE offset (the
/// caller converts to the negotiated position encoding). On a failed
/// word-boundary check the scan resumes past the whole failed match, not one
/// byte forward — `foo` against `foo_foo_foo_...` stays linear.
fn find_identifier_column(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let bytes = line.as_bytes();
    let name_len = name.len();
    let mut start = 0usize;
    while let Some(rel) = line.get(start..).and_then(|s| s.find(name)) {
        let idx = start + rel;
        let before_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let after_ok = idx + name_len >= bytes.len() || !is_ident_byte(bytes[idx + name_len]);
        if before_ok && after_ok {
            return Some(idx);
        }
        start = idx + name_len;
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `file://` `Uri` -> repo-relative path string, matching the convention
/// `call_edges.from_path`/`symbols.path` are stored in. `None` if `uri`
/// isn't a `file://` URI or doesn't fall under `root`.
fn uri_to_repo_path(uri: &lsp_types::Uri, root: &Path) -> Option<String> {
    let abs = uri_to_path(uri)?;
    let rel = abs.strip_prefix(root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn policy_allows_automatic_run(policy: RefreshPolicy, root: &Path) -> bool {
    match policy {
        RefreshPolicy::OnSave => true,
        RefreshPolicy::OnDemand => false,
        RefreshPolicy::MinInterval(secs) => match read_last_run_unix(root) {
            None => true,
            Some(last) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_sub(last) >= secs
            }
        },
    }
}

fn read_last_run_unix(root: &Path) -> Option<u64> {
    let path = root.join(".calm").join("lsp-stats.json");
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("last_run_unix").and_then(|x| x.as_u64())
}

fn write_last_run_stats(root: &Path, stats: &LspIngestStats) {
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = root.join(".calm").join("lsp-stats.json");
    let _ = std::fs::write(
        &path,
        serde_json::json!({
            "upgraded": stats.upgraded,
            "attempted": stats.attempted,
            "match_rate": stats.match_rate,
            "last_run_unix": now_unix,
        })
        .to_string(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_off_is_a_noop_even_when_rust_analyzer_is_on_path() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cfg = LspConfig {
            enabled: Some(false),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &cfg, false).unwrap(),
            LspIngestStats::default()
        );
    }

    #[test]
    fn zero_rust_files_is_a_noop_even_when_forced_on() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.py', 'h', 'python', 0.0)",
            [],
        )
        .unwrap();
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &cfg, false).unwrap(),
            LspIngestStats::default()
        );
    }

    /// The single most important behavior this whole feature exists to get
    /// right (see the roadmap's gating requirement): an automatic
    /// (`force: false`) caller under the default `OnDemand` policy must
    /// never even reach the binary probe, regardless of what's on `PATH`.
    #[test]
    fn on_demand_policy_skips_automatic_runs_even_with_candidate_edges() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.rs', 'h', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        assert_eq!(
            run_lsp_overlay(&conn, Path::new("."), &cfg, false).unwrap(),
            LspIngestStats::default(),
            "OnDemand must block an automatic (force=false) run"
        );
    }

    /// Locks the 2026-07-10 review's config finding: `LspConfig::default()`
    /// must agree with the serde default (`OnDemand`) — a derived `Default`
    /// silently resolves to `RefreshPolicy::default()` = `OnSave`, the exact
    /// value the config's own doc comment forbids as a default.
    #[test]
    fn default_policy_is_on_demand_not_on_save() {
        assert_eq!(LspConfig::default().policy, RefreshPolicy::OnDemand);
        assert_eq!(
            crate::config::RustConfig::default().lsp.policy,
            RefreshPolicy::OnDemand,
            "an unconfigured project must land on OnDemand"
        );
        // And the serde path for a config.json that never mentions `lsp`:
        let parsed: crate::config::RustConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.lsp.policy, RefreshPolicy::OnDemand);
    }

    #[test]
    fn find_identifier_column_matches_whole_word_only() {
        assert_eq!(find_identifier_column("    foo(bar);", "foo"), Some(4));
        assert_eq!(find_identifier_column("    foobar();", "foo"), None);
        assert_eq!(find_identifier_column("    barfoo();", "foo"), None);
        assert_eq!(find_identifier_column("no match here", "foo"), None);
        // resumes past a failed match without quadratic re-scanning, and
        // still finds a later real occurrence
        assert_eq!(
            find_identifier_column("foo_foo_foo(); foo();", "foo"),
            Some(15)
        );
    }

    #[test]
    fn resolve_symbol_at_picks_narrowest_span_and_skips_headings() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('a.rs::Outer', 'Outer', 'impl', 'rust', 'a.rs', 1, 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end) \
             VALUES ('a.rs::Outer::inner', 'inner', 'method', 'rust', 'a.rs', 3, 5)",
            [],
        )
        .unwrap();
        assert_eq!(
            crate::scip::ingest::resolve_unique_symbol_at_filtered(&conn, "a.rs", 4, true).unwrap(),
            Some("a.rs::Outer::inner".to_string())
        );
    }

    /// Live integration: real rust-analyzer against the same fixture the SCIP
    /// overlay's ignored test uses. Ignored by default — needs rust-analyzer
    /// on PATH/rustup/VS Code and a real `cargo metadata` resolve. Exercises
    /// the full three-phase pipeline including warm-up (the fixture takes
    /// ~5s before rust-analyzer answers definitions — see module docs).
    #[test]
    #[ignore]
    fn lsp_overlay_upgrades_a_real_edge_on_the_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust_workspace");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE edge_confidence IN ('ambiguous','textual') \
                 AND formal_source IS NULL AND call_site_line IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(before > 0, "fixture must start with unresolved edges");

        let cfg = LspConfig {
            enabled: Some(true),
            binary: None,
            policy: RefreshPolicy::OnDemand,
        };
        let stats = run_lsp_overlay(&conn, &fixture, &cfg, true).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal (attempted={})",
            stats.attempted
        );
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM call_edges WHERE formal_source = 'lsp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n as usize, stats.upgraded);
    }
}
