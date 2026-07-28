# Resolution — Multi-language Call-Graph Tier Baseline

Đo tier distribution (`formal`/`resolved`/`inferred`/`textual`/`ambiguous`/`unresolved`) của
`calm` trên **8 repo OSS thật, bên ngoài self-repo**, cho 8 ngôn ngữ mục tiêu của
[`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`](../../docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md).

**Khác B2** ([`../b2_call_graph_quality/`](../b2_call_graph_quality/)): B2 đo precision/recall
Rust so với oracle `rust-analyzer scip`, self-repo only. Benchmark này **không có oracle** — chưa
ngôn ngữ nào trong 8 ngôn ngữ này có SCIP provider (Phase 2 của kế hoạch chưa làm) — nên chỉ đo
**phân bố tier** để trả lời một câu hỏi cụ thể trước khi đổ effort vào Phase 2: heuristic Phase 0/1
(same-dir tier, type_map, PSR-4, stack-graphs JS, …) đã kéo được bao nhiêu, và ngôn ngữ nào còn
khoảng trống lớn nhất đáng ưu tiên.

## Corpus

Repo nhỏ, pin theo commit SHA thật resolve lúc clone (không phải tag cứng — xem `results.json`
mỗi lần chạy để biết chính xác commit nào được đo):

| lang | repo | lý do chọn |
|---|---|---|
| go | gin-gonic/gin | nhỏ, real-world web framework |
| java | spring-projects/spring-petclinic | nhẹ (theo gợi ý gốc của kế hoạch, thay vì guava) |
| csharp | dotnet-architecture/eShopOnWeb | mẫu kiến trúc .NET thật |
| c | redis/redis | C lớn, thật — **xem phát hiện quan trọng bên dưới** |
| cpp | fmtlib/fmt | header-only, nhiều overload — stress-test tier `ambiguous` |
| js | expressjs/express | nhỏ, thật |
| php | monicahq/monica | app PHP/Laravel thật, có composer.json cho PSR-4 |
| sql | jOOQ/sakila | mirror đa dialect (postgres/mysql/sql-server/…) của sample DB sakila |
| kotlin | square/kotlinpoet | code-gen lib thật, nhiều method trùng tên |
| swift | apple/swift-argument-parser | nhỏ, gọn, real-world |
| scala | lihaoyi/requests-scala | HTTP client nhỏ, thật |
| dart | dart-lang/args | CLI args parser nhỏ, thật |
| lua | kikito/middleclass | OOP lib nhỏ, có kế thừa/method dispatch thật |
| elixir | dashbitco/nimble_options | options-validation lib nhỏ |
| haskell | kowainik/co-log | logging lib nhỏ |
| ocaml | ocsigen/lwt | concurrency lib thật, call graph có ý nghĩa |
| zig | MasterQ32/zig-args | CLI args parser nhỏ |
| powershell | dahlbyk/posh-git | module PowerShell nhỏ, thật |
| groovy | http-builder-ng/http-builder-ng | HTTP client Groovy thật |

11 ngôn ngữ Phase B/C (Phase E, 2026-07-11) — batched vào cùng 1 lần chạy thay vì 11 lần riêng lẻ,
đúng tinh thần §1.7 của kế hoạch 25-ngôn-ngữ (đo tier-distribution, không phải accuracy tuyệt đối).

## Chạy

```bash
cargo build --release -p calm-cli --features tier0-5,lang-kotlin,lang-swift,lang-scala,lang-dart,lang-lua,lang-elixir,lang-haskell,lang-ocaml,lang-zig,lang-powershell,lang-groovy
                                     # scip-overlay không bắt buộc cho benchmark này — chỉ Rust có
                                     # provider chạy "harmless" trên corpus ngoại ngữ (đóng góp 0 edge)
benchmarks/.venv/bin/python benchmarks/resolution/run_benchmark.py
```

Corpus clone (shallow, `--depth 1`) vào `corpus/<lang>/` lần đầu, tái sử dụng lần sau (gitignored —
xem `.gitignore`'s `benchmarks/resolution/corpus/`). `--fresh-clone` để bắt buộc clone lại,
`--lang go,java` để chạy subset.

## 🐛 Phát hiện quan trọng nhất: bug crash thật, tìm thấy nhờ đo trên repo thật

Lần chạy đầu tiên **crash hoàn toàn** khi index redis (`UNIQUE constraint failed:
symbols.qualified_name`) — không sinh ra một số nào cho C. Root cause xác nhận bằng bisection nhị
phân (cắt dần file/dòng) + debug print tạm thời trên chính `server.h` (~4700 dòng, header trung tâm
của redis):

- C's symbol extractor coi mỗi lần **nhắc tên** một `struct` forward-declare làm tham số con trỏ
  (vd `struct redisObject *key` trong 1 function-pointer typedef) là một "symbol" occurrence riêng
  — không chỉ tại nơi struct đó thật sự được định nghĩa.
- `server.h` có hàng chục typedef kiểu `moduleType*Func` đều nhận `struct redisObject *` làm tham
  số → hàng chục "symbol" tên `redisObject` dồn vào 1 file.
- Dedup nội bộ (`extract_file_data`, `pipeline.rs`) chỉ thử **một lần** hậu tố `#{line_start}` khi
  trùng tên — đủ cho 2 bản trùng, nhưng **không đủ khi 3+ bản trùng đúng cùng (tên, dòng)** — trường
  hợp thật xảy ra ở dòng có **2 tham số cùng kiểu** trên cùng 1 dòng (vd
  `typedef void *(*moduleTypeCopyFunc)(struct redisObject *fromkey, struct redisObject *tokey, ...)`).
  Lần thử hậu tố thứ hai đụng lại chính hậu tố đã dùng ở lần đầu → INSERT lỗi UNIQUE constraint
  không được xử lý → **crash toàn bộ lần index**, không riêng gì 1 file.

**Đã sửa** (`crates/calm-core/src/indexer/pipeline.rs`, hàm `extract_file_data`): vòng lặp dedup
giờ thử hậu tố tăng dần (`#{line}`, `#{line}#2`, `#{line}#3`, …) cho đến khi thật sự unique, thay vì
thử đúng 1 lần. Test hồi quy `test_c_same_line_triple_name_collision_does_not_crash_indexing`
(`pipeline.rs`) tái tạo tối thiểu chính xác pattern này (5 typedef, dòng cuối nhắc `struct Foo` 2
lần) — xác nhận **không sửa fix thì crash y hệt**, có fix thì cả 6 occurrence đều thành symbol
row riêng biệt, không crash. Toàn bộ workspace 527 test xanh, clippy `-D warnings` sạch, fmt sạch
sau fix.

**Không sửa trong lượt này** (cố ý, ngoài phạm vi benchmark): nguyên nhân gốc (root cause đầu tiên
— coi type reference là symbol) vẫn còn — nghĩa là con số `symbols_total`/`ambiguous` cho C/C++
trong bảng dưới đây **bị nhiễu bởi noise thật** (nhiều "symbol" giả từ tham chiếu kiểu, không phải
định nghĩa thật). Đây là lý do `formal_pct`/`resolved_pct` không nên đọc tuyệt đối cho C/C++ ở bản
đo này — xem "Giới hạn" bên dưới.

## Kết quả đo lần đầu (2026-07-07, sau fix crash ở trên)

| lang | symbols | edges | resolved% | inferred% | textual% | ambiguous% | wall(s) |
|---|---:|---:|---:|---:|---:|---:|---:|
| go | 1,533 | 7,672 | 15.0% | 10.5% | 20.3% | **54.3%** | 5.6 |
| java | 227 | 254 | 16.9% | 10.2% | 44.1% | 28.7% | 5.4 |
| csharp | 596 | 318 | 40.9% | 23.0% | 16.0% | 20.1% | 5.4 |
| c | 11,238 | 40,573 | 37.1% | 0.0% | 51.5% | 11.5% | 9.2 |
| cpp | 5,052 | 51,399 | 4.8% | 0.2% | 2.5% | **92.5%** | 6.7 |
| js | 123 | 36 | 30.6% | 0.0% | 2.8% | 66.7% | 8.2 |
| php | 6,503 | 9,334 | 36.4% | 13.0% | 15.6% | 34.9% | 7.5 |
| sql | 0 | 0 | — | — | — | — | 5.5 |

`formal_pct` = 0.0% cho **mọi** ngôn ngữ — đúng như kỳ vọng, không phải bug: chưa ngôn ngữ nào có
Phase 2 SCIP provider. `overlay_match_rate` = `null` cho mọi dòng (không phải `0.0`) — có chủ đích,
xem docstring của `run_benchmark.py`.

**Cập nhật (2026-07-11, đo lại cùng lần với Phase B/C bên dưới):** SQL giờ **không còn 0 symbols**
— `language_for_extension` đã map `.sql` từ lâu (P3.3 đã triển khai xong sau bản đo 2026-07-07 ở
trên). Số đo mới trên cùng corpus sakila: 333 symbols, 637 edges (27.5% resolved, 72.2% ambiguous,
0% textual/formal). Bảng gốc phía trên giữ nguyên làm lịch sử — không sửa lại số cũ.

## Kết quả đo Phase B/C — 11 ngôn ngữ mới (2026-07-11)

Cùng corpus/phương pháp, batched vào 1 lần chạy (`--lang` không truyền → chạy toàn bộ 19 ngôn ngữ
trong `CORPORA`, ghi đè `results.json` với bộ đầy đủ):

| lang | symbols | edges | resolved% | inferred% | textual% | ambiguous% | wall(s) | commit |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| kotlin | 2,143 | 30,156 | 3.2% | 0.0% | 7.2% | **89.6%** | 8.1 | 8a390c9 |
| swift | 2,144 | 6,426 | 8.4% | 0.1% | 16.6% | 74.9% | 5.9 | e579882 |
| scala | 159 | 227 | 18.1% | 0.0% | 33.9% | 48.0% | 5.6 | e3619c1 |
| dart | 178 | 0 | — | — | — | — | 5.5 | 7a2dfb5 |
| lua | 69 | 5 | 100.0% | 0.0% | 0.0% | 0.0% | 5.6 | 359f0e2 |
| elixir | 105 | 362 | 7.7% | 0.0% | 5.2% | 87.0% | 5.5 | b2d36ba |
| haskell | 117 | 122 | 45.1% | 0.0% | 40.2% | 14.8% | 5.5 | 85490e9 |
| ocaml | 1,684 | 30,457 | 6.7% | 0.0% | 7.1% | **86.3%** | 15.5 | 93a576b |
| zig | 46 | 46 | 39.1% | 0.0% | 4.3% | 56.5% | 5.4 | fae95c8 |
| powershell | 142 | 156 | 52.6% | 0.0% | 46.2% | 1.3% | 5.6 | bbc5ac3 |
| groovy | 59 | 19 | 84.2% | 0.0% | 5.3% | 10.5% | 13.2 | 3f97e22 |

**`dart` = 0 edges lúc đo lần này (2026-07-11), đúng là 0 thật, không phải lỗi đo** — tài liệu hoá
từ Phase C ([[calm-25-language-expansion-research]]): grammar Dart không có node kind cho
call-expression, nên `walk_calls` không có gì để trích xuất — 178 symbols (class/method) vẫn được
index đầy đủ, chỉ riêng call-graph là khoảng trống đã biết trước, có chủ đích (deliberate scope cut,
không phải bug). **SUPERSEDED 2026-07-28 — xem cập nhật ngay bên dưới: đã cài đặt Dart call-edge
extraction (C3, Tier B audit); số 0 edges này không còn đúng ở build hiện tại.**

**Cập nhật 2026-07-28 — Dart call-edge extraction (C3), đo thật lại trên cùng corpus:**
tree-sitter-dart thật ra không thiếu node kind cho call — nó dùng chung `member_access`/`selector`/
`argument_part` cho CẢ call lẫn field access thuần (`a.b.c()` và `a.b.c` parse ra cùng shape,
khác nhau ở việc selector cuối có bọc `argument_part` hay không) — không phải "không có node
cho call" như ghi chú Phase C phía trên (giả định ban đầu đó sai, đã sửa). Cài một nhánh trích
xuất riêng cho Dart trong `walk_calls` (`dart_call_from_member_access`, `parser.rs`) thay vì cơ
chế `call_node_types`/`call_function_field` chung (không biểu đạt được luật "selector cuối có
phải call hay không"). Đo lại trên cùng corpus `dart-lang/args` (build release, cùng lệnh ở trên):

| | trước (0 edges) | sau (C3) |
|---|---:|---:|
| symbols | 178 | 178 (không đổi — extraction symbol vẫn luôn đúng) |
| edges | 0 | **2,166** |
| resolved% | — | 7.8% |
| textual% | — | 11.2% |
| **ambiguous%** | — | **80.9%** |

Không có `formal`/`inferred` (0% cả hai — Dart chưa có SCIP provider, và Tier-2 type_map chỉ có cho
5 ngôn ngữ Tier-0 gốc). `ambiguous%` cao (80.9%) cùng nguyên nhân gốc đã thấy ở Kotlin/OCaml/C++:
corpus args là 1 lib CLI-parser nhỏ với nhiều method tên phổ biến (`parse`, `format`, …) trùng lặp,
fan-out `MAX_CALLEE_CANDIDATES` cao — không phải lỗi của lần cài đặt này, mà là giới hạn chung của
mọi ngôn ngữ Tier-0.5 chưa có SCIP provider thật, giống mọi dòng khác trong bảng Phase B/C ở trên.

**Nợ ghi nhận, chưa cài trong lượt này:** `constructor_invocation` (dạng `List<int>.filled(...)`,
có type argument tường minh trước dot-named-constructor) — hình dạng node RIÊNG, hiếm hơn
`member_access`, cố ý bỏ qua cho lượt đầu này (xem doc comment của `dart_call_from_member_access`).
Cũng bỏ qua có chủ đích: 1 call đi qua `index_selector` giữa chuỗi (vd `list[0].foo()`) — bail
thay vì đoán sai, không silently miscount.

**`inferred%` = 0.0% cho toàn bộ 11 ngôn ngữ mới** — hợp lý, không phải lỗi: Tier-2 (`type_map`
receiver inference) hiện chỉ implement cho các ngôn ngữ Tier-0 gốc (Python/JS/TS/Java/C#) — 11 ngôn
ngữ Phase B/C đều là Tier-0.5 (tree-sitter thuần, không type_map), nên mọi đóng góp không-ambiguous
đến từ `resolved` (same-file/same-dir) hoặc rơi thẳng xuống `ambiguous`/`textual`.

**Kotlin/OCaml có `ambiguous%` cực cao (89.6%/86.3%)** — cùng nguyên nhân gốc đã thấy ở fmt (C++)
92.5%: method/hàm tên phổ biến trùng lặp (Kotlin: `builder`, `build`, `addStatement`, … của
kotlinpoet's fluent API; OCaml: `bind`/`return`/`map` của lwt's monadic combinator style) khiến
`MAX_CALLEE_CANDIDATES` fan-out cao — đúng là ứng viên formal-tier ưu tiên cao nếu có provider thật
(Kotlin đã có qua scip-java, xem Phase D.2; OCaml/Scala/Haskell/Elixir/Zig/Dart/Lua/PowerShell/
Groovy/Swift chưa có provider nào — ngoài phạm vi 25-ngôn-ngữ kế hoạch gốc, không tính vào Phase D).

**Lua/PowerShell có `ambiguous%` rất thấp (0%/1.3%)** — không phải "resolver tốt hơn", mà là corpus
nhỏ + ít trùng tên hàm: middleclass (lua) chỉ 69 symbols, hầu như mỗi method một tên riêng; posh-git
tương tự. Không nên so sánh trực tiếp "ambiguous% thấp = ngôn ngữ này resolve tốt hơn Kotlin" — kích
thước/phong cách API của corpus ảnh hưởng trực tiếp, đúng giới hạn "không có oracle" đã nêu ở mục
Giới hạn bên dưới.

## Diễn giải

- **`ambiguous` là trần chính, không phải `textual`** — đây là phát hiện quan trọng nhất, và khác
  với trực giác ban đầu (kế hoạch gốc coi `textual` là tier "chưa resolve được" chính). Trên
  fmt (cpp), 92.5% edge rơi vào `ambiguous`; go 54.3%; js 66.7%. Đây chính xác là cái mà
  `MAX_CALLEE_CANDIDATES=20` fan-out (kiến trúc thật đã xác minh ở plan §1.2) sinh ra: hàm/method
  tên phổ biến (`format`, `write`, `get`, …) trùng lặp khắp repo → resolver từ chối chọn bừa, gắn
  nhãn `ambiguous` thay vì đoán sai. Đây CHÍNH LÀ khoảng trống Phase 2 (SCIP overlay: match theo
  (file, line) chính xác, không quan tâm bao nhiêu candidate cùng tên) nhắm tới lấp — con số này là
  baseline "trước" để so khi Go/Java/C# provider (Phase 2) hạ cánh.
- **C++ (fmt) là ca cực đoan nhất, một phần do overload thật, một phần do noise đã biết** — fmt là
  thư viện header-only với hàng trăm hàm/overload tên giống nhau (`format`, `print`, `join`, …) nên
  fan-out thật sự cao; NHƯNG con số 92.5% cũng bị thổi phồng bởi chính bug "struct reference = symbol"
  mô tả ở trên (chưa sửa) — hai nguyên nhân cộng dồn, chưa tách được tỷ lệ đóng góp của mỗi cái ở bản
  đo này.

  **Cập nhật 2026-07-28 — đo thật với `scip-clang`, không phải mô phỏng:** trước khi đầu tư vào
  qualifier-gate heuristic cho C++ (namespace map, xem audit "Tier B"), đã thử trực tiếp câu hỏi
  "wire SCIP overlay thật vào corpus fmt này cho ra bao nhiêu". Recipe (mất ~10 phút, không cần build
  từ nguồn):
  ```bash
  # 1. Tải binary — có sẵn release Linux x86_64, không cần build từ nguồn
  curl -sL -o scip-clang \
    https://github.com/sourcegraph/scip-clang/releases/download/v0.4.0/scip-clang-x86_64-linux
  chmod +x scip-clang   # thêm vào PATH

  # 2. Sinh compile_commands.json — fmt dùng CMake, tự động ở đúng vị trí
  #    find_compile_commands (runner.rs) đã quét: <root>/compile_commands.json và
  #    <root>/build/compile_commands.json — cmake -B build khớp thẳng vị trí thứ 2.
  cmake -B build -DCMAKE_EXPORT_COMPILE_COMMANDS=ON -DFMT_TEST=ON -DFMT_DOC=OFF \
    -DCMAKE_BUILD_TYPE=Release corpus/cpp
  # FMT_TEST=ON quan trọng: build không bật test chỉ sinh 3 translation unit (src/*.cc);
  # bật test sinh 51 TU — đúng chỗ đa số hàm overload (format/print/join) thật sự được GỌI
  # nhiều lần với kiểu khác nhau, tức đúng nơi tree-sitter fan-out cao nhất.

  # 3. calm index bản release build với --features tier0-5,scip-overlay (đã tự động
  #    chạy overlay ngay sau khi index xong — xem ADR liên quan tới one-shot index)
  calm index --project-root corpus/cpp
  ```
  Kết quả thật (không mô phỏng), so với baseline tree-sitter-thuần ở trên:

  | | trước (tree-sitter thuần) | sau (+ scip-clang, 51 TU) |
  |---|---:|---:|
  | symbols | 5,052 | 5,299 (commit fmt mới hơn) |
  | edges | 51,399 | 79,386 |
  | **ambiguous%** | **92.5%** | **56.6%** |
  | **formal%** | **0.0%** | **41.2%** |
  | resolved% | 4.8% | 1.3% |
  | textual% | 2.5% | 0.8% |

  Log thật của lần chạy: `4824 edges upgraded to formal, 40191 fan-out siblings ruled out, 27920
  edges inserted, match_rate=0.38`. 40,191 site bị loại fan-out chỉ bằng cách wire 1 binary có sẵn —
  **lớn hơn nhiều** so với ước tính ban đầu của qualifier-gate heuristic (mô phỏng, chưa persist
  thành script: ~1,734 site được thu hẹp / 20.5% giảm tương đối). match_rate=0.38 không cao (nhiều
  occurrence của scip-clang không khớp 1-1 với site tree-sitter đã ghi — 51 TU vẫn chưa phủ hết mọi
  file fmt có, vd doc examples/godbolt snippets), nên 56.6% ambiguous còn lại chủ yếu là code NGOÀI
  51 translation unit này, không phải giới hạn của bản thân overlay.

  **Kết luận cho ưu tiên B1/B2 (C++ namespace qualifier-gate heuristic):** hạ ưu tiên xuống dưới việc
  làm cho recipe scip-clang này dễ dùng hơn cho user CALM (vd tài liệu hoá rõ trong install_hint, hoặc
  script tiện ích sinh compile_commands.json) — độ khó thực tế thấp hơn nhiều so với comment cũ trong
  `provider.rs` ngụ ý ("cannot be exercised end to end here regardless"). Heuristic namespace-gate vẫn
  còn giá trị RIÊNG cho phần code ngoài phạm vi compile_commands.json (build không đầy đủ, IDE-only
  headers, …) — không phải 0, nhưng không còn là đòn bẩy lớn nhất cho C++ nữa.
- **C# có `resolved%` cao nhất (40.9%)** — hợp lý: P1.5's type_map/ctor-inference mới thêm (commit
  `d7178b9`) hoạt động đúng thiết kế, và C# không nằm trong same-dir tier V1 (P1.3) nên toàn bộ đóng
  góp resolved ở đây đến từ type_map thật, không phải proxy thư mục.
- **`inferred%` = 0.0% cho C** — hợp lý, không phải bug: C không có method-call cú pháp
  receiver-kiểu (`obj.method()`) theo nghĩa OOP, nên Tier-2 (type_map receiver inference) không có
  gì để nâng cấp; toàn bộ đóng góp không-ambiguous của C đến từ Tier-1 (`resolved`, same-file/same-dir).
- **SQL = 0 symbols, đúng là 0 thật, không phải lỗi** — P3.3 (SQL support) chưa triển khai;
  `language_for_extension` (`lang_constants.rs`) chưa map `.sql` sang ngôn ngữ nào. Corpus sakila có
  hàng chục file `.sql` thật (đã xác nhận bằng `find`) nhưng `calm index` bỏ qua hoàn toàn — số 0 này
  là bằng chứng trực tiếp, đo được, cho "trần" SQL hiện tại, đúng tinh thần "không che số xấu" của
  `benchmarks/README.md`.

## Giới hạn

- **Không có oracle** — khác B2, đây không đo đúng/sai (precision/recall), chỉ đo phân bố tier.
  Không suy luận "resolved% cao = tốt hơn" giữa 2 ngôn ngữ khác nhau mà không xét đặc thù ngôn ngữ đó
  (C không có method call nên không có gì để "inferred", không có nghĩa C kém hơn C#).
- **1 repo/ngôn ngữ, 1 lần đo** — không phải benchmark suite đa-repo theo quy mô (19 ngôn ngữ × 1
  corpus/ngôn ngữ kể từ Phase E, chưa phải danh sách cuối — thêm ngôn ngữ/repo mới chỉ cần 1 dòng
  trong `CORPORA` của `run_benchmark.py`, không cần sửa code khác).
- **C/C++ noise đã biết, chưa tách** — xem mục bug ở trên; số `ambiguous`/`symbols_total` cho C/C++
  cao hơn thực tế một phần chưa định lượng được.
- **`.calm/config.json`'s `semantic_search.enabled=false`** — benchmark này tắt embeddings để đo
  nhanh và tránh nhiễu thời gian không liên quan; `wall_time_sec` vì vậy KHÔNG phản ánh thời gian
  `calm index` thật với embeddings bật (mặc định thật của `calm`).
- Pin theo commit SHA resolve lúc clone (ghi trong `results.json`), không phải tag release cứng —
  chạy lại sẽ luôn lấy state hiện tại vì các repo dùng shallow clone trên nhánh mặc định; muốn tái
  lập chính xác một lần đo cũ, checkout đúng SHA đã ghi.
