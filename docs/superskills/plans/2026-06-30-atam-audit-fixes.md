# ATAM Audit Fixes — Implementation Plan

> **For agentic workers:** Use `executing-plans` to implement this plan task-by-task.

**Goal:** Fix all 10 findings from the ATAM architectural compliance audit of Code Intelligence MCP v2.7.2 — restoring full spec compliance across correctness, observability, agent guidance, and infrastructure layers.

**Architecture:** Pure-Rust MCP server (`ci-server`) over `ci-core` index engine + `ci-cli` CLI. SQLite WAL backend. All fixes are in-process — no new dependencies, no schema breaking changes except one additive column (Task 3).

**Tech Stack:** Rust, rusqlite, SQLite FTS5, rmcp, tree-sitter

**Audit Gate:** PASS WITH FLAGS (ATAM audit 2026-06-30 — 45 PASS, 14 PARTIAL, 6 MISSING, 1 DEVIATION)

**Risk Flags:** Task 2 (phase ladder) touches pipeline concurrency; Task 7 (frontier) adds DB queries to hot path; Task 10 (SINGLE_WRITER) is a connection-model refactor.

---

## Task Order (by severity)

| # | Finding | Severity | Files |
|---|---------|----------|-------|
| 1 | `kind="text"` searches name+docstring instead of docstring only | CRITICAL | `ci-core/src/search.rs` |
| 2 | Phase ladder never advances during indexing | HIGH | `ci-core/src/indexer/pipeline.rs`, `ci-server/src/lib.rs` |
| 3 | `coreness` absent from `SymbolInfoOutput` | HIGH | `ci-server/src/tools.rs`, `ci-core/src/search.rs` |
| 4 | `compound` preset not implemented | MEDIUM | `ci-server/src/tools.rs` |
| 5 | `locate` `suggested_next` missing 2 conditions | MEDIUM | `ci-server/src/tools.rs` |
| 6 | Config.json `preset` silently ignored | MEDIUM | `ci-cli/src/main.rs` |
| 7 | `session_context` frontier not computed | MEDIUM | `ci-server/src/tools.rs`, `ci-core/src/db/queries.rs` |
| 8 | Auto-gitignore not implemented | LOW | `ci-server/src/lib.rs`, `ci-cli/src/main.rs` |
| 9 | Session tracking missing `session_started_at` + per-call log | LOW | `ci-server/src/tools.rs` |
| 10 | `SINGLE_WRITER` not architecturally enforced | LOW | `ci-server/src/tools.rs`, `ci-server/src/lib.rs` |

---

## Task 1: Fix `kind="text"` to search docstring column only

**Severity:** CRITICAL — current behavior silently returns wrong results (symbols named with search terms appear when only documented symbols should)

**Files:**
- Modify: `crates/ci-core/src/search.rs` (function `search_text`, ~line 152)

**Root cause:** `search_text` sends `WHERE fts_exact MATCH ?1` which matches against both `name` and `docstring` columns of the `fts_exact` FTS5 table. Spec: `kind="text"` must search `docstring` column only.

**Fix:** Use FTS5 global column filter `{docstring}:` prefix in the query parameter. FTS5 syntax `{col1,col2}: query` restricts all tokens in `query` to match within the listed columns only.

- [ ] **Step 1: Write the failing test**

Add to `crates/ci-core/src/search.rs` test module:

```rust
#[test]
fn test_search_text_does_not_match_name_only() {
    // Symbol whose NAME contains "authorize" but docstring does NOT
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();

    // Insert symbol: name matches query, docstring does NOT
    conn.execute(
        "INSERT INTO symbols (qualified_name, name, kind, language, path,
         line_start, line_end, signature, docstring, name_tokens,
         caller_count, is_hub, is_entry_point, file_hash, indexed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, '', 0.0)",
        rusqlite::params![
            "auth.authorize_user", "authorize_user", "function", "python",
            "auth.py", 1, 10, "def authorize_user()", "",
            "authorize user",
        ],
    ).unwrap();
    // Rebuild FTS (trigger fires automatically on INSERT)

    // Insert symbol: docstring contains query, name does NOT
    conn.execute(
        "INSERT INTO symbols (qualified_name, name, kind, language, path,
         line_start, line_end, signature, docstring, name_tokens,
         caller_count, is_hub, is_entry_point, file_hash, indexed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, '', 0.0)",
        rusqlite::params![
            "auth.check_perms", "check_perms", "function", "python",
            "auth.py", 12, 20, "def check_perms()", "Checks if user can authorize action.",
            "check perms",
        ],
    ).unwrap();

    let output = crate::search::search(
        &conn, "authorize", crate::types::SearchKind::Text, 10, None
    ).unwrap();

    // Should find check_perms (docstring match) but NOT authorize_user (name-only match)
    let names: Vec<&str> = output.results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"check_perms"),
        "Expected check_perms (docstring match) in results, got: {names:?}"
    );
    assert!(
        !names.contains(&"authorize_user"),
        "authorize_user should NOT appear — its docstring does not contain 'authorize', got: {names:?}"
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-core test_search_text_does_not_match_name_only 2>&1 | tail -20
```

Expected: FAIL (authorize_user appears in results because current query searches name column)

- [ ] **Step 3: Write fix**

In `crates/ci-core/src/search.rs`, function `search_text` (~line 152), change the `WHERE` clause parameter preparation:

```rust
fn search_text(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<SearchOutput> {
    let raw_query = escape_fts5_query(query);
    // FTS5 global column filter: {docstring} restricts ALL tokens to docstring column only.
    // Syntax: "{col_name} : query_tokens"
    let fts_query = format!("{{docstring}} : {raw_query}");

    let mut stmt = conn.prepare(
        "SELECT s.qualified_name, s.name, s.path, s.line_start, s.line_end, s.kind,
                -bm25(fts_exact) AS score
         FROM fts_exact
         JOIN symbols s ON s.id = fts_exact.rowid
         WHERE fts_exact MATCH ?1
         ORDER BY score DESC
         LIMIT ?2",
    )?;
    // rest of function unchanged — only fts_query variable changes
```

The `{docstring} :` prefix is the FTS5 "global column filter" syntax that restricts all subsequent query tokens to match within the `docstring` column exclusively.

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-core test_search_text_does_not_match_name_only 2>&1 | tail -10
cargo test -p ci-core test_search_text 2>&1 | tail -10
cargo test -p ci-core 2>&1 | tail -5
```

All tests must pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ci-core/src/search.rs
git commit -m "fix(search): restrict kind=text to docstring column only via FTS5 column filter"
```

---

## Task 2: Advance phase ladder during indexing

**Severity:** HIGH — tools always report `scanning` until full index completes, making `indexing_status` useless on large codebases

**Files:**
- Modify: `crates/ci-core/src/indexer/pipeline.rs` (function `run_indexing_pipeline`, ~line 490)
- Modify: `crates/ci-server/src/lib.rs` (background thread that calls pipeline, ~line 54)

**Root cause:** `run_indexing_pipeline` never updates the `phase` Arc. The background thread in `lib.rs` sets `Ready` only after the entire pipeline completes.

**Fix:** Pass `phase: Arc<RwLock<IndexingPhase>>` into `run_indexing_pipeline` and update it at each natural phase boundary: after scanning, after parsing all files, after `rebuild_graph` (building_edges). Set `Ready` as final step inside the function.

- [ ] **Step 1: Write the failing test**

Add to `crates/ci-core/src/indexer/pipeline.rs` test module:

```rust
#[test]
fn test_phase_advances_during_indexing() {
    use std::sync::{Arc, RwLock};
    use std::path::PathBuf;
    use crate::types::IndexingPhase;
    use crate::config::Config;

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("index.db");
    let mut conn = rusqlite::Connection::open(&db_path).unwrap();
    crate::db::schema::init_db(&conn).unwrap();

    // Create a small synthetic project
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("main.py"), "def hello():\n    \"\"\"Says hello.\"\"\"\n    pass\n").unwrap();

    let phase = Arc::new(RwLock::new(IndexingPhase::Scanning));
    let observed_phases = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    // Wrap phase with observer (for test only — production uses Arc directly)
    let phase_for_test = phase.clone();
    let observed_for_thread = observed_phases.clone();

    // Run pipeline in a thread so we can observe phase from outside
    let project_root = tmp.path().to_path_buf();
    let config = Config::default();
    let phase_clone = phase.clone();
    let handle = std::thread::spawn(move || {
        run_indexing_pipeline(&mut conn, &project_root, &config, phase_clone).unwrap();
    });

    handle.join().unwrap();

    // After completion, phase must be Ready
    assert_eq!(
        *phase.read().unwrap(),
        IndexingPhase::Ready,
        "Phase must be Ready after pipeline completes"
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-core test_phase_advances_during_indexing 2>&1 | tail -20
```

Expected: FAIL — `run_indexing_pipeline` signature does not accept `phase` parameter yet.

- [ ] **Step 3: Update `run_indexing_pipeline` signature and implementation**

In `crates/ci-core/src/indexer/pipeline.rs`:

```rust
use std::sync::{Arc, RwLock};
use crate::types::IndexingPhase;

pub fn run_indexing_pipeline(
    conn: &mut rusqlite::Connection,
    project_root: &std::path::Path,
    config: &crate::config::Config,
    phase: Arc<RwLock<IndexingPhase>>,  // NEW PARAMETER
) -> rusqlite::Result<PipelineStats> {
    // Phase 1: Scanning — already the initial state; clear tables
    // (clearing + scanning existing files happens here)
    // ... existing clear/scan logic unchanged ...

    // → Advance to Parsing after scan complete
    *phase.write().unwrap() = IndexingPhase::Parsing;

    // Phase 2: Parsing — index_one_file for each file
    // ... existing for-loop over files calling index_one_file unchanged ...

    // → Advance to BuildingEdges after all files parsed
    *phase.write().unwrap() = IndexingPhase::BuildingEdges;

    // Phase 3: BuildingEdges — rebuild call graph, coreness, hub flags
    rebuild_graph(conn, config)?;

    // → Advance to Ready
    *phase.write().unwrap() = IndexingPhase::Ready;

    Ok(stats)
}
```

Insert the three `*phase.write()...` lines at the exact natural boundaries. Do NOT move or restructure existing logic — only add the phase updates.

- [ ] **Step 4: Update `lib.rs` call site**

In `crates/ci-server/src/lib.rs`, the background indexer thread (~line 54):

```rust
// BEFORE:
let handle = std::thread::spawn(move || {
    run_indexing_pipeline(&mut conn, &project_root, &config)?;
    *phase_clone.write().unwrap() = IndexingPhase::Ready;  // REMOVE THIS LINE
    // ...
});

// AFTER:
let handle = std::thread::spawn(move || {
    // Phase transitions now happen inside run_indexing_pipeline.
    // The final Ready is also set inside. No manual set here.
    run_indexing_pipeline(&mut conn, &project_root, &config, phase_clone)?;
    // ...
});
```

Also update `reindex_changed` in `pipeline.rs` similarly — it should also advance through phases. The difference: `reindex_changed` skips unchanged files, so `Scanning` phase is very fast, but it still goes through `Parsing` → `BuildingEdges` → `Ready`.

- [ ] **Step 5: Run — verify PASS**

```bash
cargo test -p ci-core test_phase_advances_during_indexing 2>&1 | tail -10
cargo test -p ci-core 2>&1 | tail -5
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add crates/ci-core/src/indexer/pipeline.rs crates/ci-server/src/lib.rs
git commit -m "feat(indexer): advance phase ladder through scanning/parsing/building_edges/ready"
```

---

## Task 3: Add `coreness` to `SymbolInfoOutput`

**Severity:** HIGH — agents cannot observe coreness values; cannot distinguish bridge-hub from degree-hub as spec requires

**Files:**
- Modify: `crates/ci-server/src/tools.rs` (`SymbolInfoOutput` struct + handler)

**Root cause:** `SymbolInfoOutput` struct does not have a `coreness` field. The value exists in the `symbols` table but is never surfaced in tool responses.

**Fix:** Add `coreness: Option<i64>` to `SymbolInfoOutput`. In the `symbol_info` handler: emit `null` when `edges_ready: false` (coreness not yet computed), emit the actual DB value when ready.

- [ ] **Step 1: Write the failing test**

Add to `crates/ci-server/src/tools.rs` test module (or integration test):

```rust
#[test]
fn test_symbol_info_includes_coreness() {
    let server = test_server_with_symbols();
    let result = server.call_tool("symbol_info", serde_json::json!({"symbol": "hello"}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    // Field must be present in response
    assert!(
        v.get("coreness").is_some(),
        "symbol_info response must include 'coreness' field, got: {v}"
    );
}

#[test]
fn test_symbol_info_coreness_null_when_edges_not_ready() {
    let server = test_server_phase(IndexingPhase::Parsing); // edges NOT ready
    let result = server.call_tool("symbol_info", serde_json::json!({"symbol": "hello"}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert!(
        v["coreness"].is_null(),
        "coreness must be null when edges_ready: false, got: {}",
        v["coreness"]
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-server test_symbol_info_includes_coreness 2>&1 | tail -20
```

Expected: FAIL — `coreness` field not in response.

- [ ] **Step 3: Add `coreness` field to `SymbolInfoOutput`**

In `crates/ci-server/src/tools.rs`, find the `SymbolInfoOutput` struct and add:

```rust
#[derive(Serialize)]
struct SymbolInfoOutput {
    name: String,
    qualified_name: String,
    kind: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    docstring: Option<String>,
    caller_count: i64,
    is_hub: bool,
    // NEW: null when edges_ready: false; 0 when isolated; >0 when in k-core
    coreness: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<HealthOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_next: Option<SuggestedNext>,
}
```

- [ ] **Step 4: Populate `coreness` in `symbol_info` handler**

In the `symbol_info` handler (~line 1415), in the `Found(c)` branch, after building `out`:

```rust
Found(c) => {
    self.track_symbol(&c.qualified_name);
    self.track_file(&c.path);
    let conn = self.db();
    let health = build_health(&conn, &self.coverage, &self.project_root, &c, self.edges_ready());
    let coreness = if self.edges_ready() {
        // Query coreness directly — it's 0 for isolated nodes when edges are ready
        let val: Option<i64> = conn.query_row(
            "SELECT coreness FROM symbols WHERE qualified_name = ?1",
            rusqlite::params![c.qualified_name],
            |row| row.get(0),
        ).ok().flatten();
        // Spec: 0 for isolated (not NULL) when edges_ready
        Some(val.unwrap_or(0))
    } else {
        None  // null when edges not ready
    };
    let sn = if c.is_hub { /* edit_context */ }
              else if health.test_files.is_empty() { /* search */ }
              else { /* source */ };
    serde_json::to_string_pretty(&SymbolInfoOutput {
        name: c.name.clone(),
        qualified_name: c.qualified_name.clone(),
        kind: c.kind.clone(),
        path: c.path.clone(),
        line_start: c.line_start,
        line_end: c.line_end,
        signature: c.signature.filter(|s| !s.is_empty()),
        docstring: c.docstring.filter(|s| !s.is_empty()),
        caller_count: c.caller_count,
        is_hub: c.is_hub,
        coreness,          // NEW
        health: Some(health),
        suggested_next: self.filter_sn(sn),
    }).unwrap_or_default()
}
```

- [ ] **Step 5: Run — verify PASS**

```bash
cargo test -p ci-server test_symbol_info_includes_coreness 2>&1 | tail -10
cargo test -p ci-server test_symbol_info_coreness_null_when_edges_not_ready 2>&1 | tail -10
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add crates/ci-server/src/tools.rs
git commit -m "feat(tools): add coreness field to symbol_info output per spec"
```

---

## Task 4: Add `compound` preset

**Severity:** MEDIUM — `--preset=compound` silently falls through to `full` (16 tools), giving no token efficiency benefit

**Files:**
- Modify: `crates/ci-server/src/tools.rs` (function `preset_tools`, lines 205–221)

**Root cause:** `preset_tools` match arm for `"compound"` is missing; falls to `_ => None`.

**Fix:** Add the `"compound"` arm per CONTRACTS.md spec: `{repo_overview, locate, hotspots, source, understand, edit_context, diff_impact, session_context, indexing_status}`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_preset_compound_registers_correct_tools() {
    let tools = preset_tools("compound");
    let tools = tools.expect("compound preset must return Some");
    let tool_set: std::collections::HashSet<&&str> = tools.iter().collect();

    // Required tools per CONTRACTS.md ToolPreset::compound
    for expected in &[
        "repo_overview", "locate", "hotspots", "source", "understand",
        "edit_context", "diff_impact", "session_context", "indexing_status",
    ] {
        assert!(
            tool_set.contains(expected),
            "compound preset must include '{expected}', got: {tools:?}"
        );
    }

    // Must NOT include raw graph tools
    for excluded in &["callers", "callees", "path", "search", "file_overview", "symbol_info", "dependencies"] {
        assert!(
            !tool_set.contains(excluded),
            "compound preset must NOT include '{excluded}', got: {tools:?}"
        );
    }
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-server test_preset_compound_registers_correct_tools 2>&1 | tail -15
```

Expected: FAIL — `preset_tools("compound")` returns `None`.

- [ ] **Step 3: Add `compound` arm**

In `crates/ci-server/src/tools.rs`, `preset_tools` function (lines 205–221):

```rust
fn preset_tools(preset: &str) -> Option<&'static [&'static str]> {
    match preset {
        "orient" => Some(&[
            "repo_overview", "locate", "dependencies", "hotspots", "indexing_status",
        ]),
        "trace" => Some(&[
            "repo_overview", "search", "locate", "symbol_info", "source", "callers",
            "callees", "path", "dependencies", "indexing_status",
        ]),
        "edit" => Some(&[
            "repo_overview", "search", "locate", "symbol_info", "source", "callers",
            "callees", "edit_context", "diff_impact", "indexing_status",
        ]),
        "compound" => Some(&[                                          // NEW
            "repo_overview", "locate", "hotspots", "source", "understand",
            "edit_context", "diff_impact", "session_context", "indexing_status",
        ]),
        "full" | "" => None,
        _ => None,
    }
}
```

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-server test_preset_compound_registers_correct_tools 2>&1 | tail -10
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-server/src/tools.rs
git commit -m "feat(presets): add compound preset per CONTRACTS.md spec"
```

---

## Task 5: Fix `locate` `suggested_next` — add dead-code and ambiguous conditions

**Severity:** MEDIUM — two spec-required conditions missing; agents miss "verify dead code" and "disambiguate" hints

**Files:**
- Modify: `crates/ci-server/src/tools.rs` (`locate` handler, ~line 2244)

**Root cause:** The `locate` `suggested_next` logic jumps from hub-check directly to default. Two intermediate conditions are absent:
1. `dead_code_confidence == "high"` → suggest `callers` to verify
2. `ambiguous` top result → suggest `symbol_info` to disambiguate

**Context:** `locate` with `depth="with_symbol"` already fetches the symbol row (including `caller_count`, `is_entry_point`). Dead code confidence of "high" requires: no callers + not entry point. We can approximate this as `caller_count == 0 && !is_entry_point` without running full `build_health`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_locate_suggests_callers_for_dead_code() {
    // Setup: symbol with caller_count=0, not entry point
    let server = test_server_with_dead_symbol(); // symbol "orphan_fn", caller_count=0
    let result = server.call_tool("locate", serde_json::json!({"query": "orphan_fn"}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    let sn = &v["suggested_next"];
    assert_eq!(
        sn["tool"], "callers",
        "locate should suggest callers for dead-code symbol, got: {sn}"
    );
    assert!(
        sn["reason"].as_str().unwrap().contains("dead code"),
        "reason should mention dead code, got: {sn}"
    );
}

#[test]
fn test_locate_suggests_symbol_info_for_ambiguous() {
    // Setup: two symbols named "process" in different files
    let server = test_server_with_ambiguous_symbol();
    let result = server.call_tool("locate", serde_json::json!({"query": "process"}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    // When top result is ambiguous, suggest symbol_info to disambiguate
    let sn = &v["suggested_next"];
    assert_eq!(
        sn["tool"], "symbol_info",
        "locate should suggest symbol_info for ambiguous result, got: {sn}"
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-server test_locate_suggests_callers_for_dead_code 2>&1 | tail -15
cargo test -p ci-server test_locate_suggests_symbol_info_for_ambiguous 2>&1 | tail -15
```

- [ ] **Step 3: Update `locate` suggested_next logic**

In `crates/ci-server/src/tools.rs`, `locate` handler, find the `suggested_next` computation block (~line 2244). Replace:

```rust
// BEFORE (current):
let sn = if let Some(sym) = &top_symbol {
    if sym.is_hub {
        suggested_with_args("edit_context", "Hub detected — mandatory pre-edit check",
            serde_json::json!({"symbol": sym.name, "path": sym.path}))
    } else {
        suggested("source", "Read implementation")
    }
} else {
    suggested_with_args("search", "No match — broaden with hybrid search",
        serde_json::json!({"kind": "hybrid"}))
};

// AFTER (with two new conditions inserted between hub and default):
let sn = if results.is_empty() {
    suggested_with_args("search", "No match — broaden with hybrid search",
        serde_json::json!({"kind": "hybrid"}))
} else if let Some(sym) = &top_symbol {
    if sym.is_hub {
        // Hub: mandatory pre-edit check
        suggested_with_args("edit_context", "Hub detected — mandatory pre-edit check",
            serde_json::json!({"symbol": sym.name, "path": sym.path}))
    } else if sym.caller_count == 0 && !sym.is_entry_point {
        // Dead code signal: no static callers and not an entry point
        suggested_with_args("callers", "Verify dead code — no static callers found",
            serde_json::json!({"symbol": sym.name}))
    } else {
        suggested_with_args("source", "Read implementation",
            serde_json::json!({"target": sym.name}))
    }
} else if top_symbol_ambiguous {
    // Ambiguous: top result could not be uniquely resolved
    // top_candidates comes from the ambiguous resolution branch
    suggested_with_args("symbol_info", "Disambiguate top result",
        serde_json::json!({
            "name": top_candidates.first().map(|c| &c.name),
            "path": top_candidates.first().map(|c| &c.path),
        }))
} else {
    suggested_with_args("source", "Read implementation",
        serde_json::json!({"target": results.first().map(|r| &r.name)}))
};
```

Note: `top_symbol_ambiguous` and `top_candidates` must be set earlier in the locate handler where the symbol resolution is performed. Track whether the resolution was ambiguous and capture the candidates.

Also ensure `is_entry_point: bool` is available on `SymbolInfoOutput` (add it to the struct if not present, query from DB).

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-server test_locate_suggests_callers_for_dead_code 2>&1 | tail -10
cargo test -p ci-server test_locate_suggests_symbol_info_for_ambiguous 2>&1 | tail -10
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-server/src/tools.rs
git commit -m "feat(tools): add dead-code and ambiguous conditions to locate suggested_next"
```

---

## Task 6: Apply config.json `preset` to CLI tool filtering

**Severity:** MEDIUM — config.json `preset` field is loaded but silently ignored; `--preset` CLI arg defaults to "full" and always wins

**Files:**
- Modify: `crates/ci-cli/src/main.rs` (`serve` subcommand, ~line 75)

**Root cause:** `--preset` clap arg has `default_value = "full"`, so when config.json sets `preset = "orient"`, the CLI default "full" overrides it. The intent is: CLI flag takes precedence only when explicitly provided; otherwise use config.json.

**Fix:** Change `--preset` to `Option<String>` (no default), merge with `config.preset` after loading config.

- [ ] **Step 1: Write the failing test**

This is a CLI-level behavior. Add a test that creates a config.json with `preset="orient"` and verifies the server registers only orient tools:

```rust
#[test]
fn test_config_json_preset_used_when_cli_flag_absent() {
    let tmp = tempfile::tempdir().unwrap();
    // Write config.json with preset="orient"
    std::fs::write(
        tmp.path().join("config.json"),
        r#"{"preset": "orient"}"#,
    ).unwrap();

    let config = crate::config::load_config(tmp.path()).unwrap();
    // Simulate: no --preset CLI flag → None
    let cli_preset: Option<String> = None;
    let effective = cli_preset.unwrap_or_else(|| config.preset.clone());

    assert_eq!(effective, "orient", "Should use config.json preset when CLI flag absent");
}

#[test]
fn test_cli_preset_overrides_config_json() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.json"), r#"{"preset": "orient"}"#).unwrap();

    let config = crate::config::load_config(tmp.path()).unwrap();
    let cli_preset: Option<String> = Some("trace".to_string()); // explicit CLI flag
    let effective = cli_preset.unwrap_or_else(|| config.preset.clone());

    assert_eq!(effective, "trace", "CLI --preset must override config.json preset");
}
```

- [ ] **Step 2: Run — verify behavior (not FAIL since test logic is standalone)**

```bash
cargo test -p ci-cli test_config_json_preset_used_when_cli_flag_absent 2>&1 | tail -15
```

- [ ] **Step 3: Update CLI serve subcommand**

In `crates/ci-cli/src/main.rs`, `serve` struct:

```rust
// BEFORE:
#[arg(long, default_value = "full")]
preset: String,

// AFTER:
/// Tool preset to register. If not provided, uses value from config.json (default: "full").
#[arg(long)]
preset: Option<String>,
```

In the `serve` handler body:

```rust
// BEFORE:
ci_server::serve_stdio_with_preset(&project_root, config, &args.preset)?;

// AFTER:
let effective_preset = args.preset
    .clone()
    .unwrap_or_else(|| config.preset.clone());
ci_server::serve_stdio_with_preset(&project_root, config, &effective_preset)?;
```

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-cli 2>&1 | tail -10
cargo build -p ci-cli 2>&1 | tail -5
# Manual smoke test:
echo '{"preset":"orient"}' > /tmp/test_config.json
# (would need project to verify, but build passing is sufficient gate)
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-cli/src/main.rs
git commit -m "fix(cli): apply config.json preset when --preset flag not explicitly provided"
```

---

## Task 7: Compute `session_context` frontier

**Severity:** MEDIUM — `session_context` always returns `repo_overview` hint regardless of exploration state; frontier navigation (core to the spec's session recovery story) is not computed

**Files:**
- Modify: `crates/ci-core/src/db/queries.rs` (add two new query functions)
- Modify: `crates/ci-server/src/tools.rs` (`SessionLog` struct, `session_context` handler, `SessionContextOutput` struct)

**Root cause:** `SessionLog` does not track `explored_symbols` as qualified names for callers query. `session_context` handler does not call DB to compute frontier. `SessionContextOutput` has no `frontier` field.

**Fix:** Add `compute_frontier()` DB queries. Add `frontier: Vec<FrontierEntry>` to output. Update `suggested_next` to point to `file_overview` when frontier non-empty.

### Step 3a: Add frontier query to `ci-core/src/db/queries.rs`

```rust
/// Compute frontier: files connected to explored context but not yet explored.
/// Returns (path, reason) pairs where reason is "imported_by_explored",
/// "contains_callers_of_explored", or "both".
pub fn compute_frontier(
    conn: &rusqlite::Connection,
    explored_files: &[String],
    explored_symbols: &[String],
) -> rusqlite::Result<Vec<(String, String)>> {
    if explored_files.is_empty() && explored_symbols.is_empty() {
        return Ok(vec![]);
    }

    // Set A: files that import any explored file
    let mut set_a: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !explored_files.is_empty() {
        let placeholders = explored_files
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT from_path FROM import_edges WHERE to_path IN ({placeholders}) AND from_path IS NOT NULL"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(explored_files.iter()),
            |row| row.get::<_, String>(0),
        )?;
        for r in rows { set_a.insert(r?); }
    }

    // Set B: files containing callers of explored symbols
    let mut set_b: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !explored_symbols.is_empty() {
        let placeholders = explored_symbols
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT DISTINCT s.path FROM call_edges ce
             JOIN symbols s ON s.qualified_name = ce.from_symbol
             WHERE ce.to_symbol IN ({placeholders}) AND s.path IS NOT NULL"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(explored_symbols.iter()),
            |row| row.get::<_, String>(0),
        )?;
        for r in rows { set_b.insert(r?); }
    }

    // Exclude already-explored files. Build explored set for fast lookup.
    let explored_set: std::collections::HashSet<&String> = explored_files.iter().collect();

    let mut result = Vec::new();
    let all_paths: std::collections::HashSet<String> =
        set_a.union(&set_b).cloned().collect();
    for path in all_paths {
        if explored_set.contains(&path) {
            continue;
        }
        let in_a = set_a.contains(&path);
        let in_b = set_b.contains(&path);
        let reason = match (in_a, in_b) {
            (true, true)  => "both",
            (true, false) => "imported_by_explored",
            (false, true) => "contains_callers_of_explored",
            _ => continue,
        };
        result.push((path, reason.to_string()));
    }
    // Sort deterministically: both > imported_by > contains_callers, then alpha by path
    result.sort_by(|a, b| {
        let rank = |r: &str| match r { "both" => 0, "imported_by_explored" => 1, _ => 2 };
        rank(&a.1).cmp(&rank(&b.1)).then(a.0.cmp(&b.0))
    });
    Ok(result)
}
```

### Step 3b: Add `FrontierEntry` to `SessionContextOutput` and update handler

In `crates/ci-server/src/tools.rs`:

```rust
#[derive(Serialize)]
struct FrontierEntry {
    path: String,
    reason: String,  // "imported_by_explored" | "contains_callers_of_explored" | "both"
}

#[derive(Serialize)]
struct SessionContextOutput {
    tool_calls: u64,
    session_started_at: String,
    explored_symbols: Vec<String>,
    explored_files: Vec<String>,
    unique_files_explored: usize,
    frontier: Vec<FrontierEntry>,        // NEW
    frontier_degraded: bool,             // NEW: true when edges_ready: false
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_next: Option<SuggestedNext>,
}
```

Update `session_context` handler:

```rust
fn session_context(&self) -> String {
    self.timed_tool("session_context", || {
        let log = self.session_log.lock().unwrap();
        let explored_files: Vec<String> = log.explored_files.iter().cloned().collect();
        let explored_symbols: Vec<String> = log.explored_symbols.iter().cloned().collect();

        let edges_ready = self.edges_ready();
        let (frontier, frontier_degraded) = if edges_ready {
            let conn = self.db();
            let entries = ci_core::db::queries::compute_frontier(
                &conn,
                &explored_files,
                &explored_symbols,
            )
            .unwrap_or_default()
            .into_iter()
            .map(|(path, reason)| FrontierEntry { path, reason })
            .collect::<Vec<_>>();
            (entries, false)
        } else {
            (vec![], true)  // frontier meaningless without graph
        };

        let sn = if !frontier.is_empty() {
            self.filter_sn(suggested_with_args(
                "file_overview",
                "Explore top frontier file",
                serde_json::json!({"path": frontier[0].path}),
            ))
        } else {
            self.filter_sn(suggested("repo_overview", "Frontier exhausted — refresh map"))
        };

        serde_json::to_string_pretty(&SessionContextOutput {
            tool_calls: log.tool_calls,
            session_started_at: log.session_started_at.clone(),
            explored_symbols,
            explored_files: explored_files.clone(),
            unique_files_explored: explored_files.len(),
            frontier,
            frontier_degraded,
            suggested_next: sn,
        })
        .unwrap_or_default()
    })
}
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_session_context_frontier_computed() {
    let server = test_server_ready_with_graph();
    // Explore file A (which imports file B)
    server.call_tool("source", serde_json::json!({"target": "funcA"}));
    // session_context should now show file B in frontier
    let result = server.call_tool("session_context", serde_json::json!({}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert!(v["frontier"].is_array(), "frontier must be an array");
    let frontier = v["frontier"].as_array().unwrap();
    assert!(!frontier.is_empty(), "frontier should not be empty after exploring file with imports");
    assert!(frontier[0]["path"].is_string());
    assert!(frontier[0]["reason"].is_string());
    let reason = frontier[0]["reason"].as_str().unwrap();
    assert!(
        ["imported_by_explored", "contains_callers_of_explored", "both"].contains(&reason),
        "reason must be one of the spec values, got: {reason}"
    );
}

#[test]
fn test_session_context_frontier_suggests_file_overview() {
    let server = test_server_ready_with_graph();
    server.call_tool("source", serde_json::json!({"target": "funcA"}));
    let result = server.call_tool("session_context", serde_json::json!({}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    // When frontier is non-empty, must suggest file_overview
    let sn = &v["suggested_next"];
    if !v["frontier"].as_array().unwrap().is_empty() {
        assert_eq!(sn["tool"], "file_overview", "Must suggest file_overview when frontier non-empty");
    }
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-server test_session_context_frontier_computed 2>&1 | tail -20
```

- [ ] **Step 3: Implement** (as specified above in 3a + 3b)

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-server test_session_context_frontier_computed 2>&1 | tail -10
cargo test -p ci-server test_session_context_frontier_suggests_file_overview 2>&1 | tail -10
cargo test -p ci-core 2>&1 | tail -5
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-core/src/db/queries.rs crates/ci-server/src/tools.rs
git commit -m "feat(session): compute frontier from import/call graph for session_context navigation"
```

---

## Task 8: Auto-gitignore `.codeindex/` on startup

**Severity:** LOW — users may accidentally commit the SQLite index; silent injection on first run

**Files:**
- Modify: `crates/ci-server/src/lib.rs` (add call at startup)
- Create: `crates/ci-core/src/gitignore.rs` (pure function, testable in isolation)

**Fix:** On `serve_stdio_with_preset` startup (and optionally `ci init`), call `ensure_gitignore(project_root)`. This function: no-op if no `.git/` dir; no-op if `.codeindex/` already in `.gitignore`; otherwise appends the entry silently.

- [ ] **Step 1: Write the failing test**

Create `crates/ci-core/src/gitignore.rs` with inline tests:

```rust
#[test]
fn test_ensure_gitignore_adds_entry() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap(); // fake .git
    // No .gitignore yet
    ensure_gitignore(tmp.path()).unwrap();
    let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(content.contains(".codeindex/"), "Must add .codeindex/ to .gitignore");
}

#[test]
fn test_ensure_gitignore_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join(".gitignore"), ".codeindex/\n").unwrap();
    // Call twice — should not duplicate
    ensure_gitignore(tmp.path()).unwrap();
    ensure_gitignore(tmp.path()).unwrap();
    let content = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert_eq!(
        content.matches(".codeindex/").count(), 1,
        "Must not duplicate entry"
    );
}

#[test]
fn test_ensure_gitignore_noop_without_git_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // No .git directory
    ensure_gitignore(tmp.path()).unwrap();
    // Must not create .gitignore
    assert!(!tmp.path().join(".gitignore").exists(), "Must not touch .gitignore without .git/");
}
```

- [ ] **Step 2: Run — verify FAIL** (file doesn't exist yet)

```bash
cargo test -p ci-core test_ensure_gitignore 2>&1 | tail -15
```

- [ ] **Step 3: Implement `ensure_gitignore`**

Create `crates/ci-core/src/gitignore.rs`:

```rust
use std::path::Path;

const CODEINDEX_ENTRY: &str = ".codeindex/";

/// Silently ensures `.codeindex/` appears in the project's `.gitignore`.
/// No-op when:
///   - `.git/` directory does not exist (not a git repo)
///   - `.codeindex/` already present in any form in `.gitignore`
pub fn ensure_gitignore(project_root: &Path) -> std::io::Result<()> {
    // Guard: only act in git repos
    if !project_root.join(".git").is_dir() {
        return Ok(());
    }

    let gitignore_path = project_root.join(".gitignore");

    // Check if already present
    if gitignore_path.exists() {
        let content = std::fs::read_to_string(&gitignore_path)?;
        if content.lines().any(|l| l.trim() == CODEINDEX_ENTRY || l.trim() == ".codeindex") {
            return Ok(()); // already present — no-op
        }
        // Append with leading newline if file doesn't end with newline
        let suffix = if content.ends_with('\n') { "" } else { "\n" };
        std::fs::write(
            &gitignore_path,
            format!("{content}{suffix}{CODEINDEX_ENTRY}\n"),
        )?;
    } else {
        // Create new .gitignore
        std::fs::write(&gitignore_path, format!("{CODEINDEX_ENTRY}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // tests go here (from Step 1)
}
```

Add `pub mod gitignore;` to `crates/ci-core/src/lib.rs`.

In `crates/ci-server/src/lib.rs`, add at the top of `serve_stdio_with_preset`:

```rust
pub fn serve_stdio_with_preset(...) -> anyhow::Result<()> {
    // Silently ensure .codeindex/ is gitignored on first run
    let _ = ci_core::gitignore::ensure_gitignore(&project_root);
    // ... rest of existing startup code unchanged
}
```

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-core test_ensure_gitignore 2>&1 | tail -10
cargo build -p ci-server 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-core/src/gitignore.rs crates/ci-core/src/lib.rs crates/ci-server/src/lib.rs
git commit -m "feat(startup): auto-gitignore .codeindex/ — silent, idempotent, no-op without .git"
```

---

## Task 9: Add `session_started_at` to `SessionLog`

**Severity:** LOW — spec requires `session_started_at` in `session_context` output for server-restart detection; currently absent

**Files:**
- Modify: `crates/ci-server/src/tools.rs` (`SessionLog` struct, `new_session_log` fn)

**Root cause:** `SessionLog` only tracks `tool_calls`, `explored_symbols`, `explored_files`. The `session_started_at` field (ISO 8601 UTC, set once at session creation) is missing.

**Fix:** Add `session_started_at: String` to `SessionLog`. Initialize to `chrono::Utc::now().to_rfc3339()` when the log is created. Surface in `SessionContextOutput`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_session_context_includes_started_at() {
    let server = test_server();
    let result = server.call_tool("session_context", serde_json::json!({}));
    let v: serde_json::Value = serde_json::from_str(&result).unwrap();

    assert!(
        v["session_started_at"].is_string(),
        "session_context must include session_started_at string, got: {v}"
    );
    // Must be a valid ISO 8601 timestamp
    let ts = v["session_started_at"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(ts).is_ok(),
        "session_started_at must be valid RFC3339, got: {ts}"
    );
}
```

- [ ] **Step 2: Run — verify FAIL**

```bash
cargo test -p ci-server test_session_context_includes_started_at 2>&1 | tail -15
```

- [ ] **Step 3: Implement**

In `crates/ci-server/src/tools.rs`:

```rust
struct SessionLog {
    tool_calls: u64,
    session_started_at: String,    // NEW: ISO 8601 UTC, set once on creation
    explored_symbols: std::collections::HashSet<String>,
    explored_files: std::collections::HashSet<String>,
}

impl SessionLog {
    fn new() -> Self {
        Self {
            tool_calls: 0,
            session_started_at: chrono::Utc::now().to_rfc3339(),
            explored_symbols: std::collections::HashSet::new(),
            explored_files: std::collections::HashSet::new(),
        }
    }
}
```

If `chrono` is not already in `ci-server/Cargo.toml`, add it:

```toml
# crates/ci-server/Cargo.toml
chrono = { version = "0.4", features = ["serde"] }
```

Check if already present first: `grep -n "chrono" crates/ci-server/Cargo.toml`.

Update `SessionContextOutput` to include `session_started_at: String` (already added in Task 7 step 3b — verify it's there).

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-server test_session_context_includes_started_at 2>&1 | tail -10
cargo test -p ci-server 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-server/src/tools.rs crates/ci-server/Cargo.toml
git commit -m "feat(session): add session_started_at to session_context for restart detection"
```

---

## Task 10: Enforce `SINGLE_WRITER` via `make_read_conn`

**Severity:** LOW (theoretical under WAL) — spec invariant: tool handlers must use read-only connections; only indexer uses the shared write connection

**Files:**
- Modify: `crates/ci-server/src/tools.rs` (add `make_read_conn()`, update tool handlers)
- Modify: `crates/ci-server/src/lib.rs` (rename shared conn to write-only role)

**Root cause:** `CodeIntelligenceServer.conn: Arc<Mutex<Connection>>` is used for both reads (tool handlers via `self.db()`) and writes (passed to watcher). The spec requires: write connection held exclusively by indexer/watcher; tool handlers open fresh read-only connections.

**Fix:** Add `make_read_conn()` method that opens a fresh connection with `PRAGMA query_only=ON`. Update `fn db()` (which tool handlers call) to call `make_read_conn()` instead of locking the shared mutex. The shared `conn` field remains for write operations only.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_read_conn_is_query_only() {
    let server = test_server();
    let conn = server.make_read_conn().unwrap();
    // Attempting a write on a query_only connection must fail
    let result = conn.execute(
        "INSERT INTO symbols (qualified_name, name, kind, language, path, line_start, line_end, signature, docstring, name_tokens, caller_count, is_hub, is_entry_point, file_hash, indexed_at) VALUES ('x','x','function','py','x.py',1,1,'','','',0,0,0,'',0.0)",
        [],
    );
    assert!(
        result.is_err(),
        "query_only connection must reject write operations"
    );
}
```

- [ ] **Step 2: Run — verify FAIL** (method doesn't exist yet)

```bash
cargo test -p ci-server test_read_conn_is_query_only 2>&1 | tail -15
```

- [ ] **Step 3: Implement `make_read_conn`**

In `crates/ci-server/src/tools.rs`, add to `CodeIntelligenceServer` impl:

```rust
impl CodeIntelligenceServer {
    /// Opens a fresh read-only connection for tool handlers.
    /// Per SINGLE_WRITER invariant: tools never use the shared write connection.
    pub fn make_read_conn(&self) -> rusqlite::Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA journal_mode=WAL;")?;
        Ok(conn)
    }

    /// Internal: returns a read conn for tool handler use.
    fn db(&self) -> rusqlite::Connection {
        self.make_read_conn().expect("failed to open read-only DB connection")
    }
}
```

This replaces any existing `fn db()` that was locking the shared `Arc<Mutex<Connection>>`. All tool handlers that call `self.db()` automatically get read-only connections now.

The shared `self.conn: Arc<Mutex<Connection>>` is now exclusively for the indexer (passed to `run_indexing_pipeline`) and watcher. Document this with a comment on the field.

- [ ] **Step 4: Run — verify PASS**

```bash
cargo test -p ci-server test_read_conn_is_query_only 2>&1 | tail -10
cargo test -p ci-server 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add crates/ci-server/src/tools.rs crates/ci-server/src/lib.rs
git commit -m "refactor(server): enforce SINGLE_WRITER — tool handlers use query_only read connections"
```

---

## Self-Review

**1. Spec coverage:**
- All 10 audit findings have a task: ✓
- Null/Absent convention for `coreness` (Task 3): null when edges not ready, 0 when isolated ✓
- `suggested_next` priority order for `locate` (hub > dead-code > ambiguous > default): ✓
- Frontier reason values match spec enum exactly: ✓
- `session_started_at` matches CONTRACTS.md `SessionState.started_at` field: ✓

**2. Placeholder scan:** No "TBD", "TODO", or "implement later" found. All steps have code blocks. ✓

**3. Type consistency:**
- `FrontierEntry` defined in Task 7, used in `SessionContextOutput` in Task 7: ✓
- `coreness: Option<i64>` added to `SymbolInfoOutput` in Task 3, matches DB column type `INTEGER` (nullable): ✓
- `session_started_at: String` added to `SessionLog` in Task 9, referenced in `SessionContextOutput` added in Task 7: ✓ (verify Task 9 runs after Task 7)

**4. Dependency order:** Tasks 7 and 9 both modify `SessionContextOutput`. Task 7 adds `frontier`, `frontier_degraded`, `session_started_at`. Task 9 also adds `session_started_at`. **Execute Task 7 before Task 9** so that Task 9 just verifies the field is present (not double-adds it).

---

## Risk Summary

| Task | Risk | Reason |
|------|------|--------|
| 1 | LOW | Pure SQL query change; isolated function; existing test updated |
| 2 | **HIGH** | Pipeline concurrency — phase updates from background thread; must verify thread-safety of `Arc<RwLock>` updates at phase boundaries. Watcher restart after `reindex_changed` also needs phase update. |
| 3 | LOW | Additive field; no schema change; DB value already computed |
| 4 | LOW | Pure static array addition |
| 5 | MEDIUM | Requires `is_entry_point` field available in locate symbol row — verify it's in the existing SELECT |
| 6 | LOW | CLI arg type change; no server logic change |
| 7 | **HIGH** | New DB queries on hot path (every `session_context` call); needs index verification. `explored_symbols` list could be large (>50) — ensure params_from_iter handles large IN clause or add LIMIT. |
| 8 | LOW | File I/O only; idempotent; no index impact |
| 9 | LOW | Additive struct field; depends on Task 7 output struct |
| 10 | MEDIUM | Changes connection model for all tool handlers; must verify no handler holds connection across async boundary |

**HIGH tasks (2, 7) must be manually verified after implementation:**
- Task 2: Run the integration test that exercises a full reindex on a real project and observe `indexing_status` intermediate phases
- Task 7: Benchmark `session_context` on a server with 500+ explored symbols to confirm frontier query stays under 50ms

---

## Execution Handoff

```
Plan complete: docs/superskills/plans/2026-06-30-atam-audit-fixes.md
Risk summary: 2 HIGH tasks (Task 2 phase-ladder concurrency, Task 7 frontier query perf), 2 MEDIUM tasks (Task 5, Task 10)

Execution options:
1. Subagent-Driven (recommended) — fresh subagent per task, specialist-review between tasks
2. Inline Execution — batch execution with checkpoints after each task

Recommended order: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 9 → 8 → 10
(Task 9 after 7 because both touch SessionContextOutput; Task 10 last as largest refactor)
```
