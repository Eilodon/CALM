# B12 — Tier-1/Tier-2 MCP tool-surface correctness (6 Tier-0 languages, external OSS repos)

## Câu hỏi khác với mọi benchmark khác trong suite này

`b2_call_graph_quality` và benchmark 6-ngôn-ngữ 2026-07-28 đo **độ chính xác của
call-graph edge trực tiếp từ `index.db`** — đã trả lời "graph CALM resolve ra có
đúng không" (formal ~90-93% trên cả 6 ngôn ngữ Tier-0, xem memory
`calm-tier0-benchmark-rootcause-2026-07-28`, 4 bug thật đã fix+ship, commit
`ac47e0a`). `b10`/`b11` so `calm` với competitor, chỉ trên self-repo. `resolution/`
đo tier distribution, không có oracle. **Không cái nào lái thật tầng MCP tool** —
`repo_overview`, `search`, `source`, `file_overview`, `callers`, `edit_context`,
`edit_lines`, `edit_symbol`, `diff_impact`, `hotspots` — qua JSON-RPC thật, trên
repo OSS ngoài, để kiểm tra xem một user mới cài CALM vào project của họ có nhận
được hành vi đúng/an toàn từ CHÍNH các tool đó hay không — khác với việc graph bên
dưới đúng hay sai.

## Phạm vi

- **6 ngôn ngữ Tier-0 thật** (Python/Rust/Go/JavaScript/TypeScript/Java — verify
  qua `crates/calm-core/src/indexer/lang_constants.rs`, không phải danh sách nào
  khác trong docs).
- **Repo OSS ngoài thật**, đã pin commit, KHÔNG dùng self-repo:

  | lang | repo | nguồn | note |
  |---|---|---|---|
  | python | pallets/flask | `benchmarks/resolution/corpus/python` (dùng chung, read-only) | shallow clone (depth 1) |
  | go | gin-gonic/gin | `benchmarks/resolution/corpus/go` | shallow clone |
  | java | spring-projects/spring-petclinic | `benchmarks/resolution/corpus/java` | shallow clone |
  | javascript | expressjs/express | `benchmarks/resolution/corpus/js` | shallow clone |
  | typescript | colinhacks/zod | `benchmarks/resolution/corpus/typescript` | shallow clone |
  | rust | sharkdp/fd | clone riêng, **ngoài** cây thư mục CALM (`../calm-bench-corpora/fd`) | full clone |

  5/6 dùng chung corpus đã pin sẵn của `resolution/` (read-only, không đụng trực
  tiếp — B12 luôn làm việc trên một bản `git clone --local` dùng-1-lần, xoá sau
  mỗi run). Rust cần clone riêng **ngoài** cây CALM: cargo tự phát hiện
  `[workspace]` tổ tiên nếu crate nằm lồng trong workspace cargo của chính CALM,
  làm rust-analyzer gãy âm thầm (gặp và fix trong benchmark 6-ngôn-ngữ 07-28).

- **"Full power"**: build feature mặc định thật của `calm-cli`
  (`default = ["embeddings", "tier0-5", "scip-overlay"]` —
  `crates/calm-cli/Cargo.toml`), không phải bản rút gọn. `target/release/calm`
  hiện tại đã đúng bộ này.

- **Giới hạn "full power" nói thẳng, không giấu**: `scip-overlay` compile sẵn
  nhưng chỉ thật sự kích hoạt formal-tier cho ngôn ngữ nào có binary SCIP tương
  ứng trên PATH. Tại thời điểm chạy benchmark này, máy chỉ có `rust-analyzer` —
  `scip-go`/`scip-python`/`scip-typescript`/`scip-java`+`coursier` đã KHÔNG còn
  trên máy (chỉ từng cài trong sandbox của một phiên trước, giờ đã mất). Theo
  lựa chọn tường minh của user, benchmark này **không cài lại** — nên chỉ Rust có
  formal-tier SCIP overlay thật khi chạy; 5 ngôn ngữ còn lại chạy tree-sitter-only
  (vẫn là "full power" theo nghĩa mọi Cargo feature mặc định đã compile sẵn).
  Không che số — đúng tinh thần "báo cáo trung thực" đã có sẵn của suite này (xem
  B6 `find_callers=0%` trong `benchmarks/README.md`).

## Ground truth — vì sao không cần cài lại SCIP toolchain

Một bộ extractor regex đơn giản theo từng ngôn ngữ (`ground_truth.py`) —
**độc lập với chính parser tree-sitter của CALM**, đó là điều làm nó thành oracle
thật — cộng `git grep -n -w` cho call-site ground truth. Không đạt độ chính xác
cấp parser, nhưng đủ để lấy mẫu các định nghĩa/call-site *thật, verify độc lập
được* — đúng tinh thần `function_ground_truth_lines`/`grep_oracle_callers` của
B11. Đây cũng chính là kiểu tín hiệu đã bắt được bug B1 thật (JS/TS bỏ sót hàm gán
qua property/prototype) — tool nào trả về gần-0 kết quả trong khi grep tìm ra
hàng chục thì đáng nghi ngay.

## Mỗi tool được kiểm tra thế nào

- **repo_overview**: gọi 1 lần trên corpus vừa clone sạch — thành công, phát hiện
  đúng ngôn ngữ chính, `indexing_phase` đạt `ready` trong thời gian chờ giới hạn.
- **search** (`grep`/`symbol`/`hybrid`/`file`): ~20 symbol/string lấy mẫu từ ground
  truth mỗi ngôn ngữ, đo recall so với ground truth độc lập; cờ đỏ khi
  ngôn-ngữ×kind nào recall bất thường thấp.
- **source**: so khớp byte-for-byte với đọc trực tiếp từ đĩa (range mode); round
  trip `etag`/`if_none_match`; symbol-mode phải chứa đúng dòng định nghĩa ground
  truth tìm được.
- **file_overview**: số symbol trả về so với số ground truth độc lập tìm được
  trong cùng file.
- **callers**: so với số call-site thật từ `git grep` — cờ đỏ khi trả về 0 trong
  khi grep tìm thấy ≥3 (đúng profile đã lộ ra bug B1).
- **edit_lines**/**edit_symbol**: cả hai mode (`old_text` và `expected_hash`) đều
  phải round-trip đúng trên bản clone dùng-1-lần; dùng lại `old_text`/hash CŨ sau
  khi file đã đổi phải bị TỪ CHỐI, không được âm thầm áp dụng (probe trực tiếp lớp
  lỗi trong memory `feedback-edit-symbol-stale-index-same-file-chain`); chèn qua
  `position="append_inside"` được verify đúng vị trí.
- **edit_context**: `caller_count`/`is_hub` đối chiếu ground truth grep; symbol
  `is_hub`/high-risk phải bị gate từ chối nếu `edit_lines`/`edit_symbol` không có
  `confirm: true` (kiểm tra thật, không giả định).
  cần thật để `diff_impact` phân tích, đối chiếu ground truth (kiểm tra false-
  positive theo memory `calm-diff-impact-signature-false-positive`).
- **hotspots**: smoke-test (không có oracle) — 5/6 corpus là shallow clone (1
  commit lịch sử) nên tín hiệu churn gần như vô nghĩa **do giới hạn của corpus,
  không phải lỗi CALM** — nói rõ trong kết quả, không báo nhầm thành bug.
- **Cross-cutting, 1 lần/ngôn ngữ**: cold-start từ `.calm/` rỗng không treo/crash
  trên corpus lớn nhất (spring-petclinic, zod); path-traversal
  (`../../../../etc/passwd`, ghi ra `/tmp`) phải bị chặn (regression check cho fix
  thật trong memory `calm-security-audit-multiagent-2026-07-12`); input dị dạng
  (query rỗng, `limit` khổng lồ, symbol không tồn tại) phải trả lỗi sạch, không
  crash server.

## Chạy

```bash
cargo build --release -p calm-cli   # default features đã là full power, không cần --features gì thêm
benchmarks/.venv/bin/python benchmarks/b12_tier1_tier2_tool_correctness/run_benchmark.py
# hoặc 1 ngôn ngữ cho dry-run nhanh:
benchmarks/.venv/bin/python benchmarks/b12_tier1_tier2_tool_correctness/run_benchmark.py --lang python
```

`results.json` (gitignored, như mọi benchmark khác trong suite) chứa kết quả chi
tiết per-tool/per-language. Script chỉ **báo cáo**, không tự sửa code CALM — bug
thật tìm được được note lại để user review/ưu tiên, không auto-fix.
