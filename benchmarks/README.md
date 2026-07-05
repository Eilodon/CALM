# Benchmarks

Đo lợi thế thật của `ci` MCP server so với workflow naive (`cat`/`grep`) hoặc so với các
competitor cùng nhóm (CodeGraph, Serena, Semble, Semgrep, GitNexus). Mỗi benchmark có anchor code
thực tế, phương pháp đo cụ thể, và baseline so sánh — không dùng số liệu ước lượng khi có thể đo
thật; khi số đo ra ngoài kỳ vọng (vd B6 `find_callers` = 0%), báo cáo trung thực thay vì ẩn đi.

| # | Chiều | Mục tiêu chứng minh | Trạng thái |
|---|---|---|---|
| B1 | AST Indexing Accuracy | Tree-sitter vs regex | Planned |
| B2 | Call Graph Resolution Quality | Tier-1/2/3 vs textual (scope: Rust, SCIP oracle) | **Implemented** — [`b2_call_graph_quality/`](b2_call_graph_quality/) |
| B3 | Search Quality | Hybrid RRF vs FTS-only vs raw grep (NDCG@10) | **Implemented** — [`b3_search_quality/`](b3_search_quality/) |
| B4 | Token Efficiency | MCP tools vs `cat`/`grep` naive workflow | **Implemented** — [`b4_token_efficiency/`](b4_token_efficiency/) |
| B5 | Incremental Indexing Speed | Reindex chỉ file thay đổi | Planned |
| B6 | Tool-Call Efficiency | Số round-trip naive vs 1 MCP call (ý tưởng từ CodeGraph) | **Implemented** — [`b6_tool_call_efficiency/`](b6_tool_call_efficiency/) |
| B7 | Task Correctness / Regression | Agent thật làm refactor, có/không `edit_context`+`diff_impact`, đếm callsite bị bỏ sót (ý tưởng từ Serena) | Planned |
| B8 | Model-Tier Leveling | Model rẻ + ci tools vs model đắt không có tools, cùng task (ý tưởng từ GitNexus) | Planned |
| B9 | Scaling Curve | Lợi thế `ci` co giãn theo quy mô repo (nhỏ → lớn) | Planned |
| B10 | Real Competitor A/B | `ci` vs CodeGraph vs Semble — tool call thật trên cả 3 MCP server thật (không phải số tự báo cáo) | **Implemented** — [`b10_real_competitor_ab/`](b10_real_competitor_ab/) |

Nguồn cảm hứng B6-B9: xem phần "Nghiên cứu competitor" bên dưới. Khác với B6 (dùng ý tưởng đo của
CodeGraph nhưng chỉ chạy `ci`), B10 cài thật CodeGraph + Semble và gọi tool thật của cả 3 — xem
B10's README cho lý do vì sao ratio thô không nên đọc như bảng xếp hạng.

## Hạ tầng dùng chung — `lib/`

`mcp_client.py` (MCP stdio client cho `ci`), `generic_mcp_client.py` (client tổng quát cho MCP
server bất kỳ — CodeGraph, Semble, dùng ở B10), `tasks.yaml` (task definitions cho B4/B6/B10),
`competitor_tasks.yaml` (mapping cùng task id đó sang tool call của CodeGraph/Semble, dùng ở B10),
`naive_workflow.py` (mô phỏng naive cat/grep + đếm call, cộng `naive_grep_ranked_files` cho
baseline ranking của B3) nằm ở `benchmarks/lib/`, dùng chung — không định nghĩa lại task hay logic
mô phỏng ở mỗi benchmark.

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

# B10 cần thêm CodeGraph cài thật + index build sẵn (Semble tự tải qua uvx, không cần bước riêng):
npm i -g @colbymchenry/codegraph && codegraph init
benchmarks/.venv/bin/python benchmarks/b10_real_competitor_ab/run_benchmark.py
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
- **Semgrep** — bài học về minh bạch: số official (250% true-positive) bị audit độc lập chỉ ra chỉ
  50-71%. Áp dụng: không che số xấu (B6 `find_callers` = 0% được giữ nguyên, không loại khỏi báo cáo;
  B10 Semble 2/4 task đánh dấu `unsupported` nhưng vẫn đo, không loại khỏi bảng).

B6-B9 dùng số liệu công khai của competitor làm nguồn cảm hứng phương pháp, không phải A/B trực
tiếp. **B10 là A/B trực tiếp thật** — cài CodeGraph + Semble thật, gọi tool thật, trên cùng self-repo
corpus với B4/B6 — nên đọc B10 khi cần số so sánh thật giữa `ci` và 2 tool kia, đọc B6 khi chỉ cần
hiểu ý tưởng đo tool-call efficiency.

## Phạm vi hiện tại

Corpus dùng chung: **self-repo** (chính CALM, ~40 file Rust) — zero-setup, không cần
clone gì thêm. Corpus đa ngôn ngữ/quy mô lớn hơn (httpx, FastAPI, Django như trong thiết kế gốc)
để ở Phase 2, sau khi phương pháp đo được xác nhận đúng trên self-repo.
