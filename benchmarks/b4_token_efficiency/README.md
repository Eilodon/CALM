# B4 — Token Efficiency

Đo số token (GPT-4 tokenizer, `tiktoken`) một agent tốn khi dùng workflow naive (`cat` file,
`grep` text thô) so với gọi thẳng MCP tool tương ứng (`source`, `callers`, `edit_context`,
`locate`), trên chính repo Code-Intelligence.

## Chạy

```bash
benchmarks/.venv/bin/python benchmarks/b4_token_efficiency/run_benchmark.py
```

Script tự spawn `ci serve --project-root .`, đợi index `ready`, gọi lần lượt từng task trong
`../lib/tasks.yaml`, tokenize cả hai phía, in bảng kết quả và ghi `results.json`.

## Task set

Dùng chung task set với [B6](../b6_tool_call_efficiency/) — định nghĩa 1 lần tại
`../lib/tasks.yaml`, không lặp lại. 4 task ánh xạ 4 kịch bản coding thật (đọc hàm / tìm callers /
kiểm tra blast radius trước khi sửa / locate 3-in-1), dùng symbol thật trong
`crates/ci-core/src/indexer/pipeline.rs` (`run_indexing_pipeline`, `collect_source_files`,
`reindex_changed`). `mcp_client.py` (client MCP stdio) cũng nằm ở `../lib/`, tái dùng cho các
benchmark sau.

## Kết quả mẫu (chạy lần đầu, self-repo, 42 files)

| Task | ci tool | naive tokens | ci tokens | ratio |
|---|---|---|---|---|
| read_one_function | `source` | 12861 | 916 | **14.0x** |
| find_callers | `callers` | 146 | 373 | **0.4x** |
| pre_edit_blast_radius | `edit_context` | 16430 | 2729 | **6.0x** |
| locate_and_inspect | `locate` | 17809 | 4798 | **3.7x** |

median 4.9x, mean 6.0x (N=4 — quá nhỏ để tính p90/p99 có ý nghĩa, xem "Giới hạn" bên dưới).

## Phát hiện quan trọng: `find_callers` ratio < 1

`collect_source_files` chỉ có 4 call site trong toàn repo → `grep -n` thô đã rất gọn (146 tokens),
trong khi JSON của `callers` (`edge_confidence`, `preview`, `suggested_next`...) tốn nhiều hơn (373
tokens) dù thông tin có cấu trúc hơn (đã phân loại confidence, không cần agent tự đoán). Đã verify
bằng tay: cả 2 phía đều tìm đúng 4 call site, không phải bug.

**Kết luận thật, không phải marketing**: token efficiency của MCP tools **tỷ lệ thuận với blast
radius / kích thước ngữ cảnh cần đọc**. Khi 1 symbol chỉ có vài chỗ dùng trong 1 file nhỏ, JSON
overhead có thể vượt raw text. Lợi thế lớn nhất xuất hiện ở các task cần agent tự tổng hợp thông
tin từ nhiều file hoặc đọc nguyên 1 file lớn chỉ để lấy 1 hàm (`read_one_function`: 14x,
`pre_edit_blast_radius`: 6x) — đúng như premise gốc "tốn nhiều token nhất khi file to / blast
radius rộng".

## Giới hạn của lần đo này

- **Self-repo only** — 42 file Rust, không đại diện cho repo Python/TS/Java lớn hơn. Corpus đa
  ngôn ngữ (httpx, FastAPI, Django) để Phase 2.
- **N=4 task** — đủ để validate phương pháp đo, chưa đủ cho phân phối percentile đáng tin. Cần mở
  rộng tasks.yaml trước khi dùng số liệu này làm marketing claim chính thức.
- **JSON overhead chưa được trừ hao** — số liệu bao gồm nguyên request/response tối thiểu qua
  stdio, không tối ưu prompt engineering thêm.
- Kết quả phụ thuộc trạng thái index (`edges_ready`, số symbol đã embed) tại thời điểm chạy — chạy
  lại có thể ra số hơi khác nếu code đã đổi.
