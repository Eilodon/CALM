# B6 — Tool-Call Efficiency

Khác với B4 (đo token payload), B6 đo **số lần agent phải gọi tool** (round-trip / latency
overhead) để hoàn thành cùng 1 task — naive workflow (`grep` rồi mở từng file match) so với 1 lệnh
MCP duy nhất. Ý tưởng lấy từ cách CodeGraph báo cáo "92% fewer tool calls" — token thấp chưa chắc
đồng nghĩa ít round-trip, nên tách thành benchmark riêng.

Dùng chung task set với B4 (`../lib/tasks.yaml`), không định nghĩa lại.

## Chạy

```bash
benchmarks/.venv/bin/python benchmarks/b6_tool_call_efficiency/run_benchmark.py
```

Script vẫn gọi `ci serve` thật cho mỗi task (không chỉ giả định "1 call") để xác nhận tool thực sự
trả về nội dung — nếu response rỗng, script raise lỗi thay vì báo cáo "1 call" sai sự thật.

## Cách đếm naive calls

- `cat` (đọc 1 file): 1 call.
- `grep` (chỉ grep, không cần mở file): 1 call.
- `grep_then_cat_matches` (grep tìm file, rồi phải mở từng file match để hiểu ngữ cảnh):
  `1 + số file match` — vì agent không có call graph phải tự mở từng file mới biết được nội dung.

## Kết quả mẫu (self-repo)

| Task | ci tool | naive calls | ci calls | reduction |
|---|---|---|---|---|
| read_one_function | `source` | 1 | 1 | 0% |
| find_callers | `callers` | 1 | 1 | 0% |
| pre_edit_blast_radius | `edit_context` | 4 | 1 | 75% |
| locate_and_inspect | `locate` | 5 | 1 | 80% |

median 38%, mean 39%.

## Nhận xét

2 task đầu (`read_one_function`, `find_callers`) có naive_calls=1 vì naive workflow của chúng chỉ
cần 1 lệnh (`cat` 1 file, hoặc `grep` không cần mở thêm file) — nên reduction=0%, dù B4 vẫn cho
thấy chênh lệch token (14.0x và 0.4x). Điều này càng củng cố việc **B4 và B6 đo hai chiều độc lập**:
token efficiency không suy ra được tool-call efficiency và ngược lại. Lợi thế tool-call chỉ lộ rõ ở
những task cần agent tổng hợp từ **nhiều file** (`pre_edit_blast_radius`, `locate_and_inspect`) —
đúng pattern CodeGraph mô tả: gains scale với số lượng file liên quan, không phải với kích thước 1
file đơn lẻ.

## Giới hạn

Cùng giới hạn với B4: self-repo only, N=4 task. Cách đếm `naive_calls` giả định agent grep đúng 1
lần rồi mở đúng các file match được — thực tế agent có thể cần thử lại pattern grep nhiều lần
(broaden query) khi lần đầu không ra kết quả, việc đó sẽ làm gap thực tế còn lớn hơn số ở đây.
