# Code Intelligence (CI)

**Code Intelligence** là một [Model Context Protocol (MCP)](https://modelcontextprotocol.io) server
viết bằng Rust thuần, giúp AI coding agent (Claude Code, Cursor, v.v.) *hiểu* codebase thay vì chỉ
grep text mù quáng. `ci` parse code bằng `tree-sitter`, dựng call graph + import graph có mức độ tin
cậy rõ ràng, tính graph metrics (hub/coreness) để phát hiện các symbol "lõi" dễ vỡ khi sửa, và cung
cấp full-text + semantic search + khả năng sửa file trực tiếp (hash-verified, risk-gated) — tất cả
phục vụ qua 21 MCP tools, chạy local, không gọi ra ngoài.

## Triết lý

`ci` không chỉ là một MCP server nhiều tool — nó được thiết kế như **bản đồ + trợ lý chủ động** cho
chính agent đang cầm lái, không phải cho người vận hành đứng ngoài nhìn vào. Mọi response đều có
`suggested_next` (chỉ đường từng bước, agent hiếm khi phải tự đoán lộ trình); những chỗ rủi ro thật
sự cao (`edit_context` trước khi sửa, `diff_impact` trước khi commit) được hard-gate chứ không chỉ
khuyến nghị suông; những chỗ còn lại chỉ nudge mềm, không chặn cứng, để agent vẫn giữ quyền tự
quyết khi có lý do chính đáng. `fitness_report`, `session_context`'s `pending_diff_impact`/
`possibly_stuck`, `repo_overview`'s `memory_notes_count` là các tín hiệu chủ động — agent không
cần tự nhớ "mình đã diff_impact chưa" hay tự đếm "mình có đang loanh quanh không", `ci` tự trả lời
trước khi được hỏi. Mục tiêu cuối: giảm tải nhận thức cho agent, để agent dồn sức vào phần việc tạo
giá trị thật, thay vì tự quản lý trạng thái điều hướng.

`ci` không phải công cụ duy nhất trong nhóm "code intelligence cho AI agent" — xem
[`docs/comparison.md`](docs/comparison.md) để biết vị trí của `ci` so với các lựa chọn khác
(Serena, CodeGraph, Sourcegraph/Cody, Cursor, Aider...), và khi nào nên/không nên chọn `ci`.

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

Repo đã có sẵn config cho Claude Code (`.mcp.json`), Cursor (`.cursor/mcp.json`)
và VS Code (`.vscode/mcp.json`) — cả ba đều trỏ vào `scripts/mcp-launcher.sh`,
một launcher dùng chung: tự tìm binary đã build/cache, tải bản prebuilt đã
verify checksum nếu đang ở đúng git tag, hoặc build từ source nếu không có
gì sẵn — clone repo về là chạy được ngay, không cần build tay bước 1 ở trên
trước. Xem [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md) để biết
cách dùng với Windsurf/JetBrains (config toàn cục, không check-in vào repo
được) và chi tiết cách launcher hoạt động.

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
  → edit_symbol("getUserByEmail", expected_hash=..., new_text=...) # sửa trực tiếp, hoặc dùng tool edit khác
      → risk_assessment=high, is_hub=true, không có confirm:true → bị từ chối kèm giải thích
  → edit_symbol(..., confirm=true)  # xác nhận đã review xong, ghi thật + reindex ngay
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
  `repo_overview.core_symbols` dùng lại chính `coreness` này để vẽ "khung xương kiến trúc" ngay từ
  câu gọi đầu tiên (lấy cảm hứng từ repo-map PageRank của Aider, nhưng tận dụng metric đã có sẵn
  thay vì tính riêng) — loại bỏ symbol trong test file, rỗng cho tới khi `edges_ready`.
- **Incremental watcher** — chỉ re-parse file thay đổi (FNV-1a hash-diff), rebuild call graph tăng
  dần; parallel hoá bằng `rayon`. `ci serve` tự động chọn incremental reindex khi đã có index cũ.
- **Full-text + semantic search** — FTS5 (BM25) kết hợp semantic embeddings (`model2vec-rs`,
  pure-Rust, không cần ONNX) qua Reciprocal Rank Fusion 3-way (FTS + symbol-identity vector +
  code-body chunk vector) — tìm được cả khi câu query không trùng tên symbol. KNN là brute-force
  cosine scan thuần Rust (cache theo path DB trong RAM, không phải re-fetch SQL mỗi query) — không
  còn phụ thuộc extension C nào, nên hoạt động giống hệt trên mọi platform release (trước đây
  `sqlite-vec` không compile được trên musl libc, khiến bản Linux/Docker bị tắt semantic). Model mặc
  định (`minishlab/potion-code-16M`, MIT license) được vendor sẵn vào binary lúc compile
  (`crates/ci-core/assets/potion-code-16m/`, qua Git LFS) — load model mặc định không cần mạng, chỉ
  model tuỳ biến qua `semantic_search.model` mới tải từ HuggingFace Hub.
- **Grep/glob thật, quét trực tiếp trên đĩa** — `search(kind="grep")` dùng regex thật (crate `regex`)
  + glob filter (`globset`) qua walker tôn trọng `.gitignore`/`.git/info/exclude` thật (crate
  `ignore`), không qua FTS/DB nên phủ được cả file indexer không parse (`Cargo.toml`, `docs/*.md`).
  Mỗi match được enrich thêm symbol bao quanh (nếu có) qua join ngược vào graph. `search(kind="file")`
  cũng nhận glob pattern thật (`*.rs`, `src/**`), không chỉ substring như trước.
- **Sửa file trực tiếp — `edit_lines`/`edit_symbol`** — line-range write tool duy nhất của `ci`, hoạt
  động trên mọi file track được (không chỉ symbol đã parse). Conflict guard bằng hash nội dung
  (FNV-1a) theo đúng range — sai hash bị từ chối và trả lại hash/nội dung hiện tại để đọc lại; nhiều
  hunk trong 1 lệnh áp dụng bottom-up để không lệch offset giữa các hunk. Validate cú pháp bằng
  tree-sitter **trước khi ghi đĩa** (từ chối nếu phát sinh lỗi cú pháp, không ghi gì). Sửa 1 symbol
  `is_hub=true` hoặc >10 caller bị từ chối trừ khi có `confirm:true` — chính sách chỉ tool có call
  graph mới làm được. Ghi file atomic (temp + fsync + rename) và reindex đồng bộ ngay sau khi ghi
  (không đợi file watcher), response trả kèm risk/callers hậu-sửa như 1 `diff_impact` thu nhỏ.
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
  trong 1 session, mất khi server restart). `repo_overview.memory_notes_count` báo số lượng ghi chú
  đang có (chỉ đếm, không kèm nội dung) — agent tự quyết định có đáng `recall()` hay không, thay vì
  bị bơm nội dung note vào response một cách bị động.
- **Git co-change mining** — `edit_context` mine `git log` để tìm file hay đổi cùng lúc với file
  đang sửa dù không có quan hệ import/call nào (VD model + migration) — tín hiệu coupling logic mà
  call graph tĩnh không thấy được.
- **Session progress signal** — `session_context.possibly_stuck`/`calls_since_progress` báo agent
  đang loanh quanh (10+ tool call không có file/symbol mới) — chỉ mang tính thông tin, không chặn;
  quyết định dừng vòng lặp vẫn thuộc về host (VD Claude Code's `/goal`).
- **Build freshness minh bạch** — `ci doctor` in git commit đã build binary (`ci_core::BUILD_INFO`)
  và so với `HEAD` hiện tại của repo, cảnh báo rõ nếu lệch; `scripts/mcp-launcher.sh` tự kiểm tra
  mtime của mọi source file trước khi tin một `target/{debug,release}/ci` có sẵn, rebuild nếu cũ hơn
  thay vì âm thầm chạy binary lỗi thời.
- **MCP Prompts** — 3 prompt đóng gói sẵn workflow lặp lại nhiều (`review_symbol`, `debug_symbol`,
  `onboard_area`), MCP client như Claude Code hiện chúng dưới dạng slash-command
  (`/mcp__ci__review_symbol`). Lưu ý: prompt chỉ trả về 1 message hướng dẫn sẵn, không tự chạy tool
  — agent vẫn tự gọi từng bước, khác `suggested_next` (gợi ý per-response) ở chỗ đóng gói cả workflow
  thành 1 lệnh gọi trước khi agent bắt đầu.

## Cấu trúc Crates

- `crates/ci-core/` — Index Engine: tree-sitter parser, SQLite schema, resolver đa cấp (conservative
  → inferred → formal/StackGraph), graph algorithms (coreness, hub), FTS5/semantic search (2-layer:
  symbol identity + code-body chunks), analysis (hotspot/coverage/codeowners/diff_impact/dead_code),
  fitness metrics, gitignore management.
- `crates/ci-server/` — MCP server (rmcp/stdio) phơi bày 21 tools + incremental file watcher.
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

## 21 MCP Tools cho AI agents

Hỗ trợ CLI presets lọc tool theo phase làm việc: `orient`, `trace`, `edit`, `compound`, `full`
(mặc định) qua `ci serve --preset` hoặc field `preset` trong `config.json`. Mọi response đều kèm
`suggested_next` để hướng dẫn bước tiếp theo — xem chi tiết từng tool và workflow đầy đủ trong
[AGENTS.md](AGENTS.md).

| Nhóm | Tools |
|---|---|
| Orient | `repo_overview`, `hotspots`, `fitness_report` (health snapshot — cùng metrics với `ci fitness-check`, hỏi được giữa phiên), `indexing_status` |
| Locate | `locate`, `search`, `file_overview` |
| Inspect | `source`, `symbol_info`, `understand` |
| Trace | `callers`, `callees`, `path`, `dependencies` |
| Edit | `edit_context` (bắt buộc trước khi sửa), `edit_lines`/`edit_symbol` (write tool duy nhất — hash-verified, risk-gated), `diff_impact` (bắt buộc trước khi commit) — 2 mục đầu/cuối hook-enforced dưới Claude Code, xem `.claude/hooks/ci-nudge.sh`; `session_context`'s `pending_diff_impact` là tín hiệu tương đương, hoạt động ở mọi MCP client chứ không riêng Claude Code |
| Recover | `session_context`, `remember`, `recall` |

### MCP Prompts — workflow đóng gói thành slash-command

Khác primitive `tools` ở trên — MCP Prompts (`prompts/list`, `prompts/get`) trả về 1 message hướng
dẫn sẵn cho workflow lặp lại nhiều, MCP client hiện chúng dưới dạng slash-command:

| Prompt | Argument | Workflow đóng gói |
|---|---|---|
| `review_symbol` | `symbol` | `locate` → `source` → `edit_context` (bắt buộc) → tóm tắt risk trước khi sửa |
| `debug_symbol` | `symbol` | `understand` → `callers(max_depth=3)` → kiểm tra `test_files`/`dead_code_confidence` |
| `onboard_area` | `path` | `repo_overview` → `file_overview`/`dependencies` → `hotspots` khoanh vùng path đó |

## Fitness Check — CI Gate

`ci fitness-check` đo 9 metrics và so sánh với ngưỡng trong `thresholds.toml`:

| Metric | Mô tả | Ngưỡng mặc định |
|---|---|---|
| `hub_count` | Số symbols được phân loại là hub | ≤ 1000 |
| `hub_pct` | % symbols là hub trên tổng symbol (scale-invariant) | ≤ 20.0% |
| `avg_coreness` | Coreness trung bình (k-core) của graph | ≤ 15.0 |
| `dead_code_pct` | % symbols có confidence "high" là dead code | ≤ 10% |
| `hotspot_risk` | Hotspot score cao nhất trong codebase | ≤ 0.75 |
| `edge_coverage_pct` | % symbols có ít nhất 1 call edge | ≥ 60% |
| `high_complexity_pct` | % function/method có cyclomatic complexity > 10 (McCabe, đếm branch qua AST — chỉ 6 ngôn ngữ Tier-0 có parse tree thật; Tier-0.5 luôn báo complexity=1) | ≤ 15.0% |
| `boundary_violations` | Số `import_edges` phạm luật kiến trúc khai báo trong `[[boundaries]]` | ≤ 0 |
| `config_drift_count` | Số reference file-path trong doc (khai báo qua `[config_drift].doc_paths`) không trỏ tới file thật nào | ≤ 0 |

Mỗi lần chạy `ci fitness-check` còn snapshot metrics vào DB để `edit_context` có thể hiển thị
trend (delta so với ngày trước).

### Architecture boundaries — `[[boundaries]]`

Khai báo luật "module A không được import module B" ngay trong `thresholds.toml` (cùng file với
`[thresholds]`), match theo path-prefix (không phải glob/regex):

```toml
[[boundaries]]
from = "crates/ci-core/"
to = "crates/ci-server/"
reason = "core không được phụ thuộc server layer"
```

`ci fitness-check` báo từng vi phạm cụ thể (from/to path thật + rule + reason) khi chạy không kèm
`--json`; mặc định `max_boundary_violations = 0` — khai báo luật nào là luật đó phải giữ đúng.

## Deployment

- `cargo build --release` → binary tĩnh musl qua `.github/workflows/release.yml`, matrix:
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (kèm `SHA256SUMS`),
  `aarch64-apple-darwin`. `scripts/mcp-launcher.sh` tự tải + verify checksum bản
  đúng platform khi checkout đang ở đúng git tag — xem
  [`docs/mcp-client-setup.md`](docs/mcp-client-setup.md).
- `Containerfile` multi-stage (`rust:alpine` → `scratch`) — single static binary,
  không cần runtime image nào khác, publish lên `ghcr.io/eilodon/code-intelligence`
  (tag theo version + `latest`) mỗi khi push git tag.
- `compose.yaml` mẫu hardened (`read_only`, `cap_drop: ALL`, `no-new-privileges`, `pids_limit: 64`,
  `mem_limit: 256m`).
- Repo dùng Git LFS cho `crates/ci-core/assets/potion-code-16m/*.safetensors` (~61MB) — cần
  `git lfs install && git lfs pull` để lấy đúng weight file. Không có LFS, `git clone`/`cargo build`
  vẫn chạy và **compile thành công** (`include_bytes!` chỉ nhúng byte thô, không parse) — nhưng file
  đó chỉ là pointer text (~130 byte) thay vì model thật, nên lúc **runtime** việc load model sẽ fail
  ("failed to parse safetensors"), `indexing_status` báo `embeddings_status: "failed"`, và
  `search(kind="semantic"/"hybrid")` tự động degrade về FTS-only — không crash, nhưng semantic search
  không hoạt động cho tới khi chạy `git lfs pull` rồi rebuild.

## Testing

```bash
cargo test --workspace                        # unit + integration (mặc định)
cargo test -p ci-core --features embeddings   # bao gồm semantic/vector path (brute-force cosine KNN)
cargo test --test parity_test test_formal_edges   # Stack Graphs regression corpus
```

Ba CI jobs chạy trên mọi PR: `verify` (fmt/clippy/test/audit), `stack-graphs-corpus`
(formal resolver parity), `embeddings` (clippy + test với feature `embeddings`).

## Tài liệu kỹ thuật sâu

Chi tiết resolver internals, ADR, migration plans nằm trong [`docs/`](docs/).

## License

[MIT](LICENSE)
