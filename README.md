# Code Intelligence (CI)

**Code Intelligence** là một [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server
viết bằng Rust thuần, giúp AI coding agent (Claude Code, Cursor, v.v.) *hiểu* codebase thay vì chỉ
grep text mù quáng. `ci` parse code bằng `tree-sitter`, dựng call graph + import graph có mức độ tin
cậy rõ ràng, tính graph metrics (hub/coreness) để phát hiện các symbol "lõi" dễ vỡ khi sửa, và cung
cấp full-text + semantic search — tất cả phục vụ qua 18 MCP tools, chạy local, không gọi ra ngoài.

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

# 2. Khởi tạo config cho project
ci init  --project-root .

# 3. Build index (bao gồm cả semantic embeddings nếu enabled trong config.json)
ci index --project-root .

# 4. Chạy MCP server (stdio) — tự incremental reindex nếu đã có index
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

> **Lưu ý**: `ci serve` tự động thêm `.codeindex/` vào `.gitignore` khi khởi động để tránh
> commit DB vào repo.

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

- **AST indexing — 6 ngôn ngữ Tier-0** (Python, TypeScript, JavaScript, Java, Rust, Go): parse đầy
  đủ bằng tree-sitter, dựng call graph + import graph, áp resolver đa cấp.
- **Shallow indexing — 8 ngôn ngữ Tier-0.5** (C, C++, C#, Ruby, PHP, Kotlin, Swift, Shell): trích
  xuất symbol bằng line-scan regex; không có call-graph hay import resolution — built-in, không cần
  feature flag.
- **Call graph có độ tin cậy** — mỗi edge được gắn nhãn `resolved` / `inferred` / `formal` /
  `textual` tuỳ vào mức độ chắc chắn khi resolve. `formal` (Tier-3, StackGraph) hiện hỗ trợ Python.
- **Import graph** — file-level dependency graph cho tool `dependencies`.
- **Graph metrics** — `coreness` (k-core) và `is_hub` để nhận diện symbol trung tâm trước khi sửa.
- **Incremental watcher** — chỉ re-parse file thay đổi (FNV-1a hash-diff), rebuild call graph tăng
  dần; parallel hoá bằng `rayon`. `ci serve` tự động chọn incremental reindex khi đã có index cũ.
- **Full-text + semantic search** — FTS5 (BM25) kết hợp semantic embeddings (`model2vec-rs`,
  pure-Rust, không cần ONNX) qua Reciprocal Rank Fusion 3-way (FTS + symbol-identity vector +
  code-body chunk vector) — tìm được cả khi câu query không trùng tên symbol.
- **Index freshness minh bạch** — mọi response đều báo trạng thái index (`scanning → parsing →
  building_edges → ready`) để agent không tin nhầm dữ liệu cũ.
- **Coverage-aware dead code** — tự detect lcov/`.coverage`/Go `coverage.out`/Cobertura XML khi
  khởi động; kết hợp với static analysis cho `dead_code_confidence`.
- **Output sanitization** — `source`/`understand` redact credentials (PEM key, GitHub/AWS/Slack
  token, JWT, password assignment...) trước khi trả về, và gắn cờ `content_warning` khi code chứa
  văn bản giống prompt-injection (`"ignore previous instructions"`, fake `system:` marker...) —
  không sửa nội dung code, chỉ cảnh báo, vì false positive ở đây sẽ làm hỏng code thật.
- **Mandatory tools thật sự bắt buộc khi dùng Claude Code** — `.claude/hooks/ci-nudge.sh` (PreToolUse
  hook, không phải chỉ quy ước trong docs) chặn cứng: `Edit` đầu tiên lên file code mỗi session bị từ
  chối tới khi gọi `edit_context`; `git commit`/`git push` bị từ chối nếu có file đổi từ lần gọi
  `diff_impact` gần nhất.
- **Noise-penalty ranking** — `search`/`locate` hạ điểm (×0.6) kết quả nằm trong file test/generated/
  example khi có kết quả tương đương ở code thật, để implementation thật lên trước thay vì bị chôn
  dưới test file trùng tên.
- **Memory tool (`remember`/`recall`)** — ghi chú diễn giải bền vững (quyết định kiến trúc, gotcha đã
  gặp) theo topic, sống qua nhiều session/restart — khác `session_context` (chỉ track điều hướng
  trong 1 session, mất khi server restart).

## Cấu trúc Crates

- `crates/ci-core/` — Index Engine: tree-sitter parser, SQLite schema, resolver đa cấp (conservative
  → inferred → formal/StackGraph), graph algorithms (coreness, hub), FTS5/semantic search (2-layer:
  symbol identity + code-body chunks), analysis (hotspot/coverage/codeowners/diff_impact/dead_code),
  fitness metrics, gitignore management.
- `crates/ci-server/` — MCP server (rmcp/stdio) phơi bày 18 tools + incremental file watcher.
- `crates/ci-cli/` — CLI: `ci init`, `ci index`, `ci serve`, `ci fitness-check`, `ci doctor`.

## CLI Reference

```bash
ci init     --project-root .    # tạo .codeindex/config.json với defaults
ci index    --project-root .    # one-shot full index (Scanning → Parsing → BuildingEdges → Ready)
                                # tự embed symbols+chunks nếu semantic_search.enabled=true
ci serve    --project-root .    # MCP server qua stdio + incremental reindex + file watcher
ci serve    --project-root /project --db-path /data/index.db   # tách DB (container deployment)
ci serve    --project-root . --preset orient   # chỉ đăng ký tools của phase orient
ci doctor   --project-root .    # kiểm tra config, DB (symbols/files/metrics history), git
ci fitness-check --project-root .                             # CI gate, exit 1 nếu fail
ci fitness-check --project-root . --json                      # output JSON
ci fitness-check --project-root . --config thresholds.toml    # thresholds tùy chỉnh
```

## 18 MCP Tools cho AI agents

Hỗ trợ CLI presets lọc tool theo phase làm việc: `orient`, `trace`, `edit`, `compound`, `full`
(mặc định) qua `ci serve --preset` hoặc field `preset` trong `config.json`. Mọi response đều kèm
`suggested_next` để hướng dẫn bước tiếp theo — xem chi tiết từng tool và workflow đầy đủ trong
[AGENTS.md](AGENTS.md).

| Nhóm | Tools |
|---|---|
| Orient | `repo_overview`, `hotspots`, `indexing_status` |
| Locate | `locate`, `search`, `file_overview` |
| Inspect | `source`, `symbol_info`, `understand` |
| Trace | `callers`, `callees`, `path`, `dependencies` |
| Edit | `edit_context` (bắt buộc trước khi sửa), `diff_impact` (bắt buộc trước khi commit) — hook-enforced dưới Claude Code, xem `.claude/hooks/ci-nudge.sh` |
| Recover | `session_context`, `remember`, `recall` |

## Fitness Check — CI Gate

`ci fitness-check` đo 7 metrics và so sánh với ngưỡng trong `thresholds.toml`:

| Metric | Mô tả | Ngưỡng mặc định |
|---|---|---|
| `hub_count` | Số symbols được phân loại là hub | ≤ 50 |
| `hub_pct` | % symbols là hub trên tổng symbol (scale-invariant) | ≤ 20.0% |
| `avg_coreness` | Coreness trung bình (k-core) của graph | ≤ 15.0 |
| `dead_code_pct` | % symbols có confidence "high" là dead code | ≤ 10% |
| `hotspot_risk` | Hotspot score cao nhất trong codebase | ≤ 0.75 |
| `edge_coverage_pct` | % symbols có ít nhất 1 call edge | ≥ 60% |
| `high_complexity_pct` | % function/method có cyclomatic complexity > 10 (McCabe, đếm branch qua AST — chỉ 6 ngôn ngữ Tier-0 có parse tree thật; Tier-0.5 luôn báo complexity=1) | ≤ 15.0% |

Mỗi lần chạy `ci fitness-check` còn snapshot metrics vào DB để `edit_context` có thể hiển thị
trend (delta so với ngày trước).

## Deployment

- `cargo build --release` → binary tĩnh musl qua `.github/workflows/release.yml`, matrix:
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`.
- `Containerfile` multi-stage (`rust:alpine` → `scratch`), image ~10MB.
- `compose.yaml` mẫu hardened (`read_only`, `cap_drop: ALL`, `no-new-privileges`, `pids_limit: 64`,
  `mem_limit: 256m`).

## Testing

```bash
cargo test --workspace                        # unit + integration (mặc định)
cargo test -p ci-core --features embeddings   # bao gồm semantic/vector path (vec0 KNN)
cargo test --test parity_test test_formal_edges   # Stack Graphs regression corpus
```

Ba CI jobs chạy trên mọi PR: `verify` (fmt/clippy/test/audit), `stack-graphs-corpus`
(formal resolver parity), `embeddings` (clippy + test với feature `embeddings`).

## Tài liệu kỹ thuật sâu

Chi tiết resolver internals, ADR, migration plans nằm trong [`docs/`](docs/).

## License

MIT
