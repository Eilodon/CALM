pub mod cache;
pub mod ingest;
pub mod parse;
pub mod provider;
pub mod runner;

use std::path::Path;

use rusqlite::Connection;

use crate::config::{RustConfig, ScipConfig};

/// Run the full SCIP overlay: detect rust-analyzer, run batch scip into a temp
/// file, parse, and upgrade edges. Fail-silent — every failure mode (disabled,
/// no binary, timeout, parse error) returns `Ok(IngestStats::default())`,
/// leaving the syntactic graph untouched.
///
/// `rust.scip.enabled` is three-state (see `ScipConfig`): `Some(false)` skips
/// without even probing for a binary; unset (`None`, the default) or
/// `Some(true)` both probe for `rust-analyzer` and run if found — the only
/// difference is `Some(true)` logs once when the probe comes up empty (the
/// user explicitly asked, so a no-op is worth explaining), while unset stays
/// silent (finding nothing is the common, expected case for a checkout that
/// never configured this at all, not worth a log line every session).
///
/// Caches on (rust-analyzer version, active toolchain fingerprint, Cargo.lock
/// hash, Cargo.toml hash, `dirty`): an unchanged toolchain, dependency set,
/// edition/workspace shape, and Rust source state means a re-run would find
/// the same call graph, so the (comparatively expensive) rust-analyzer pass
/// is skipped and the previous upgrades — already persisted as
/// `formal`/`ruled_out_by_scip` in the DB — stand. `dirty` is the caller's
/// current Rust-source fingerprint (see `rust_source_dirty_keys`) — pass it
/// so a source-only change (no lockfile/toolchain difference, e.g. editing a
/// function body) still invalidates the cache instead of silently standing
/// forever; an empty slice degrades to the old (lockfile/toolchain-only) key,
/// which remains safe on its own because it can only widen a "skip" into a
/// "run", never the reverse.
pub fn run_overlay_for(
    provider: &provider::ScipProvider,
    conn: &Connection,
    root: &Path,
    sub_root: &Path,
    cfg: &ScipConfig,
    dirty: &[String],
    force: bool,
) -> anyhow::Result<ingest::IngestStats> {
    if cfg.enabled == Some(false) {
        return Ok(ingest::IngestStats::default());
    }
    // Cheap DB check before any subprocess: a project with zero files of
    // this provider's language(s) has nothing this provider could ever
    // upgrade, so there's no reason to probe for its binary at all — for
    // Python/JS that probe can be a real `npx --yes <package> --version`
    // network round-trip (see `runner.rs`'s `npx_can_run_scip_python`/
    // `npx_can_run_scip_typescript`), paid on *every* incremental reindex
    // regardless of what changed. Found the hard way: adding the JS
    // provider doubled this unconditional per-reindex network cost on the
    // watcher's hot path, which was enough to push
    // `watcher_integration.rs`'s `watcher_reindexes_add_and_delete` (a
    // Python-only fixture, so both the Python *and* the wastefully-probed
    // Go/JS providers paid this tax on every reindex) past its 30s timeout
    // budget in CI.
    if !provider_has_any_files(conn, provider) {
        return Ok(ingest::IngestStats::default());
    }
    if !force && !policy_allows_automatic_run(cfg.policy, root, provider) {
        tracing::info!(
            "SCIP overlay ({}): refresh policy ({}) skips this automatic run — \
             use `calm scip run --lang {}` or the `scip_refresh` MCP tool to force it",
            provider.lang,
            cfg.policy,
            provider.lang
        );
        return Ok(ingest::IngestStats::default());
    }
    let Some(bin) = (provider.resolve_binary)(cfg.binary.as_deref(), root) else {
        if cfg.enabled == Some(true) {
            tracing::info!(
                "SCIP overlay enabled but no {} indexer found — skipping",
                provider.lang
            );
        }
        return Ok(ingest::IngestStats::default());
    };

    let cache_path = root.join(".calm").join(provider.cache_file_name);
    let key = (provider.cache_key)(&bin, root, dirty);
    if std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key) {
        tracing::info!(
            "SCIP overlay ({}): cache key unchanged, skipping indexer run",
            provider.lang
        );
        return Ok(ingest::IngestStats::default());
    }

    let tmp = tempfile::Builder::new().suffix(".scip").tempfile()?;
    if let Err(e) = runner::run_indexer(provider, &bin, root, tmp.path()) {
        tracing::warn!(
            "SCIP overlay ({}) run failed, keeping syntactic graph: {e}",
            provider.lang
        );
        return Ok(ingest::IngestStats::default());
    }
    let occ = match parse::parse_scip_file(tmp.path(), sub_root) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("SCIP parse failed ({}): {e}", provider.lang);
            return Ok(ingest::IngestStats::default());
        }
    };
    let insert_missing = cfg.insert_missing != Some(false);
    let stats = ingest::ingest_occurrences(conn, &occ, insert_missing)?;
    tracing::info!(
        "SCIP overlay ({}): {} edges upgraded to formal, {} fan-out siblings ruled out, \
         {} edges inserted, match_rate={:.2}",
        provider.lang,
        stats.upgraded,
        stats.ruled_out,
        stats.inserted,
        stats.match_rate
    );
    // Best-effort: a failed cache write just means the next run pays the cost
    // of re-running this provider's indexer again, never a correctness issue.
    let _ = std::fs::write(&cache_path, &key);
    // Best-effort sidecar so `indexing_status`/`overlay_status` can surface
    // this run's `inserted`/`match_rate`/`last_run` without re-running the
    // overlay — none of those are derivable from `call_edges` alone the way
    // `available`/`up_to_date` are (there's no column recording "how many
    // SCIP-resolved sites exist" once the pass is done, or when it ran).
    // Stands until the next real (non-cache-skip) run overwrites it; reading
    // code should treat it as "as of the last real run", not "live". One
    // file per provider (P2.6) — was a single shared `scip-stats.json` for
    // every provider through P2.1/P2.4, a known bug (each provider's run
    // clobbered the others' stats) fixed here alongside adding `last_run`,
    // which `policy_allows_automatic_run`'s `MinInterval` case also needs
    // per-provider, not shared.
    let stats_path = root.join(".calm").join(stats_file_name(provider));
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(
        &stats_path,
        serde_json::json!({
            "upgraded": stats.upgraded,
            "ruled_out": stats.ruled_out,
            "inserted": stats.inserted,
            "match_rate": stats.match_rate,
            "last_run_unix": now_unix,
        })
        .to_string(),
    );
    Ok(stats)
}

/// `<provider.cache_file_name minus ".cache">-stats.json` — e.g. Rust's
/// `scip.cache` -> `scip-stats.json` (same name the old shared sidecar
/// used, so an existing checkout's Rust stats aren't orphaned by this
/// rename), Go's `scip-go.cache` -> `scip-go-stats.json`. Derived from the
/// cache filename (already unique per provider) rather than adding yet
/// another `ScipProvider` field for the same purpose.
fn stats_file_name(provider: &provider::ScipProvider) -> String {
    format!(
        "{}-stats.json",
        provider
            .cache_file_name
            .strip_suffix(".cache")
            .unwrap_or(provider.cache_file_name)
    )
}

/// Whether an *automatic* caller (not an explicit `force`d manual refresh)
/// may run this provider's indexer right now, per `cfg.policy`. `OnSave`
/// (the default) always allows it — the pre-P2.6 behavior, gated only by
/// the cache-key check just above this function's call site.
/// `MinInterval` reads the provider's own last-run timestamp from its stats
/// sidecar (`None` — never run for real — always allows a first run).
fn policy_allows_automatic_run(
    policy: crate::config::RefreshPolicy,
    root: &Path,
    provider: &provider::ScipProvider,
) -> bool {
    use crate::config::RefreshPolicy;
    match policy {
        RefreshPolicy::OnSave => true,
        RefreshPolicy::OnDemand => false,
        RefreshPolicy::MinInterval(secs) => match read_last_run_unix(root, provider) {
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

fn read_last_run_unix(root: &Path, provider: &provider::ScipProvider) -> Option<u64> {
    let path = root.join(".calm").join(stats_file_name(provider));
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("last_run_unix").and_then(|x| x.as_u64())
}

/// Run the full SCIP overlay for Rust — thin wrapper around
/// `run_overlay_for(&provider::RUST, ...)`. Kept as its own function (rather
/// than inlining the `RustConfig` unwrap at every call site) because all 3
/// production callers (`lib.rs`, `watcher.rs`, `main.rs`) already call this
/// exact signature; changing it would touch 3 files for zero behavior gain.
/// See `run_overlay_for`'s doc comment for the actual contract (fail-silent,
/// caching, three-state `enabled`).
pub fn run_overlay(
    conn: &Connection,
    root: &Path,
    rust: &RustConfig,
    dirty: &[String],
) -> anyhow::Result<ingest::IngestStats> {
    run_overlay_for(
        &provider::RUST,
        conn,
        root,
        Path::new(""),
        &rust.scip,
        dirty,
        false,
    )
}
/// Fingerprint of every currently-indexed file's content for one or more
/// `file_index.language` values, for `run_overlay_for`'s `dirty` parameter —
/// one `"path@hash"` entry per file (`hash` already computed by the indexer,
/// so this is a cheap read, no re-hashing). Changes whenever any matching
/// file's content differs from what was indexed at the last successful
/// overlay run, regardless of whether the lockfile/toolchain also changed —
/// see `run_overlay_for`'s doc comment for why that matters. `langs` lets a
/// future multi-language provider (e.g. a combined C/C++ one) scope this to
/// more than one `file_index.language` value at once.
pub fn source_dirty_keys(conn: &Connection, langs: &[&str]) -> Vec<String> {
    if langs.is_empty() {
        return Vec::new();
    }
    let placeholders = langs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT path, hash FROM file_index WHERE language IN ({placeholders}) ORDER BY path"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params_from_iter(langs.iter()), |r| {
        Ok(format!(
            "{}@{}",
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Whether the project has at least one indexed file in any of `provider`'s
/// `dirty_langs` — see `run_overlay_for`'s call site for why this gate
/// exists. Fails open (`true`, i.e. don't skip) on a query error, same
/// posture as `source_dirty_keys`'s own error handling, so a transient DB
/// hiccup degrades to "probe anyway" rather than silently going blind to a
/// language that's actually present.
fn provider_has_any_files(conn: &Connection, provider: &provider::ScipProvider) -> bool {
    let langs = provider.dirty_langs;
    if langs.is_empty() {
        return true;
    }
    let placeholders = langs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT EXISTS(SELECT 1 FROM file_index WHERE language IN ({placeholders}))");
    conn.query_row(&sql, rusqlite::params_from_iter(langs.iter()), |r| {
        r.get::<_, i64>(0)
    })
    .map(|n| n != 0)
    .unwrap_or(true)
}

/// Rust-only convenience wrapper around `source_dirty_keys` — all existing
/// callers (`lib.rs`, `watcher.rs`, `main.rs`, this module's own tests) call
/// this exact name; kept so P0.4 touches zero call sites.
pub fn rust_source_dirty_keys(conn: &Connection) -> Vec<String> {
    source_dirty_keys(conn, &["rust"])
}

/// Run one provider's overlay, refresh `caller_count` if it changed
/// anything, and return the stats — the shared core behind
/// `run_go_overlay_and_log`/`run_python_overlay_and_log`/
/// `run_js_overlay_and_log` (each a 1-line `force: false` wrapper for their
/// existing public signatures, unchanged since P2.1/P2.4/P3.2) and
/// `refresh_language`'s `force: true` manual-refresh path (P2.6). Bundled
/// into one function (rather than inlining ~15 lines at every call site) so
/// a future 4th provider's callers only need one new line, not a new copy
/// of the refresh dance.
fn run_and_refresh(
    provider: &provider::ScipProvider,
    conn: &Connection,
    root: &Path,
    cfg: &ScipConfig,
    force: bool,
) -> anyhow::Result<ingest::IngestStats> {
    let dirty = source_dirty_keys(conn, provider.dirty_langs);
    let stats = run_overlay_for(provider, conn, root, Path::new(""), cfg, &dirty, force)?;
    if stats.upgraded > 0 || stats.ruled_out > 0 || stats.inserted > 0 {
        crate::indexer::pipeline::refresh_caller_counts(conn)?;
    }
    Ok(stats)
}

/// Run the Go overlay (`provider::GO`) — the Go-specific counterpart to the
/// Rust-only block each of the 3 production call sites (`lib.rs`,
/// `watcher.rs`, `main.rs`) already had before this existed.
/// Multi-module counterpart to `run_and_refresh`/`run_overlay_for`, for a
/// project using `go.work`. `scip-go` has no native workspace flag of its
/// own — a documented upstream limitation (Sourcegraph's own docs say to
/// run the indexer once per module root and rebase paths) — so this runs
/// `scip-go` once per member module (each at its own module root, so
/// `go_build_command`'s `--module-root` is correct per module) and ingests
/// each with `sub_root` = that module's path relative to `root`, reusing
/// the exact rebase mechanism `parse::parse_index` already had for
/// precisely this case (the same one the CLI's `--sub-root` flag exposes
/// for a pre-built `.scip` file). One combined cache key/cache file/stats
/// file at the project root, not per-module — Phase 1 doesn't attempt
/// per-module incremental skip (any member's go.mod/go.sum change
/// re-indexes every module); that's a real optimization left for if/when
/// dogfooding on an actual multi-module Go project shows it's worth the
/// extra complexity, not guessed up front.
fn run_go_workspace_overlay(
    conn: &Connection,
    root: &Path,
    cfg: &ScipConfig,
    modules: &[std::path::PathBuf],
    force: bool,
) -> anyhow::Result<ingest::IngestStats> {
    let provider = &provider::GO;
    if cfg.enabled == Some(false) {
        return Ok(ingest::IngestStats::default());
    }
    if !provider_has_any_files(conn, provider) {
        return Ok(ingest::IngestStats::default());
    }
    let dirty = source_dirty_keys(conn, provider.dirty_langs);
    if !force && !policy_allows_automatic_run(cfg.policy, root, provider) {
        tracing::info!(
            "SCIP overlay (go, {} workspace modules): refresh policy ({}) skips this automatic run — \
             use `calm scip run --lang go` or the `scip_refresh` MCP tool to force it",
            modules.len(),
            cfg.policy,
        );
        return Ok(ingest::IngestStats::default());
    }
    let Some(bin) = (provider.resolve_binary)(cfg.binary.as_deref(), root) else {
        if cfg.enabled == Some(true) {
            tracing::info!("SCIP overlay enabled but no go indexer found — skipping");
        }
        return Ok(ingest::IngestStats::default());
    };

    let cache_path = root.join(".calm").join(provider.cache_file_name);
    let key = (provider.cache_key)(&bin, root, &dirty);
    if std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key) {
        tracing::info!("SCIP overlay (go, workspace): cache key unchanged, skipping indexer run");
        return Ok(ingest::IngestStats::default());
    }

    let insert_missing = cfg.insert_missing != Some(false);
    let mut total = ingest::IngestStats::default();
    let mut match_rate_sum = 0.0;
    let mut ok_modules = 0usize;

    for module_rel in modules {
        let module_root = root.join(module_rel);
        if !module_root.is_dir() {
            tracing::warn!(
                "SCIP overlay (go workspace): module {} listed in go.work but not found on disk — skipping",
                module_rel.display()
            );
            continue;
        }
        let tmp = match tempfile::Builder::new().suffix(".scip").tempfile() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("SCIP overlay (go workspace): tempfile create failed: {e}");
                continue;
            }
        };
        if let Err(e) = runner::run_indexer(provider, &bin, &module_root, tmp.path()) {
            tracing::warn!(
                "SCIP overlay (go workspace, module {}) run failed, keeping syntactic graph: {e}",
                module_rel.display()
            );
            continue;
        }
        let occ = match parse::parse_scip_file(tmp.path(), module_rel) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(
                    "SCIP parse failed (go workspace, module {}): {e}",
                    module_rel.display()
                );
                continue;
            }
        };
        let stats = ingest::ingest_occurrences(conn, &occ, insert_missing)?;
        tracing::info!(
            "SCIP overlay (go, module {}): {} edges upgraded to formal, {} fan-out siblings ruled out, \
             {} edges inserted, match_rate={:.2}",
            module_rel.display(),
            stats.upgraded,
            stats.ruled_out,
            stats.inserted,
            stats.match_rate
        );
        total.upgraded += stats.upgraded;
        total.ruled_out += stats.ruled_out;
        total.inserted += stats.inserted;
        match_rate_sum += stats.match_rate;
        ok_modules += 1;
    }
    total.match_rate = if ok_modules > 0 {
        match_rate_sum / ok_modules as f64
    } else {
        0.0
    };

    if total.upgraded > 0 || total.ruled_out > 0 || total.inserted > 0 {
        crate::indexer::pipeline::refresh_caller_counts(conn)?;
    }

    // Best-effort, same posture as `run_overlay_for`'s equivalent writes —
    // a failed write here just costs the next run a redundant re-index.
    let _ = std::fs::write(&cache_path, &key);
    let stats_path = root.join(".calm").join(stats_file_name(provider));
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(
        &stats_path,
        serde_json::json!({
            "upgraded": total.upgraded,
            "ruled_out": total.ruled_out,
            "inserted": total.inserted,
            "match_rate": total.match_rate,
            "last_run_unix": now_unix,
            "workspace_modules": modules.len(),
        })
        .to_string(),
    );

    Ok(total)
}

/// Run the Go overlay (`provider::GO`) — the Go-specific counterpart to the
/// Rust-only block each of the 3 production call sites (`lib.rs`,
/// `watcher.rs`, `main.rs`) already had before this existed. Branches to
/// `run_go_workspace_overlay` when `root` has a `go.work` file (see
/// `provider::parse_go_work_modules`); every other language, and a
/// single-module Go project (the common case — no `go.work` present at
/// all), takes the unchanged single-root `run_and_refresh` path.
pub fn run_go_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::GoConfig,
) -> anyhow::Result<ingest::IngestStats> {
    if let Some(modules) = provider::parse_go_work_modules(root) {
        return run_go_workspace_overlay(conn, root, &cfg.scip, &modules, false);
    }
    run_and_refresh(&provider::GO, conn, root, &cfg.scip, false)
}

/// Run the Python overlay (`provider::PYTHON`) — the Python-specific
/// counterpart to `run_go_overlay_and_log`, same shape. Coexists with
/// Python's existing stack-graphs formal tier (`resolver::formal`) via the
/// `formal_source` provenance P0.3 already built — no special handling
/// needed here for that.
pub fn run_python_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::PythonConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::PYTHON, conn, root, &cfg.scip, false)
}

/// Run the JS/TS overlay (`provider::TYPESCRIPT`) — same shape as
/// `run_go_overlay_and_log`/`run_python_overlay_and_log`. Coexists with the
/// pre-existing stack-graphs formal tier for TypeScript (and the P1.1
/// stopgap for JavaScript) via the same `formal_source` provenance
/// mechanism Python's provider already relies on.
pub fn run_js_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::JsConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::TYPESCRIPT, conn, root, &cfg.scip, false)
}

/// Run the Java overlay (`provider::JAVA`) — same shape as
/// `run_go_overlay_and_log`/`run_python_overlay_and_log`/
/// `run_js_overlay_and_log`. Java, like Python/TypeScript, already has a
/// pre-existing stack-graphs formal tier (`resolver::formal::load_java`,
/// wired into both `run_indexing_pipeline` and `reindex_changed`) — the two
/// coexist via the same `formal_source` provenance P0.3 already built
/// (`ingest_occurrences` may override a `'stack_graphs'` row but never its
/// own prior `'scip'` verdict), no special handling needed here for that.
pub fn run_java_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::JavaConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::JAVA, conn, root, &cfg.scip, false)
}

/// Run the C# overlay (`provider::CSHARP`) — same shape as
/// `run_go_overlay_and_log`/`run_java_overlay_and_log`. No pre-existing
/// formal tier to coexist with (C# was never in `resolver::formal`'s
/// stack-graphs registrations — confirmed against the real registration
/// list in `formal.rs`, unlike the Java doc comment above which got this
/// wrong on the first pass), so there's no provenance-override concern.
pub fn run_csharp_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::CSharpConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::CSHARP, conn, root, &cfg.scip, false)
}

/// Run the PHP overlay (`provider::PHP`) — same shape as the other
/// providers. Like C#, PHP has no pre-existing formal tier to coexist with.
pub fn run_php_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::PhpConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::PHP, conn, root, &cfg.scip, false)
}

/// Run the C/C++ overlay (`provider::CLANG`) — same shape as the other
/// providers, but see `provider::CLANG`'s own doc comment: this session
/// could not obtain a real `scip-clang` binary (GitHub Releases blocked,
/// no Bazel), so this path is exercised only by unit tests, never a real
/// external indexer, in this checkout's CI. Like C#/PHP, C/C++ has no
/// pre-existing formal tier to coexist with.
pub fn run_clang_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::ClangConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::CLANG, conn, root, &cfg.scip, false)
}

/// Run the Ruby overlay (`provider::RUBY`) — same shape as the other
/// providers. Like C#/PHP/C++, Ruby has no pre-existing formal tier to
/// coexist with (not in `resolver::formal`'s stack-graphs registrations).
pub fn run_ruby_overlay_and_log(
    conn: &Connection,
    root: &Path,
    cfg: &crate::config::RubyConfig,
) -> anyhow::Result<ingest::IngestStats> {
    run_and_refresh(&provider::RUBY, conn, root, &cfg.scip, false)
}

/// Manually refresh one or every SCIP provider right now, bypassing
/// `cfg.policy`'s automatic-run gate (`force: true` — see
/// `run_overlay_for`) — the shared entry point behind `calm scip run` and
/// the `scip_refresh` MCP tool (P2.6). `lang`: `None` or `Some("all")` runs
/// every provider in the table, in this fixed order; `Some("go"|"python"|
/// "javascript"|"rust")` runs just that one. An unrecognized `lang` is an
/// `Err`, not a silent no-op — this is an explicit user request, not an
/// auto-detect probe. Stops at the first hard error (a real DB failure from
/// `ingest_occurrences`, not an unavailable/failing external indexer, which
/// `run_overlay_for` already swallows into `Ok(default)`) rather than
/// silently skipping the remaining providers.
pub fn refresh_language(
    conn: &Connection,
    root: &Path,
    config: &crate::config::Config,
    lang: Option<&str>,
) -> anyhow::Result<Vec<(String, ingest::IngestStats)>> {
    let all = [
        "rust",
        "go",
        "python",
        "javascript",
        "java",
        "csharp",
        "php",
        "c",
        "ruby",
    ];
    let want: &[&str] = match lang {
        None | Some("all") => &all,
        Some(l) if all.contains(&l) => std::slice::from_ref(
            all.iter()
                .find(|x| **x == l)
                .expect("just checked contains"),
        ),
        Some(other) => anyhow::bail!(
            "unknown SCIP provider {other:?} — expected one of: rust, go, python, javascript, java, csharp, php, c, ruby, all"
        ),
    };
    let mut out = Vec::with_capacity(want.len());
    for lang in want {
        let stats = match *lang {
            "rust" => run_and_refresh(&provider::RUST, conn, root, &config.rust.scip, true)?,
            "go" => match provider::parse_go_work_modules(root) {
                Some(modules) => {
                    run_go_workspace_overlay(conn, root, &config.go.scip, &modules, true)?
                }
                None => run_and_refresh(&provider::GO, conn, root, &config.go.scip, true)?,
            },
            "python" => run_and_refresh(&provider::PYTHON, conn, root, &config.python.scip, true)?,
            "javascript" => {
                run_and_refresh(&provider::TYPESCRIPT, conn, root, &config.js.scip, true)?
            }
            "csharp" => run_and_refresh(&provider::CSHARP, conn, root, &config.csharp.scip, true)?,
            "java" => run_and_refresh(&provider::JAVA, conn, root, &config.java.scip, true)?,
            "php" => run_and_refresh(&provider::PHP, conn, root, &config.php.scip, true)?,
            "c" => run_and_refresh(&provider::CLANG, conn, root, &config.clang.scip, true)?,
            "ruby" => run_and_refresh(&provider::RUBY, conn, root, &config.ruby.scip, true)?,
            _ => unreachable!("want is filtered to `all` above"),
        };
        out.push((lang.to_string(), stats));
    }
    Ok(out)
}
/// Cheap, non-invoking snapshot of the overlay's readiness — never spawns an
/// external indexer, just checks binary presence and compares the cache key
/// that `run_overlay_for` would compute against what's already on disk.
/// Backs `indexing_status`'s `scip_overlay` field so an agent can tell
/// whether the call graph for currently-edited files has actually been
/// upgraded by SCIP yet, without waiting on or triggering a real run. `None`
/// when `cfg.enabled == Some(false)` — overlay is off, nothing to report.
pub fn overlay_status_for(
    provider: &provider::ScipProvider,
    conn: &Connection,
    root: &Path,
    cfg: &ScipConfig,
) -> Option<OverlayStatus> {
    if cfg.enabled == Some(false) {
        return None;
    }
    let bin = (provider.resolve_binary)(cfg.binary.as_deref(), root);
    let available = bin.is_some();
    let up_to_date = match &bin {
        Some(bin) => {
            let dirty = source_dirty_keys(conn, provider.dirty_langs);
            let key = (provider.cache_key)(bin, root, &dirty);
            let cache_path = root.join(".calm").join(provider.cache_file_name);
            std::fs::read_to_string(&cache_path).is_ok_and(|prev| prev.trim() == key)
        }
        None => false,
    };
    let (last_match_rate, last_inserted) = read_last_stats(root, provider);
    let last_run_unix = read_last_run_unix(root, provider);
    Some(OverlayStatus {
        available,
        up_to_date,
        last_match_rate,
        last_inserted,
        last_run_unix,
    })
}

/// Rust-only convenience wrapper around `overlay_status_for` — kept so the
/// one production caller (`recover.rs`'s `indexing_status`) doesn't need to
/// know about `ScipProvider` yet.
pub fn overlay_status(conn: &Connection, root: &Path, rust: &RustConfig) -> Option<OverlayStatus> {
    overlay_status_for(&provider::RUST, conn, root, &rust.scip)
}

/// Best-effort read of the sidecar `run_overlay_for` writes after a real
/// (non-cache-skip) run — `inserted`/`match_rate` aren't derivable from
/// `call_edges` alone at read time the way `available`/`up_to_date` are, so
/// they have to come from whatever the last real run actually observed.
/// Absent (never run) or corrupt — both `(None, None)`, not an error; this is
/// a diagnostic nicety, not load-bearing.
fn read_last_stats(root: &Path, provider: &provider::ScipProvider) -> (Option<f64>, Option<usize>) {
    let path = root.join(".calm").join(stats_file_name(provider));
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (None, None);
    };
    (
        v.get("match_rate").and_then(|x| x.as_f64()),
        v.get("inserted")
            .and_then(|x| x.as_u64())
            .map(|n| n as usize),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayStatus {
    /// `rust-analyzer` binary was found (PATH/rustup/VS Code) at last check.
    pub available: bool,
    /// The current Rust source fingerprint + toolchain + lockfile match the
    /// last successful overlay run's cache key — `false` means the next
    /// `run_overlay` call (or the next non-noop incremental reindex, if
    /// wired to call it) would actually invoke rust-analyzer again rather
    /// than cache-skip. Always `false` when `available` is `false`.
    pub up_to_date: bool,
    /// `IngestStats::match_rate` from the last real (non-cache-skip)
    /// `run_overlay` invocation, or `None` if it has never actually run
    /// (a fresh checkout, or `available == false`). Stale the moment
    /// `up_to_date` is `false` — read that first.
    pub last_match_rate: Option<f64>,
    /// `IngestStats::inserted` from that same last real run, or `None` for
    /// the same reasons as `last_match_rate`.
    pub last_inserted: Option<usize>,
    /// Unix seconds of that same last real run, or `None` if it has never
    /// actually run. Raw epoch (not formatted) — this type lives in
    /// `calm-core`, which doesn't depend on the ISO8601 formatter
    /// `calm-server`'s MCP output types already use for `last_updated`; the
    /// MCP boundary formats it the same way.
    pub last_run_unix: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Some(false)` (explicit force-off) must be a no-op regardless of what's
    /// actually on this machine's `PATH` — unlike unset/auto-detect, this
    /// path skips before ever probing for a binary, so it's safe to assert
    /// deterministically even on a dev box with rust-analyzer installed (this
    /// one is; see `runner::tests::detect_returns_none_when_binary_absent`
    /// for why the unset/auto-detect case can't be tested the same way).
    #[test]
    fn explicit_off_is_a_noop_even_when_rust_analyzer_is_on_path() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let rust = RustConfig {
            lsp: crate::config::LspConfig::default(),
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_overlay(&conn, Path::new("."), &rust, &[]).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee as `explicit_off_is_a_noop_even_when_rust_analyzer_is_on_path`,
    /// for the Go provider added in P2.1 — `run_go_overlay_and_log` must
    /// short-circuit on `enabled: Some(false)` before ever probing `PATH` for
    /// `scip-go`, deterministically regardless of whether this machine
    /// happens to have it installed (this sandbox does).
    #[test]
    fn go_explicit_off_is_a_noop_even_when_scip_go_is_on_path() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let go = crate::config::GoConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
            lsp: crate::config::LspConfig::default(),
        };
        assert_eq!(
            run_go_overlay_and_log(&conn, Path::new("."), &go).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee as the Go/Rust equivalents, for the Python provider
    /// added in P2.4 — `run_python_overlay_and_log` must short-circuit on
    /// `enabled: Some(false)` before ever probing for `scip-python`
    /// (deterministic regardless of whether this sandbox can reach npm).
    #[test]
    fn python_explicit_off_is_a_noop_even_when_scip_python_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let python = crate::config::PythonConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_python_overlay_and_log(&conn, Path::new("."), &python).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee as the Go/Python equivalents, for the JS/TS provider
    /// added in P3.2 — `run_js_overlay_and_log` must short-circuit on
    /// `enabled: Some(false)` before ever probing for `scip-typescript`
    /// (deterministic regardless of whether this sandbox can reach npm).
    #[test]
    fn js_explicit_off_is_a_noop_even_when_scip_typescript_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let js = crate::config::JsConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_js_overlay_and_log(&conn, Path::new("."), &js).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee as the Go/Python/JS equivalents, for the Java provider
    /// added in P2.2 — `run_java_overlay_and_log` must short-circuit on
    /// `enabled: Some(false)` before ever probing for `scip-java`
    /// (deterministic regardless of whether this sandbox has one bootstrapped
    /// on `PATH`).
    #[test]
    fn java_explicit_off_is_a_noop_even_when_scip_java_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let java = crate::config::JavaConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_java_overlay_and_log(&conn, Path::new("."), &java).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee, for the C# provider added in P2.3.
    #[test]
    fn csharp_explicit_off_is_a_noop_even_when_scip_dotnet_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let csharp = crate::config::CSharpConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_csharp_overlay_and_log(&conn, Path::new("."), &csharp).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee, for the PHP provider added in P2.5.
    #[test]
    fn php_explicit_off_is_a_noop_even_when_scip_php_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let php = crate::config::PhpConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_php_overlay_and_log(&conn, Path::new("."), &php).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// Same guarantee, for the C/C++ provider added in P3.1.
    #[test]
    fn clang_explicit_off_is_a_noop_even_when_scip_clang_is_reachable() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let clang = crate::config::ClangConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
            lsp: crate::config::LspConfig::default(),
        };
        assert_eq!(
            run_clang_overlay_and_log(&conn, Path::new("."), &clang).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// A project with zero indexed files of a provider's language(s) must
    /// short-circuit to a no-op *before* ever probing for a binary — even
    /// with `enabled: Some(true)`, which would otherwise log a "not found"
    /// line if the probe actually ran and came up empty. Deterministic
    /// regardless of whether this machine has rust-analyzer installed (it
    /// does): the in-memory DB below has a "python" file but no "rust"
    /// ones, so `run_overlay` must never reach `resolve_binary` at all.
    #[test]
    fn provider_with_zero_matching_files_is_a_noop_even_when_forced_on() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('main.py', 'h', 'python', 0.0)",
            [],
        )
        .unwrap();
        let rust = RustConfig {
            lsp: crate::config::LspConfig::default(),
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(
            run_overlay(&conn, Path::new("."), &rust, &[]).unwrap(),
            ingest::IngestStats::default()
        );
    }

    /// `refresh_language` rejects an unrecognized provider name outright
    /// (an explicit user request, not an auto-detect probe) — deterministic
    /// regardless of any binary/network availability since the check runs
    /// before any provider is touched.
    #[test]
    fn refresh_language_rejects_unknown_lang() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let config = crate::config::Config::default();
        let err = refresh_language(&conn, Path::new("."), &config, Some("cobol")).unwrap_err();
        assert!(err.to_string().contains("cobol"));
    }

    /// `None`/`Some("all")` runs every provider in the table, in the same
    /// fixed order, one result per provider — all 8 configs are disabled so
    /// this is deterministic regardless of what's installed on this machine.
    #[test]
    fn refresh_language_all_covers_every_provider_in_order() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let mut config = crate::config::Config::default();
        let off = crate::config::ScipConfig {
            enabled: Some(false),
            binary: None,
            insert_missing: None,
            policy: crate::config::RefreshPolicy::OnSave,
        };
        config.rust.scip = off.clone();
        config.go.scip = off.clone();
        config.python.scip = off.clone();
        config.js.scip = off.clone();
        config.java.scip = off.clone();
        config.csharp.scip = off.clone();
        config.php.scip = off.clone();
        config.clang.scip = off.clone();
        config.ruby.scip = off;

        let results = refresh_language(&conn, Path::new("."), &config, None).unwrap();
        let langs: Vec<&str> = results.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            langs,
            vec![
                "rust",
                "go",
                "python",
                "javascript",
                "java",
                "csharp",
                "php",
                "c",
                "ruby"
            ]
        );
        assert!(
            results
                .iter()
                .all(|(_, s)| *s == ingest::IngestStats::default())
        );

        let results_explicit_all =
            refresh_language(&conn, Path::new("."), &config, Some("all")).unwrap();
        assert_eq!(results_explicit_all.len(), 9);
    }

    /// A specific `lang` runs only that one provider.
    #[test]
    fn refresh_language_single_lang_runs_only_that_one() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let mut config = crate::config::Config::default();
        config.go.scip.enabled = Some(false);

        let results = refresh_language(&conn, Path::new("."), &config, Some("go")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "go");
    }

    #[test]
    fn rust_source_dirty_keys_reflects_path_and_hash_rust_only() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, 0.0)",
            rusqlite::params!["src/a.rs", "hashA", "rust"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES (?1, ?2, ?3, 0.0)",
            rusqlite::params!["src/main.py", "hashP", "python"],
        )
        .unwrap();

        let keys = rust_source_dirty_keys(&conn);
        assert_eq!(
            keys,
            vec!["src/a.rs@hashA".to_string()],
            "must include only rust files, keyed by path+hash"
        );
    }

    #[test]
    fn rust_source_dirty_keys_changes_when_a_file_hash_changes() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO file_index (path, hash, language, last_indexed) VALUES ('src/a.rs', 'hash1', 'rust', 0.0)",
            [],
        )
        .unwrap();
        let before = rust_source_dirty_keys(&conn);

        conn.execute(
            "UPDATE file_index SET hash = 'hash2' WHERE path = 'src/a.rs'",
            [],
        )
        .unwrap();
        let after = rust_source_dirty_keys(&conn);

        assert_ne!(
            before, after,
            "editing a rust file's content must change its dirty-key entry"
        );
    }

    #[test]
    fn overlay_status_none_when_explicitly_disabled() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let rust = RustConfig {
            lsp: crate::config::LspConfig::default(),
            scip: crate::config::ScipConfig {
                enabled: Some(false),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        assert_eq!(overlay_status(&conn, Path::new("."), &rust), None);
    }

    /// Live integration: real rust-analyzer against the Rust fixture workspace
    /// used throughout Phase A. Ignored by default -- requires rust-analyzer
    /// on PATH/rustup/VS Code, and a real `cargo metadata` resolve, neither of
    /// which CI is guaranteed to have for this opt-in feature.
    #[test]
    #[ignore]
    fn overlay_upgrades_a_real_edge_on_the_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust_workspace");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let rust = RustConfig {
            lsp: crate::config::LspConfig::default(),
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let dirty = rust_source_dirty_keys(&conn);
        let stats = run_overlay(&conn, &fixture, &rust, &dirty).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'app/src/main.rs::main' \
                   AND to_symbol = 'core/src/engine.rs::Engine::start'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }

    /// Live integration: real `scip-go` against `multi_lang_workspace/go`
    /// (P2.1 shipped without this — gap closed alongside P3.2). Ignored by
    /// default — requires `scip-go` on `PATH`/`$GOBIN`/`$HOME/go/bin` (`go
    /// install github.com/scip-code/scip-go/cmd/scip-go@latest`), which CI
    /// only installs on the nightly `scip-nightly.yml` job.
    #[test]
    #[ignore]
    fn go_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/go");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let go = crate::config::GoConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
            lsp: crate::config::LspConfig::default(),
        };
        let stats = run_go_overlay_and_log(&conn, &fixture, &go).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.go::main' \
                   AND to_symbol = 'helper.go::Greet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }

    #[test]
    fn parse_go_work_returns_none_when_no_go_work_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(provider::parse_go_work_modules(dir.path()), None);
    }

    #[test]
    fn parse_go_work_single_line_use_directives() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.work"),
            "go 1.22\n\nuse ./moda\nuse ./modb\n",
        )
        .unwrap();
        let modules = provider::parse_go_work_modules(dir.path()).unwrap();
        assert_eq!(
            modules,
            vec![
                std::path::PathBuf::from("./moda"),
                std::path::PathBuf::from("./modb")
            ]
        );
    }

    #[test]
    fn parse_go_work_grouped_use_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.work"),
            "go 1.22\n\nuse (\n\t./moda\n\t./modb\n)\n",
        )
        .unwrap();
        let modules = provider::parse_go_work_modules(dir.path()).unwrap();
        assert_eq!(
            modules,
            vec![
                std::path::PathBuf::from("./moda"),
                std::path::PathBuf::from("./modb")
            ]
        );
    }

    #[test]
    fn parse_go_work_ignores_comments_and_non_use_directives() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.work"),
            "go 1.22 // pinned toolchain\n\n// a leading comment\nuse ./moda // trailing comment\nreplace example.com/foo => ./vendor/foo\nuse (\n\t./modb\n)\n",
        )
        .unwrap();
        let modules = provider::parse_go_work_modules(dir.path()).unwrap();
        assert_eq!(
            modules,
            vec![
                std::path::PathBuf::from("./moda"),
                std::path::PathBuf::from("./modb")
            ]
        );
    }

    #[test]
    fn run_go_workspace_overlay_short_circuits_when_explicitly_disabled() {
        // Deterministic, no real `scip-go` binary needed — proves the
        // guard clauses run before any subprocess spawn, same contract as
        // `run_overlay_for`'s equivalent early-return.
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cfg = crate::config::ScipConfig {
            enabled: Some(false),
            binary: None,
            insert_missing: None,
            policy: crate::config::RefreshPolicy::OnSave,
        };
        let modules = vec![std::path::PathBuf::from("./moda")];
        let stats =
            run_go_workspace_overlay(&conn, std::path::Path::new("."), &cfg, &modules, false)
                .unwrap();
        assert_eq!(stats, ingest::IngestStats::default());
    }

    #[test]
    fn run_go_workspace_overlay_short_circuits_when_no_go_files_indexed() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cfg = crate::config::ScipConfig {
            enabled: Some(true),
            binary: None,
            insert_missing: None,
            policy: crate::config::RefreshPolicy::OnSave,
        };
        let modules = vec![std::path::PathBuf::from("./moda")];
        // No `file_index` rows of language "go" at all — must return the
        // default (empty) stats without ever attempting to resolve a
        // `scip-go` binary or spawn anything.
        let stats =
            run_go_workspace_overlay(&conn, std::path::Path::new("."), &cfg, &modules, false)
                .unwrap();
        assert_eq!(stats, ingest::IngestStats::default());
    }

    /// Live integration: real `scip-go`, run once per member module, against
    /// the 2-module `go_workspace` fixture — proves module enumeration +
    /// per-module `sub_root` rebasing both work end-to-end, not just the
    /// pure `parse_go_work_modules` parsing logic above. Ignored by default,
    /// same reason and same `scip-go` install path as the single-module live
    /// test above; unlike that one, this fixture/test pair has not yet been
    /// run against a real `scip-go` binary in any environment (none was
    /// available while writing it — see the roadmap plan's own note on this)
    /// — treat a first real run's result with appropriately less confidence
    /// than the already-proven single-module path until it has been.
    #[test]
    #[ignore]
    fn go_workspace_overlay_upgrades_edges_in_both_member_modules() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/go_workspace");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let go = crate::config::GoConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
            lsp: crate::config::LspConfig::default(),
        };
        let stats = run_go_overlay_and_log(&conn, &fixture, &go).unwrap();
        assert!(
            stats.upgraded >= 2,
            "expected at least one upgraded edge per module (2 modules), got: {stats:?}"
        );

        for (module, from_sym, to_sym) in [
            ("moda", "moda/main.go::main", "moda/helper.go::Greet"),
            ("modb", "modb/main.go::main", "modb/helper.go::Greet"),
        ] {
            let conf: String = conn
                .query_row(
                    "SELECT edge_confidence FROM call_edges WHERE from_symbol = ?1 AND to_symbol = ?2",
                    [from_sym, to_sym],
                    |r| r.get(0),
                )
                .unwrap_or_else(|e| panic!("module {module}: edge {from_sym} -> {to_sym} not found ({e})"));
            assert_eq!(
                conf, "formal",
                "module {module}: edge should be upgraded to formal"
            );
        }
    }

    /// Live integration: real `scip-python` (via `npx`) against
    /// `multi_lang_workspace/python` (P2.4 shipped without this — gap closed
    /// alongside P3.2). Ignored by default — requires Node/npm reachable on
    /// `PATH` and network access to the npm registry on first run.
    #[test]
    #[ignore]
    fn python_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/python");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let python = crate::config::PythonConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_python_overlay_and_log(&conn, &fixture, &python).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.py::run' \
                   AND to_symbol = 'pkg/helper.py::helper'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }

    /// Live integration: real `scip-typescript` (via `npx`) against
    /// `multi_lang_workspace/js` (P3.2). Ignored by default — requires
    /// Node/npm reachable on `PATH` and network access to the npm registry
    /// on first run.
    #[test]
    #[ignore]
    fn js_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/js");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let js = crate::config::JsConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_js_overlay_and_log(&conn, &fixture, &js).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'main.js::run' \
                   AND to_symbol = 'helper.js::greet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }

    /// Live integration: real `scip-java` against `multi_lang_workspace/java`
    /// (P2.2). Ignored by default — requires a `scip-java` launcher on
    /// `PATH` (e.g. `cs bootstrap com.sourcegraph:scip-java_2.13:<version>
    /// -o scip-java`, or an equivalent wrapper resolving the same Maven
    /// Central artifact — see the 8-language plan's P2.2 for how this was
    /// verified without `coursier`/Docker in the session that wrote this
    /// test) plus a JDK and Maven/Gradle reachable on `PATH` — `scip-java`
    /// drives a real build.
    #[test]
    #[ignore]
    fn java_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/java");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let java = crate::config::JavaConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_java_overlay_and_log(&conn, &fixture, &java).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'src/main/java/com/example/Main.java::Main::main' \
                   AND to_symbol = 'src/main/java/com/example/Helper.java::Helper::greet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }
    /// Live integration: real `scip-java` against
    /// `multi_lang_workspace/kotlin` (Phase D.2, 2026-07-11) — proves the
    /// `JAVA` provider's `dirty_langs` extension actually resolves Kotlin
    /// occurrences end to end, not just that the config field compiles.
    /// Ignored by default — same prerequisites as the Java test above (a
    /// `scip-java` launcher on `PATH`, plus a JDK and a Gradle new enough
    /// for the Kotlin Gradle plugin — this repo's system `gradle` was found
    /// to be 4.4.1 during the session that wrote this test, too old for
    /// `kotlin("jvm") version "1.9.22"`; a `gradle` 8.x ahead of it on
    /// `PATH` is required). `scip-java` drives a real Gradle build
    /// (confirmed live: `clean scipPrintDependencies scipCompileAll`).
    ///
    /// The fixture's `Dispatcher.dispatch` deliberately calls
    /// `handler.process()` from two different `is X ->` smart-cast branches
    /// of the same `when` on the same variable — CALM's own syntactic
    /// resolver cannot follow Kotlin smart-casting, so both call sites land
    /// as `ambiguous` with the same 2 candidates (`AlphaHandler::process`/
    /// `BetaHandler::process`) before this overlay runs. `scip-java`'s real
    /// Kotlin type-checker resolves each site to its OWN specific target
    /// and correctly rules out the other branch's candidate — asserting
    /// both sites land on the RIGHT one each (not just "some edge went
    /// formal") is the actual value proposition of a real semantic indexer
    /// over the heuristic resolver, and was verified once by hand before
    /// this test was written (`calm scip-run --lang java` against a copy of
    /// this exact fixture: 8 edges upgraded, 2 fan-out siblings ruled out).
    #[test]
    #[ignore]
    fn kotlin_overlay_upgrades_ambiguous_smart_cast_calls_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/kotlin");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before_alpha: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'src/main/kotlin/com/example/Handlers.kt::Dispatcher::dispatch' \
                   AND to_symbol = 'src/main/kotlin/com/example/Handlers.kt::AlphaHandler::process' \
                   AND call_site_line = 23",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            before_alpha, "ambiguous",
            "fixture must start ambiguous — CALM's syntactic resolver can't follow Kotlin smart-casts"
        );

        let java = crate::config::JavaConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_java_overlay_and_log(&conn, &fixture, &java).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one Kotlin edge upgraded to formal via the java provider"
        );

        // Line 23 (`is AlphaHandler -> handler.process()`) must resolve to
        // AlphaHandler's own process(), not BetaHandler's.
        let (alpha_conf, alpha_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'src/main/kotlin/com/example/Handlers.kt::Dispatcher::dispatch' \
                   AND to_symbol = 'src/main/kotlin/com/example/Handlers.kt::AlphaHandler::process' \
                   AND call_site_line = 23",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(alpha_conf, "formal");
        assert_eq!(alpha_src.as_deref(), Some("scip"));

        // Line 24 (`is BetaHandler -> handler.process()`) must resolve to
        // BetaHandler's own process(), not AlphaHandler's.
        let (beta_conf, beta_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'src/main/kotlin/com/example/Handlers.kt::Dispatcher::dispatch' \
                   AND to_symbol = 'src/main/kotlin/com/example/Handlers.kt::BetaHandler::process' \
                   AND call_site_line = 24",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(beta_conf, "formal");
        assert_eq!(beta_src.as_deref(), Some("scip"));

        // And the WRONG cross-pairing at each site must still be ambiguous
        // (ruled out, not silently upgraded) — this is the actual
        // disambiguation proof, not just "something changed".
        let wrong_at_23: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'src/main/kotlin/com/example/Handlers.kt::Dispatcher::dispatch' \
                   AND to_symbol = 'src/main/kotlin/com/example/Handlers.kt::BetaHandler::process' \
                   AND call_site_line = 23",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_23, "ambiguous");
        let wrong_at_24: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'src/main/kotlin/com/example/Handlers.kt::Dispatcher::dispatch' \
                   AND to_symbol = 'src/main/kotlin/com/example/Handlers.kt::AlphaHandler::process' \
                   AND call_site_line = 24",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_24, "ambiguous");
    }

    /// Live integration: real `scip-ruby` against
    /// `multi_lang_workspace/ruby` (Phase D.1, 2026-07-11) — the plan's own
    /// flagged open question ("Sorbet vốn thiết kế cho codebase có type
    /// annotation; cần research độ chính xác thật trên Ruby không có
    /// annotation trước khi cam kết formal tier cho Ruby") answered with a
    /// real binary against a genuinely `# typed: false` (untyped, no `sig`
    /// blocks anywhere) fixture, not assumed.
    ///
    /// `route`'s `case handler when AlphaHandler ... when BetaHandler ...`
    /// is the Ruby analogue of Kotlin's `is X ->` / Go's type switch: CALM's
    /// syntactic resolver can't follow it, so `handler.process` in both
    /// branches lands `ambiguous` with the same 2 candidates
    /// (`AlphaHandler#process`/`BetaHandler#process`) before this overlay
    /// runs. Confirmed by inspecting the raw `.scip` output's own
    /// hover-type strings before writing this test: `handler (AlphaHandler)`
    /// inside the first `when`, `handler (BetaHandler)` inside the second,
    /// `handler (T.any(AlphaHandler, BetaHandler))` before the narrowing
    /// point — Sorbet performs real flow-sensitive type narrowing on
    /// `case/when` even with zero `sig` annotations in the file, and that
    /// narrowing is what resolves each branch's `.process` call to its own
    /// specific target and rules out the other. Ignored by default — needs
    /// a *standalone* `scip-ruby` binary on `PATH` (the README's own
    /// "download binary and index" method) — see
    /// `scip::runner::ruby_resolve_binary`'s doc comment for why the
    /// ordinary `gem install scip-ruby` wrapper script doesn't work here.
    #[test]
    #[ignore]
    fn ruby_overlay_upgrades_ambiguous_case_when_calls_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/ruby");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let before_alpha: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.rb::Dispatcher::route' \
                   AND to_symbol = 'handlers.rb::AlphaHandler::process' \
                   AND call_site_line = 24",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            before_alpha, "ambiguous",
            "fixture must start ambiguous — CALM's syntactic resolver can't follow Ruby's case/when"
        );

        let ruby = crate::config::RubyConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_ruby_overlay_and_log(&conn, &fixture, &ruby).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one Ruby edge upgraded to formal via scip-ruby"
        );

        // Line 24 (`when AlphaHandler ... handler.process`) must resolve to
        // AlphaHandler's own process, not BetaHandler's.
        let (alpha_conf, alpha_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'handlers.rb::Dispatcher::route' \
                   AND to_symbol = 'handlers.rb::AlphaHandler::process' \
                   AND call_site_line = 24",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(alpha_conf, "formal");
        assert_eq!(alpha_src.as_deref(), Some("scip"));

        // Line 26 (`when BetaHandler ... handler.process`) must resolve to
        // BetaHandler's own process, not AlphaHandler's.
        let (beta_conf, beta_src): (String, Option<String>) = conn
            .query_row(
                "SELECT edge_confidence, formal_source FROM call_edges \
                 WHERE from_symbol = 'handlers.rb::Dispatcher::route' \
                   AND to_symbol = 'handlers.rb::BetaHandler::process' \
                   AND call_site_line = 26",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(beta_conf, "formal");
        assert_eq!(beta_src.as_deref(), Some("scip"));

        // The wrong cross-pairing at each site must still be ambiguous —
        // the actual disambiguation proof, not just "something changed".
        let wrong_at_24: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.rb::Dispatcher::route' \
                   AND to_symbol = 'handlers.rb::BetaHandler::process' \
                   AND call_site_line = 24",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_24, "ambiguous");
        let wrong_at_26: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'handlers.rb::Dispatcher::route' \
                   AND to_symbol = 'handlers.rb::AlphaHandler::process' \
                   AND call_site_line = 26",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(wrong_at_26, "ambiguous");
    }

    /// Live integration: real `scip-dotnet` against
    /// `multi_lang_workspace/csharp` (P2.3). Ignored by default — requires a
    /// `scip-dotnet` launcher on `PATH` (`dotnet tool install --global
    /// scip-dotnet` — a real published NuGet package, no bespoke bootstrap
    /// needed unlike scip-java) plus a .NET SDK reachable on `PATH` —
    /// `scip-dotnet` drives a real `dotnet restore` + build.
    #[test]
    #[ignore]
    fn csharp_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/csharp");
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let csharp = crate::config::CSharpConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_csharp_overlay_and_log(&conn, &fixture, &csharp).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'Program.cs::Program::Main' \
                   AND to_symbol = 'Helper.cs::Helper::Greet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");
    }

    /// Live integration: real `scip-php` against `multi_lang_workspace/php`
    /// (P2.5). Ignored by default — requires `composer` and a `scip-php`
    /// launcher on `PATH` (`composer install` inside a checkout of
    /// `davidrjenni/scip-php`, then this test's own `composer install` step
    /// below to give the *fixture* its own `vendor/autoload.php`, which
    /// `php_resolve_binary` gates on).
    ///
    /// Real bugs found in the session that wrote this test:
    ///
    /// 1. `scip-php` itself crashes (`TypeError: Cannot assign null to
    ///    Composer::$pkgVersion`) when the project it indexes has no git
    ///    commit reference at all (Composer's generated
    ///    `vendor/composer/installed.php` sets the root package's
    ///    `reference` to `null` outside any VCS) — there's no CLI flag to
    ///    work around this, unlike scip-python's `--project-version`. This
    ///    fixture works specifically *because* it lives inside CALM's own
    ///    git checkout (Composer's VCS detection walks up to the nearest
    ///    `.git`, which it finds at the repo root) — confirmed by
    ///    reproducing the crash against an out-of-repo copy of this same
    ///    fixture and getting a real `index.scip` against the in-repo path
    ///    unchanged.
    /// 2. `scip-php`'s `Types::type()` (`src/Types/Types.php`) has no
    ///    local-variable data-flow analysis: a plain `Variable` node only
    ///    resolves when its name is literally `this`, so
    ///    `$helper = new Helper(); $helper->greet();` never resolves
    ///    `$helper`'s type and silently emits no occurrence for the
    ///    `->greet()` call at all — no crash, just a missing reference. It
    ///    *does* resolve a `New_` expression inline, so this fixture calls
    ///    `(new Helper())->greet(...)` directly (see `index.php`) to stay
    ///    within what the tool can actually type-check, rather than
    ///    asserting on a call shape it cannot resolve.
    #[test]
    #[ignore]
    fn php_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture() {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/multi_lang_workspace/php");
        // Composer-managed `vendor/` is gitignored and not committed (see
        // .gitignore) — regenerate it here, matching what a real project's
        // own setup step would already have done before enabling this
        // overlay. No network round-trip: the fixture's composer.json
        // declares zero external dependencies, so this only builds the
        // PSR-4 autoloader from `composer.lock`.
        let status = std::process::Command::new("composer")
            .args(["install", "--no-interaction"])
            .current_dir(&fixture)
            .status()
            .expect("composer must be on PATH for this ignored test");
        assert!(status.success(), "composer install failed");

        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let phase = std::sync::Arc::new(std::sync::RwLock::new(
            crate::types::IndexingPhase::Scanning,
        ));
        crate::indexer::pipeline::run_indexing_pipeline(&mut conn, &fixture, phase).unwrap();

        let php = crate::config::PhpConfig {
            scip: crate::config::ScipConfig {
                enabled: Some(true),
                binary: None,
                insert_missing: None,
                policy: crate::config::RefreshPolicy::OnSave,
            },
        };
        let stats = run_php_overlay_and_log(&conn, &fixture, &php).unwrap();
        assert!(
            stats.upgraded > 0,
            "expected at least one edge upgraded to formal"
        );
        let conf: String = conn
            .query_row(
                "SELECT edge_confidence FROM call_edges \
                 WHERE from_symbol = 'index.php::run' \
                   AND to_symbol = 'src/Helper.php::Helper::greet'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(conf, "formal");

        // Best-effort cleanup so a local `--ignored` run doesn't leave
        // gitignored-but-real build artifacts sitting in a tracked fixture
        // directory indefinitely between runs.
        let _ = std::fs::remove_dir_all(fixture.join("vendor"));
        let _ = std::fs::remove_file(fixture.join("composer.lock"));
        let _ = std::fs::remove_file(fixture.join("index.scip"));
    }
}
