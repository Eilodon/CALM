# B3 — Search Quality (NDCG)

Đo chất lượng ranking của `ci search` bằng NDCG@10 trên ground truth đã curate thủ công (12 query
ngắn, thực tế — kiểu agent thật sẽ gõ, không phải câu hỏi đầy đủ), so với baseline `grep -l` (không
ranking, chỉ theo thứ tự file-scan) trên chính repo Code-Intelligence.

## Chạy

```bash
benchmarks/.venv/bin/python benchmarks/b3_search_quality/run_benchmark.py
```

Script spawn `ci serve --project-root .` (qua `cargo run --release`), đợi index `ready`, chạy từng
query trong `queries.yaml` với `kind=symbol` và `kind=hybrid`, tính NDCG@10 hai phía cộng thêm
baseline `grep -l` (file-scan order), in bảng kết quả và ghi `results.json`.

## Ground truth — `queries.yaml`

Mỗi query có `relevant`: danh sách `(name, path, grade)` đã verify thật bằng cách gọi `search` sống
trên index đã build (không đoán tên hàm). `grade=2` là kết quả lý tưởng, `grade=1` là kết quả chấp
nhận được (thường là unit test của chính symbol đó, cùng file). `grep_pattern` là 1 từ khoá một
người grep tay sẽ gõ — **cố tình không phải tên hàm chính xác**, để baseline không bị "gian lận".

## Kết quả mẫu (chạy lần đầu, self-repo, 46 file)

| Query | ci (symbol) | ci (hybrid) | naive grep | hybrid degraded? |
|---|---|---|---|---|
| cyclomatic_complexity | 1.000 | 1.000 | 0.431 | yes |
| detect_is_test | 1.000 | 1.000 | 0.431 | yes |
| noise_multiplier | 1.000 | 1.000 | 1.000 | yes |
| rrf_merge | 1.000 | 1.000 | 1.000 | yes |
| sanitize_source_output | 1.000 | 1.000 | 1.000 | yes |
| detect_injection_patterns | 1.000 | 1.000 | 1.000 | yes |
| compute_hotspots | 1.000 | 1.000 | 0.431 | yes |
| fitness_thresholds | 1.000 | 1.000 | 0.631 | yes |
| compute_coreness | 1.000 | 1.000 | 0.387 | yes |
| update_is_hub_flags | 1.000 | 1.000 | 0.631 | yes |
| dead_code_confidence | 1.000 | 1.000 | 0.387 | yes |
| resolve_symbol | 0.860 | 0.860 | 0.000 | yes |

mean NDCG@10 — ci(symbol): **0.988**, ci(hybrid): **0.988**, naive grep: **0.611**.

## Phát hiện quan trọng #1: bug thật bắt được trong lúc xây benchmark, không phải sau

Trước khi tinh chỉnh `queries.yaml`, chạy thử với query `"rrf merge"` cho kết quả: `rrf_merge_n`
(mục tiêu thật) xếp hạng **#3/5**, sau `test_rrf_merge_combines_results` và
`test_rrf_merge_n_respects_limit` — cả hai đều nằm CÙNG FILE `search.rs` (convention Rust
`#[cfg(test)] mod tests`). Noise-penalty theo path (đã làm trước đó) không bắt được case này vì
không có thư mục `tests/` riêng để flag. Fix: dùng thẳng cột `symbols.is_test` (đã có sẵn từ
DEBT-008) thay vì đoán qua tên/đường dẫn — xem `crates/ci-core/src/search.rs::noise_multiplier`.
Sau fix, `rrf_merge_n` lên #1. Số liệu trong bảng trên là SAU fix; đây chính là giá trị của việc có
benchmark thật: bắt được vấn đề thay vì đoán.

## Phát hiện quan trọng #2: naive grep thất bại hoàn toàn khi từ khoá phổ biến

`resolve_symbol` (`crates/ci-server/src/tools.rs`) NDCG naive-grep = 0.000 — không phải bug, đã
verify tay: `grep -l resolve` khớp 16 file (từ "resolve" xuất hiện khắp `resolver/*.rs`,
`indexer/*.rs`...), và `tools.rs` xếp thứ 16/16 theo thứ tự file-scan, ngoài cửa sổ NDCG@10. `ci
search` xếp đúng file này ở #1 nhờ BM25 trên tên symbol thay vì so khớp text thô toàn file.

## Giới hạn của lần đo này

- **`kind=hybrid` degraded ở MỌI query** — môi trường build không có feature `embeddings` +
  model đã tải, nên `ndcg_hybrid == ndcg_symbol` ở đây là giới hạn môi trường, **không phải** kết
  luận về chất lượng semantic search. Chạy lại với `cargo build --release --features embeddings`
  (và model đã sẵn) để có số hybrid thật.
- **12 query, tự curate** — đủ để validate phương pháp đo và bắt bug thật (xem trên), chưa đủ cho
  benchmark chuẩn hoá quy mô lớn. Query cố tình ngắn (2-4 từ) vì `search(kind=symbol)` hiện bọc
  toàn bộ query thành 1 FTS5 phrase match — câu hỏi tự nhiên dài gần như không match gì (đã verify
  tay trước khi chốt ground truth). Đây là đặc tính thật của implementation hiện tại, ghi nhận ở
  đây thay vì che bằng cách chọn query dễ.
- **Self-repo only** — corpus đa ngôn ngữ/quy mô lớn hơn để Phase 2, theo đúng scope chung của
  `benchmarks/README.md`.
- **naive-grep baseline ở mức file, không phải symbol** — `grep -l` không resolve được về symbol,
  nên relevance được gộp về file (grade = max của mọi symbol liên quan trong file đó). NDCG hai
  phía vẫn so sánh được (mỗi bên tự chuẩn hoá theo IDCG của chính nó) nhưng không cùng đơn vị đo
  tuyệt đối.
