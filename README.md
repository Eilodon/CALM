# Code Intelligence (CI) MCP Server

**Code Intelligence** là một Model Context Protocol (MCP) Server **thuần Rust**, cung cấp năng
lực đọc hiểu codebase siêu tốc cho AI agents. Thay vì grep text mù quáng, `ci` parse codebase
bằng `tree-sitter`, dựng đồ thị call/import edges với **3 mức độ tin cậy** (3-tier resolution),
tính graph metrics (coreness/hubs), và phục vụ qua SQLite FTS5 + (tuỳ chọn) semantic vector search.

> **Kiến trúc**: Pure-Rust. Python oracle đã được gỡ hoàn toàn khỏi runtime (chỉ còn golden
> JSON tĩnh cho parity test). Incremental indexing thời gian thực qua file watcher.

## 🚀 Năng lực lõi

1. **AST indexing** — extract classes/functions/methods/docstrings cho **6 ngôn ngữ tier-0**:
   Python, TypeScript, JavaScript, Java, Rust, Go. Ngôn ngữ khác được bỏ qua an toàn.
2. **Call graph 3-tier** — mỗi edge mang một mức tin cậy:
   - `resolved` — khớp file symbol / import / alias (tier-1, conservative resolver).
   - `inferred` — method call phân giải theo kiểu của receiver (tier-2: `self`/`this` →
     class bao quanh; biến typed → `type_map`).
   - `textual` — chỉ khớp tên (fallback).
3. **Import graph** — `import_edges` (file→module/file) cho tool `dependencies`.
4. **Graph metrics** — `coreness` (k-core, O(V+E)) và `is_hub` để AI biết đâu là lõi hệ thống.
5. **Incremental watcher** — hash-diff chỉ re-parse file đổi; call graph rebuild từ `call_sites`
   đã lưu trong DB (không re-parse file không đổi). Debounce 500ms, lọc bỏ noise (`.codeindex/`).
6. **FTS5 search** — full-text search native qua SQLite triggers, BM25 dual-column.
7. **Semantic search (tuỳ chọn)** — static code embeddings (`model2vec-rs` + `sqlite-vec`),
   fuse với FTS bằng Reciprocal Rank Fusion. Tắt mặc định để giữ binary gọn.
8. **`edges_ready` gating** — tool báo trung thực trạng thái index (`scanning → parsing →
   building_edges → ready`); agent không tin nhầm graph khi chưa build xong.

## 📦 Cấu trúc Crates

- `crates/ci-core/` — Index Engine: tree-sitter parser, SQLite schema, resolver 3-tier,
  graph algorithms, FTS5/semantic search, analysis (hotspot/coverage/codeowners/diff_impact).
- `crates/ci-server/` — MCP server (rmcp/stdio) phơi bày **16 tools** + file watcher.
- `crates/ci-cli/` — CLI: `ci init`, `ci index`, `ci serve`, `ci fitness-check`, `ci doctor`.

## 🛠 Sử dụng

```bash
ci init     --project-root .   # tạo .codeindex/ + config.json
ci index    --project-root .   # one-shot index (Scanning → Parsing → BuildingEdges → Ready)
ci serve    --project-root .   # MCP server qua stdio + watcher (giữ graph đồng bộ)
ci doctor   --project-root .   # kiểm tra config, DB, tree-sitter, git
ci fitness-check --project-root .   # CI gate dựa trên thresholds.toml (hub/dead-code/complexity)
```

## 🧠 Cho AI agents — 16 MCP tools

Workflow chuẩn: `repo_overview` → `locate`/`search` → `callers`/`callees`/`edit_context`
(trượt trên graph) → `source` (đọc code thật). Mọi response kèm `suggested_next` để agent
không phải suy luận bước kế tiếp. Tools: `repo_overview`, `search`, `file_overview`,
`symbol_info`, `source`, `callers`, `callees`, `dependencies`, `path`, `edit_context`,
`session_context`, `diff_impact`, `indexing_status`, `locate`, `hotspots`, `understand`.

## 🔎 Semantic search (opt-in)

Tắt mặc định. Bật bằng Cargo feature `embeddings` (kéo `model2vec-rs` + `sqlite-vec`) và
`semantic_search.enabled = true` trong `.codeindex/config.json`:

```bash
cargo build -p ci-cli --features embeddings
```

Model mặc định `minishlab/potion-code-16M` (256-dim, static code embeddings, pure-Rust, không
ONNX). `search(kind="semantic")` và `kind="hybrid"` (RRF: FTS + vector) sẽ hoạt động; khi tắt,
chúng degrade về FTS.

> Lưu ý: feature `embeddings` kéo thêm dependency (tokenizers/TLS). Binary musl tĩnh phân phối
> ở Phase IV build **không** bật feature này để giữ kích thước tối thiểu.

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
