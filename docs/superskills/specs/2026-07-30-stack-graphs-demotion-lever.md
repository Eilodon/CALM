---
title: Stack-graphs demotion lever — gate (D1) + surface provenance (D2) + observe overrides (D3)
date: 2026-07-30
author: gokuderafight (via Claude Sonnet 5)
SPEC_APPROVED: true
SPEC_ESCALATION: false
ESCALATION_FINDING: ""
related_adrs:
  - docs/adr/0001-stack-graphs-scope.md
  - docs/adr/0002-formal-resolver-stack-graphs.md
  - docs/adr/0004-lsp-optional-confidence-upgrade.md
---

# CALM — Stack-graphs demotion lever (D1 + D2 + D3)

## 1. Context

Điều tra trực tiếp trên code (không phải aspiration trong ADR) cho thấy vai trò
thật của `stack-graphs` trong CALM đã tự thu hẹp so với danh tiếng gốc của nó
("cross-file name resolution at scale"):

- `FormalResolver::resolve_file` (`crates/calm-core/src/resolver/formal.rs:598-708`)
  chỉ build 1 `StackGraph` chứa **đúng 1 file** (+ builtins graph) — comment gốc
  tại dòng 624-627 nói thẳng: *"without this, `graph` only ever contains the
  single file being analyzed."* Không có cross-file/cross-module stitching nào.
- Chỉ phủ 4 ngôn ngữ (Python/TypeScript/JavaScript/Java), và với 2 trong số đó
  (Python, TS/JS) một verdict `formal_source = 'stack_graphs'` bị SCIP overlay
  ghi đè bất cứ khi nào SCIP có mặt (`provider.rs:305`, tự gọi đây là *"exit
  ramp from the archived stack-graphs formal tier"*). Java chưa có SCIP overlay
  thay thế nên vẫn thuần stack-graphs.
- `stack-graphs` upstream đã bị GitHub archive (2025-09-09) — ADR-0002 đã fork
  phòng ngừa sang `Eilodon/stack-graphs` (2026-07-15).

Ba vấn đề cụ thể, đã verify bằng code/`cargo tree`/tiền lệ thật, đáng sửa cùng
một đợt vì cùng xoay quanh việc hạ cấp stack-graphs về đúng vai trò
*"fallback formal cho 4 ngôn ngữ, chỉ trong-phạm-vi-1-file, chỉ khi thiếu
SCIP/LSP"*:

1. **D1 — Khóa version cứng cho cả hệ sinh thái ngôn ngữ.** 6 crate của họ
   stack-graphs (`stack-graphs`, `tree-sitter-stack-graphs`, và 4 biến thể
   `-python`/`-typescript`/`-java`/`-javascript`) là dependency **non-optional**
   trong `crates/calm-core/Cargo.toml:30-35`. `cargo tree -i -p
   tree-sitter@0.24.7` xác nhận đây là **nhánh duy nhất** trong toàn workspace
   ép core `tree-sitter` = `"^0.24"` (qua `lsp-positions` → `stack-graphs`, và
   `tree-sitter-graph`/`tree-sitter-loader`/`tree-sitter-highlight`/
   `tree-sitter-tags` → `tree-sitter-stack-graphs`). 21 grammar Tier-0/0.5 khác
   chỉ phụ thuộc `tree-sitter-language` (ABI shim, ABI min-compat = 13, ổn định
   từ tree-sitter v0.20.0 → v0.26.1 theo research 2026-07-10) nên không bị ảnh
   hưởng bởi việc bump core. Đây **không phải rủi ro giả định**: `Perl` đã bị
   loại khỏi danh sách 25 ngôn ngữ (2026-07-10, `docs/superskills/plans/
   2026-07-10-25-language-expansion.md` §1.4) đúng vì `tree-sitter-perl` cần
   core `^0.26.3`, bị chặn bởi pin cứng `^0.24` của `tree-sitter-stack-graphs`
   (archived, không có version mới) — team đã swap sang Groovy thay vì
   fork+bump.
2. **D2 — Provenance thật bị che khuất sau nhãn chung `"formal"`.** Cột
   `call_edges.formal_source` (migration tại `db/schema.rs:302`) đã phân biệt
   `stack_graphs`/`scip`/`lsp` ở tầng DB, dùng dày đặc nội bộ
   (`ingest.rs`/`pipeline.rs`/`lsp/overlay.rs`) nhưng **không xuất hiện per-edge**
   ở bất kỳ tool đọc nào (`callers`/`callees`/`edit_context`/`understand`/
   `symbols_batch`) — chỉ có dạng aggregate count ở `lsp_refresh`. Agent hiện
   thấy `edge_confidence = "formal"` giống hệt nhau dù đằng sau là heuristic
   per-file (stack-graphs, có giới hạn đã nêu ở D1-context) hay exact
   cross-module (SCIP) hay runtime probe (LSP).
3. **D3 — Xung đột SCIP-vs-stack-graphs bị nuốt hoàn toàn.** `ingest.rs` có cơ
   chế ghi đè thật (`scip_overrides_stack_graphs_target`, `mark_ruled_out_
   siblings`) nhưng **0 dòng `tracing::`** trong toàn file (verify bằng grep).
   Mỗi lần override là tín hiệu (stack-graphs sai / SCIP sai / ambiguity ngôn
   ngữ thật) hiện không quan sát được qua log hay `indexing_status`.

## 2. Decision

Một spec, **3 unit độc lập** (không phụ thuộc kỹ thuật lẫn nhau, ship riêng
commit/PR — đúng convention ADR-0001 "mỗi ngôn ngữ Stack Graphs ship riêng PR,
không gộp"). Không unit nào cần migration DB.

| Unit | Lớp chạm | File chính | Rủi ro nếu làm nửa vời (stub) |
|---|---|---|---|
| D1 | Build config (Cargo feature) | `Cargo.toml` ×2, `resolver/mod.rs`, `indexer/pipeline.rs` | Feature flag tồn tại trên giấy nhưng build thiếu feature vẫn fail compile |
| D2 | MCP tool output schema | `tools/detail.rs`, `tools/trace.rs`, `tools/guardrails.rs`, `tools/inspect.rs` | Field thêm vào struct nhưng SQL không SELECT cột thật → luôn `null` |
| D3 | Observability (tracing + counter) | `scip/ingest.rs`, `tools/recover.rs` | Log/counter thêm nhưng không có test nào thực sự trigger đường override |

## 3. Unit D1 — Gate stack-graphs family thành Cargo feature `stack-graphs-formal`

### 3.1 Cargo.toml

`crates/calm-core/Cargo.toml:30-35` — 6 dependency chuyển non-optional →
optional:

```toml
stack-graphs = { workspace = true, optional = true }
tree-sitter-stack-graphs = { workspace = true, optional = true }
tree-sitter-stack-graphs-python = { workspace = true, optional = true }
tree-sitter-stack-graphs-typescript = { workspace = true, optional = true }
tree-sitter-stack-graphs-java = { workspace = true, optional = true }
tree-sitter-stack-graphs-javascript = { workspace = true, optional = true }
```

`[features]` block (sau dòng 118 hiện tại):

```toml
default = ["embeddings", "tier0-5", "scip-overlay", "stack-graphs-formal"]
# Formal name-binding resolution (Tier-3) cho Python/TypeScript/JavaScript/Java
# via github/stack-graphs (fork: Eilodon/stack-graphs — upstream archived
# 2025-09-09). Default-on, giữ nguyên hành vi hôm nay. Đây là nhánh DUY NHẤT
# trong toàn workspace ép core `tree-sitter` = "^0.24" (xem ADR-0002
# Consequences + docs/superskills/plans/2026-07-10-25-language-expansion.md
# §1.4, vụ Perl) -- build KHÔNG kèm feature này để mở khóa core tree-sitter
# lên phiên bản mới hơn khi cần một grammar mà stack-graphs chặn.
stack-graphs-formal = [
    "dep:stack-graphs",
    "dep:tree-sitter-stack-graphs",
    "dep:tree-sitter-stack-graphs-python",
    "dep:tree-sitter-stack-graphs-typescript",
    "dep:tree-sitter-stack-graphs-java",
    "dep:tree-sitter-stack-graphs-javascript",
]
```

### 3.2 Cfg-gate boundary (đúng 2 điểm chạm, đã xác định chính xác qua code)

Pattern giống hệt `scip-overlay` (đã lặp lại 15+ chỗ trong repo: `lib.rs:19`,
`orient.rs`, `recover.rs`, `scip.rs`, `edit.rs`, `watcher.rs`, `main.rs`) —
không phải cơ chế mới.

1. `crates/calm-core/src/resolver/mod.rs:2`:
   ```rust
   #[cfg(feature = "stack-graphs-formal")]
   pub mod formal;
   ```
2. `crates/calm-core/src/indexer/pipeline.rs` — đúng 1 điểm tích hợp production
   (`FormalResolver` không được gọi ở đâu khác trong toàn crate ngoài đây +
   test):
   - `cached_formal_resolver()` (dòng 2064-2076, `static FORMAL_RESOLVER:
     OnceLock<FormalResolver>`) — toàn bộ hàm dưới `#[cfg(feature =
     "stack-graphs-formal")]`.
   - `extract_file_data`'s tham số `formal: &FormalResolver` (dòng 366) và nơi
     dùng nó (dòng 536-540):
     ```rust
     #[cfg(feature = "stack-graphs-formal")]
     let formally_resolved: HashSet<String> = if formal.has_language(lang) {
         formally_resolved_names(formal.resolve_file(lang, rel, source), lang, rel)
     } else {
         HashSet::new()
     };
     #[cfg(not(feature = "stack-graphs-formal"))]
     let formally_resolved: HashSet<String> = HashSet::new();
     ```
     Nhánh `not(...)` tương đương chính xác hành vi "unsupported language" đã
     có sẵn (dòng 538-539) — không phải logic mới, chỉ ép nó luôn chạy.
   - Call site gọi `cached_formal_resolver()` (trong
     `run_indexing_pipeline_cancellable`, ~dòng 1925) cũng cần cfg-gate tương
     ứng để không truyền `&FormalResolver` khi kiểu đó không tồn tại.

### 3.3 Precondition môi trường — đã verify, không phải giả định

`grep -r "no-default-features" .github/workflows/ scripts/` = **0 kết quả**.
Không job CI hay script nào build `calm-core` với `--no-default-features`.
→ **0 thay đổi CI cần thiết** cho D1 (feature default-on tự động có mặt ở mọi
job hiện tại, kể cả job "all-languages" ở `ci.yml:99,102` liệt kê feature list
tường minh nhưng không loại trừ default).

### 3.4 Tiêu chí "not a stub" (bắt buộc, không phải nice-to-have)

- `cargo build -p calm-core --no-default-features --features tier0-5,scip-overlay`
  (thiếu `stack-graphs-formal`) phải compile sạch, 0 warning.
- `cargo tree -i -p tree-sitter@0.24.7` chạy trên cùng build phải **rỗng
  hoàn toàn** — bằng chứng thật trần 0.24 đã gỡ, không chỉ "feature flag tồn
  tại trên giấy".
- Test mới `pipeline_runs_without_stack_graphs_formal_feature` (cfg-gated
  `#[cfg(not(feature = "stack-graphs-formal"))]`, chạy trong CI job build
  không kèm feature): full reindex trên fixture đa ngôn ngữ, assert
  Python/TS/JS/Java vẫn ra edge `resolved`-tier bình thường, không panic, chỉ
  thiếu tier `formal` từ nguồn `stack_graphs` (SCIP formal vẫn hoạt động nếu
  build có `scip-overlay`).

## 4. Unit D2 — Surface `formal_source` qua `CallerEntry`/`CalleeEntry`

### 4.1 Phạm vi

`CallerEntry`/`CalleeEntry` (`crates/calm-server/src/tools/detail.rs:299-366`)
dùng chung bởi **5 tool** — sửa 2 struct này phủ cả 5, không cần đụng tool nào
khác:

| Tool | File | Cách dùng struct |
|---|---|---|
| `callers` | `tools/trace.rs:44-76` | trực tiếp |
| `callees` | `tools/trace.rs:246-282` | trực tiếp |
| `edit_context` | `tools/guardrails.rs:62-148` | trực tiếp |
| `understand` | `tools/inspect.rs::UnderstandOutput::callers_summary` (dòng 1058) | trực tiếp |
| `symbols_batch` | `tools/inspect.rs::SymbolsBatchEntry::direct_callers/direct_callees` (dòng 1114-1116) | trực tiếp |

`symbol_info` (bucket-đếm-theo-tier, cấu trúc khác hẳn, xem
`symbol_info_caller_count_by_confidence_buckets_formal_tier_separately`) —
**ngoài phạm vi lever này**, để lại cho lever sau nếu cần (cần thiết kế lại
shape dữ liệu, không phải thêm field).

### 4.2 Struct + SQL

```rust
// detail.rs
#[derive(Serialize, JsonSchema)]
pub(crate) struct CallerEntry {
    pub(crate) symbol: String,
    pub(crate) edge_confidence: String,
    /// Nguồn cụ thể đứng sau `edge_confidence == "formal"`:
    /// `"stack_graphs"` (heuristic per-file, xem resolver/formal.rs) |
    /// `"scip"` (exact file,line, có thể cross-module) | `"lsp"` (runtime
    /// probe). `None` khi `edge_confidence != "formal"`, hoặc build thiếu
    /// mọi formal-tier feature. Không suy ra "formal" đáng tin bằng nhau —
    /// stack_graphs chỉ resolve trong phạm vi 1 file (xem ADR-0002).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) formal_source: Option<String>,
    pub(crate) edge_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}
```
`CalleeEntry` tương tự (thêm cùng field, cùng vị trí tương đối, cùng doc
comment rút gọn tham chiếu `CallerEntry`).

SQL: mỗi `SELECT ce.from_symbol, ce.from_path, ce.edge_confidence,
ce.call_site_line, ce.edge_kind ...` (4 vị trí: `trace.rs:44`, `trace.rs:246`,
`guardrails.rs:62`, `guardrails.rs:109`, cộng `inspect.rs:425` và
`symbols_batch`'s query) thêm cột `ce.formal_source` vào danh sách SELECT và
struct-literal tương ứng.

### 4.3 Etag/hash — `formal_source` tham gia fingerprint

`hash_caller_entries`/`hash_callee_entries` (`detail.rs:332-393`) thêm
`formal_source` vào chuỗi băm, cùng vị trí với `edge_confidence` hiện tại
(ngăn bằng `\u{1}` như field khác):

```rust
buf.push_str(&e.edge_confidence);
buf.push('\u{1}');
buf.push_str(e.formal_source.as_deref().unwrap_or(""));
buf.push('\u{1}');
buf.push_str(&e.edge_kind);
```

Lý do (đã quyết định ở vòng brainstorm): SCIP overlay có thể âm thầm flip
`formal_source` (`stack_graphs` → `scip`) ở background thread mà không đổi
`edge_confidence`/`edge_kind`/`line`/`preview` — nếu không hash, client dùng
`if_none_match` sẽ bỏ lỡ bản nâng cấp provenance, thấy y hệt bản cũ.

Cập nhật đồng bộ (bắt buộc theo AGENTS.md):
- Doc comment `hash_caller_entries`/`hash_callee_entries` (liệt kê tuple field
  — hiện ghi `(symbol, edge_confidence, edge_kind, line, preview)`, thêm
  `formal_source`).
- `callers.snap`/`callees.snap` toolsnap description của field `etag`.
- `CONTRACTS.md` §10.4 (Ambiguity Contract, liệt kê `symbol_info, source,
  callers, callees, path, edit_context`) + schema output của 5 tool.

### 4.4 Tiêu chí "not a stub"

- Integration test trên Python fixture: 1 edge có `formal_source =
  'stack_graphs'` thật (không SCIP overlay) — `callers`/`edit_context`/
  `understand`/`symbols_batch` phải trả đúng giá trị đó, không phải `null` do
  quên SELECT.
- Test etag: gọi `callers` 2 lần; giữa 2 lần, `UPDATE call_edges SET
  formal_source = 'scip' WHERE ...` thủ công trong test (không đổi
  `edge_confidence`/`edge_kind`/`line`/`preview`) → etag lần 2 phải **khác**
  lần 1, chứng minh hash thật sự nhạy với field mới.

## 5. Unit D3 — Quan sát SCIP override stack-graphs

### 5.1 Counter — mirror chính xác pattern `formal_resolution_timeouts`

Tiền lệ thật (`pipeline.rs:316-327`, không phải suy diễn):
```rust
static FORMAL_RESOLUTION_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
pub fn formal_resolution_timeout_count() -> u64 { ... }
```
D3 lặp lại đúng pattern này trong `crates/calm-core/src/scip/ingest.rs` (module
này đã nằm trọn dưới `#[cfg(feature = "scip-overlay")]` ở `lib.rs:19`, nên
không cần cfg riêng ở đây):

```rust
static SCIP_STACK_GRAPHS_OVERRIDES: AtomicU64 = AtomicU64::new(0);

/// Count of edges where SCIP overrode a `formal_source = 'stack_graphs'`
/// verdict since this process started -- see `ingest_occurrences`'s upgrade
/// loop and `mark_ruled_out_siblings`. Mỗi lần tăng là 1 bất đồng thật giữa 2
/// nguồn formal-tier: stack-graphs (heuristic per-file) và SCIP (exact
/// file,line, cross-module) -- xem ADR-0002. Không tự nói bên nào đúng, chỉ
/// đánh dấu chỗ đáng nhìn kỹ hơn trước khi tin `formal` không điều kiện.
pub fn scip_stack_graphs_override_count() -> u64 {
    SCIP_STACK_GRAPHS_OVERRIDES.load(Ordering::Relaxed)
}
```

**Sửa sau audit-design (citation ban đầu sai — `scip_overrides_stack_graphs_
target` KHÔNG phải production code, đó là `#[test] fn` ở `ingest.rs:985`, tự
UPDATE dữ liệu fixture để verify hành vi, không phải nơi override thật xảy
ra).** Điểm production thật:

1. `ingest_occurrences`'s upgrade loop (`ingest.rs`, khối `UPDATE call_edges
   SET edge_confidence = 'formal', formal_source = 'scip' WHERE id = ?1`,
   ~dòng 161-167) — chạy cho **mọi** id trong `to_upgrade`, không phân biệt
   "vừa nâng cấp từ textual/inferred" (không phải bất đồng với stack-graphs,
   vì stack-graphs chưa từng có ý kiến trên edge đó) với "ghi đè 1 verdict
   `formal_source = 'stack_graphs'` đã có" (bất đồng thật). **Bắt buộc** thêm
   điều kiện lọc trước khi tăng counter/log: chỉ khi `row.formal_source ==
   Some("stack_graphs")` tại thời điểm trước UPDATE (đã có sẵn trong `row`,
   đọc từ SELECT ở đầu hàm — xem field `formal_source` của struct `EdgeRow`,
   `ingest.rs:61`).
2. `mark_ruled_out_siblings` (`ingest.rs:238`, gọi tại dòng 171) — đánh dấu
   sibling thua trong nhóm ambiguous fan-out. Theo doc comment của hàm này
   (dòng 199-204), **bất kỳ** sibling không được chọn đều bị `ruled_out_by_scip`
   — không riêng sibling có nguồn stack_graphs. Cùng yêu cầu lọc: chỉ tăng
   counter khi sibling bị ruled-out có `formal_source == Some("stack_graphs")`
   trước khi bị đánh dấu. **Thân hàm `mark_ruled_out_siblings` chưa được đọc
   đầy đủ trong spec này** — plan thực thi phải đọc lại toàn bộ hàm để thêm
   điều kiện lọc đúng vị trí, không suy đoán từ doc comment không thôi.

Không lọc đúng ở cả 2 điểm → counter đếm cả những lần nâng cấp không liên
quan gì đến stack-graphs (overcounting), làm hỏng chính mục đích của D3.

Increment + `tracing::debug!` (cùng style `run_overlay_for`/`run_go_workspace_
overlay` đã dùng trong `scip/mod.rs`) tại 2 điểm đã lọc đúng ở trên. Log
fields: `from_symbol`, `to_symbol`, `file`, `old_formal_source =
"stack_graphs"`, `new_formal_source = "scip"` (điểm 1) hoặc `ruled_out = true`
(điểm 2).

### 5.2 Surface qua `indexing_status`

`IndexingStatusOutput` (`tools/recover.rs:583-645`) thêm field, cùng style
`scip_overlay`/`scip_overlays` (`recover.rs:80-103`, `None` khi build thiếu
`scip-overlay`):

```rust
/// Số edge SCIP đã ghi đè 1 verdict stack-graphs kể từ khi process khởi động
/// -- xem `calm_core::scip::ingest::scip_stack_graphs_override_count`. `None`
/// khi build thiếu feature `scip-overlay` (không có gì để báo cáo). Một con
/// số dương, tăng dần trên 1 repo thật là tín hiệu đáng điều tra thêm trước
/// khi tin tuyệt đối `formal_source = 'stack_graphs'` trên các ngôn ngữ chưa
/// có SCIP overlay (hiện: Java).
#[serde(skip_serializing_if = "Option::is_none")]
pub(crate) scip_stack_graphs_overrides: Option<u64>,
```

### 5.3 Tiêu chí "not a stub"

- Fixture cố ý tạo bất đồng thật (1 file Python có 1 call site mà stack-graphs
  per-file resolve khác SCIP exact cross-module) — test assert counter tăng
  đúng 1 SAU khi chạy `ingest_occurrences`, không chỉ assert hàm tồn tại/biên
  dịch.
- **Test âm tính bắt buộc (chặn chính xác lỗi audit-design vừa bắt được):**
  fixture chỉ có edge `textual`/`inferred` được SCIP nâng cấp thẳng lên
  `formal`/`'scip'` — **không** có `formal_source = 'stack_graphs'` nào trước
  đó — assert counter **không** tăng. Không có test này, một implementation
  đếm mọi lần `ingest_occurrences` nâng cấp edge (không lọc theo
  `formal_source` cũ) vẫn pass mọi test dương tính nhưng sai mục đích D3.
- Test `indexing_status` sau khi chạy fixture đó → `scip_stack_graphs_
  overrides` phản ánh đúng số, theo đúng convention test đã có
  (`indexing_status_includes_formal_resolution_timeouts_field`).
- `CONTRACTS.md` Tool 13 (`indexing_status`) cập nhật field mới.

## 6. Testing strategy tổng hợp

Mỗi unit tự chứa test riêng (liệt kê ở §3.4/§4.4/§5.3) — không có test nào
phụ thuộc cả 3 unit cùng lúc, khớp với việc ship độc lập. `cargo test
--workspace --features tier0-5,...,scip-overlay,lsp-overlay` (job hiện có ở
`ci.yml:102`) phải xanh sau mỗi unit riêng lẻ.

## 7. Rollout / sequencing

Không có phụ thuộc kỹ thuật thật giữa 3 unit — thứ tự ship tùy ý. Gợi ý theo
rủi ro tăng dần: **D3 (an toàn nhất, chỉ thêm quan sát, 0 thay đổi hành vi) →
D2 (thay đổi output schema + etag semantics, cần cập nhật CONTRACTS.md) → D1
(build config, blast radius rộng nhất dù default-on giữ hành vi bằng 0)**.
Mỗi unit qua `adr-commit` review riêng khi merge (không đợi cả 3 xong).

## 8. Out of scope (rõ ràng, để audit-design không cần tự hỏi lại)

- **Không** viết TSG rules mới cho ngôn ngữ khác (đó là hướng "B — mở rộng
  stack-graphs" đã bị deferred ở `2026-07-30-calm-dfb-levers-design.md` §4,
  không liên quan lever này).
- **Không** fork/patch `Eilodon/stack-graphs` — D1 chỉ gate, không đụng code
  crate đó.
- **Không** đổi `symbol_info`'s data shape (D2 giới hạn ở 5 tool dùng chung
  `CallerEntry`/`CalleeEntry`).
- **Không** thêm UI/tool mới để query "danh sách edge nào bị override" — D3
  dừng ở counter + log, truy vấn chi tiết (nếu cần) là lever riêng sau này.
- **Không** đổi mặc định `stack-graphs-formal` sang off — spec này giữ
  default-on tuyệt đối, chỉ mở khả năng build without nó.

## 9. Risks để audit-design soi

- D1: nhánh `#[cfg(not(...))]` trong `pipeline.rs` là code path gần như không
  ai chạy trong CI mặc định (chỉ chạy khi build tường minh thiếu feature) —
  rủi ro bit-rot nếu không có job CI riêng build+test cấu hình đó định kỳ
  (không chỉ 1 lần lúc ship). Cần quyết định: thêm 1 job CI matrix mới, hay
  chấp nhận rủi ro và dựa vào test D1 §3.4 chạy local trước mỗi release.
- D2: thêm `formal_source` vào etag là thay đổi hành vi backward-incompatible
  cho client đang cache etag cũ (etag sẽ đổi ngay lần gọi đầu sau khi field
  này có mặt, kể cả khi mọi field khác giữ nguyên) — cần xác nhận không tool
  nào phụ thuộc etag ổn định qua deploy boundary theo cách sẽ vỡ.
- D3: `Ordering::Relaxed` (giống `FORMAL_RESOLUTION_TIMEOUTS`) đủ cho 1 counter
  thuần quan sát, nhưng đáng note lại trong PR để không ai "sửa" thành
  `SeqCst` không cần thiết sau này.

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-30 | trigger: NORMAL -->

**Tier:** 2 (Production — CALM là dev-tool đang chạy hook-enforced gate
(`edit_context`) mà session khác phụ thuộc; không PII/payments/multi-tenant)
**Date:** 2026-07-30

```
CONTEXT_MODE:      DESIGN
STAKEHOLDER:       CALM maintainer (gokuderafight) + agent sessions tiêu thụ
                   MCP tool output (stakeholder gián tiếp cho D2/D3)
GOAL:              pre-mortem trước khi viết plan cho D1+D2+D3
AUDIT_TARGET_TIER: 2
```

### Failure Modes

1. **D3's cited production integration point sai — 1 trong 2 "điểm override
   thật" ban đầu là `#[test] fn scip_overrides_stack_graphs_target`
   (`ingest.rs:985`), không phải code production.** Đọc trực tiếp source
   trong lúc audit xác nhận: hàm đó tự `UPDATE ... SET formal_source =
   'stack_graphs'` để dựng fixture, rồi gọi `ingest_occurrences` và assert kết
   quả — chỉ là test. Điểm production thật là khối `UPDATE ... formal_source
   = 'scip'` bên trong `ingest_occurrences` (~dòng 161-167). Thêm nữa: cả
   điểm đó lẫn `mark_ruled_out_siblings` đều áp dụng cho **mọi** edge được
   nâng cấp/loại bỏ, không riêng edge có `formal_source = 'stack_graphs'`
   trước đó — thiếu điều kiện lọc sẽ làm counter đếm cả những lần nâng cấp
   không liên quan gì đến stack-graphs (overcounting), đánh mất chính mục
   đích D3 muốn đạt (tín hiệu bất đồng thật, đáng tin). — **HIGH** — mitigation
   in plan: **YES** (đã sửa trực tiếp §5.1 + thêm test âm tính bắt buộc ở
   §5.3 trong lúc audit này).
2. **D1: persisted-index staleness khi feature flag đổi giữa các lần build
   trên cùng 1 SQLite index.** Spec không mô tả điều gì xảy ra với các row
   `formal_source = 'stack_graphs'` đã có sẵn trong DB khi 1 build sau đó
   thiếu `stack-graphs-formal` (downgrade, đổi kênh cài đặt, hoặc CI matrix
   không nhất quán). Incremental reindex chỉ xử lý file dirty — file không
   đổi giữ nguyên `formal_source = 'stack_graphs'` vĩnh viễn dù resolver tạo
   ra verdict đó không còn tồn tại trong binary, không có cách nào re-verify
   hay downgrade nó. Kết hợp D2 (field này giờ hiển thị trực tiếp cho agent
   với hàm ý "kém tin cậy hơn scip") tạo ra 1 lớp dữ liệu mới: "tự tin dán
   nhãn nhưng mồ côi kiến trúc" — agent thấy `formal_source: "stack_graphs"`
   và hiệu chỉnh độ tin theo đúng hướng dẫn của D2, nhưng không biết verdict
   đó có thể không bao giờ được re-verify trên build này. — **MED-HIGH** —
   mitigation in plan: **NO** (chưa có trong spec — cần thêm vào §8/§9 hoặc
   plan: ít nhất 1 dòng log/field cảnh báo khi `indexing_status` phát hiện
   `formal_source = 'stack_graphs'` tồn tại trong DB nhưng build hiện tại
   thiếu feature `stack-graphs-formal`).
3. **D1: nhánh `#[cfg(not(feature = "stack-graphs-formal"))]` không có CI
   job nào chạy định kỳ** (tự spec §9 đã nêu, audit xác nhận đây là rủi ro
   thật và cần 1 quyết định dứt khoát, không để mở). Một code path chỉ được
   test 1 lần lúc ship, không nằm trong `ci.yml`'s `--features` matrix hiện
   có, dễ bit-rot âm thầm (ví dụ 1 PR sau này thêm 1 dòng dùng
   `crate::resolver::formal::*` không cfg-gate đúng, không ai phát hiện cho
   tới lần build-without-feature tiếp theo, có thể là nhiều tháng sau). —
   **HIGH** — mitigation in plan: **NO** (quyết định "thêm CI job hay chấp
   nhận rủi ro" phải chốt trong writing-plans, không để lại làm "TBD" trong
   code).

### Layer Signals

- **L1 Logic:** xem Failure Mode 1 — nhánh untested + citation sai đã được
  audit bắt và sửa trực tiếp trong spec.
- **L2 Concurrency:** no new signal — `AtomicU64`/`Relaxed` đúng pattern đã
  có (`FORMAL_RESOLUTION_TIMEOUTS`), không có shared-state mới rủi ro hơn.
- **L3 Data:** xem Failure Mode 2 (persisted-index staleness qua feature-flag
  boundary) — đây là tín hiệu L3 thật, không phải "no signal".
- **L4 Integration:** no signal — không có external API/service mới.
- **L5 Security:** no signal — không đụng auth/permission/scope.
- **L6 Observability:** D3 chính là cải thiện observability — nhưng bản thân
  nó có 1 gap nhỏ: `indexing_status.scip_stack_graphs_overrides` không có
  ngưỡng/alert nào định nghĩa, thuần "phải tự nhìn" — chấp nhận được vì khớp
  đúng pattern `formal_resolution_timeouts` đã có từ trước (không phải
  regression riêng của lever này).
- **L7 Cross-cutting (idempotency):** đã gộp vào Failure Mode 1 — 2 hàm tăng
  cùng 1 counter phải được xác nhận không double-count cùng 1 sự kiện logic
  (production `ingest_occurrences` + `mark_ruled_out_siblings` là 2 pass
  riêng biệt trên cùng 1 lần `ingest_occurrences()` call — cần plan xác nhận
  chúng không overlap trên cùng 1 edge).

### Assumptions to Verify

- **ASSUMED** (spec §9, D2): "không tool nào phụ thuộc etag ổn định qua
  deploy boundary theo cách sẽ vỡ" — nêu như điều cần xác nhận, chưa xác
  nhận. Plan phải grep toàn bộ nơi `hash_caller_entries`/`hash_callee_entries`
  hoặc `edit_context`'s combined etag được đọc lại ở phía client/hook (đặc
  biệt: xác nhận etag này KHÔNG phải cơ chế gate an toàn edit riêng biệt so
  với `source`'s content-hash/`expected_hash` — nếu có, hậu quả nặng hơn 1
  cache-miss đơn thuần).
- **ASSUMED** (spec §5.1, đã sửa trong audit này): thân hàm
  `mark_ruled_out_siblings` chưa được đọc đầy đủ — điều kiện lọc theo
  `formal_source` cũ phải được xác nhận đọc code thật lúc viết plan, không
  suy đoán tiếp từ doc comment.
- **ASSUMED implicit**: build-without-`stack-graphs-formal` không bao giờ
  chạy trên 1 index đã có sẵn `formal_source = 'stack_graphs'` — spec D1
  không phát biểu điều này rõ ràng, và Failure Mode 2 cho thấy giả định này
  (nếu có) không được đảm bảo bởi thiết kế hiện tại.

### Abductive Hypotheses

1. **D1 × D2 interaction:** một khi `stack-graphs-formal` optional và 1 build
   ship thiếu nó, `formal_source` không bao giờ là `'stack_graphs'` cho edge
   MỚI trong build đó — nhưng edge CŨ (từ build trước, có feature) vẫn mang
   nhãn đó mãi mãi (Failure Mode 2). D2 làm nhãn này hiển thị trực tiếp và có
   hàm ý "kém tin cậy hơn" cho agent — kết hợp lại tạo ra 1 lớp lỗi mà từng
   unit riêng lẻ không tự sinh ra: dữ liệu "tự tin dán nhãn nhưng không ai
   còn có thể verify lại được" — không thấy được nếu chỉ pre-mortem D1 hoặc
   D2 riêng.
2. **Scale/steady-state alarm fatigue:** counter D3 monotonic từ lúc process
   start (giống `formal_resolution_timeouts`), không phân biệt "1 bất đồng
   lịch sử" với "cùng 1 edge bị re-confirm bất đồng qua N lần reindex" trên 1
   daemon sống lâu, repo lớn với SCIP overlay chạy thường xuyên (Go
   workspace overlay, rust-analyzer). Codebase này đã có tiền lệ đúng vấn đề
   này ở nơi khác (ADR-A1/A2: edge "silently flip between formal and textual
   confidence across reindexes purely due to machine load") — số đếm có thể
   tăng nhanh trong vận hành bình thường, không phải bất thường, huấn luyện
   agent/người dùng bỏ qua tín hiệu (alarm fatigue) thay vì điều tra khi thật
   sự cần. Chỉ lộ ra ở quy mô/thời gian chạy dài, không thấy được trên
   fixture nhỏ trong test.

### Gate Result
PASS WITH FLAGS — proceed to writing-plans. Failure Mode 1 đã fix trực tiếp
trong spec (không cần plan xử lý thêm). Failure Mode 2 và 3 **MUST** có
mitigation cụ thể trong plan trước khi implementation bắt đầu:
- FM2: plan phải thêm 1 cơ chế phát hiện/cảnh báo khi DB có
  `formal_source = 'stack_graphs'` nhưng build hiện tại thiếu
  `stack-graphs-formal` (tối thiểu: 1 field/log trong `indexing_status`,
  không cần tự động sửa dữ liệu).
- FM3: plan phải chốt dứt khoát "thêm CI job matrix mới cho
  `--no-default-features --features tier0-5,scip-overlay`" hay "chấp nhận rủi
  ro + ghi lý do" — không để lại như câu hỏi mở trong code review sau này.
