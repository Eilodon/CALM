# B10 — Real Competitor A/B (`calm` vs CodeGraph vs Semble)

Khác với `docs/comparison.md` (dựa trên tài liệu công khai của từng dự án) và B6 (lấy "ý tưởng"
từ cách CodeGraph báo cáo số của họ), benchmark này chạy **tool call thật** trên **cả 3 MCP
server thật** (`calm`, [CodeGraph](https://github.com/colbymchenry/codegraph) v1.2.0, Semble) —
cùng self-repo corpus, cùng 4 task với B4/B6 (`../lib/tasks.yaml` + `../lib/competitor_tasks.yaml`),
đo 3 chiều: token cost, tool-call count, và độ chính xác trên task `find_callers`.

## Chạy

```bash
npm i -g @colbymchenry/codegraph   # 1 lần
codegraph init                     # build .codegraph/ tại repo root — 1 lần, ~1s cho self-repo
cargo build --release -p calm-cli    # nếu chưa build
benchmarks/.venv/bin/python benchmarks/b10_real_competitor_ab/run_benchmark.py
```

Semble không cần cài riêng — `uvx --from semble[mcp] semble` tự tải + cache môi trường ở lần
chạy đầu (giống cách `.mcp.json` khai báo server `semble` trong repo này).

## Kết quả (self-repo, 1 run — xem giới hạn bên dưới)

| Task | naive tok | `calm` tok (ratio) | CodeGraph tok (ratio) | Semble tok (ratio) |
|---|---|---|---|---|
| read_one_function | 18,543 | 962 (19.3x) | 1,470 (12.6x) | 214 (86.6x) |
| find_callers | 149 | 302 (0.5x) | 54 (2.8x) | 863 (0.2x) *unsupported* |
| pre_edit_blast_radius | 43,476 | 2,366 (18.4x) | 77 (564.6x) | 735 (59.2x) *unsupported* |
| locate_and_inspect | 27,423 | 5,660 (4.8x) | 3,311 (8.3x) | 482 (56.9x) |

median ratio: `calm` 11.6x · CodeGraph 10.4x · Semble 58.0x
mean ratio: `calm` 10.7x · CodeGraph 147.1x · Semble 50.7x

### Accuracy — `find_callers` (collect_source_files) vs grep oracle

Oracle (đếm call site thật bằng `grep -rn 'collect_source_files(' crates --include=*.rs`, loại
dòng định nghĩa/comment): 2 file gọi — `crates/calm-core/src/indexer/pipeline.rs` và
`crates/calm-server/src/tools/recover.rs` (khác crate, gọi qua fully-qualified path
`calm_core::indexer::pipeline::collect_source_files`).

| Tool | Recall | Ghi chú |
|---|---|---|
| `calm` | 2/2 | Bắt được cả caller khác-crate qua fully-qualified path |
| CodeGraph | 1/2 | **Bỏ sót** `recover.rs` — cross-crate call qua fully-qualified path không được resolve |
| Semble | N/A | Không có khái niệm "callers" (embedding search thuần) — task đánh dấu `unsupported`, vẫn đo token/call nhưng không tính vào accuracy |

Đây là điểm khác biệt thật, verify được, không phải suy diễn từ tài liệu marketing: CodeGraph
matching theo tên trong cùng ngữ cảnh gần thì tốt, nhưng bỏ sót lời gọi qua đường dẫn
fully-qualified xuyên crate boundary trong trường hợp cụ thể này.

## Đọc số liệu này thế nào cho đúng — đừng chỉ nhìn ratio

Ratio token **không tự nó nói lên "tool nào tốt hơn"** — 3 tool trả lời 3 mức độ khác nhau cho
cùng câu hỏi:

- **`pre_edit_blast_radius`**: CodeGraph ratio 564.6x trông như thắng áp đảo, nhưng
  `codegraph_impact` trả về **danh sách symbol bị ảnh hưởng** (4 symbol, 77 token) — không kèm
  source, không risk assessment, không khuyến nghị hành động. `calm`'s `edit_context` trả về
  **source đầy đủ + danh sách caller + `is_hub` + `risk_assessment` + `suggested_next`** (2,366
  token) — nhiều hơn vì làm nhiều việc hơn (đây chính là hard-gate trước khi sửa mà
  `docs/comparison.md` ghi nhận CodeGraph không có: "read-only, không sửa file"). So ratio thô ở
  đây là so token của hai loại output khác nhau về bản chất, không phải so "ai nén tốt hơn cùng
  một câu trả lời".
- **`find_callers`**: cả `calm` và CodeGraph đều ratio <1x (tốn token hơn naive `grep`) — vì `grep`
  không cần mở file nào (naive.type=grep, không phải grep_then_cat_matches), nên baseline đã rẻ
  sẵn. Giống nhận xét trong B6: lợi thế chỉ lộ rõ khi naive cần mở nhiều file.
- **Semble** cho 2 task `unsupported` (`find_callers`, `pre_edit_blast_radius`) trả lời bằng
  embedding search — có thể trông "rẻ" (735 token, ratio 59.2x) nhưng **không xác nhận được quan
  hệ gọi hàm thật**, nên ratio cao ở đây không phải "hiệu quả hơn", mà là "trả lời một câu hỏi
  khác, dễ hơn". Giữ nguyên trong bảng theo đúng chính sách của repo (không ẩn số xấu/số
  không-so-sánh-được), nhưng đọc kèm chú thích `unsupported`.

## Giới hạn

- **N=1 run, N=4 task, self-repo only** — cùng giới hạn với B4/B6. Không có median-of-N như
  phương pháp CodeGraph tự công bố (N=4 run/repo); wall-clock/cost không đo (chỉ token + call
  count + accuracy).
- **Semble base image thiếu tree-sitter grammar** cho rust/python/typescript/json/bash trong môi
  trường chạy benchmark này (`Language rust not found, falling back to line chunking` — xem log
  stderr khi chạy `semble search` trực tiếp) — search vẫn ra kết quả đúng vị trí, nhưng chunk
  boundary là line-based thay vì AST-based, có thể ảnh hưởng chất lượng snippet ở repo khác/pattern
  phức tạp hơn. Không kết luận đây là giới hạn chung của Semble — có thể chỉ là thiếu dependency
  trong container này.
- **`find_callers` accuracy oracle** dùng grep đơn giản (loại dòng `fn `/comment) — đủ cho 1
  symbol nhỏ trong Rust, không phải oracle tổng quát (không tính polymorphism, macro, re-export).
- CodeGraph's 7 tool phụ (`node`/`search`/`callers`/`callees`/`impact`/`files`/`status`) mặc định
  **ẩn** trừ khi set `CODEGRAPH_MCP_TOOLS` — benchmark này bật hết để so 1-1 với tool tương ứng
  của `calm`; một agent dùng CodeGraph mặc định (chỉ `codegraph_explore`) sẽ có số khác.

## File liên quan

- `../lib/generic_mcp_client.py` — MCP stdio client tổng quát (không hardcode `calm serve`), dùng
  cho cả CodeGraph và Semble.
- `../lib/competitor_tasks.yaml` — mapping task → tool call cho CodeGraph/Semble, cùng task id với
  `../lib/tasks.yaml`.
- `results.json` — không commit (xem `.gitignore`), chạy lại để lấy số mới nhất.
