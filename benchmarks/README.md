# Benchmarks

Đo lợi thế thật của `calm` MCP server so với workflow naive (`cat`/`grep`) hoặc so với các
competitor cùng nhóm (CodeGraph, Serena, Semble, Semgrep, GitNexus). Mỗi benchmark có anchor code
thực tế, phương pháp đo cụ thể, và baseline so sánh — không dùng số liệu ước lượng khi có thể đo
thật; khi số đo ra ngoài kỳ vọng (vd B6 `find_callers` = 0%), báo cáo trung thực thay vì ẩn đi.

Mỗi claim đã đo (kể cả claim đã bị supersede) có một dòng structured, máy-đọc-được trong
[`claims.registry.jsonl`](claims.registry.jsonl) -- commit/corpus/harness cụ thể, kết quả canonical,
và chuỗi supersede tường minh (`scripts/check-claims-registry.sh` giữ nó nhất quán trong CI). Prose
bên dưới và trong từng benchmark README vẫn là nơi đọc full context; registry là index tra cứu
nhanh, không thay thế nó.

| # | Chiều | Mục tiêu chứng minh | Trạng thái |
|---|---|---|---|
| B1 | AST Indexing Accuracy | Tree-sitter vs regex | Planned |
| B2 | Call Graph Resolution Quality | Tier-1/2/3 vs textual (scope: Rust, SCIP oracle) | **Implemented** — [`b2_call_graph_quality/`](b2_call_graph_quality/) |
| B3 | Search Quality | Hybrid RRF vs FTS-only vs raw grep (NDCG@10) | **Implemented** — [`b3_search_quality/`](b3_search_quality/) |
| B4 | Token Efficiency | MCP tools vs `cat`/`grep` naive workflow | **Implemented** — [`b4_token_efficiency/`](b4_token_efficiency/) |
| B5 | Incremental Indexing Speed | Reindex chỉ file thay đổi | Planned |
| B6 | Tool-Call Efficiency | Số round-trip naive vs 1 MCP call (ý tưởng từ CodeGraph) | **Implemented** — [`b6_tool_call_efficiency/`](b6_tool_call_efficiency/) |
| B7 | Task Correctness / Regression | Refactor thật (rename) trên corpus thật per-language (fd/Rust, flask/Python, express/JS, zod/TS, gin/Go, spring-petclinic/Java), oracle xác định (build/test pass + reference recall độc lập, không LLM-judge), đếm reference bị bỏ sót (ý tưởng từ Serena) | **Implemented (Phase 1-3, all 6 languages)** — [`b7_task_correctness/`](b7_task_correctness/) |
| B8 | Model-Tier Leveling | Model rẻ + calm tools vs model đắt không có tools, cùng task (ý tưởng từ GitNexus) | Planned |
| B9 | Scaling Curve | Lợi thế `calm` co giãn theo quy mô repo (nhỏ → lớn) | Planned |
| B10 | Real Competitor A/B | `calm` vs CodeGraph vs Semble — tool call thật trên cả 3 MCP server thật (không phải số tự báo cáo) | **Superseded by B11** — [`b10_real_competitor_ab/`](b10_real_competitor_ab/) (giữ lại, xem B11 cho methodology đã fix) |
| B11 | Extended Real Competitor A/B | `calm` vs CodeGraph vs Semble vs grepai vs Serena — sửa các lỗ hổng methodology của B10 (oracle đúng-sai cho mọi task, N=5 thay vì N=1, thêm task risk_gate_refusal + memory_recall test thật tính năng khác biệt của `calm`) | **Implemented** — [`b11_extended_competitor_ab/`](b11_extended_competitor_ab/) |
| B12 | Tier-1/Tier-2 Tool-Surface Correctness | Lái thật 9 MCP tool (repo_overview/search/source/file_overview/callers/edit_context/edit_lines/edit_symbol/diff_impact/hotspots) qua JSON-RPC trên 6 repo OSS ngoài (Tier-0: Python/Rust/Go/JS/TS/Java), full-power build, ground truth độc lập (regex + git grep, không phải call-graph precision) — khác trục đo với B2/resolution | **Implemented** — [`b12_tier1_tier2_tool_correctness/`](b12_tier1_tier2_tool_correctness/) |

Ngoài chuỗi B1-B11 (đo lợi thế `calm` so với naive/competitor), còn một track riêng đo **chất lượng
resolution đa ngôn ngữ** cho kế hoạch 8-ngôn-ngữ Formal-tier
(`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`) — không thuộc số B, vì trục đo khác
hẳn (độ rộng/độ chính xác hỗ trợ ngôn ngữ, không phải calm-vs-naive): **Resolution** — tier
distribution (formal/resolved/inferred/textual/ambiguous) trên 8 repo OSS thật (không oracle, khác
B2) — [`resolution/`](resolution/). Đây chính là corpus "Phase 2" mà B2's Giới hạn từng nhắc tới.

Nguồn cảm hứng B6-B9: xem phần "Nghiên cứu competitor" bên dưới. Khác với B6 (dùng ý tưởng đo của
CodeGraph nhưng chỉ chạy `calm`), B10/B11 cài thật các competitor và gọi tool thật của từng cái — xem
B11's README (methodology hiện hành) cho lý do vì sao ratio thô không nên đọc như bảng xếp hạng, và
cho audit trail giải thích vì sao B10 bị thay thế thay vì chỉ sửa tại chỗ.

## Hạ tầng dùng chung — `lib/`

`mcp_client.py` (MCP stdio client cho `calm`), `generic_mcp_client.py` (client tổng quát cho MCP
server bất kỳ — CodeGraph, Semble, grepai, Serena, dùng ở B10/B11), `tasks.yaml` (task definitions
cho B4/B6/B10/B11), `competitor_tasks.yaml` (mapping cùng task id đó sang tool call của
CodeGraph/Semble/grepai/Serena, dùng ở B10/B11), `naive_workflow.py` (mô phỏng naive cat/grep + đếm
call, cộng `naive_grep_ranked_files` cho baseline ranking của B3) nằm ở `benchmarks/lib/`, dùng
chung — không định nghĩa lại task hay logic mô phỏng ở mỗi benchmark.

## Chạy benchmark

```bash
python3 -m venv benchmarks/.venv
benchmarks/.venv/bin/pip install -r benchmarks/requirements.txt
cargo build --release -p calm-cli   # cần build sẵn trước khi chạy bất kỳ benchmark nào
cargo build --release -p calm-cli --features scip-overlay  # cần cho B2 (scip-dump subcommand)

benchmarks/.venv/bin/python benchmarks/b2_call_graph_quality/run_benchmark.py
benchmarks/.venv/bin/python benchmarks/b3_search_quality/run_benchmark.py
benchmarks/.venv/bin/python benchmarks/b4_token_efficiency/run_benchmark.py
benchmarks/.venv/bin/python benchmarks/b6_tool_call_efficiency/run_benchmark.py

# B11 (methodology hiện hành — xem b11_extended_competitor_ab/README.md cho setup đầy đủ
# grepai/Serena/CodeGraph và LÝ DO chạy trên isolated worktree, không phải live repo):
benchmarks/.venv/bin/python benchmarks/b11_extended_competitor_ab/run_benchmark.py --corpus <isolated-worktree-path>
```

`benchmarks/.venv/` và `results.json` không commit (xem `.gitignore`) — kết quả phụ thuộc vào
trạng thái index tại thời điểm chạy, chạy lại để lấy số mới nhất.

## Nghiên cứu competitor

- **CodeGraph** (colbymchenry) — A/B thật với Claude Code (N=4 run/repo, median), đo cost/tokens/
  wall-clock/tool-calls; thừa nhận lợi thế co hẹp trên repo nhỏ → nguồn gốc B6 và B9 (scaling curve,
  chưa triển khai).
- **Serena** (oraios) — không đo token mà đo giảm lỗi khi thao tác đa file (rename/refactor 8-12
  bước thủ công dễ sai → 1 call) → nguồn gốc B7.
- **GitNexus** — nhấn mạnh model yếu vẫn dùng được nhờ tool đã tiền xử lý cấu trúc → nguồn gốc B8.
  Chưa cài/đo thật trong B10/B11 (loại khỏi vòng đo hiện tại theo yêu cầu).
- **grepai** (yoanbernabeu) — semantic search + call graph thật (Ollama, 100% local); dùng thật
  trong B11, không chỉ tham khảo tài liệu.
- **Semgrep** — bài học về minh bạch: số official (250% true-positive) bị audit độc lập chỉ ra chỉ
  50-71%. Áp dụng: không che số xấu (B6 `find_callers` = 0% được giữ nguyên, không loại khỏi báo cáo;
  B10/B11 Semble các task `unsupported` vẫn đo, không loại khỏi bảng).

B6-B9 dùng số liệu công khai của competitor làm nguồn cảm hứng phương pháp, không phải A/B trực
tiếp. **B10/B11 là A/B trực tiếp thật** — cài CodeGraph + Semble (+ grepai + Serena ở B11) thật, gọi
tool thật, trên cùng self-repo corpus với B4/B6 — đọc B11 khi cần số so sánh thật mới nhất (B10 giữ
lại làm lịch sử, methodology của nó có lỗ hổng đã audit — xem B11's README), đọc B6 khi chỉ cần hiểu
ý tưởng đo tool-call efficiency.

## Phạm vi hiện tại

Corpus dùng chung: **self-repo** (chính CALM, ~40 file Rust) — zero-setup, không cần
clone gì thêm. Corpus đa ngôn ngữ/quy mô lớn hơn (httpx, FastAPI, Django như trong thiết kế gốc)
để ở Phase 2, sau khi phương pháp đo được xác nhận đúng trên self-repo.
