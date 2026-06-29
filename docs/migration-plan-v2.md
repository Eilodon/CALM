---
title: "Kế hoạch Migration CI → Rust v2 — Revised"
date: 2026-06-29
based_on: "Kế hoạch migration CI.md (v1)"
status: DRAFT
---

# Kế hoạch Migration CI → Rust v2

> Bản cập nhật dựa trên: (1) đánh giá tình trạng thực thi hiện tại, (2) nghiên cứu chuyên sâu SUPER-MCP để mượn pattern.

---

## Phần A — Nghiên cứu SUPER-MCP: Gì mượn được, gì không

### A.1 — Mượn trực tiếp (giảm thiết kế lại)

| # | Pattern từ SUPER-MCP | Áp dụng cho Code-Intelligence | Lý do |
|---|---------------------|-------------------------------|-------|
| 1 | **CI pipeline** (`.github/workflows/ci.yml`) — typecheck → test → audit → outdated signal | Viết CI cho Rust: `cargo fmt --check` → `cargo clippy` → `cargo test` → `cargo audit` → `cargo outdated` | Code-Intelligence **chưa có CI nào**. SUPER-MCP pattern đơn giản, hiệu quả, copy gần nguyên. |
| 2 | **Containerfile multi-stage** — builder (compile+prune) → runtime (minimal alpine) | Phase 6 distribution: builder (`cargo build --release`) → runtime (scratch/alpine + static binary) | Giảm image size, non-root user, read-only fs — áp dụng trực tiếp cho Rust binary. |
| 3 | **Graceful shutdown + signal handling** — SIGINT/SIGTERM → ordered cleanup → drain tasks → close | `ci-server` MCP server: signal handler → flush DB WAL → close watcher → close embedder → exit | MCP server chạy persistent; cần shutdown sạch để không corrupt SQLite WAL. |
| 4 | **Execution pipeline middleware chain** — policy → validation → rate-limit → handler → output-filter → telemetry | `ci-server` tool dispatch: preset-filter → input-validate → handler → suggested-next-inject → output-sanitize | 16 tools cần pipeline chung. Không cần rate-limit/quota (local tool), nhưng cần validate + sanitize + inject suggested_next. |
| 5 | **Schema guard** — JSON Schema validation với depth/size limits, DoS prevention | Input validation cho 16 MCP tools — cap depth, cap string length, reject remote $ref | Protect against malicious input khi MCP chạy qua HTTP transport. |
| 6 | **Output firewall concept** — scan output cho credentials/PII trước khi trả client | Tool responses chứa source code → có thể chứa hardcoded secrets, API keys. Scan `source` tool output bằng regex patterns tương tự SUPER-MCP. | Code analysis tool đọc source code thật → risk lộ secrets trong response cao hơn SaaS tool bình thường. |
| 7 | **Tool definition metadata** — `ToolDefinition` với annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`), `capabilities`, `requiredScopes` | rmcp `#[tool]` macro + custom annotations. Tất cả 16 tools đều `readOnlyHint: true` (đọc index, không sửa code). | MCP 2026-07-28 spec yêu cầu tool annotations. SUPER-MCP đã implement đúng spec. |
| 8 | **Config validation fail-fast** — Zod schema validate toàn bộ config tại startup, crash ngay nếu invalid | `config.rs` đã có Pydantic-equivalent validate. Thêm: fail-fast nếu `project_root` không tồn tại, nếu `config.json` malformed, nếu DB path không writable. | Hiện `load_config` chỉ fallback default — không báo lỗi rõ ràng khi config file tồn tại nhưng malformed. |
| 9 | **Pattern debt registry** — YAML file tracking technical debt với status, urgency, current_control, remaining | Tạo `docs/pattern-debt-registry.yaml` cho Code-Intelligence. Debt hiện tại: parity harness thiếu, analysis modules chưa port, CI chưa có. | SUPER-MCP pattern mature — track debt formally thay vì để trong đầu. |
| 10 | **ADR pattern** — `docs/adr/` cho architectural decisions | Ghi lại decisions: rmcp version lock, Stack Graphs scope, embedding model choice, ConservativeResolver keep/replace. | Decisions hiện nằm rải rác trong design docs, không có single place to look. |
| 11 | **Telemetry pluggable interface** — ITelemetryLogger với factory pattern | Rust: `tracing` subscriber đã là pluggable. Thêm: structured event logging cho tool calls (tool_name, duration_ms, result_size) — giống SUPER-MCP telemetry points. | Cần cho Phase 5 `session_context` và debugging. |
| 12 | **Test helper patterns** — `ctx()` helper, deterministic fixtures, async lifecycle cleanup | Rust test: `setup_db()` helper đã có. Mở rộng: `setup_indexed_project()` fixture cho integration tests, `assert_parity!(python_output, rust_output)` macro cho parity harness. | Giảm boilerplate test, tăng consistency. |

### A.2 — Không áp dụng (SUPER-MCP-specific)

| Pattern | Lý do không áp dụng |
|---------|---------------------|
| Multi-tenant state management | Code-Intelligence là local single-user tool, không có tenant concept. |
| Encryption/KMS (v2/v3/v4 envelopes) | Không có persistent encrypted state — SQLite DB là local, unencrypted. |
| Redis storage backend | Local SQLite là storage duy nhất, không cần remote store. |
| Rate limiting / quota | Local tool, không phải SaaS — không có abuse scenario. |
| OIDC/JWT/API-key auth | MCP chạy stdio (local). HTTP transport nếu cần sau này, nhưng không phải priority. |
| Idempotency manager | 16 tools đều read-only (đọc index). Không có side-effect cần deduplicate. |
| Plugin system | Code-Intelligence là 1 MCP server duy nhất, không host plugins. |
| Vault / secrets management | Không có multi-tenant secrets. |
| Execution lock (tenant mutex) | Single-user, single-writer (WAL handles concurrent reads). |

### A.3 — Adapt có chọn lọc

| Pattern | Adapt như thế nào |
|---------|-------------------|
| **AsyncLocalStorage request context** | Rust equivalent: `tokio::task_local!` hoặc pass `&ServerContext` explicit. CI đã có `ServerContext` — giữ explicit passing, không cần magic thread-local. |
| **Deterministic key generation** (idempotency) | Không cần idempotency, nhưng pattern "deterministic hash of input" hữu ích cho **cache key** của search results / embedding lookups. |
| **compose.yaml hardening** (`cap_drop: ALL`, `read_only`, `pids_limit`, `mem_limit`) | Phase 6 Containerfile: áp dụng compose hardening patterns cho production deployment example. |

---

## Phần B — Tình trạng hiện tại (Baseline)

### B.1 — Những gì đã hoàn thành và đạt chất lượng

| Module | File | Tests | Chất lượng |
|--------|------|-------|-----------|
| DB schema + FTS5 + WAL + migrations | `db/schema.rs` | 2 tests (idempotency, FTS trigger sync) | 1:1 với Python, migration idempotent |
| DB batch queries | `db/queries.rs` | — | batch_callees, batch_callers đúng |
| K-core coreness O(V+E) | `graph/coreness.rs` | 4 tests | Self-loop excluded, bucket-by-degree |
| Hub detection (C-2 fix) | `graph/hub.rs` | 2 tests | min_callers_bridge fix chính xác |
| Bidirectional BFS (F1,F2,F3,F10) | `graph/path.rs` | 6 tests | Batch queries, balanced expansion |
| Identifier tokenization | `graph/tokenize.rs` | 8 tests | Regex patterns 1:1 |
| Config loading + defaults | `config.rs` | — | Defaults khớp CONTRACTS.md |
| Type enums | `types.rs` | — | 11 enums khớp Python + mcp_types.ts |
| Workspace structure | `Cargo.toml` | — | 3 crate đúng kế hoạch |

### B.2 — Những gì chưa hoàn thành (theo kế hoạch v1)

| Hạng mục | Phase gốc | Trạng thái | Ghi chú |
|----------|-----------|-----------|---------|
| `hotspot.py` → Rust | Phase 1 | ❌ Stub | `analysis.rs` chỉ có comment |
| `coverage_reader.py` → Rust | Phase 1 | ❌ Stub | |
| `codeowners.py` → Rust | Phase 1 | ❌ Stub | |
| `diff_impact.py` → Rust | Phase 1 | ❌ Stub | |
| Parity test harness | Phase 1 | ❌ Không tồn tại | Safety net chính của migration |
| `rmcp` dependency | Phase 0 | ❌ Chưa thêm | |
| Stack Graphs scope decision | Phase 0 | ❌ Chưa formal | |
| `search.rs` (FTS5 logic) | Phase 3 | ❌ Stub | |
| `ci-server` MCP wiring | Phase 4 | ❌ Stub | |
| `ci-cli` entrypoint | Phase 4 | ❌ Stub | |
| 16 tool handlers | Phase 4 | ❌ | |
| Bug fixes H-1, H-3, H-4 | Phase 1/4 | ❌ | |
| GitHub Actions CI | — | ❌ | Không có trong kế hoạch v1 |
| Regression test C-1 riêng | Prerequisites | ❌ | C-1 fix ngầm nhưng chưa có test chứng minh |

---

## Phần C — Kế hoạch mới

### Nguyên tắc xuyên suốt (giữ nguyên từ v1)

**Không đổi 2 biến cùng lúc** — tách rõ "port logic đã đúng" khỏi "build cái mới chưa từng có".

### Nguyên tắc bổ sung (mới từ nghiên cứu SUPER-MCP)

1. **CI-first**: Mọi phase phải có CI gate trước khi merge. Không có phase nào được coi là "done" mà không có automated verification.
2. **Debt tracking**: Mọi known gap phải nằm trong `pattern-debt-registry.yaml`, không trong đầu.
3. **Output safety**: Tool responses chứa source code thật — output firewall là requirement, không phải nice-to-have.

---

### Phase 0.5 — CI Foundation + Debt Baseline *(MỚI — làm ngay)*

**Mục tiêu**: Thiết lập safety net tối thiểu trước khi tiếp tục port.

**Deliverables**:

1. **`.github/workflows/ci.yml`** — Mượn pattern từ SUPER-MCP:
   ```yaml
   name: CI
   on:
     push:
       branches: [main]
     pull_request:
       branches: [main]
   jobs:
     verify:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: dtolnay/rust-toolchain@stable
           with:
             components: clippy, rustfmt
         - uses: Swatinem/rust-cache@v2
         - name: Format check
           run: cargo fmt --all -- --check
         - name: Clippy
           run: cargo clippy --workspace --all-targets -- -D warnings
         - name: Test
           run: cargo test --workspace
         - name: Audit
           run: cargo install cargo-audit && cargo audit
   ```

2. **`docs/pattern-debt-registry.yaml`** — Mượn format từ SUPER-MCP:
   ```yaml
   version: 1
   items:
     DEBT-001-parity-harness:
       status: open
       urgency: high
       description: "Parity test harness Python↔Rust chưa tồn tại"
       remaining: ["Fixture design", "Oracle selection", "Diff runner"]
     DEBT-002-analysis-modules:
       status: open
       urgency: high
       description: "4 analysis modules chưa port: hotspot, coverage, codeowners, diff_impact"
     DEBT-003-bug-fixes:
       status: open
       urgency: medium
       description: "H-1 (last_changed ISO8601), H-3 (understand.edges_ready), H-4 (AmbiguousResult.signature)"
     DEBT-004-c1-regression:
       status: open
       urgency: medium
       description: "C-1 stale coreness fix ngầm nhưng chưa có regression test"
   ```

3. **`docs/adr/0001-stack-graphs-scope.md`** — Formal decision (mượn ADR format từ SUPER-MCP):
   - **Decision**: Tier-0 formal (Python, TS, JS, Java) sẽ dùng Stack Graphs khi Phase 2 bắt đầu. Rust, Go, C, C++, Ruby, PHP giữ ConservativeResolver.
   - **Status**: Accepted — scope locked, execution deferred to Phase 2.

**Effort**: ~1 ngày. Không code logic nào, chỉ infrastructure + documentation.

**Exit criteria**: CI pipeline green trên `main`. Debt registry committed. ADR committed.

---

### Phase 1B — Hoàn tất Port Analysis Modules *(tiếp nối Phase 1 đang dở)*

> Phase 1A (graph algorithms + DB) đã hoàn thành ở lần trước. Phase 1B hoàn tất phần còn lại.

**Thứ tự port** (theo dependency — module ít phụ thuộc trước):

| Bước | Module | Input từ | Output | Complexity |
|------|--------|----------|--------|-----------|
| 1 | `coverage_reader` | filesystem (lcov, .coverage, coverage.out, coverage.xml) | `CoverageData { source, covered_lines }` | Trung bình — 4 format parsers, graceful fallback |
| 2 | `analyzer` (dead_code) | `coverage_reader` output + symbol metadata | `(DeadCodeConfidence, DeadCodeSource)` | Thấp — pure function, logic tree đơn giản |
| 3 | `codeowners` | filesystem (CODEOWNERS file) + git blame | `Vec<(pattern, owners)>` | Trung bình — glob matching + git subprocess |
| 4 | `hotspot` | git log + DB symbols + `codeowners` | `Vec<HotspotEntry>` | Cao — git subprocess + 2-phase scoring + normalization |
| 5 | `diff_impact` | git diff + tree-sitter + DB symbols | `DiffImpactOutput` | Cao — diff parsing + signature range detection + risk escalation |

**Port strategy cho mỗi module**:
- Dịch dòng-qua-dòng từ Python (nguyên tắc v1: không redesign).
- Git subprocess: dùng `std::process::Command` (blocking) wrapped trong `tokio::task::spawn_blocking`.
- File I/O: `std::fs` wrapped trong `spawn_blocking`.
- Mỗi module có unit tests dịch từ Python tests (nếu có) + thêm edge cases.

**Bug fixes đi kèm** (thực hiện ngay trong lúc port, không tách PR riêng):
- **C-1 regression test**: Thêm test chứng minh coreness được recompute sau edge changes.
- **H-1**: `last_changed` output `Option<String>` — `None` thay vì `""`. Khi serialize: `skip_serializing_if = "Option::is_none"`.
- **H-2**: Fix tương tự nếu phát hiện khi port.

**Parity test harness** (làm song song với port, không chờ port xong):
- **Fixture**: Synthetic project nhỏ (~50 files, ~200 symbols, mixed languages) committed vào `tests/fixtures/synthetic_project/`.
- **Oracle**: Python implementation **đã fix C-1/C-2** (sửa trong `codeindex/` Python, commit rõ ràng).
- **Runner**: Script chạy cả Python oracle lẫn Rust binary trên cùng fixture, diff JSON output.
- **Scope giới hạn**: Chỉ test `coreness`, `is_hub`, `path`, `hotspot`, `coverage`, `diff_impact` output. KHÔNG test MCP tool responses (chưa có server).

**Exit criteria**:
- `cargo test --workspace` pass (bao gồm unit tests cho 5 module mới).
- Parity harness: 0 diff trên synthetic fixture.
- CI green.
- DEBT-001, DEBT-002, DEBT-004 chuyển status `resolved` trong registry.

**Effort**: ~5-7 ngày.

---

### Phase 2 — Resolver Formal *(giữ nguyên scope từ v1, thêm ADR)*

> Rủi ro cao nhất — làm riêng, sau Phase 1B ổn.

**Không thay đổi so với v1**:
- Thêm `stack-graphs` + `tree-sitter-stack-graphs-python` trước.
- Mở rộng tuần tự: Python → TypeScript → JavaScript → Java.
- `EdgeConfidence` thêm `Formal` tier thứ 4. Cập nhật `types.rs`, `CONTRACTS.md`, `mcp_types.ts` đồng bộ.

**Bổ sung từ nghiên cứu SUPER-MCP**:
- **ADR-0002**: Ghi lại decision về corpus test, validation strategy, fallback behavior khi Stack Graphs fail.
- **CI gate mới**: Thêm job `stack-graphs-corpus` chạy regression trên corpus test riêng cho mỗi ngôn ngữ.

**Exit criteria**: `formal` confidence edges xuất hiện trong parity harness output cho Python files. CI green bao gồm corpus tests.

**Effort**: ~10-14 ngày (learning curve DSL là bottleneck chính).

---

### Phase 3 — Search Layer *(giữ nguyên scope, thêm output safety)*

**Không thay đổi so với v1**:
- FTS5 dual-column search logic trong `search.rs`.
- `ast-grep-core` cho `kind="structural"`.
- Embedding: Model2Vec Rust-native (tokenizers crate + potion-code-16M).
- RRF (k=20) fusion.

**Bổ sung từ nghiên cứu SUPER-MCP**:
- **Output sanitizer** (mượn concept từ SUPER-MCP output firewall):
  - `source` tool trả về raw source code → scan cho credential patterns trước khi trả client.
  - Patterns: PEM private keys, `sk-*`, `ghp_*`, `AKIA*`, generic `password=`, `secret=` trong string literals.
  - Implement dưới dạng `fn sanitize_source_output(code: &str) -> String` — replace matches bằng `[REDACTED]`.
  - Không cần full SUPER-MCP firewall (no Luhn, no SSN, no structured content traversal) — chỉ cần credential patterns trong source code.
- **Embedding sanity check**: Script so sánh cosine similarity giữa Python baseline (fastembed) và Rust (Model2Vec) trên 100 symbol descriptions. Threshold: Spearman rank correlation ≥ 0.85.

**Exit criteria**: `search` tool trả kết quả chính xác cho tất cả 5 `SearchKind`. Embedding sanity check pass. Output sanitizer catch test fixtures chứa fake credentials. CI green.

**Effort**: ~7-10 ngày.

---

### Phase 4 — Tool Surface *(16 handlers — mượn nặng từ SUPER-MCP execution pipeline)*

**Thay đổi lớn so với v1**: Áp dụng SUPER-MCP execution pipeline pattern.

#### 4.1 — rmcp Integration

- Thêm `rmcp = "1.8"` vào `ci-server/Cargo.toml`.
- `#[tool]` macro cho mỗi handler. `#[tool_router]` cho preset-based routing.
- `#[serde(skip_serializing_if = "Option::is_none")]` cho null-vs-absent (giữ nguyên từ v1).

#### 4.2 — Execution Pipeline (mượn từ SUPER-MCP, đơn giản hóa)

SUPER-MCP pipeline có 14 bước. Code-Intelligence chỉ cần 6:

```
┌─────────────────────────────────────────────┐
│  1. Preset filter (tool allowed in preset?) │
│  2. Input validate (Pydantic→serde schema)  │
│  3. Handler execute (ci-core logic)         │
│  4. Suggested_next inject                   │
│  5. Output sanitize (credential scan)       │
│  6. Telemetry log (tool, duration, size)    │
└─────────────────────────────────────────────┘
```

Không cần: rate-limit, quota, idempotency, execution-lock, scope enforcement, confidence gate, phase check, output firewall (PII/Luhn) — vì local single-user tool.

#### 4.3 — Tool Annotations (mượn từ SUPER-MCP ToolDefinition)

Tất cả 16 tools:
```rust
ToolAnnotations {
    read_only_hint: true,       // Không sửa code, chỉ đọc index
    destructive_hint: false,
    idempotent_hint: true,      // Cùng input → cùng output
    open_world_hint: false,     // Bounded by codebase
}
```

#### 4.4 — Graceful Shutdown (mượn từ SUPER-MCP)

```rust
// Signal handler
tokio::signal::ctrl_c().await;
// 1. Stop file watcher
// 2. Cancel embedding task (if running)
// 3. Flush WAL checkpoint
// 4. Close DB connections
// 5. Exit 0
```

#### 4.5 — Bug Fixes (phải include — từ v1)

- **H-3**: `understand` response thêm field `edges_ready: bool` — `true` khi `indexing_phase == "ready"`.
- **H-4**: `AmbiguousResult.signature` → `Option<String>`, không phải required field.
- **H-1**: Đã fix trong Phase 1B (nếu chưa, fix ở đây).

#### 4.6 — CLI (mượn pattern từ SUPER-MCP scripts)

```
ci serve [--project-root PATH] [--preset PRESET] [--db-path PATH] [--config PATH]
ci index [--project-root PATH]            # One-shot index, exit
ci fitness-check [--config thresholds.toml] # Phase 5, stub for now
ci doctor                                  # Validate config, DB, tree-sitter
```

`ci doctor` mượn concept từ SUPER-MCP `env_validation` — validate tất cả prerequisites tại startup:
- tree-sitter grammars loadable?
- SQLite writable?
- Config parseable?
- Git available? (optional, warn if missing)

**Exit criteria**: 16 tools callable qua MCP stdio. `ci serve` chạy được. `ci doctor` pass. Parity harness mở rộng so sánh MCP tool responses. CI green.

**Effort**: ~10-14 ngày.

---

### Phase 5 — Capability Mới *(giữ nguyên scope + thêm telemetry)*

**Không thay đổi so với v1**:
- `symbol_metrics_history` table.
- `ci fitness-check --config thresholds.toml` — chạy trong CI/pre-commit.
- Tasks primitive: hoãn cho đến khi MCP spec 2026-07-28 final + rmcp update.

**Bổ sung từ SUPER-MCP**:
- **Structured telemetry** (mượn concept từ SUPER-MCP telemetry points):
  ```
  tool_execution_started { tool, input_hash, timestamp }
  tool_execution_completed { tool, duration_ms, result_size, suggested_next }
  tool_execution_failed { tool, error_code, duration_ms }
  indexing_phase_changed { from, to, files_count, symbols_count }
  ```
  Output qua `tracing` subscriber — stderr default, file optional.

- **`fitness-check` as CI gate** (mượn pattern từ SUPER-MCP `npm run ci`):
  ```yaml
  # .github/workflows/fitness.yml — chạy trên target project, không phải CI repo
  - name: Code fitness check
    run: ci fitness-check --config .codeindex/thresholds.toml
  ```
  Ý tưởng: user project có thể thêm `ci fitness-check` vào CI riêng của họ.

**Exit criteria**: `fitness-check` exit non-zero khi vượt threshold. Telemetry events xuất hiện trong stderr. CI green.

**Effort**: ~5-7 ngày.

---

### Phase 6 — Distribution *(giữ nguyên scope + mượn container patterns)*

**Không thay đổi so với v1**:
- `cargo build --release` cross-compile musl static binary 3 platform (linux-x86_64, linux-aarch64, macos-aarch64).
- `cargo install`, Homebrew tap, npm postinstall-fetch-binary.
- `ci init` — detect project root, viết config block.

**Bổ sung từ SUPER-MCP**:

1. **Containerfile** (mượn multi-stage pattern):
   ```dockerfile
   # Stage 1: Builder
   FROM rust:1.85-alpine AS builder
   RUN apk add --no-cache musl-dev
   WORKDIR /build
   COPY . .
   RUN cargo build --release --target x86_64-unknown-linux-musl

   # Stage 2: Runtime
   FROM scratch
   COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/ci /ci
   ENTRYPOINT ["/ci"]
   ```

2. **compose.yaml** example (mượn hardening patterns):
   ```yaml
   services:
     code-intelligence:
       build: .
       read_only: true
       cap_drop: [ALL]
       security_opt: [no-new-privileges:true]
       pids_limit: 64
       mem_limit: 256m
       volumes:
         - ./:/project:ro          # Mount project read-only
         - ci-data:/data           # DB + index writable
       command: ["serve", "--project-root", "/project", "--db-path", "/data/index.db"]
   ```

3. **GitHub Actions release workflow** (mượn CI pattern):
   ```yaml
   on:
     push:
       tags: ['v*']
   jobs:
     release:
       strategy:
         matrix:
           target: [x86_64-unknown-linux-musl, aarch64-unknown-linux-musl, aarch64-apple-darwin]
       steps:
         - cargo build --release --target ${{ matrix.target }}
         - gh release upload ${{ github.ref_name }} target/${{ matrix.target }}/release/ci
   ```

**Contingency path** (từ v1, giờ có giải pháp rõ):
- Nếu Phase 2 trễ và cần ship Phase 1 binary sớm → build Phase 6 Containerfile + release workflow ngay sau Phase 4 xong, ship binary dùng ConservativeResolver cho mọi ngôn ngữ. Stack Graphs thêm vào release sau.

**Exit criteria**: Binary downloadable cho 3 platform. `ci init` hoạt động. Container image buildable. CI release workflow green trên tag push.

**Effort**: ~3-5 ngày.

---

## Phần D — Tổng hợp Timeline & Rủi ro

### Timeline tổng

```
Phase 0.5  CI + Debt baseline           ~1 ngày    ← BẮT ĐẦU NGAY
Phase 1B   Hoàn tất analysis modules    ~5-7 ngày
Phase 2    Resolver formal              ~10-14 ngày  (có thể song song Phase 3 FTS5)
Phase 3    Search layer                 ~7-10 ngày
Phase 4    Tool surface (16 handlers)   ~10-14 ngày
Phase 5    Capability mới               ~5-7 ngày
Phase 6    Distribution                 ~3-5 ngày
                                        ─────────
                              Tổng:     ~41-58 ngày
```

### Rủi ro cập nhật

| Phase | Rủi ro | Mức độ | Mitigation | Thay đổi so với v1 |
|-------|--------|--------|------------|-------------------|
| 0.5 | CI setup fail (Rust toolchain cache) | Thấp | `Swatinem/rust-cache@v2` đã proven | MỚI |
| 1B | Git subprocess flaky trong test | Trung bình | Mock git output trong unit test, real git chỉ trong integration test | MỚI |
| 1B | Parity harness false positive (floating point diff) | Thấp | Epsilon comparison cho scores | MỚI |
| 2 | Stack Graphs DSL learning curve | Cao | Phase 1B ship-able độc lập (giữ từ v1) | Giữ nguyên |
| 3 | Embedding tokenization lệch | Trung bình | Sanity check vs Python baseline (giữ từ v1) | Giữ nguyên |
| 3 | `kind="structural"` breaking change | Thấp-Trung | CONTRACTS.md + presets sync (giữ từ v1) | Giữ nguyên |
| 4 | rmcp API unstable cho tools | Thấp | rmcp 1.8.0 core tools API ổn định (chỉ Tasks unstable) | MỚI — giảm risk vì đã research |
| 5 | Tasks spec chưa final | Cao | Hoãn (giữ từ v1) | Giữ nguyên |
| 6 | Cross-compile musl fail | Trung bình | Test trên CI matrix trước khi release | MỚI |

### Dependency graph giữa các phase

```
Phase 0.5 ──→ Phase 1B ──→ Phase 2 ──→ Phase 4 ──→ Phase 5
                  │                        ↑           │
                  │         Phase 3 ───────┘           ↓
                  │                                 Phase 6
                  │
                  └──→ (contingency: Phase 6 sớm nếu Phase 2 trễ)
```

Phase 2 và Phase 3 **có thể song song một phần**: FTS5 search (Phase 3) không phụ thuộc Stack Graphs (Phase 2). Chỉ `kind="structural"` (ast-grep) và embedding cần Phase 2 xong trước (nếu muốn embed formal confidence).

---

## Phần E — Checklist hành động ngay (Priority order)

- [ ] Tạo `.github/workflows/ci.yml` — cargo fmt + clippy + test + audit
- [ ] Tạo `docs/pattern-debt-registry.yaml` — 4 debt items hiện tại
- [ ] Tạo `docs/adr/0001-stack-graphs-scope.md` — lock decision
- [ ] Port `coverage_reader` → `ci-core/src/analysis/coverage.rs`
- [ ] Port `analyzer` (dead_code) → `ci-core/src/analysis/dead_code.rs`
- [ ] Port `codeowners` → `ci-core/src/analysis/codeowners.rs`
- [ ] Port `hotspot` → `ci-core/src/analysis/hotspot.rs`
- [ ] Port `diff_impact` → `ci-core/src/analysis/diff_impact.rs`
- [ ] Tạo parity test harness + synthetic fixture
- [ ] Thêm C-1 regression test
- [ ] Fix H-1 (last_changed) trong hotspot port
