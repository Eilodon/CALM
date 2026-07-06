# Code Intelligence MCP — Design v2.7.2

> 16 tools. 4 layers. Zero new external dependencies.

---

## Architecture: 4 Layers — 16 Tools

**Layer 1 — Index Engine** (background process, persistent)
**Layer 2 — Tool Surface** (MCP server, stdio transport) — **16 tools**
**Layer 3 — Behavioral Guidance** (AGENTS.md + `workflow_guide` embedded) — 8 stages
**Layer 4 — Tool Presets** (selective registration, zero logic change)

Behavioral layer không phải add-on, là half the system.

---

## Design Rationale: Vấn đề mỗi quyết định kiến trúc giải quyết

**Lookup-then-inspect tốn 3 round-trip.** Pattern phổ biến nhất trong mọi session — agent
muốn biết "symbol X là gì?" — về bản chất cần 3 bước: tìm xem X ở đâu, xem structure file
đó, đọc metadata + health của X. Mỗi bước là 1 round-trip riêng qua MCP transport.
`locate` compound cả 3 bước vào 1 call — giảm pattern phổ biến nhất từ 3 calls xuống 1.

**Tool description luôn chiếm context, bất kể agent đang ở stage nào.** Mỗi tool
description nằm cố định trong context window của agent ở mọi turn, dù agent chỉ đang làm
việc tập trung ở một workflow stage cụ thể (chỉ orient, chỉ edit, ...). Token overhead này
không giảm theo workflow — nó là phí cố định cho toàn bộ session. Tool Presets (Layer 4)
giải quyết bằng selective registration: chỉ tools relevant với stage hiện tại mới xuất
hiện trong context của agent.

**Quyết định "gọi tool gì tiếp theo" đòi hỏi 1 vòng suy luận riêng.** Guidance kiểu
`workflow_guide`/AGENTS.md nằm *ngoài* response — agent phải internalize rồi tự áp dụng
cho từng context khác nhau, tốn thêm 1 inference round mỗi lần quyết định bước kế tiếp.
`suggested_next` giải quyết bằng cách nhúng hint trực tiếp vào mọi response — agent không
cần suy luận, chỉ cần đọc field.

**Static analysis một mình không phân biệt được "không có caller" với "không bao giờ chạy".**
`dead_code_confidence: "high"` chỉ dựa trên "không có static callers + private scope" sẽ
false-positive với dynamic dispatch, reflection, hay decorator-registered handlers — những
symbol này thực ra được gọi ở runtime nhưng static analysis không thấy được edge. Coverage
Reader giải quyết bằng cách cross-reference runtime coverage data: có lcov/coverage hit
trong `[line_start, line_end]` → symbol *được* execute → downgrade confidence, không báo
nhầm là dead.

**Agent mặc định dùng native tool (grep/Read) nếu description không nói rõ phải dùng tool
nào thay thế.** Một description chỉ liệt kê schema/behavior không đủ để agent ưu tiên tool
chuyên dụng over native tool — cần ngôn ngữ trigger rõ ràng. Mọi tool description trong hệ
thống này follow 1 khung 4 phần cố định để giải quyết đúng vấn đề này (pattern này đã được
chứng minh hiệu quả ở các IDE/agent tooling khác, ví dụ JetBrains):
1. **USE WHEN** — trigger condition rõ ràng
2. **USE THIS INSTEAD OF** — explicit replacement của native tool tương ứng
3. **NOT FOR** — negative example, tránh agent dùng sai tool
4. **vs X** — disambiguation từ tool dễ nhầm lẫn nhất

Khung 4 phần này là quy ước bắt buộc cho mọi tool mới thêm vào hệ thống — không chỉ là
lịch sử rewrite, mà là convention sống cho future growth.

---

## Layer 1: Index Engine

### Core Engine

tree-sitter parse toàn bộ codebase → extract symbols (signature + docstring + line range),
import edges, call edges, inheritance edges → SQLite FTS5 dual-column search → incremental
indexing 2-level (node hash + edge invalidation lan ra `imported_by`) → file watcher
(`watchfiles`). `building_edges` phase thêm k-core coreness computation (O(V+E)) sau khi toàn bộ edges
ready → `symbols.coreness` → `is_hub` revised với OR(degree, coreness) condition, được
tính trong một function audited duy nhất (`update_is_hub_flags`).

**Coverage data** (in-memory, loaded on startup): `CoverageReader.load(project_root)` detect
và parse coverage file tự động — lcov, Python `.coverage`, Go `coverage.out`, Cobertura XML.
Kết quả inject vào `_compute_dead_code_confidence()`. Re-load khi `indexing_status(retry_embeddings=true)`.

**Session tracking** (in-memory, powers `session_context`): server ghi compact log mỗi
tool call — `{tool, input, compact_summary, timestamp, session_started_at}`. Không ghi
full response.

---

### FTS5 Dual-Column Search

SQLite built-in tokenizers không split camelCase hay snake_case. Solution: **pre-tokenize
tại index time** với hai FTS5 virtual tables:

**Pre-tokenization algorithm** (pure Python, no dependencies):

```python
def tokenize_identifier(name: str) -> str:
    s = re.sub(r'[_\-]+', ' ', name)            # snake_case, kebab-case → spaces
    s = re.sub(r'([a-z0-9])([A-Z])', r'\1 \2', s)  # camelCase → camel Case
    s = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1 \2', s)  # HTTPSRequest → HTTPS Request
    return s.lower().strip()

# Examples:
# "getUserByEmail"   → "get user by email"
# "HTTP_STATUS_CODE" → "http status code"
# "parseXMLFile"     → "parse xml file"
# "parse_lcov_file"  → "parse lcov file"
```

`fts_exact` lưu `name` gốc (unicode61 tokenizer — split by whitespace/punctuation).
`fts_tokens` lưu kết quả `tokenize_identifier(name)` — enable sub-word search.

**Search routing**: `search(kind="symbol")` query chạy trên cả hai tables, merge bằng
BM25 score:
```
symbol_score(d) = fts_exact_BM25(d) × 1.5  +  fts_tokens_BM25(d) × 1.0
```
Merge on `qualified_name` (sum scores), sort desc. `kind="text"` chỉ dùng
`fts_exact` trên `docstring` column.

---

### Phase-based Indexing

- **`scanning`**: liệt kê file, hash check. Chưa có symbol nào.
- **`parsing`**: symbols đã extract, edges **chưa build**.
- **`building_edges`**: đang build call/import/inheritance edges + confidence labels
  + coreness computation + `is_hub` flag update (một pass duy nhất, audited).
- **`ready`**: full graph sẵn sàng. `edges_ready: true` khi và chỉ khi `phase == "ready"`.

Embeddings không thuộc phase ladder này — track độc lập.

---

### Entry Point Detection

Entry points được detect trong `parsing` phase và lưu vào `symbols.is_entry_point`.
Một symbol là entry point nếu thoả MỘT TRONG các điều kiện:

`is_entry_point: true` ảnh hưởng: `dead_code_confidence` (không bao giờ là dead code),
`edit_context.callers[]` priority ranking (sub-priority (c): closest to entry points), `repo_overview.entry_points[]`.

---

### Call Graph Conservative Resolver

**Vấn đề**: tree-sitter thuần túy extract call edge bằng name-only matching.
`notifier.send()` → edge tới *tất cả* symbol tên `send` trong codebase. Interface
dispatch (TS), duck typing (Python), trait methods (Rust) → false edge ngập.

**Tại sao không dùng `tree-sitter-analyzer` (TSA)**:

Lý do kiến trúc (chính): Code Intelligence MCP tự nó là MCP server — không depend
runtime vào MCP server khác, tránh nested-MCP coupling và phụ thuộc vào internal API
không được cam kết stable. Self-contained resolver mượn design philosophy từ TSA, không
mượn package.

Lý do phụ: TSA kéo 33+ transitive deps (`mcp`, `uvicorn`, `numpy`, `networkx`, ...) —
không phù hợp minimal-dep philosophy.

*TSA v1.28.0 thực ra đã có call-edge extraction mature — quyết định self-contained đứng vì
lý do kiến trúc, không vì capability.*

**Solution**: Self-contained resolver trong `codeindex/resolver/` — implement bằng
tree-sitter-languages (đã là dep của codeindex).

**Algorithm — 3-tier resolution với alias tracking, audited**:

**[F7] Multi-assignment Guard**:

Nếu một biến được assign nhiều lần trong cùng file (`x = notify_user` rồi sau đó
`x = send_email`), alias_map dùng last-write-wins → resolver confident-resolve sai.
Pre-pass detect multi-assigned LHS, skip hoàn toàn khỏi alias_map:

**Rust `let_declaration` handler**:

**Go `short_var_declaration` / `var_declaration` handler**:

**Dispatch** (F7 guard unchanged):

**Tier 1/2 modifications** — không đổi (alias-aware lookup đã có sẵn):

**Implementation**: AST navigation dùng tree-sitter `child_by_field_name()` API (stable
trong tree-sitter ≥ 0.20). Per-language node type constants trong
`codeindex/resolver/lang_constants.py`, bao gồm `ASSIGNMENT_NODES`.

**Conservative contract**: khi không chắc, prefer `"textual"` hơn wrong `"resolved"`.
*Unknown beats mis-classified.*

**Language coverage**:

| Language   | Import | Type Annotation              | Call | Alias Tracking | Effective Coverage                  |
|------------|--------|------------------------------|------|----------------|-------------------------------------|
| Python     | ✓      | ✓ (typed_param + annotation) | ✓    | ✓              | Full (resolved/inferred/textual)     |
| TypeScript | ✓      | ✓ (type_annotation node)     | ✓    | ✓              | Full                                |
| JavaScript | ✓      | ✗ (dynamic)                  | ✓    | ✓              | Full (Tier 1/2 — dynamic)           |
| Java       | ✓      | ✓ (explicit type decl)       | ✓    | ✓              | Full                                |
| Rust       | ✓      | ✓ (type bound)               | ✓    | ✓              | Full (Tier 1/2 + alias)             |
| Go         | ✓      | ✗ (structural typing)        | ✓    | ✓              | Full (Tier 1/2 + alias)             |
| C / C++    | ✓      | ✗                            | ✓    | ✗              | Partial (Tier 1/2 only — defer)     |
| C#         | ✓      | ✓                            | ✓    | ✗              | Full (Tier 1/2 unchanged)           |
| Kotlin     | ✓      | ✓                            | ✓    | ✗              | Full (Tier 1/2 unchanged)           |
| Swift      | ✓      | ✓                            | ✓    | ✗              | Full (Tier 1/2 unchanged)           |
| Ruby       | ✓      | ✗ (dynamic)                  | ✓    | ✗              | Partial                             |
| PHP        | ✓      | ✓ (7.4+ type hints)          | ✓    | ✗              | Partial                             |
| Other      | ✗      | ✗                            | ✗    | ✗              | All textual — không crash           |

Language không có trong bảng hoặc grammar thiếu → resolver silent fallback, tất cả edges
là `"textual"`. Không crash, không error.

---

### Coverage Reader

**Vị trí trong pipeline**: Cuối `scanning` phase (sau DB init, migration, và scan_files; trước `parsing` phase), chạy một lần, cache kết quả trong memory.
Không write vào DB — chỉ cần khi tính `dead_code_confidence`.

**Trigger**: Auto-detect trên startup, tìm coverage file trong project root.

**Coverage reload policy** (H-9):
- Coverage data load **một lần duy nhất** tại startup — KHÔNG auto-reload khi file thay đổi trên disk.
- `retry_embeddings=true` trong `indexing_status` **KHÔNG** trigger coverage reload (chỉ reset embedding state).
- Để refresh coverage sau khi test suite chạy: **restart server**.
- `dead_code_source` transition: `"static"` (no file found at startup) → `"static+coverage"` (file found) — immutable trong session, không thể thay đổi mà không restart.

**Integration vào `_compute_dead_code_confidence()`** (trong indexer):

`dead_code_source: "static+coverage"` có nghĩa là cả static analysis lẫn coverage data đều
được dùng. Agent có thể trust `"high"` mạnh hơn khi `dead_code_source == "static+coverage"`.

**Server startup** (trong `serve` command):

```python
# Thứ tự khởi động (sequential, trong serve()):
conn = open_db(project_root / ".codeindex/index.db")  # WAL mode ON
init_schema(conn)          # CREATE TABLE IF NOT EXISTS
migrate_to_v2_6(conn)      # ALTER TABLE / CREATE INDEX idempotent
codeowners_patterns = load_codeowners(project_root)   # cached in-memory
coverage_data = CoverageReader.load(project_root)     # cuối scanning phase
register_tools(mcp_server, preset=config.preset)
start_background_indexer()                            # scanning → parsing → building_edges → ready
```

Sau khi `register_tools`, server bắt đầu accept requests — tools hoạt động ngay dù indexer chưa xong
(`edges_ready: false` trong responses cho đến khi phase = `ready`).

---

### WAL Mode

Multiple concurrent readers + one writer không block nhau.

---

### Auto-gitignore

Silent khi entries đã exist. Không modify `.gitignore` nếu không có `.git/` directory.

---

### K-core Coreness Computation — audited (F4, F11)

Chạy cuối Phase 2, sau khi toàn bộ `call_edges` đã insert.

**Lý do**: Pure in-degree (`caller_count`) miss bridge functions — node kết nối modules,
in-degree vừa phải nhưng đứng ở điểm giao quan trọng. K-core captures mutual reinforcement:
node A có coreness cao khi nó liên kết với nhiều nodes có coreness cao (recursive). Research
basis: Pan 2014, Meyer 2014, Qu 2021 (IEEE TSE, 47 citations) — k-core outperforms degree
centrality, betweenness, closeness cho software dependency graphs. Qu 2021: +11.5–12.6%
bug prediction accuracy trên 18 Java systems.

**Undirected interpretation**: mỗi directed edge `(from → to)` contribute adjacency cả
hai chiều. Cả ba papers đều dùng undirected variant — phù hợp với hub identification.

**Peeling algorithm** (O(V+E) thật — [F11] v2.6.1 implement `min(degree[v] for v in remaining)`
là O(V·√E+E), không phải O(V+E) như claim. v2.6.2 fix bằng bucket-by-degree với pointer
chỉ tăng, đạt O(V+E) thật. Cascading peel, handles cycles/isolates — semantics không đổi):

**Vì sao đạt O(V+E) thật**: `k_ptr` chỉ tăng, tổng số bước của vòng `while k_ptr <= max_deg`
cộng dồn suốt toàn bộ hàm ≤ `max_deg + 1`. Mỗi edge bị "chạm" tối đa 1 lần thực sự.
Mỗi node bị peel đúng 1 lần.
→ O(V) (peel) + O(E) (decrement) + O(max_deg) (k_ptr advance, `max_deg ≤ V`) = **O(V+E)**.

**[F4] Batch UPDATE dùng `qualified_name`**:

v2.6 dùng `UPDATE symbols SET coreness = ? WHERE name = ?` — `name` **không unique**
cross-file. Fix:

**`is_hub` revised — `update_is_hub_flags`, [F6] `caller_count_percentile` định nghĩa
inline**:

**[F12] Tách threshold degree-hub / bridge-hub**:
- `min_callers` (default 5) vẫn giữ cho degree-hub.
- `min_callers_bridge` (default 2) cho bridge-hub — mở cho "moderate in-degree, high coreness"
  (caller_count 2–4) như example gốc mô tả.

**[F5] Incremental update strategy — scale guidance + debounce**:

| Codebase size      | Python CPython estimate | Recommendation                                   |
|--------------------|--------------------------|---------------------------------------------------|
| ≤ 50k edges         | < 50ms                   | Global recompute — OK                              |
| 50k–500k edges       | 50ms–500ms               | Global recompute + 500ms debounce                  |
| > 500k edges         | > 500ms                  | Global recompute + debounce; log elapsed; consider incremental (v2.8) |

Debounce 500ms cooldown trên `on_change` trigger đủ để prevent thrashing trong active
coding sessions (10+ files/giây).

---

### Semantic Embedding Pipeline

Track độc lập với phase ladder. Opt-in, disabled by default.

**State machine**:

**Stack**: `fastembed` (ONNX runtime, ~60-100MB, không PyTorch) + `sqlite-vec` (KNN
virtual table trong cùng `index.db`). Model mặc định `BAAI/bge-base-en-v1.5` (768-dim).
Option chất lượng cao hơn: `nomic-ai/nomic-embed-text-v1.5` (768-dim, 8192 context).

**Hybrid fusion** — Reciprocal Rank Fusion với configurable k:

```
RRF(d) = Σ_L  1 / (k + rank(d, L))

L ∈ {fts_exact_results, fts_tokens_results}   (kind="symbol")
L ∈ {fts_exact_results, semantic_results}     (kind="hybrid")

rank(d, L) = vị trí 1-indexed của document d trong list L.
             Nếu d không có trong L → bỏ qua term đó.
Exact match boost: nhân hệ số ×1.5 vào RRF term từ fts_exact.
Final: deduplicate bằng qualified_name, sort by RRF score desc.
```

`k = config.search.rrf_k` (default 20). k=20 cho discrimination tốt hơn trên short
result lists (top-10) so với k=60 được thiết kế cho large IR corpora. Deduplicated,
re-ranked bằng RRF score.

---

## Layer 2: Tool Surface — 16 Tools

---

### Shared Type: `SuggestedNext`

Tất cả 16 tools đều emit field `suggested_next?: SuggestedNext` (optional, absent khi
không có gợi ý rõ ràng):

**Logic per tool** (pure function, không có side effects, không persist):

| Tool | Condition | `tool` | `reason` | `args` |
|---|---|---|---|---|
| `repo_overview` | `indexing_phase != "ready"` | `"indexing_status"` | `"Monitor until phase=ready before using graph tools"` | — |
| `repo_overview` | `embeddings_status == "failed"` | `"indexing_status"` | `"Recover embeddings"` | `{retry_embeddings: true}` |
| `repo_overview` | phase ready, embeddings ready OR disabled | `"locate"` | `"Start exploration"` | — |
| `search` | top result exists, kind=symbol | `"locate"` | `"Full context in 1 call (replaces symbol_info)"` | `{query: results[0].name, kind: "symbol"}` |
| `search` | results empty, kind != hybrid, kind != semantic | `"search"` | `"Try hybrid for broader recall"` | `{kind: "hybrid"}` |
| `search` | results empty, kind == semantic | `"search"` | `"Semantic index may not cover this — try text or hybrid search"` | `{kind: "text"}` |
| `search` | results empty, kind == hybrid | `"search"` | `"Embeddings may not cover this query — try exact text search or broaden wording"` | `{kind: "text"}` |
| `locate` | `top_result.symbol.is_hub == true` | `"edit_context"` | `"Hub detected — mandatory pre-edit check"` | `{symbol: ..., path: ...}` |
| `locate` | `top_result.symbol.dead_code_confidence == "high"` | `"callers"` | `"Verify dead code — no static callers found"` | `{symbol: ...}` |
| `locate` | `top_result.symbol.ambiguous == true` | `"symbol_info"` | `"Disambiguate top result"` | `{name: candidates[0].name, path: candidates[0].path}` |
| `locate` | default; results non-empty | `"source"` | `"Read implementation"` | `{target: results[0].name}` |
| `locate` | top_result absent (empty results) | `"search"` | `"No match — broaden with hybrid search"` | `{kind: "hybrid"}` |
| `symbol_info` | `is_hub == true` | `"edit_context"` | `"Hub — check blast radius before modifying"` | `{symbol: name, path: path}` |
| `symbol_info` | `health.test_files == []` | `"search"` | `"No tests found — search for coverage"` | `{query: name + " test", kind: "text"}` |
| `symbol_info` | default | `"source"` | `"Read implementation"` | `{target: name}` |
| `source` | `metadata.is_hub == true` *(chỉ khi `include_metadata=true`)* | `"edit_context"` | `"Hub — mandatory pre-edit context"` | — |
| `source` | default | `"callers"` | `"Check who uses this before modifying"` | `{symbol: ...}` |
| `callers` | `any edge_confidence=="textual"` OR `total_direct > 10` | `"edit_context"` | `"High blast radius or uncertain edges — verify before modifying"` | — |
| `callers` | `0 < total_direct <= 10` AND no textual edge | `"source"` | `"Read top caller implementation"` | `{target: direct[0].caller_symbol}` |
| `callees` | `total_direct > 0` | `"path"` | `"Trace specific call chain"` | — |
| `dependencies` | `imported_by_total > 20` | `"callers"` | `"High fan-in — check symbol blast radius"` | — |
| `path` | `exists == true` | `"source"` | `"Read meeting node implementation"` | — |
| `path` | `terminated_by == "timeout"` | `"path"` | `"Retry with smaller max_hops"` | `{max_hops: 4}` |
| `path` | `terminated_by == "max_hops"` | `"path"` | `"Path may exceed hop limit — retry with larger max_hops, or check the reverse direction"` | `{max_hops: <current+4>, from_symbol: <to_symbol>, to_symbol: <from_symbol>}` |
| `edit_context` | always | `"diff_impact"` | `"MANDATORY after changes — verify blast radius"` | — |
| `session_context` | `frontier non-empty` | `"file_overview"` | `"Explore top frontier file"` | `{path: frontier[0].path}` |
| `session_context` | `frontier empty` | `"repo_overview"` | `"Frontier exhausted — refresh map"` | — |
| `diff_impact` | `aggregate_risk in [critical, high]`, no `pending_scan` entry | `"callers"` | `"Verify high-risk callers manually"` | `{symbol: affected_symbols[0].name}` |
| `diff_impact` | `aggregate_risk == "medium"`, no `pending_scan` entry | `"callers"` | `"Medium-risk changes — spot-check key callers"` | `{symbol: affected_symbols[0].name}` |
| `diff_impact` | `aggregate_risk == "unknown"`, no `pending_scan` entry | `"indexing_status"` | `"Risk unknown — check index state"` | — |
| `diff_impact` | `unindexed_files` has a `reason == "pending_scan"` entry | `"indexing_status"` | `"Wait for index before treating as safe"` | — |
| `hotspots` | default | `"file_overview"` | `"Inspect highest-risk file"` | `{path: hotspots[0].path}` |
| `understand` | `ambiguous` present | `"symbol_info"` | `"Ambiguous — retry with specific candidate"` | `{name: ambiguous.candidates[0].name, path: ambiguous.candidates[0].path}` |
| `understand` | `is_hub == true` | `"edit_context"` | `"Hub — mandatory pre-edit check"` | `{symbol: name, path: path}` |
| `understand` | default | `"edit_context"` | `"Pre-edit: verify blast radius before modifying"` | `{symbol: name, path: path}` |
| `file_overview` | any `is_hub == true` | `"locate"` | `"Inspect hub symbol"` | `{query: top_hub.name}` |
| `file_overview` | default | `"source"` | `"Read a symbol implementation"` | — |
| `indexing_status` | `phase == "ready"` | `"locate"` | `"Index ready — begin exploration"` | — |
| `indexing_status` | `phase != "ready"` | `"indexing_status"` | `"Still indexing — poll again or use search/source while edges build"` | — |

**Priority rules** (within each tool, conditions evaluated top-to-bottom, first match wins):

- **repo_overview**: Condition 1 (phase not ready) beats Condition 2 (embeddings failed). Embeddings retry chỉ có ý nghĩa khi `phase == "ready"` — retry trước khi index xong là vô nghĩa.
- **diff_impact**: Condition "`unindexed_files` có entry `reason == pending_scan`" beats `aggregate_risk in [critical, high]`. aggregate_risk không thể trust khi có file đang chờ scan — có thể under-estimated. Giải quyết index trước, sau đó re-evaluate risk. Entry `reason == "out_of_scope"` KHÔNG kích hoạt điều kiện này — file đó sẽ không bao giờ được scan, không có gì để đợi.

**Note `path` + `max_hops`**: `"timeout"` gợi ý giảm search space bằng `max_hops` nhỏ hơn.
`"max_hops"` nghĩa là path thật có thể dài hơn limit — gợi ý *tăng* `max_hops`. `args` swap
`from_symbol`/`to_symbol` vì bidirectional BFS có thể asymmetric — retry chiều ngược lại
với cùng budget có thể tìm ra path mà chiều gốc chưa kịp chạm tới.

**Implementation**: pure function, preset-aware.

Server truyền `available_tools` từ `PRESET_TOOL_SETS[preset]` khi gọi compute.
Default `None` = no filtering (`--preset=full`).

---

### Tool 1 — `repo_overview`

*ALWAYS call this FIRST at the start of every session — never skip. USE WHEN: starting a new session, switching projects, or after server restart. NOT FOR: per-file details (use file_overview), searching for symbols (use search/locate). Call indexing_status() only when you need file-level counts or embedding error details.*

`symbol_count: null` (không phải `0`) khi phase chưa `"ready"`.

`health_summary.undocumented_hubs` có thể tăng sau khi upgrade lên v2.6+, vì `is_hub`
giờ bao gồm bridge hubs trước đây bị miss bởi pure degree detection — expected behavior,
không phải regression.

**`module_map` inclusion threshold**: file được include khi `symbol_count > 0`. Sort by
`symbol_count DESC`. Cap tại `config.repo_overview.max_modules` (default 50); khi vượt,
`truncated: true`. Files không có symbols (config files, assets, etc.) bị exclude hoàn toàn.

**`workflow_guide` exact content**:

---

### Tool 2 — `search`

*USE THIS INSTEAD OF native grep, text search, or file browsing tools. USE WHEN: you don't have an exact file path and line number. kind="hybrid" → highest recall (preferred when embeddings ready). NOT FOR: inspecting a file you already have (use file_overview). vs locate: search returns a result list; locate returns search + symbol metadata in one call.*

**Sort policy**:
- `kind="symbol"`: exact match first → BM25 (dual-column weighted) → caller_count desc
- `kind="text"`: BM25 → recency (file mtime desc)
- `kind="file"`: exact match first → path depth asc
- `kind="semantic"`: cosine similarity desc
- `kind="hybrid"`: RRF score desc (k = `config.search.rrf_k`), deduplicated

**`kind="file"` trên directory path**: nếu query khớp với directory (không phải file), trả
`match_type: "dir_match"` với `symbols_in_file: null`. Không crash, không expand tree.

**`kind="hybrid"` degraded suggestions**: khi `embeddings_status != "ready"` và `degraded: true`,
`suggestions[]` luôn include:
`"Embeddings still indexing — retry kind='hybrid' after indexing_status shows embeddings_status=ready"`

**Chunk boundary cho `kind="text"`**: N = `config.search.text_chunk_context_lines` (default 10).

**`degraded`**: `true` khi `kind="hybrid"` request nhưng embeddings chưa ready → kết
quả FTS5-only.

**Behavior khi `embeddings_status != "ready"`**:

| `embeddings_status`         | `kind="semantic"`                             | `kind="hybrid"`                                    |
|-----------------------------|-----------------------------------------------|----------------------------------------------------|
| `"disabled"`                | Error `FEATURE_UNAVAILABLE`                   | `degraded: true`, FTS5-only, note gợi ý enable embeddings |
| `"downloading"/"embedding"` | `results: []`, note gợi ý dùng `kind="text"` | `degraded: true`, FTS5-only, note                  |
| `"failed"`                  | Error `EMBEDDING_FAILED`                      | Error `EMBEDDING_FAILED`                            |
| `"ready"`                   | Bình thường, `degraded: false`                | Bình thường, `degraded: false`                      |

---

### Tool 3 — `file_overview`

*USE WHEN: you have a file path and want to see its symbols, structure, and inferred role. vs source: file_overview shows ALL symbols in a file; source reads ONE symbol's body. vs dependencies: file_overview shows what's INSIDE the file; dependencies shows what the file IMPORTS/IS IMPORTED BY.*

**`inferred_role` heuristics** — first-match-wins, `null` nếu không match:

---

### Tool 4 — `symbol_info`

*USE WHEN: you have a symbol name and want metadata + health signals BEFORE reading source. Check is_hub + coreness before deciding whether to modify — hub symbols need edit_context. NOT FOR: reading source (use source), finding symbols (use search/locate). vs source: symbol_info is metadata-only (no code body).*

`coreness` là raw coreness number từ k-core peeling. Agent tự judge mức độ "high
coreness" dựa vào context — không có threshold cứng trong output.

---

### Tool 5 — `source`

*USE THIS INSTEAD OF native Read file tool — reads symbol-precise code, always fresh from disk. USE WHEN: you need to read the actual implementation of a specific function/class/method. Use source(target, include_metadata=true) to skip a prior symbol_info call. Use source("path:line_start-line_end") for exact location. NEVER use native Read tool on a full file — it floods context with unrelated code.*

`context_lines` mở rộng đối xứng từ symbol range, cap tại file boundaries. Khi
`target = "path:line_start-line_end"` — exact format — không bao giờ ambiguous.

---

### Tool 6 — `callers`

*USE WHEN: you need to know who calls a specific symbol — blast radius scan, refactoring impact. USE THIS for SYMBOL-LEVEL call sites. NOT for file-level imports (use dependencies). vs edit_context: callers is for exploration; edit_context is the mandatory pre-edit tool.*

**`transitive_count` semantics**: tổng unique symbols reachable trong `max_depth` hops,
không cap bởi `limit`. `null` khi BFS timeout (kèm `transitive_capped: true`). Khi
`max_depth == 1`: cả hai field **absent**. Transitive BFS bounded bởi
`config.callers.transitive_timeout_ms` (default 3000ms) — khi timeout, trả
`transitive_count: null` và `transitive_capped: true`.

**Partial count policy**: khi timeout, **KHÔNG trả partial count** — `transitive_count: null`
kể cả khi BFS đã visit được một số symbols. Rationale: partial count gây hiểu nhầm
(agent có thể tưởng đó là tổng thực); `null` + `transitive_capped: true` là signal rõ ràng
"số thực không biết, cần retry với smaller depth hoặc lớn hơn timeout."

**`max_depth=1` special case**: transitive BFS **KHÔNG được khởi chạy**. `transitive_count`
và `transitive_capped` đều **absent** (không phải `null`, không phải `0`). Rationale:
depth=1 chỉ trả direct callers — đã có trong `direct[]` — chạy thêm BFS sẽ yield same
result với overhead không cần thiết.

`direct[].line` là call site — singular, không phải definition range.

**Performance note**: query `SELECT from_symbol FROM call_edges WHERE to_symbol = ?`
hưởng lợi từ `idx_call_edges_to` — không còn full scan trên codebase lớn.

---

### Tool 7 — `callees`

*USE WHEN: you need to trace what a symbol calls — understanding logic flow, internal deps. NOT for finding who calls this symbol (use callers). vs callers: callers=upward (who calls X); callees=downward (what X calls).*

Same transitive semantics và depth cap như `callers`. Transitive BFS bounded bởi
`config.callees.transitive_timeout_ms` (default 3000ms).

---

### Tool 8 — `dependencies`

*USE WHEN: you need to understand file-level architectural connections. USE THIS for FILE-LEVEL import graph. NOT for symbol-level call sites (use callers/callees). vs callers/callees: dependencies is file-level; callers/callees is symbol-level.*

Không nhận symbol name → không cần Ambiguity Contract. `imports` capped tại
`config.dependencies.max_imports` (default 100). `imported_by` capped tại
`config.dependencies.max_imported_by` (default 100).

---

### Tool 9 — `path`

*USE WHEN: you need to trace if and how symbol A can reach symbol B through call chain. Bidirectional BFS — cycles terminate cleanly. path is DIRECTED: A→B ≠ B→A. terminated_by=null + exists=true/false → certain result. terminated_by="timeout"/"max_hops" → exists=null → inconclusive.*

`routes[].steps[0]` (source node) không có incoming edge → `edge_confidence` absent
(field bị omit, không phải `null`).

**`max_hops` defaults và capping**:

| Client gửi                    | Behavior                                                         |
|-------------------------------|------------------------------------------------------------------|
| `max_hops` absent             | Server default = **8**                                           |
| `max_hops > cap (20)`         | Clamp xuống 20, set `hops_clamped: true` trong response         |
| `max_hops = 0`                | Chỉ check `from_sym == to_sym` (0-edge path); else `exists: false` |

`config.path.max_hops_cap` (default 20) — rationale: diameter của production call graph điển hình < 15 hops; trên cap thường timeout trước khi có ích.

**Mapping `terminated_by` → `exists`**:

| `terminated_by` | `exists`         | Ý nghĩa                                                      |
|-----------------|------------------|--------------------------------------------------------------|
| `null`          | `true` / `false` | Search hoàn tất — kết quả chắc chắn                         |
| `"path_count"`  | `true`           | Found, dừng vì đủ `max_paths`                                |
| `"max_hops"`    | `null`           | Chưa reach trong giới hạn hop — không loại trừ path sâu hơn |
| `"timeout"`     | `null`           | Search bị interrupt                                          |

**BFS algorithm — Bidirectional, audited (F1, F2, F3, F10)**

v2.5.2 dùng unidirectional BFS: worst case O(V+E) — visit gần hết graph trước khi reach
`to_symbol`. v2.6 nâng cấp lên Bidirectional BFS với 4 vấn đề được fix:

- **F1 — N queries/level**: batch 1 query/level thay vì N queries (N = frontier size).
  Frontier 500 nodes với SQLite local disk ~0.1–0.5ms/query = 250ms/level → overhead át
  lợi ích thuật toán. Fix: batch query.
- **F2 — tie-break luôn nghiêng forward**: `<=` → effectively unidirectional. Fix:
  alternate theo `tie_toggle` khi bằng nhau.
- **F3 — `meeting_nodes` là list**: O(n) duplicate check mỗi lần thêm. Fix: `set[str]`.
- **F10 — empty-frontier deadlock + tie-break parity không robust**:
  - Bug 1: `len(forward_frontier) == 0` luôn nhỏ hơn `len(backward_frontier) > 0` → khi
    forward cạn, code mãi chọn expand forward (expand `set()` = noop). Fix: exhaustion flags.
  - Bug 2: global `step` tăng mọi vòng → parity tại 2 lần tie cách nhau bởi vòng không-tie
    lẻ trùng nhau. Fix: `tie_toggle` chỉ flip khi thực sự vào nhánh tie.

**Batch DB Helpers — F1**:

Với batch, overhead giảm từ ~250ms/level xuống ~1–2ms/level → claim speedup bidirectional
(2.9–6.89×, Haeupler et al. 2024; Dong et al. 2025) **trở thành thực tế** thay vì lý thuyết.

**Algorithm — F1, F2, F3, F10 áp dụng đầy đủ**:

**Path reconstruction trace** (correctness verification): với graph
`from_sym → A → meeting → B → to_sym`:
`fwd` (sau reverse) = `[(from_sym,None),(A,conf_from→A),(meeting,conf_A→meeting)]`;
`bwd` = `[(B,conf_meeting→B),(to_sym,conf_B→to_sym)]`; route ghép lại đúng từng bước.

**Correctness trên directed graph**: `path` không require *shortest* path — chỉ cần
*any* path thoả `max_hops`. Khi meeting node M tìm thấy, path `from → ... → M → ... → to`
tồn tại và total length ≤ forward_depth + backward_depth ≤ max_hops. Classic directed-graph
caveat (meeting node có thể không trên shortest path) không ảnh hưởng vì không claim shortest.

**Path enumeration limitation**: BFS predecessor dicts (`forward_pred`/`backward_pred`)
store 1 predecessor per node — mỗi meeting node chỉ yield 1 path. `max_paths` giới hạn
số meeting nodes được report, không phải tổng simple paths qua mỗi meeting node. Nếu graph
có ít meeting nodes, `len(routes)` có thể < `max_paths` dù có nhiều simple paths tồn tại.
`total_found` reflect số meeting nodes đã yield paths (xem below).

**Cycle detection**: visited sets (`forward_pred`/`backward_pred` keys) prevent revisit —
cycles không cause infinite loop; BFS terminates cleanly. Result (`exists: true/false`) phụ
thuộc vào reachability, không phải sự tồn tại của cycle.

---

### Tool 10 — `edit_context`

*ALWAYS CALL THIS before any code modification — mandatory, never skip. USE WHEN: you are about to edit, refactor, or delete a symbol. NOT FOR: read-only inspection (use symbol_info + source). NOT post-edit (use diff_impact).*

**Priority order — `callers`** (confidence-first):
1. `edge_confidence: "resolved"` — sub-order: (a) referenced trong test files (b) `is_hub: true`
   (c) closest to entry points — đo bằng BFS hop count từ entry point symbols (main, handler, CLI
   root, exported API); fewer hops = higher priority (d) caller_count desc — tie-break khi
   (a)–(c) bằng nhau
2. `edge_confidence: "inferred"` — cùng sub-order
3. `edge_confidence: "textual"` — cùng sub-order

**Priority order — `callees`**: 1. `"resolved"` 2. `"inferred"` 3. `"textual"`, trong
cùng tier sort by `line` asc.

`edges_ready: false` → cold-start. `index_freshness.stale_callers: true` → index ready
nhưng file vừa đổi chưa re-parse.

**Risk planning: Bridge-hub vs Degree-hub**:

`risk_assessment` dùng `is_hub` làm input chính. `is_hub` capture thêm bridge functions
(caller_count medium, coreness cao) — không chỉ pure high-degree hubs. Khi đọc output:

- `is_hub: true` + `caller_count` **thấp** (không nằm trong top 5%) = **bridge hub**.
  Edit ảnh hưởng cross-module integration — risk cao hơn apparent degree suggest.
- `is_hub: true` + `caller_count` **cao** = degree hub (classic behavior).
- Dùng `coreness` trong `symbol_info`/`source.metadata` để phân biệt:
  `coreness >> p75` + `caller_count` thấp = strong bridge-hub signal.

---

### Tool 11 — `session_context`

*USE WHEN: after 10+ tool calls without convergence, or when starting a new sub-task. session_started_at: save on first call as T₀. Changed T₀ = server restarted.*

**`explored.symbols[]` ordering khi truncated**: MRU-ordered (LRU-eviction) — sort by `last_accessed_at DESC`,
keep top 50. Most-recently-used symbols first; least-recently-used evicted khi vượt 50.

**`already_fetched` overflow policy**: FIFO-capped tại `config.session.max_fetched` (default 200).
Khi vượt 200 entries, oldest entries bị drop. Ordering trong response: insertion order (most recent last).
`session_context.already_fetched` không bao giờ raise error khi overflow — silent eviction.

**`frontier` algorithm**:

`reason`: `"imported_by_explored"`, `"contains_callers_of_explored"`, `"both"`.
`frontier_degraded: true` khi `edges_ready: false`.

**`session_started_at` — restart detection protocol**: lần gọi đầu tiên, agent lưu giá
trị làm T₀. Khác T₀ ở lần gọi sau = server đã restart — `already_fetched`/`explored`
không liên tục với trước.

---

### Tool 12 — `diff_impact`

*CALL THIS after every code change, BEFORE commit or push — never skip. USE WHEN: you have uncommitted changes and want to verify blast radius. NOT FOR: pre-edit analysis (use edit_context). diff=<text> → no git needed. staged=true or commits="..." → requires git in PATH. vs edit_context: edit_context=pre-edit (proactive risk check); diff_impact=post-edit (verification before commit).*

**`signature_changed` detection** — range-overlap-check:

**Risk escalation rule**:

**`aggregate_risk` algorithm**:

`"unknown"` luôn override bất kỳ indexed-file risk nào — kể cả `"critical"`. Lý do: không thể trust risk estimate khi có unindexed files, bất kể indexed portion nói gì.

**`change_type: "renamed"` theo input mode**:

| Input mode        | Rename detection                                                              |
|-------------------|--------------------------------------------------------------------------------|
| `staged: true`    | `git diff --cached -M` — detect qua content similarity                       |
| `commits: string` | `git diff -M <commits>`                                                       |
| `diff: string`    | Không detect rename — appear như delete + add; `note` warns                  |

**Validation Rules**:

| Input case                                                       | Behavior                                                                                                           |
|------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------|
| Không có cái nào trong 3 option                                  | Error `INVALID_INPUT`: "Provide exactly one of diff, staged, or commits."                                          |
| Có ≥ 2 trong 3 option                                            | Error `INVALID_INPUT`: "Provide exactly one, not multiple."                                                        |
| `diff: ""` hoặc không parse được                                 | Error `INVALID_INPUT` với parse failure message                                                                    |
| `staged`/`commits` nhưng git không trong PATH                    | Error `FEATURE_UNAVAILABLE`, `suggestions: ["Provide diff text via diff parameter"]`                               |
| Diff hợp lệ nhưng chỉ touch file non-code                        | `affected_symbols: []`, `aggregate_risk: "low"`                                                                    |
| `staged: true` nhưng `git diff --cached` trả empty              | `affected_symbols: []`, `aggregate_risk: "low"`, note: "No staged changes found."                                  |
| `commits` với empty range (same commit)                          | `affected_symbols: []`, `aggregate_risk: "low"`, note: "Commit range resolves to no changes."                      |
| File trong diff chưa index (recognized extension, chờ scan)      | `affected_symbols: []`, `aggregate_risk: "unknown"`, `unindexed_files: [{path, reason: "pending_scan"}]`           |
| File trong diff ngoài phạm vi ngôn ngữ (docs/config/...)         | `affected_symbols: []`, `aggregate_risk` không bị ảnh hưởng, `unindexed_files: [{path, reason: "out_of_scope"}]`   |

`edge_confidence` không bao giờ bị discount trong `diff_impact` — `edge_confidence_note` được
expose, agent tự quyết định mức tin cậy. KHÔNG treat `"textual"` edges là safe, chỉ treat
là uncertain.

**Implementation**: parse diff → `(file, line_start, line_end)` mỗi hunk →
`SELECT * FROM symbols WHERE path=? AND line_start<=? AND line_end>=?` → blast_radius
logic shared với `edit_context`.

**Hunk overlap per symbol (M-10)**: range-overlap check được thực hiện **per symbol** — nếu một hunk
chạm vào 2 symbols khác nhau, mỗi symbol nhận `signature_changed` check độc lập dựa trên range của
chính symbol đó. Không aggregate: symbol A có thể `signature_changed: true`, symbol B trong cùng
hunk có thể `signature_changed: false`.

**`unindexed_files` scope (M-11)**: chỉ chứa files **trực tiếp xuất hiện trong diff** mà chưa có
row trong `file_index` (file đã scan nhưng 0 symbol — vd `mod.rs` chỉ có `pub mod` — KHÔNG tính là
unindexed, vì đã có row trong `file_index`). Mỗi entry có `reason`: `"pending_scan"` (extension được hỗ
trợ, indexer chưa kịp scan — tạm thời, tự hết khi index xong) hoặc `"out_of_scope"` (extension không
được parse — vĩnh viễn, không bao giờ tự hết dù đợi bao lâu). Chỉ `"pending_scan"` mới kéo
`aggregate_risk` xuống `"unknown"`; `"out_of_scope"` không ảnh hưởng risk. KHÔNG expand ra transitive
dependencies. Agent phải tự suy luận về transitive risk từ `edges_ready` state.

**`change_type` trong `diff` mode (M-12)**:
- `"modified"` — default cho mọi symbol bị touch bởi diff.
- `"added"` — chỉ khi diff header có `--- /dev/null` (file mới hoàn toàn).
- `"deleted"` — chỉ khi diff header có `+++ /dev/null` (file bị xoá hoàn toàn).
- `"renamed"` — **không detect** trong `diff` mode (không có git context); `note` warns về limitation này.
  Renamed files appear như 1 deleted + 1 added symbol.

**Truncation sort** (khi `affected_symbols_total > config.diff_impact.max_affected_symbols`):

Khi `affected_symbols_truncated: true`, symbols có risk thấp hơn bị cắt — không miss critical items.

**CODEOWNERS Integration** (chỉ tính khi `edges_ready: true` và `aggregate_risk >= medium`):

**`codeowners.py`** (pure Python, no new deps):

---

### Tool 13 — `indexing_status`

*USE WHEN: you need file-level index stats, embedding error details, or to trigger embedding recovery. NOT a replacement for repo_overview() at session start — repo_overview has indexing_phase already. retry_embeddings=true → triggers re-download of embedding model.*

`retry_embeddings: true` → server clear error state, trigger re-download → response ngay
`embeddings_status: "downloading"`.

---

### Tool 14 — `locate`

*Compound: search + file_overview + symbol_info in 1 call. Replaces the most common 3-call chain (66% reduction). USE INSTEAD OF calling search then file_overview then symbol_info separately. NOT FOR: reading source (use source after locate), pre-edit (use edit_context).*

**Behavior**:

- `depth="search_only"` → identical to `search` tool, no enrichment.
- `depth="with_file"` → search + file_overview of top result's file.
- `depth="with_symbol"` (default) → search + file_overview + symbol_info of top result.
- `kind ∈ {"text", "file"}` + `depth="with_symbol"` → **auto-downgrade to `depth="with_file"`**.
  Reason: text/file results carry no symbol name — `results[0].name` is a text snippet or
  filename, not a callable symbol — so `symbol_info()` cannot be called meaningfully.
  Response sets `depth_adjusted: "with_file"` to signal enrichment was capped. `kind ∈ {"symbol",
  "hybrid", "semantic"}` is unaffected — full enrichment proceeds as requested.

**Top result selection**: `results[0]` sau sort. Nếu top result ambiguous →
`top_result.symbol = AmbiguousResult`. Agent chọn candidate, gọi `symbol_info(name, path=candidate.path)` riêng.

**Performance**: search → file_overview → symbol_info trong same process, không có MCP
round-trip giữa các bước. Latency overhead ~1-2ms vs 3 separate calls (~15-30ms × 3).

**Preset membership**: `orient`, `trace`, `edit`, `compound`, `full`.

---

### Tool 15 — `hotspots`

*Proactive churn × complexity analysis. New capability — no equivalent in previous versions. USE WHEN: starting exploration of a codebase or after orientation to identify high-risk files before diving in.*

**Algorithm** (Adam Thornhill's method: hotspot_score = normalize(churn) × normalize(complexity)):

**`hotspot_method`**: `"git+index"` khi git available và có churn data. `"index_only"` khi
git không có hoặc timeout — fallback pure complexity ranking, vẫn useful.

**`note` field** (luôn có trong output):
- Git available: `"Analyzed commits since {since}."`
- Git available nhưng `hotspots: []` vì filter: `"No files exceeded min_churn={min_churn} commits since {since}. Try reducing min_churn."`
- Git không available: `"Git unavailable: ranking by complexity only. min_churn parameter not applied."`

**Risk level thresholds** (tunable qua config):

**Preset membership**: `orient`, `compound`, `full`.

---

### Tool 16 — `understand`

*Compound: locate + source + callers summary in 1 call. USE INSTEAD OF calling locate then source then callers separately. USE WHEN: you want to find, read, AND understand usage context of a symbol. NOT FOR: pre-edit (use edit_context — more complete blast radius). NOT FOR: browsing results list (use locate with depth="search_only").*

**Implementation**: ~60 lines composition — `search_logic` → `symbol_info_logic` →
`source_logic` → `callers_logic(limit=5)`. Không có new logic, chỉ là orchestration. Không
có MCP round-trips giữa steps (same process).

**Preset membership**: `compound`, `full`.

---

## Layer 4: Tool Presets

**Concept**: CLI flag lọc tools nào được register với MCP server. Không thay đổi bất cứ
logic bên trong nào — chỉ là selective tool registration.

**Motivation**: 16 tools × ~70 tokens/description = ~1120 tokens overhead cố định. Với
preset, agent trong từng workflow stage chỉ thấy tools relevant.

### CLI

### Preset Definitions

### Implementation

**Config.json support** (alternative to CLI):

CLI flag overrides config.json nếu cả hai có.

**Dynamic preset limitation**: `--preset` set một lần khi boot server — không thể thay đổi
giữa session. Nếu agent bắt đầu với `--preset=trace` nhưng cần edit tools ở Stage 7,
agent phải restart server với `--preset=edit` hoặc `--preset=full`. Khuyến nghị:
dùng `--preset=full` (default) nếu workflow span nhiều stages; dùng preset cụ thể chỉ
khi workflow đã xác định rõ scope (chỉ orient, chỉ trace, chỉ edit).

---

## Layer 3: Behavioral Guidance

### AGENTS.md — Navigational Workflow (v2.7.2)

---

## Shared Field Types

Contract cứng — thêm field vào shared type tự động propagate sang mọi nơi dùng type đó.

### Null vs Absent Convention (bắt buộc cho mọi tool response)

| TypeScript type        | JSON wire format                              | Khi nào dùng                                |
|------------------------|-----------------------------------------------|---------------------------------------------|
| `field?: T`            | Key **vắng mặt** hoàn toàn trong JSON object | Không có gợi ý / không applicable          |
| `field?: T \| null`    | Key **có mặt** với `null`, hoặc key vắng mặt | Phân biệt "explicitly empty" vs "not set"  |
| `field: T \| null`     | Key **luôn có mặt**, value có thể `null`      | Mandatory nullable — không được omit       |

**Quy tắc implementation**:
- `suggested_next?: SuggestedNext` → **không emit key** khi không có hint (không emit `"suggested_next": null`).
- `coreness?: number | null` → emit `null` khi `edges_ready: false`; emit `0` khi isolated (`edges_ready: true`); **omit key** khi `UnderstandOutput` ở trạng thái ambiguous.
- `note?: string` → omit khi không có note (không emit empty string `""`).
- `note: string` (mandatory) → luôn emit, kể cả khi là empty string (hiếm — xem tool spec).

**Python → JSON mapping**: `None` → `null` (nếu field type là nullable); không include key (nếu field type là optional-absent).

### `Health`

*(dùng ở `symbol_info.health`, `source.metadata.health`, `understand.health`)*

`dead_code_confidence`: `"none"` có callers HOẶC entry point; `"low"` không callers nhưng
runtime-covered (dynamic dispatch/reflection), HOẶC scope không xác định được (scope_clear=false);
`"medium"` không callers, không runtime-covered, scope xác định được (scope_clear=true) nhưng
không private; `"high"` không callers, private scope, không entry point.
`caller_count_by_confidence: null` khi `edges_ready: false`.

**`coreness` null vs 0 semantics** (dùng ở `symbol_info`, `file_overview.symbols[]`, `source.metadata`, `understand`):
- `coreness: null` — `edges_ready: false` (building_edges chưa chạy xong); client không nên interpret.
- `coreness: 0` — `edges_ready: true`, symbol isolated (không có call_edges nào connect tới nó).
- `coreness: N > 0` — symbol thuộc k-core với coreness = N; càng cao càng là bridge/hub.
Rule: **không bao giờ** emit `coreness: null` khi `edges_ready: true`.

`dead_code_source: "static"` khi chỉ có static analysis. `"static+coverage"` khi coverage
data có và được dùng. `dead_code_source` luôn present khi `Health` object tồn tại — mandatory
within the type. `Health` itself is optional in `understand` (absent khi ambiguous case), nhưng
khi present, tất cả fields bao gồm `dead_code_source` đều phải có.

### `SuggestedNext`

*(dùng ở tất cả 16 tool responses)*

Optional field trong mọi tool output — absent khi không có gợi ý rõ ràng hoặc tool bị
filter bởi preset.

### `AmbiguousResult`

*(dùng ở `locate.top_result.symbol`, `understand.ambiguous`; shape identical used inline by `symbol_info`, `source`, `callers`, `callees`, `path`, `edit_context`)*

Tools chỉ resolve symbol (`callers`, `callees`, `path`, `edit_context`) trả candidates
với 5 base fields. Tools có metadata sẵn (`symbol_info`, `source`, `locate`, `understand`)
trả full shape bao gồm cả extended fields. Agent dùng `path:line_start-line_end` format
để disambiguate bất kể tool nào.

### `ReviewerSuggestion`

*(dùng ở `diff_impact.suggested_reviewers`)*

### `EdgeConfidence`

*(dùng ở `callers.direct[]`, `callees.direct[]`, `path.routes[].steps[]`,
`edit_context.callers[]`, `edit_context.callees[]`, `diff_impact.high_risk_callers[]`)*

- `"resolved"`: callee defined cùng file, explicit import, hoặc alias confirmed. Reliable.
- `"inferred"`: callee type inferred qua import + type hints (hoặc alias type). Mostly reliable.
- `"textual"`: name-only — dễ false positive cả hai chiều.

### Ambiguity Contract

*(dùng ở `symbol_info`, `source`, `callers`, `callees`, `path`, `edit_context` —
KHÔNG dùng ở `dependencies` vì input là `path` exact)*

**NOT_FOUND case**: khi input không match bất kỳ symbol nào trong index, trả error
`NOT_FOUND` (xem §Unified Error Schema). Đây là case khác biệt với AMBIGUOUS: 0 matches
vs nhiều matches. Tất cả 6 tools trong contract xử lý cả hai case.

Khi input match nhiều candidate và không có `path`: trả `ambiguous: true`,
`candidates: [...]`, mọi field "kết quả chính" rỗng/null. Agent chọn candidate, gọi lại
với `path` cụ thể. 6 tools trong contract đều có `path?`/`from_path?`/`to_path?`.

**Candidate shape**: xem §AmbiguousResult type definition. Base 5 fields (`name`, `path`,
`kind`, `line_start`, `line_end`) có ở tất cả 6 tools. Extended fields (`class_context`,
`caller_count`, `language`, `signature`) chỉ có ở `symbol_info`, `source`, `locate`,
`understand`. Agent dùng `path:line_start-line_end` format để disambiguate — hoạt động
với cả base lẫn extended shape.

### Edge-Readiness Note Convention

*(dùng ở `callers`, `callees`, `dependencies`, `path`, `edit_context`, `diff_impact`,
`symbol_info`, `file_overview`, `locate`; `understand` có `edges_ready?: bool` optional nhưng không
có `note` convention — field này passthrough từ callers_logic)*

**`edges_ready` race condition policy**: `edges_ready: false` cho đến khi FULL `building_edges`
pass complete — không bao giờ `true` ở giữa chừng dù một số edges đã có. Nếu file thay đổi
trong khi `building_edges` đang chạy, `edges_ready` vẫn là `false` cho đến khi pass tiếp theo
hoàn tất. Không có partial-true state.

**Note field type**: `edit_context.note`, `callers.note`, `callees.note`,
`dependencies.note`, `path.note` — `string`, mandatory. `symbol_info.note`,
`file_overview.note`, `diff_impact.note`, `locate.note` — `string?`, optional.

**`diff_impact` note khi `edges_ready == false`**:
"Call graph đang build — affected_symbols và high_risk_callers có thể chưa đầy đủ. Kết
quả diff_impact lúc này chỉ nên dùng tham khảo, không nên dùng để quyết định an toàn
push code."

### Line-Range Convention

Symbol *definition* → `line_start`/`line_end` (range). Call/reference *site* → `line`
(singular).

| Field                                          | Convention                                       |
|------------------------------------------------|---------------------------------------------------|
| `symbol_info.line_start/line_end`              | definition range                                   |
| `file_overview.symbols[].line_start/end`       | definition range                                   |
| `search.results[].line_start?/line_end?`       | definition/chunk range (omit khi `kind="file"`)   |
| `source.line_start/line_end`                   | definition/range                                   |
| `edit_context.target.line_start/line_end`      | definition range                                   |
| `diff_impact.affected_symbols[].line_*`        | definition range                                   |
| `path.routes[].steps[].line_start/end`         | definition range                                   |
| `callers.direct[].line`                        | call site (singular)                              |
| `callees.direct[].line`                        | call site (singular)                              |
| `edit_context.callers[].line`                  | call site (singular)                              |
| `edit_context.callees[].line`                  | call site (singular)                              |
| `diff_impact.high_risk_callers[].line`         | call site (singular)                              |
| `session_context.already_fetched[].line_*`     | actual fetched range                              |

---

## Unified Error Schema

| Code                  | Recoverable | Meaning                                              |
|-----------------------|-------------|--------------------------------------------------------|
| `NOT_FOUND`           | false       | Symbol/file không tồn tại trong index                |
| `AMBIGUOUS`           | true        | Ambiguity không thể resolve in-band                  |
| `INDEX_PARTIAL`       | true        | Index chưa complete; retry sau                       |
| `PARSE_FAILED`        | false       | File có syntax error                                 |
| `TIMEOUT`             | true        | BFS/query timeout                                    |
| `DB_LOCKED`           | true        | SQLite write contention (rare với WAL)               |
| `INVALID_INPUT`       | true        | Bad params                                           |
| `FEATURE_UNAVAILABLE` | true        | Feature cần enable trong config                      |
| `EMBEDDING_FAILED`    | true        | Download/embedding lỗi                               |

**Error-to-tool mapping**:

| Error Code            | Emitted by                                                              |
|-----------------------|-------------------------------------------------------------------------|
| `NOT_FOUND`           | `symbol_info`, `source`, `callers`, `callees`, `path`, `edit_context`   |
| `AMBIGUOUS`           | (in-band via `ambiguous: true` — error form chỉ khi không có in-band path) |
| `INDEX_PARTIAL`       | `callers`, `callees`, `path`, `edit_context`, `diff_impact`              |
| `PARSE_FAILED`        | `source`, `file_overview`                                                |
| `TIMEOUT`             | `path`, `callers` (transitive), `callees` (transitive)                   |
| `DB_LOCKED`           | bất kỳ tool nào (rare với WAL)                                           |
| `INVALID_INPUT`       | `diff_impact`, `search`, `path`                                          |
| `FEATURE_UNAVAILABLE` | `search` (semantic/hybrid khi disabled), `diff_impact` (git unavailable) |
| `EMBEDDING_FAILED`    | `search` (semantic/hybrid khi embeddings failed)                         |

`AMBIGUOUS` error chỉ dùng khi không có natural in-band path.

---

## Implementation Decisions

**Tech stack**:

**Project structure**:

**Stack Graphs note**: GitHub Stack Graphs (formally-grounded, incremental scope resolution)
— plan cho v2.8+ sau khi ConservativeResolver Python/TS/Java/Rust/Go production-verified ở
v2.7. Self-contained resolver ở v2.7 không preclude Stack Graphs migration — two complementary
approaches (current: alias-tracking + confidence tiers; future: formally-grounded binding).

**`config.json`**:

`hub_threshold.coreness_pct: 75` — p75 coreness làm ngưỡng bridge-hub. `min_callers_bridge: 2`
[F12] — floor thấp hơn cho bridge-hub, cho phép "moderate in-degree, high coreness" (caller_count
2–4). `search.rrf_k: 20` — k=20 cho score discrimination tốt hơn k=60 trên top-10 code symbol
lists (rank=1 vs rank=10: k=60 → chỉ ~1.1× diff; k=20 → ~1.4× diff — discrimination rõ ràng
hơn cho danh sách ngắn). Nếu cần behavior cũ: set `"rrf_k": 60` trong config.json. `edit_context.max_callers: 15`
— up từ 10, giảm `callers_truncated: true` cases.

**Startup migration detection** (trong server.py, sau khi load config):

`callers.max_depth_cap = callees.max_depth_cap = 4` (symmetric). Server nên log
`transitive_capped` events per tool+direction — dùng data thực tế để quyết định asymmetry
trong tương lai nếu cần (e.g. callers cần depth lớn hơn callees).

---

**Indexing pipeline**:

---

**DB Schema**:

`idx_call_edges_to` benefit cả `path` tool (backward BFS) lẫn `callers` tool.
`symbols.qualified_name` là unique identifier — cùng format với
`call_edges.from_symbol`/`to_symbol` — coreness/is_hub UPDATE (F4) dùng cột này.

---

**DB Migration** — simplified idiom (F8):

---

## Phase Name Canonical Mapping

Tài liệu này dùng tên phase chính thức (code enum) — không dùng số thứ tự:

| Phase name (enum)   | Ý nghĩa                                               |
|---------------------|-------------------------------------------------------|
| `scanning`          | File enumeration, hash check, DB init, migration      |
| `parsing`           | Symbol extraction, import/call edge collection        |
| `building_edges`    | Edge resolution, coreness computation, `is_hub` flag  |
| `ready`             | Full graph sẵn sàng, `edges_ready: true`              |

Coverage Reader chạy cuối `scanning` phase. Coreness/is_hub chạy cuối `building_edges` phase.

---

## Implementation Order

### Base System (v2.6.2)

### v2.7 Additions (dependency-safe)

---

**Khởi động**:

---

## Navigational Map: 16 Tools × Intent

---

## Maintenance Checklist

Trước khi merge bất kỳ patch nào thêm shared field vào một tool, grep xác nhận tool
đó đã được liệt trong đúng §Shared Field Types:

| Nếu patch thêm field...     | Phải có tool trong...                                         |
|-----------------------------|---------------------------------------------------------------|
| `edges_ready: bool`         | §Edge-Readiness Note Convention                               |
| `ambiguous`/`candidates`    | §Ambiguity Contract                                           |
| `health` / `Health` type    | §Health *(symbol_info.health, source.metadata.health, understand.health)* |
| `EdgeConfidence`            | §EdgeConfidence                                               |
| `coreness`                  | §symbol_info, §file_overview.symbols[], §source.metadata      |
| `suggested_next`            | §SuggestedNext type definition + logic table                  |
| `dead_code_source`          | §Health type (`dead_code_source` present, always non-optional)|
| `suggested_reviewers`       | §diff_impact output + §ReviewerSuggestion type                |
| Tool mới                    | phải có trong §Preset Definitions                             |

Không tin vào trí nhớ — grep tên field trong toàn bộ §Shared Field Types, confirm tool
đang sửa đã xuất hiện trước khi approve merge.

---

## Regression Test Targets

### `path` tool

- `bidirectional_bfs(A,B) == exists:true` ↔ `unidirectional_bfs(A,B) == exists:true` trên
  cùng graph — existence phải identical, chỉ speed khác
- `routes[0]` là valid path — walk từng step, verify edge tồn tại trong `call_edges`
- Cycle trong graph → terminate cleanly, không loop
- `terminated_by: "max_hops"` chỉ khi `f_depth + b_depth >= max_hops`
- **[F1]** Graph với frontier 1000 nodes → verify 1 DB query/BFS level — check qua `conn.set_trace_callback`
- **[F2]** Linear chain `A→B→C→D→E`, branching factor 1 → verify cả `f_depth` và `b_depth` tăng
- **[F3]** `assert isinstance(meeting_nodes, set)` — no duplicate meeting nodes
- **[F10-test-1]** `from_symbol` là leaf (0 callees), `to_symbol` có backward chain dài 5 hop
  trong `max_hops=8` → `exists: true`, KHÔNG rơi vào `"max_hops"`/`"timeout"`
- **[F10-test-2]** Cả hai phía cạn cùng lúc, không gặp nhau → `exists: false`, `terminated_by: null`
- **[F10-test-3]** Tie xảy ra ở bước 1, rồi 3 bước không-tie, rồi tie lại ở bước 5 →
  `tie_toggle` tại 2 lần tie đó phải khác nhau
- `path(terminated_by="max_hops")` → `suggested_next.tool == "path"`, `suggested_next.args.max_hops > original` ✓
- `path(terminated_by="max_hops")` → `suggested_next.args.from_symbol == original to_symbol` (reverse heuristic) ✓
- `path(terminated_by="timeout")` hint unchanged: still suggests smaller max_hops, not reversed ✓

### `is_hub` và coreness

- Symbol với `caller_count < min_callers_bridge` → `is_hub: false` bất kể coreness
- Symbol từng `is_hub: true` ở v2.6.1 → vẫn `is_hub: true` (min_callers_bridge ≤ min_callers)
- Codebase rỗng (0 edges) → tất cả `coreness: 0`, is_hub fall back về pure degree check
- **[F4]** Hai symbols khác file cùng tên `process` nhưng khác `qualified_name` → mỗi symbol nhận coreness riêng, không clobber nhau
- **[F6]** 10 symbols caller_counts `[1..10]` → symbol caller_count=8 phải có `percentile_rank = 0.8`
- **[F5]** Log elapsed time của `compute_coreness` + `update_is_hub_flags` tại 50k/200k/500k edges. Fail build nếu > 2s trên reference machine
- **[F11-test]** Graph "staircase" ở scale 50k+ edges → verify `compute_coreness` tăng gần-linear theo (V+E)
- `caller_count=2` (≥ min_callers_bridge, < min_callers), coreness >> p75 → `is_hub: true` ✓
- `caller_count=1` (< min_callers_bridge), coreness >> p75 → `is_hub: false` ✓

### Conservative Resolver

- Edge confidence từ v2.5.2 không bao giờ downgrade
- `alias_map` chỉ chứa bare-identifier assignments — complex RHS không vào alias_map
- Resolver không crash khi `alias_map` rỗng
- **[F7]** Single-assignment: `x = notify_user` → `alias_map["x"] == "notify_user"` ✓
- **[F7]** Multi-assignment: `x = notify_user` line 10, `x = send_email` line 50 → `"x" not in alias_map` ✓
- **[F7]** Cross-scope: `x = notify_user` trong func A, `x = send_email` trong func B → `"x" not in alias_map` (file-level conservative) ✓
- **[F7]** Complex RHS: `x = func()`, `x = obj.attr`, `x = a if c else b` → `"x" not in alias_map` ✓

### Rust/Go Alias Tracking

- Rust `let x = some_func;` → `alias_map["x"] == "some_func"` ✓
- Rust `let mut x = some_func;` → `"x" not in alias_map` (mutable, skip) ✓
- Rust `let (a, b) = pair;` → neither in alias_map (destructuring) ✓
- Rust `let x = func();` → `"x" not in alias_map` (call, not bare identifier) ✓
- Rust `let x: Type = y;` → `"x" not in alias_map` (type annotation) ✓
- Go `x := y` (single LHS, single RHS, both identifiers) → `alias_map["x"] == "y"` ✓
- Go `x, y := a, b` (multi LHS) → neither in alias_map ✓
- Go `x := func()` (RHS is call) → `"x" not in alias_map` ✓
- Go `var x = someFunc` → `alias_map["x"] == "someFunc"` ✓
- Go `var x int = someFunc` (typed) → `"x" not in alias_map` ✓
- F7 guard: Rust `let x = a;` at line 5, `let x = b;` at line 10 → `"x" not in alias_map` ✓
- F7 guard: Go `x := a` at line 5, `x = b` at line 10 → `"x" not in alias_map` ✓

### Schema consistency

- `coreness: null` khi `edges_ready: false`
- `coreness: 0` khi symbol isolated (`edges_ready: true`)
- Migration idempotent: chạy 2 lần không crash, không duplicate index
- `dead_code_source` present in ALL symbol_info responses (not optional) ✓
- `dead_code_source == "static"` when no coverage file found ✓
- `dead_code_confidence` logic không đổi khi coverage data không available — graceful
  degradation về static-only, không phá behavior cũ ✓

### `locate` tool

- `locate(query).results` semantically equivalent to `search(query).results` khi cùng query + kind
- `locate(query, depth="search_only")` == `search(query)` (output identical)
- `locate("X", depth="with_symbol").top_result.symbol.name == symbol_info("X").name`
- `locate("X").top_result.file.inferred_role == file_overview(locate("X").results[0].path).inferred_role`
- `locate(query_no_match).top_result` absent — không crash
- Ambiguous top result → `top_result.symbol.ambiguous == true`, `candidates` non-empty
- `locate(kind="text", depth="with_symbol")` → `depth_adjusted == "with_file"`, symbol_info NOT called ✓
- `locate(kind="file", depth="with_symbol")` → same downgrade ✓
- `locate(kind="symbol", depth="with_symbol")` → `depth_adjusted` absent (no downgrade) ✓

### `hotspots` tool

- `hotspot_score` ∈ [0.0, 1.0] cho mọi entry
- `risk_level` consistent với score thresholds (critical ≥ 0.75, high ≥ 0.50, medium ≥ 0.25)
- Git unavailable → `git_available: false`, `hotspot_method: "index_only"`, không crash
- Git unavailable + min_churn=5 → `note` field: "min_churn parameter not applied" ✓
- Git unavailable → `hotspot_score == norm_compl` (không nhân churn) ✓
- Git unavailable → `commit_count in output == 0` ✓
- Git available + min_churn=5 → chỉ files có commit_count >= 5 ✓
- `top_n=5` → at most 5 entries returned
- `total_files_analyzed` == candidate count BEFORE top_n slice (không phải 5) ✓
- `compute_hotspots()` luôn trả tuple `(list, int)`, KHÔNG bao giờ trả bare list kể cả khi
  `candidates` rỗng ✓
- Empty codebase (0 symbols) → `hotspots: []`, không crash
- `include_symbols=true` → `top_symbols` present cho mọi hotspot entry ✓
- mỗi hotspot entry có `language` non-null khi file có symbols ✓
- Symbol với coreness=0 → NOT counted in connected_coreness_count ✓
- Symbol với coreness=1 → counted in connected_coreness_count ✓
- Doc mô tả field nói "coreness > 0", không phải ">= p75" — khớp với SQL:
  `SUM(CASE WHEN coreness > 0 THEN 1 ELSE 0 END)` ✓
- `churn.last_changed` là `null` khi git unavailable ✓; khi git available, parses với `datetime.fromisoformat()` ✓ (format %aI, strict ISO 8601)

### Coverage Reader

- lcov DA:10,3 → line 10 in covered set ✓
- lcov DA:10,0 → line 10 NOT in covered set ✓
- Symbol at lines 10-20, lcov hit at line 15 → `is_covered(path, 10, 20) == True` ✓
- Symbol at lines 10-20, lcov hits at 5 and 25 only → `is_covered(path, 10, 20) == False` ✓
- Corrupt/unreadable coverage file → `CoverageData(source="none")`, no exception ✓
- No coverage file → `CoverageData(source="none")` ✓
- Cobertura XML với 2 `<class>` elements cùng `filename` → `covered_lines` là UNION, không mất line ✓
- `dead_code_confidence` with coverage hit + no callers → `"low"`, `dead_code_source="static+coverage"` ✓
- `dead_code_confidence` with no coverage + no callers + private → `"high"`, `dead_code_source="static"` ✓

### `suggested_next` hints

- `repo_overview` phase=ready, embeddings=ready → `suggested_next.tool == "locate"` ✓
- `repo_overview` phase=parsing → `suggested_next.tool == "indexing_status"` ✓
- `symbol_info` is_hub=true → `suggested_next.tool == "edit_context"` ✓
- `source` is_hub=true in metadata → `suggested_next.tool == "edit_context"` ✓
- `edit_context` → `suggested_next.tool == "diff_impact"` ALWAYS ✓
- `diff_impact` aggregate_risk=low, indexed files all → `suggested_next` absent ✓
- `suggested_next.args` consistent với tool input schema (no unknown keys) ✓
- `locate()` trong `--preset=orient` → `suggested_next.tool` nằm trong `{"repo_overview", "locate", "dependencies", "hotspots", "indexing_status"}` ✓
- `--preset=full` (available_tools=None) → no filtering, all hints allowed ✓
- `search(kind="hybrid")` empty results → `suggested_next.args.kind == "text"` (NOT "semantic") ✓
- `search(kind="text")` empty results → `suggested_next` suggests "hybrid" ✓

### CODEOWNERS Integration

- Pattern `src/auth/` matches `src/auth/login.py` → owners correct ✓
- Pattern `*.ts` matches `src/user.ts` → owners correct ✓
- Pattern `src/*.py` matches `src/user.py` ✓
- Pattern `src/*.py` does NOT match `src/auth/user.py` (nested — `*` không cross `/`) ✓
- Last matching rule wins: `*.py @team-a` then `src/*.py @team-b` → `src/x.py` → `[@team-b]` ✓
- Pattern "Makefile @build-team" matches file "Makefile" ✓
- Pattern "Dockerfile @ops" matches file "Dockerfile" ✓
- Pattern "Procfile @platform" matches file "Procfile" ✓
- Pattern "Jenkinsfile @ci" matches file "Jenkinsfile" ✓
- Pattern "src/" → NOT match file literally named "src" ✓
- Pattern "src" (không trailing slash) → match file literally named "src" (basename match, not directory prefix) ✓
- Pattern "src/" (trailing slash) → match "src/anything" qua explicit directory prefix ✓
- No CODEOWNERS file → fallback to git_blame (or empty if git unavailable) ✓
- `aggregate_risk == "low"` → `suggested_reviewers` absent ✓
- CODEOWNERS present but file not matched → git_blame fallback, source="git_blame" ✓
- `load_codeowners` gọi đúng 1 lần trên startup, không gọi trong diff_impact handler ✓
- Server startup log: "[codeindex] CODEOWNERS loaded: N patterns" ✓
- CODEOWNERS file không tồn tại → codeowners_patterns = [] → no error, git_blame fallback ✓

### Preset System

- `--preset=orient` → only 5 tools registered, `locate` available, `callers` NOT available ✓
- `--preset=full` → all 16 tools registered ✓
- Preset in config.json overridden by CLI flag ✓
- Unknown preset value → `ValueError` at startup with meaningful message ✓
- `locate` trong `--preset=full` → available (full = all 16 tools) ✓

---
