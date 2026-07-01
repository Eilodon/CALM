# Code Intelligence (CI)

**Code Intelligence** là một [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server
viết bằng Rust thuần, giúp AI coding agent (Claude Code, Cursor, v.v.) *hiểu* codebase thay vì chỉ
grep text mù quáng. `ci` parse code bằng `tree-sitter`, dựng call graph + import graph có mức độ tin
cậy rõ ràng, tính graph metrics (hub/coreness) để phát hiện các symbol "lõi" dễ vỡ khi sửa, và cung
cấp full-text + semantic search — tất cả phục vụ qua 16 MCP tools, chạy local, không gọi ra ngoài.

## Vì sao cần cái này?

Khi một AI agent sửa code mà không biết ai đang gọi hàm nó sắp đổi, nó dễ:
- Xoá "dead code" mà thực ra vẫn có người dùng.
- Đổi signature mà bỏ sót vài chục call site.
- Refactor một symbol tưởng nhỏ nhưng hoá ra là hub trung tâm của cả module.

`ci` trả lời trực tiếp các câu hỏi đó trước khi agent đụng vào code: "ai gọi hàm này?", "sửa hàm
này ảnh hưởng bao nhiêu file?", "hàm này có phải hub không?" — thay vì để agent tự đoán qua grep.

## Quick Start

```bash
# 1. Build binary
cargo build --release -p ci-cli

# 2. Khởi tạo index cho project
ci init  --project-root .
ci index --project-root .

# 3. Chạy MCP server (stdio)
ci serve --project-root .
```

Tích hợp vào MCP client (ví dụ Claude Code) qua `.mcp.json`:

```json
{
  "mcpServers": {
    "ci": {
      "type": "stdio",
      "command": "ci",
      "args": ["serve", "--project-root", "."]
    }
  }
}
```

## Ví dụ sử dụng (agent workflow)

```
agent: repo_overview()
  → 41 files, 710 symbols, 101 hub symbols, indexing_phase=ready

agent: "tôi cần sửa hàm getUserByEmail"
  → locate("getUserByEmail")       # tìm file + symbol metadata
  → source("getUserByEmail")       # đọc đúng thân hàm, không flood context cả file
  → edit_context("getUserByEmail") # BẮT BUỘC trước khi sửa
      → 12 callers, risk_assessment=high → agent review từng caller trước khi đổi signature
  → (sửa code)
  → diff_impact(staged=true)       # xác nhận blast radius trước khi commit
```

## Tính năng chính

- **AST indexing** — 6 ngôn ngữ tier-0 (Python, TypeScript, JavaScript, Java, Rust, Go) với AST đầy
  đủ; 8 ngôn ngữ tier-0.5 (C, C++, C#, Ruby, PHP, Kotlin, Swift, Shell) quét nông qua feature flags.
- **Call graph có độ tin cậy** — mỗi edge được gắn nhãn `resolved` / `inferred` / `formal` /
  `textual` tuỳ vào mức độ chắc chắn khi resolve, giúp agent biết khi nào nên tin và khi nào nên
  double-check thủ công.
- **Import graph** — file-level dependency graph cho tool `dependencies`.
- **Graph metrics** — `coreness` (k-core) và `is_hub` để nhận diện symbol trung tâm trước khi sửa.
- **Incremental watcher** — chỉ re-parse file thay đổi (hash-diff), rebuild call graph tăng dần;
  song song hoá bằng `rayon`. `ci serve` tự động incremental reindex khi có index cũ.
- **Full-text + semantic search** — FTS5 (BM25) kết hợp semantic embeddings (`model2vec-rs`,
  pure-Rust, không cần ONNX) qua Reciprocal Rank Fusion — tìm được cả khi câu query không trùng tên
  symbol, chỉ trùng ý nghĩa/idiom trong thân hàm.
- **Index freshness minh bạch** — mọi response đều báo trạng thái index (`scanning → parsing →
  building_edges → ready`) để agent không tin nhầm dữ liệu cũ.

## Cấu trúc Crates

- `crates/ci-core/` — Index Engine: tree-sitter parser, SQLite schema, resolver đa cấp, graph
  algorithms, FTS5/semantic search, analysis (hotspot/coverage/codeowners/diff_impact/dead_code).
- `crates/ci-server/` — MCP server (rmcp/stdio) phơi bày 16 tools + file watcher.
- `crates/ci-cli/` — CLI: `ci init`, `ci index`, `ci serve`, `ci fitness-check`, `ci doctor`.

## CLI Reference

```bash
ci init     --project-root .   # tạo .codeindex/ + config.json
ci index    --project-root .   # one-shot index (Scanning → Parsing → BuildingEdges → Ready)
ci serve    --project-root .   # MCP server qua stdio + incremental reindex + watcher
ci serve    --project-root /project --db-path /data/index.db   # tách DB khỏi project root (container)
ci doctor   --project-root .   # kiểm tra config, DB, tree-sitter, git
ci fitness-check --project-root .                            # CI gate, exit 1 nếu fail
ci fitness-check --project-root . --json                     # output JSON
ci fitness-check --project-root . --config thresholds.toml   # thresholds tùy chỉnh
```

## 16 MCP Tools cho AI agents

Hỗ trợ CLI presets lọc tool theo phase làm việc: `orient`, `trace`, `edit`, `compound`, `full`
(mặc định) qua `ci serve --preset`. Mọi response đều kèm `suggested_next` để hướng dẫn bước tiếp
theo — xem chi tiết từng tool và workflow đầy đủ trong [AGENTS.md](AGENTS.md).

| Nhóm | Tools |
|---|---|
| Orient | `repo_overview`, `hotspots`, `indexing_status` |
| Locate | `locate`, `search`, `file_overview` |
| Inspect | `source`, `symbol_info`, `understand` |
| Trace | `callers`, `callees`, `path`, `dependencies` |
| Edit | `edit_context` (bắt buộc trước khi sửa), `diff_impact` (bắt buộc trước khi commit) |
| Recover | `session_context` |

## Fitness Check — CI Gate

`ci fitness-check` đo 6 metrics và so sánh với ngưỡng trong `thresholds.toml`:

| Metric | Mô tả | Ngưỡng mặc định |
|---|---|---|
| `hub_count` | Số symbols được phân loại là hub | ≤ 50 |
| `hub_pct` | % symbols là hub trên tổng symbol (scale-invariant) | ≤ 20.0% |
| `avg_coreness` | Coreness trung bình (k-core) của graph | ≤ 15.0 |
| `dead_code_pct` | % symbols có confidence "high" là dead code | ≤ 10% |
| `hotspot_risk` | Hotspot score cao nhất trong codebase | ≤ 0.75 |
| `edge_coverage_pct` | % symbols có ít nhất 1 call edge | ≥ 60% |

## Deployment

- `cargo build --release` → binary tĩnh musl (x86_64/aarch64 linux, aarch64 macOS) qua
  `.github/workflows/release.yml`.
- `Containerfile` multi-stage (`rust:alpine` → `scratch`), image ~10.8MB.
- `compose.yaml` mẫu hardened (`read_only`, `cap_drop: ALL`, `no-new-privileges`).

## Testing

```bash
cargo test --workspace                        # mặc định
cargo test -p ci-core --features embeddings   # gồm semantic/vector path
```

Property-based + spec-based + parity với golden JSON tĩnh (không cần Python runtime).

## Tài liệu kỹ thuật sâu

Chi tiết resolver internals, ADR, migration plans nằm trong [`docs/`](docs/).

## License

MIT
