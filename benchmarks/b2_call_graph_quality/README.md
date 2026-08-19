# B2 — Call Graph Resolution Quality (Rust, SCIP oracle)

Đo `calm`'s Tier-0/Tier-2 syntactic call-graph resolver (Phase A của
`docs/superskills/plans/2026-07-03-rust-support.md`) so với `rust-analyzer scip` làm oracle ground
truth, trên chính repo CALM.

**Scope hiện tại: Rust only.** SCIP có oracle trưởng thành cho Rust (`rust-analyzer scip`); các
ngôn ngữ khác chưa có oracle tương đương sẵn có nên để lại cho lần triển khai sau.

## Chạy

```bash
cargo build --release -p calm-cli --features scip-overlay   # cần cho `ci scip-dump`
benchmarks/.venv/bin/python benchmarks/b2_call_graph_quality/run_benchmark.py --repo .
```

Mặc định `--repo` là chính repo này. Script: chạy `rust-analyzer scip` lấy oracle, decode qua `ci
scip-dump` (dùng lại `calm_core::scip::parse`, không viết lại protobuf decoder ở Python), chạy `ci
index` (mặc định feature, tức **chỉ Phase A**, SCIP overlay không bật) rồi so `call_edges` (Rust)
với oracle.

Oracle chạy với **`--all-features`** (`--ra-features`, mặc định `all`) — xem issue #72: một lần
`rust-analyzer scip` mặc-định-feature không phát occurrence cho các file sau feature-gate
(`bundle.rs` sau `index-bundles`, `lsp/*.rs` sau `lsp-overlay`, `http.rs` sau `http`, …), trong khi
indexer tree-sitter của CALM parse chúng vô điều kiện — làm mọi edge CALM chạm các file đó không bao
giờ khớp oracle và thổi phồng false-positive. Output giờ báo `oracle_covered_files`,
`uncovered_from_files` và `precision_on_covered` (precision chỉ tính trên các file oracle thực sự
thấy) để chỉ số coverage-gap luôn quan sát được.

## Kết quả đo lần đầu (self-repo, sau Phase A, trước khi bật SCIP overlay)

| | |
|---|---|
| Oracle edges (SCIP, non-local ref → def) | 8117 |
| `calm` call edges (Rust, có call-site line) | 1972 |
| **Precision** (`ci ∩ oracle / ci`) | **0.795** (1568/1972) |
| **Recall** (`ci ∩ oracle / oracle`) | **0.193** (1568/8117) |

Theo `edge_confidence`:

| confidence | count | precision |
|---|---|---|
| inferred | 273 | 0.967 |
| resolved | 1024 | 0.935 |
| textual | 675 | 0.514 |

## Diễn giải

- **Precision tăng đúng theo confidence tier** — đây là phát hiện quan trọng nhất: `inferred`
  (96.7%) và `resolved` (93.5%) gần như luôn đúng, còn `textual` (51.4%) đúng hơn nửa nhưng sai gần
  một nửa. Điều này validate trực tiếp lý do tồn tại của hệ thống confidence tier: agent tiêu thụ
  `edge_confidence` nên tin `inferred`/`resolved` nhiều hơn `textual`.
- **Recall thấp (19.3%) là kỳ vọng, không phải bug** — SCIP/rust-analyzer làm full type inference
  + trait resolution + generic monomorphization; `calm`'s Tier-0/Tier-2 resolver cố tình chỉ làm
  same-file/same-class name matching + constructor inference nông (Task A5), không type-check.
  Khoảng cách 8117 vs 1972 edges phần lớn là các cạnh đòi hỏi suy luận kiểu đầy đủ (trait dispatch,
  generic call, method trên kiểu suy từ chuỗi biểu thức phức tạp) mà Tầng A không nhắm tới — đúng
  theo README's nguyên tắc không che số xấu.
- Đây chính là khoảng trống mà **Phase B (SCIP overlay)** lấp: chạy lại benchmark này sau khi bật
  `rust.scip.enabled=true` trong `config.json` sẽ cho thấy các cạnh `resolved`/`textual` được nâng
  lên `formal` khi SCIP xác nhận — số `formal` xuất hiện trong bảng breakdown chính là thước đo
  trực tiếp cho "Phase B thêm được bao nhiêu".

## Giới hạn

- Self-repo only (theo scope chung `benchmarks/README.md`) — corpus lớn hơn để Phase 2.
- Oracle match theo `(file, line)` chính xác tuyệt đối; sai lệch dòng nhỏ giữa cách `rust-analyzer`
  và tree-sitter đếm dòng cho cùng một khai báo (nếu có) sẽ đếm là miss, làm recall đo được là cận
  dưới thực tế chứ không phải chính xác tuyệt đối.
- Chưa đo lần chạy có bật SCIP overlay (Phase B) — số trên là baseline Phase A thuần, mốc để đối
  chiếu khi B cải thiện.
- **Bảng “Kết quả đo lần đầu” và floors trong `scripts/check-b2-thresholds.sh` chưa phản ánh
  oracle `--all-features` mới.** Việc re-baseline (đo lại honest baseline rồi reset floors) cố tình
  hoãn đến sau khi fix qualified-path `std::`/`core::`/`alloc::` (issue #72 ask #1) — issue tự ghi:
  ~967 edge non-ambiguous/non-formal còn lại bị chi phối bởi cả coverage-gap (đã fix ở PR này) *và*
  qualified-path gap, nên reset floors bây giờ sẽ phải reset lại lần nữa. Đo + set floors một lần
  sau khi cả hai fix đã land.
