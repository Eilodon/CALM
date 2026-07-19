# B4 — Token Efficiency

Đo số token (GPT-4 tokenizer, `tiktoken`) một agent tốn khi dùng workflow naive (`cat` file,
`grep` text thô) so với gọi thẳng MCP tool tương ứng (`source`, `callers`, `edit_context`,
`locate`), trên chính repo CALM.

## Chạy

```bash
benchmarks/.venv/bin/python benchmarks/b4_token_efficiency/run_benchmark.py
```

Script tự spawn `calm serve --project-root .`, đợi index `ready`, gọi lần lượt từng task trong
`../lib/tasks.yaml`, tokenize cả hai phía, in bảng kết quả và ghi `results.json`.

## Task set

Dùng chung task set với [B6](../b6_tool_call_efficiency/) — định nghĩa 1 lần tại
`../lib/tasks.yaml`, không lặp lại. 4 task ánh xạ 4 kịch bản coding thật (đọc hàm / tìm callers /
kiểm tra blast radius trước khi sửa / locate 3-in-1), dùng symbol thật trong
`crates/calm-core/src/indexer/pipeline.rs` (`run_indexing_pipeline`, `collect_source_files`,
`reindex_changed`). `mcp_client.py` (client MCP stdio) cũng nằm ở `../lib/`, tái dùng cho các
benchmark sau.

## Kết quả mẫu (chạy lần đầu, self-repo, 42 files) — LỊCH SỬ, xem bản cập nhật bên dưới

| Task | ci tool | naive tokens | ci tokens | ratio |
|---|---|---|---|---|
| read_one_function | `source` | 12861 | 916 | **14.0x** |
| find_callers | `callers` | 146 | 373 | **0.4x** |
| pre_edit_blast_radius | `edit_context` | 16430 | 2729 | **6.0x** |
| locate_and_inspect | `locate` | 17809 | 4798 | **3.7x** |

median 4.9x, mean 6.0x (N=4 — quá nhỏ để tính p90/p99 có ý nghĩa, xem "Giới hạn" bên dưới).

## Cập nhật 2026-07-19: `find_callers` ratio < 1 — đã tìm ra root cause và fix, không còn đúng nữa

Con số 0.4x ở trên (và các lần đo lại tương tự trước khi fix, dao động 0.6x-0.8x tùy trạng thái
repo) **không phải giới hạn cấu trúc của JSON, mà là 1 field thật sự thừa**: `CallerEntry.path`
lặp lại y hệt tiền tố đã có sẵn trong `symbol` (`"{path}::{name}"`) trên MỌI entry — verify bằng
cách quét toàn bộ 6.166 dòng `call_edges` thật trong index của chính repo này (11 ngôn ngữ), 0 case
lệch, cộng với việc lần theo code tạo ra `path`/`symbol` (`indexer::pipeline`, `scip::ingest`) — cả
hai luôn được gán từ cùng 1 biến. Không mất thông tin gì khi bỏ field này (path suy ra được 100%
bằng `symbol.split("::", 1)[0]`).

Đã fix ở commit `e6a4d7e` (bỏ `path` khỏi `CallerEntry`, xem `crates/calm-server/src/tools/common.rs`).
Chạy lại `run_benchmark.py` trên HEAD sau fix (repo hiện đã lớn hơn nhiều so với lần đo đầu — xem
bảng dưới):

| Task | ci tool | naive tokens | ci tokens | ratio |
|---|---|---|---|---|
| read_one_function | `source` | 51380 | 213 | **241.2x** |
| find_callers | `callers` | 219 | 217 | **1.0x** |
| pre_edit_blast_radius | `edit_context` | 145386 | 755 | **192.6x** |
| locate_and_inspect | `locate` | 130262 | 4530 | **28.8x** |

median 110.7x, mean 115.9x. `find_callers` giờ ngang bằng (thực ra rẻ hơn 2 token) naive grep thay
vì đắt hơn — không phải vì blast radius của `collect_source_files` đổi (vẫn 3 call site), mà vì
phần overhead thừa đã bị cắt. `pre_edit_blast_radius` cũng lợi theo vì `edit_context` dùng chung
`CallerEntry`.

**Kết luận cập nhật**: premise gốc "token efficiency tỷ lệ thuận với blast radius" vẫn đúng và giờ
thể hiện rõ hơn nhiều (`read_one_function` từ 14x lên 241.2x, đơn giản vì file đó đã phình to theo
thời gian) — nhưng `find_callers` ratio<1 KHÔNG phải bằng chứng cho premise đó; nó là 1 bug lãng phí
token cụ thể, đã fix, không đại diện cho giới hạn cấu trúc chung của JSON-typed tool response.

## Giới hạn của lần đo này

- **Self-repo only** — 42 file Rust, không đại diện cho repo Python/TS/Java lớn hơn. Corpus đa
  ngôn ngữ (httpx, FastAPI, Django) để Phase 2.
- **N=4 task** — đủ để validate phương pháp đo, chưa đủ cho phân phối percentile đáng tin. Cần mở
  rộng tasks.yaml trước khi dùng số liệu này làm marketing claim chính thức.
- **JSON overhead chưa được trừ hao** — số liệu bao gồm nguyên request/response tối thiểu qua
  stdio, không tối ưu prompt engineering thêm.
- Kết quả phụ thuộc trạng thái index (`edges_ready`, số symbol đã embed) tại thời điểm chạy — chạy
  lại có thể ra số hơi khác nếu code đã đổi.
