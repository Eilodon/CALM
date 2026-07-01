# Code Intelligence (CI) MCP Server

**Code Intelligence** là một Model Context Protocol (MCP) Server **thuần Rust**, cung cấp năng
lực đọc hiểu codebase siêu tốc cho AI agents. Thay vì grep text mù quáng, `ci` parse codebase
bằng `tree-sitter`, dựng đồ thị call/import edges với **đa mức độ tin cậy** (multi-tier resolution),
tính graph metrics (coreness/hubs), và phục vụ qua SQLite FTS5 + semantic vector search.

> **Kiến trúc**: Pure-Rust. Python oracle đã được gỡ hoàn toàn khỏi runtime (chỉ còn golden
> JSON tĩnh cho parity test). Incremental indexing thời gian thực qua file watcher.

## 🚀 Năng lực lõi

1. **AST indexing** — extract classes/functions/methods/docstrings cho **6 ngôn ngữ tier-0**:
   Python, TypeScript, JavaScript, Java, Rust, Go. Hỗ trợ quét nông (Tier-0.5) cho 8 ngôn ngữ phụ:
   C, C++, C#, Ruby, PHP, Kotlin, Swift, Shell/Bash — được bật qua Cargo feature flags.
   > **Lưu ý Tier-0.5**: Kotlin và Swift **không có tree-sitter grammar tương thích** với API hiện tại
   > (version conflict); chúng luôn fall back về regex extraction bất kể feature flag nào được bật.
   > Các ngôn ngữ còn lại (C, C++, C#, Ruby, PHP, Shell) sử dụng AST thực khi built với flag tương ứng.

2. **Call graph phân cấp** — mỗi edge mang một mức tin cậy:
   - `resolved` — khớp file symbol / import / alias (tier-1, conservative resolver).
   - `inferred` — method call phân giải theo kiểu của receiver (tier-2: `self`/`this` → class bao quanh; biến typed → `type_map`).
   - `formal` — phân giải phạm vi tĩnh qua `stack-graphs` (tier-3, hiện hỗ trợ Python). Được bảo vệ bởi **hai deadline độc lập**: một cho bước build stack-graph (TSG) và một cho bước path-stitching, cộng thêm cap `MAX_WORK_PER_PHASE = 4096` để chống DoS.
   - `textual` — chỉ khớp tên (fallback).
3. **Import graph** — `import_edges` (file→module/file) cho tool `dependencies`.
4. **Graph metrics** — `coreness` (k-core, O(V+E)) và `is_hub` để AI biết đâu là lõi hệ thống.
5. **Incremental watcher** — hash-diff chỉ re-parse file đổi; call graph rebuild từ `call_sites` đã lưu trong DB. Quá trình parse được song song hoá (`rayon`). Khi `ci serve` khởi động và đã có index cũ, tự động chạy **incremental reindex** thay vì full index — giảm thời gian warm-up. Debounce 500ms, lọc bỏ noise (`.codeindex/`, `target/`, v.v.).
6. **FTS5 search** — full-text search native qua SQLite triggers, BM25 dual-column.
7. **Semantic search — 2 tầng (Bật theo mặc định trong config)** — static code embeddings
   (`model2vec-rs` + `sqlite-vec`), fuse với FTS bằng Reciprocal Rank Fusion (tỉ lệ 1.5x FTS / 1.0x
   mỗi tầng semantic). Tầng 1 embed *symbol identity* (tên + signature + docstring); Tầng 2
   (`indexer::chunker`) embed *code body* thực tế — toàn bộ thân hàm nếu ≤30 dòng, sliding window
   30 dòng/stride 20 nếu dài hơn, cộng với các đoạn code nằm giữa các symbol (module scaffolding,
   field declarations) — nên một query chỉ khớp từ vựng *bên trong* thân hàm (một tên thư viện, một
   biến, một idiom) vẫn tìm ra kết quả dù tên/docstring của symbol không chứa từ đó. `kind=semantic`
   tự fuse 2 tầng nội bộ; `kind=hybrid` fuse cả 3 (FTS + Tầng 1 + Tầng 2) trong một lượt RRF phẳng.
   Được kiểm soát bởi `semantic_search.enabled` trong `config.json` (mặc định `true`). Tự động cấu
   hình trên bản build native.
8. **`edges_ready` gating** — tool báo trung thực trạng thái index (`scanning → parsing →
   building_edges → ready`); agent không tin nhầm graph khi chưa build xong.

## 📦 Cấu trúc Crates

- `crates/ci-core/` — Index Engine: tree-sitter parser, SQLite schema, resolver đa cấp,
  graph algorithms, FTS5/semantic search, analysis (hotspot/coverage/codeowners/diff_impact/dead_code).
- `crates/ci-server/` — MCP server (rmcp/stdio) phơi bày **16 tools** + file watcher.
- `crates/ci-cli/` — CLI: `ci init`, `ci index`, `ci serve`, `ci fitness-check`, `ci doctor`.

## 🛠 Sử dụng

```bash
ci init     --project-root .   # tạo .codeindex/ + config.json
ci index    --project-root .   # one-shot index (Scanning → Parsing → BuildingEdges → Ready)
                               # In ra: "Indexed N files, M symbols." + Tip khi embeddings sẵn có
ci serve    --project-root .   # MCP server qua stdio + incremental reindex on startup + watcher
ci doctor   --project-root .   # kiểm tra config, DB, tree-sitter, git
ci fitness-check --project-root .              # CI gate (text output, exit 1 nếu fail)
ci fitness-check --project-root . --json       # output JSON thay vì text
ci fitness-check --project-root . --config thresholds.toml   # dùng thresholds tùy chỉnh
```

## 🧠 Cho AI agents — 16 MCP tools (Tuân thủ AGENTS.md v2.7.2)

Hỗ trợ **CLI Presets** lọc danh sách tool theo từng phase làm việc: `orient`, `trace`, `edit`, `compound`, `full` (mặc định) thông qua `ci serve --preset`.
Mọi response đều đi kèm `suggested_next` để hướng dẫn agent bước tiếp theo. Các điểm nhấn về tool:

- `edit_context`: **Bắt buộc** gọi trước khi sửa code. Báo cáo chi tiết `blast_radius` (tổng files/callers chịu ảnh hưởng gián tiếp), `edges_ready`, và `index_freshness`.
- `repo_overview`: Trả về `module_map` và `health_summary` gồm `hub_count` (số symbols là hub) và `edges_ready` (graph đã sẵn sàng hay chưa).
- `indexing_status`: Trả về `files_indexed` (đã có trong DB) và `files_total` (file thực tồn tại trên disk theo config.ignore) — so sánh hai giá trị để phát hiện index đang lag.
- **Cấu trúc DB an toàn**: Toàn bộ 16 tools bị cô lập trong kết nối `read-only` (`PRAGMA query_only = ON`), loại bỏ hoàn toàn rủi ro ghi đè cơ sở dữ liệu từ MCP request (Single-writer paradigm). Thuật toán biên (Frontier computation) ở `session_context` cũng đã được thiết kế lại theo chunking để vượt qua giới hạn nội tại 999-biến của SQLite.

## 🔎 Semantic search (Bật mặc định trên Native)

Tính năng `embeddings` (kéo `model2vec-rs` + `sqlite-vec`) được **BẬT MẶC ĐỊNH** trên bản build native thông qua `semantic_search.enabled: true` trong `config.json`. Semantic index được build ở lần chạy `ci index` đầu tiên (hoặc sau background indexing khi `ci serve` khởi động).

```bash
cargo build -p ci-cli # Đã bao gồm embeddings
```

Model mặc định `minishlab/potion-code-16M` (256-dim, static code embeddings, pure-Rust, không
ONNX) — dùng chung cho cả 2 tầng. Tầng 1 (`embedding_vecs`) index tên/signature/docstring; Tầng 2
(`code_chunk_vecs`) index các đoạn code body thực tế (bảng quan hệ `code_chunks` lưu text/dòng, luôn
được tạo; chỉ được embed khi feature `embeddings` bật). `search(kind="semantic")` và `kind="hybrid"`
(RRF: FTS + vector) sẽ hoạt động; khi tắt, chúng degrade về FTS.

> Lưu ý: feature `embeddings` kéo thêm dependency (tokenizers/TLS). Binary musl tĩnh phân phối
> ở Phase IV build **không** bật feature này để giữ kích thước tối thiểu.

## 🏋 Fitness Check — CI Gate

`ci fitness-check` đo 5 metrics và so sánh với ngưỡng trong `thresholds.toml`:

| Metric | Mô tả | Ngưỡng mặc định |
|---|---|---|
| `hub_count` | Số symbols được phân loại là hub | ≤ 50 |
| `avg_coreness` | Coreness trung bình (k-core) của graph | ≤ 15.0 |
| `dead_code_pct` | % symbols có confidence "high" là dead code | ≤ 10% |
| `hotspot_risk` | Hotspot score cao nhất trong codebase | ≤ 0.75 |
| `edge_coverage_pct` | % symbols có ít nhất 1 call edge | ≥ 60% |

## 📦 Phân phối

- `cargo build --release` → binary tĩnh musl (x86_64/aarch64 linux, aarch64 macOS) qua
  `.github/workflows/release.yml`.
- `Containerfile` multi-stage (`rust:alpine` → `scratch`), image ~10.8MB.
- `compose.yaml` mẫu hardened (`read_only`, `cap_drop: ALL`, `no-new-privileges`).

## 🧪 Testing

Property-based + spec-based + parity với golden JSON tĩnh (không cần Python). CI cũng chạy
một job riêng cho feature `embeddings` (vec0 KNN chạy offline).

```bash
cargo test --workspace                       # mặc định
cargo test -p ci-core --features embeddings  # gồm semantic/vector path
```

## 📄 License

MIT
