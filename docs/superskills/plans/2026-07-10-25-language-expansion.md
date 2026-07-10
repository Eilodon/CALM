# CALM — Kế hoạch mở rộng lên 25 ngôn ngữ (bản đã verify lại bằng code thật)

> **Ngày:** 2026-07-10 · **Trạng thái:** **PHASE B (Kotlin + Swift) ĐÃ XONG.** Tiếp theo: Phase A (registry). Xem §4-DONE ngay dưới đây để biết chi tiết + đừng làm lại.
> **Nguồn gốc:** Báo cáo nghiên cứu "Đưa CALM lên 25 ngôn ngữ" của user (dựa trên [[calm-25-language-expansion-research]] + [[calm-language-support-audit-2026-07-10]] + [[calm-gortex-adaptation-roadmap]]) + một vòng verify độc lập bằng 3 Explore agent đọc trực tiếp code ngày 2026-07-10 (không tin lại memory/docs cũ).
> **Việc đã làm trong phiên lập kế hoạch (an toàn, không cần chờ chốt):** `crates/calm-cli/Cargo.toml` thiếu feature `lang-csharp` (bug thật do agent verify tìm ra — calm-core và calm-server đều có, calm-cli thì không) → đã thêm dòng `lang-csharp = ["calm-core/lang-csharp", "calm-server/lang-csharp"]`, verify bằng `cargo check -p calm-cli --no-default-features --features lang-csharp` (xanh).

## 4-DONE — PHASE B thực thi xong (2026-07-10, cùng ngày lập kế hoạch)

**Kết quả:** Kotlin + Swift giờ có grammar thật (không còn regex/line-scan-only), đủ call-graph (bare call + receiver call), đủ test theo convention `*_real_grammar_symbols_and_calls_are_accurate`. `cargo test --workspace --features lang-kotlin,lang-swift` = 615 passed/0 failed/8 ignored. `cargo clippy --workspace --all-targets --features tier0-5,lang-kotlin,lang-swift,embeddings,scip-overlay,lsp-overlay -- -D warnings` sạch. `cargo fmt --all -- --check` sạch.

**Phát hiện quan trọng khi làm B.1 (khác dự đoán trong §1.1):**
- `tree-sitter-kotlin-ng` 1.1.0: load OK ngay, không vấn đề ABI.
- `tree-sitter-swift`: bản mới nhất trên crates.io (0.7.3, 06/2026) là **ABI 15** — KHÔNG tương thích runtime hiện tại (0.24.7, chỉ nhận ABI 13-14). Xác nhận trực tiếp bằng cách download+grep `LANGUAGE_VERSION` trong `src/parser.c` của nhiều version: 0.7.0 (02/2025) là ABI 14 (bản mới nhất còn ABI 14), 0.7.1 (06/2025) nhảy lên ABI 15. **Đã pin `=0.7.0`, không phải bản mới nhất** — comment giải thích đầy đủ trong `Cargo.toml`. Bài học: "grammar mới nhất trên crates.io" ≠ "grammar tương thích ABI" — luôn tự grep `parser.c` trước khi pin, đừng tin số version suông.

**Phát hiện quan trọng khi làm B.3 (AST-dump thật, `node-types.json` + `to_sexp()`):**
- Kotlin: KHÔNG có node kind `"interface_declaration"` — `interface Foo {}` parse thành `class_declaration` bình thường. KHÔNG có field nào trên `call_expression` cả (`fields: []`) — field `"callee"` cũ (chưa từng test) là bịa.
- Swift: KHÔNG có `"struct_declaration"`/`"enum_declaration"` — `struct`/`enum` đều parse thành `class_declaration` (mất phân biệt struct/enum/class trong `SymbolKind`, quyết định có chủ đích, xem comment trong code). `call_expression` cũng `fields: []`.
- **Xử lý chung cho cả 2:** thêm sentinel `call_function_field: "$first_child"` — `walk_calls` (`parser.rs`) lấy thẳng child đầu tiên của call node (bất kể kind gì: bare `identifier` hay `navigation_expression`), rồi để `split_receiver_callee` tách receiver/callee từ TEXT thô của node đó (đã hoạt động sẵn, không cần logic mới). An toàn cho 16 ngôn ngữ cũ: sentinel string không trùng field/kind name nào khác.
- File đổi: `Cargo.toml` (workspace, +2 dep pin), `crates/calm-core/Cargo.toml` (+2 optional dep, sửa `lang-kotlin`/`lang-swift` từ no-op thành `dep:` thật), `indexer/lang_constants.rs` (sửa hẳn 2 arm kotlin/swift), `indexer/parser.rs` (`parse_tree` +2 arm thật, `walk_calls` +1 sentinel fallback, +4 test mới: `test_tier0_5_grammar_loads_kotlin/swift`, `test_kotlin/swift_real_grammar_symbols_and_calls_are_accurate`).
- **Bonus fix ngoài kế hoạch (do user chủ động yêu cầu sau khi phát hiện `cargo fmt -- <2 file>` vô tình format lại TOÀN BỘ workspace):** user quyết định giữ luôn phần fmt ngoài ý muốn (dọn fmt-drift cũ có sẵn, đã verify 100% chỉ đổi whitespace) + yêu cầu kiểm tra clippy cho chắc → phát hiện 3 lỗi `collapsible_if` có sẵn từ trước (rustc/clippy 1.96 mới hỗ trợ let-chains, không liên quan Kotlin/Swift) trong `calm-server/src/tools/inspect.rs` → đã sửa theo đúng gợi ý của clippy. Full `cargo clippy --workspace --all-targets --all-features -- -D warnings` giờ sạch hoàn toàn (trước đây luôn đỏ do 3 lỗi này, kể cả trên `main` sạch, đã verify bằng `git stash`).

**Audit "hoàn thiện triệt để" (2026-07-10, cùng ngày, sau khi user yêu cầu kiểm tra kỹ lại) — tìm và sửa thêm 1 bug thật + dọn docs stale:**
- **Bug thật, tìm bằng end-to-end hand-index với binary thật (không phải unit test)** — đúng phương pháp đã có tiền lệ trong lịch sử repo này (xem [[calm-language-support-audit-2026-07-10]]): tạo fixture Kotlin+Swift thật, build release binary, `calm index`, đọc trực tiếp `.calm/index.db` qua sqlite3. Phát hiện: `object_declaration` (Kotlin `object Singleton {}`) và `protocol_declaration` (Swift `protocol Named {}`) không có arm nào trong `node_kind_to_symbol_kind` (`parser.rs:35-116`) — rơi vào default fallback, bị gán nhầm `SymbolKind::Function` thay vì `Class`/`Interface`. Unit test cũ chỉ check tên symbol (`shallow_names`), không check `kind`, nên không bắt được. **Đã sửa:** thêm 2 arm (`"object_declaration" => Class`, `"protocol_declaration" => Interface`), verify lại bằng chính binary thật — `Singleton` giờ đúng `class`, `Named` giờ đúng `interface`. Đã thêm assertion `kind` vào cả 2 test `*_real_grammar_symbols_and_calls_are_accurate` để khoá lại, không dựa mỗi vào tên.
- **Docs stale đã sửa:** `README.md` (dòng nói Kotlin/Swift "regex/line-scan by default... on by default via tier0-5" — sai vì 2 ngôn ngữ này KHÔNG bundle vào tier0-5); `docs/comparison.md` (2 chỗ: bảng so sánh ghi "8 Tier-0.5" trong khi README ghi "9" — inconsistency có từ trước, đã sửa thành 9; ví dụ "Kotlin/Swift/PHP không có call-graph" ở mục "khi nào không nên chọn calm" — PHP đã có call-graph từ lâu, Kotlin/Swift giờ cũng có, đã đổi ví dụ sang đúng nhóm ngôn ngữ thật sự chưa hỗ trợ). Memory `calm-language-support-audit-2026-07-10` (rating "D — Kotlin, Swift: NO tree-sitter grammar exists... permanently" — đã thêm ghi chú SUPERSEDED, giữ nguyên bản gốc bên dưới làm lịch sử).
- **Đã kiểm tra không cần sửa:** `crates/calm-core/src/fitness.rs`'s SQL list `(c,cpp,csharp,ruby,shell,kotlin,swift,php)` cho `high_complexity_pct_note` — comment nói "no real parse tree" nhưng hành vi thật (complexity luôn =1) là do THIẾU `branch_node_kinds` entry (đúng cho toàn bộ 8 ngôn ngữ này, không riêng kotlin/swift, có từ trước) — hành vi vẫn đúng sau Phase B, chỉ có comment hơi lỏng (pre-existing, không phải do Phase B). `docs/adr/0004-lsp-optional-confidence-upgrade.md`'s ghi chú Kotlin/Swift sourcekit-lsp/kotlin-language-server maturity — trục khác (LSP formal-tier server, Phase D), không bị Phase B ảnh hưởng.
- **1 test flaky phát hiện khi chạy full suite (`db::instance_lock::tests::first_acquirer_succeeds_second_fails_until_first_drops`, fail 1/2 lần khi máy đang tải nặng do build release song song)** — re-run riêng 5/5 lần đều pass; đây là lock-timing test nhạy tải máy, đã có tiền lệ y hệt trong lịch sử repo (xem watcher_integration.rs's flaky note trong [[calm-gortex-adaptation-roadmap]]) — không liên quan Phase B, không sửa file này.
- **Kết luận audit:** Phase B giờ mới thật sự "hoàn thiện triệt để" — code đúng + test khoá đúng + docs nhất quán + verify bằng cả unit test và binary thật.
- Đừng làm lại B — nếu cần sửa, xem commit tương ứng (chưa commit tại thời điểm ghi chú này, xem `git log`/`git status` để biết chính xác).

---

## 0. Mục tiêu

Đưa CALM từ 16 ngôn ngữ + markdown + SQL lên 25 ngôn ngữ, giữ nguyên triết lý silent-degrade (thiếu grammar/binary ngoài → vẫn chạy, chỉ rơi tier thấp hơn) và nguyên tắc "không copy module N lần — tổng quát hoá thành bảng data-driven" đã áp dụng thành công cho `ScipProvider` (`crates/calm-core/src/scip/provider.rs`).

---

## 1. Sự thật đã verify lại — sai lệch so với báo cáo gốc

Phiên sau đọc mục này thay vì tự khảo sát lại. Mọi dòng dưới đều có file:line thật, đọc trực tiếp 2026-07-10 (không phải suy ra từ comment/docs).

### 1.1 Kotlin/Swift — rào cản còn NHẸ hơn báo cáo gốc mô tả

- **Không tồn tại dependency Kotlin/Swift nào cả** (không phải "đã gỡ vì incompatible" — chưa từng khai báo). `Cargo.lock` xác nhận 0 entry `tree-sitter-kotlin`/`tree-sitter-swift`. Comment ở `Cargo.toml:25-27` và `crates/calm-core/Cargo.toml:73-74` chỉ là prose giải thích một sự **vắng mặt**, không phải một dependency bị lỗi đang tồn tại.
- `lang-kotlin = []` / `lang-swift = []` (`crates/calm-core/Cargo.toml:75-76`) là feature no-op — không gate gì cả, không có `#[cfg(feature = "lang-kotlin")]` nào trong toàn bộ `.rs` source.
- **Tin tốt:** `get_lang_constants` (`indexer/lang_constants.rs:182-221`) ĐÃ CÓ sẵn struct `LangConstants` đầy đủ cho cả `"kotlin"` và `"swift"` — nhưng là **dead code** vì `parse_tree` (`indexer/parser.rs:106-137`) không có match arm nào cho 2 ngôn ngữ này (không phải cfg-gated-nhưng-tắt như ruby/php — là **không tồn tại arm**), nên `extract_symbols_from_tree`/`walk_symbols` không bao giờ được gọi cho kotlin/swift trong pipeline thật. Những field này **chưa từng được verify bằng grammar thật** — phải coi là "viết mù, chưa test" khi làm Phase B, không phải "viết cho grammar cũ, cần sửa" như báo cáo gốc suy đoán.
- `strip_modifiers` (`parser.rs:1688`) đã có arm riêng cho `kotlin`/`swift` (cùng nhóm với csharp/php/cpp|c) — tái dùng được ngay, không cần viết lại.
- Fallback shallow hiện tại (`detect_kotlin`/`detect_swift`, `parser.rs:1929-1974`, đã có `test_shallow_kotlin`/`test_shallow_swift` ở `parser.rs:3299-3319`) **không dùng crate `regex`** — chỉ `str::strip_prefix` quét từ khoá theo dòng. Đây là fallback an toàn cần giữ nguyên (khi grammar ABI lệch, phải rơi về đây, không panic) — Phase B chỉ *thêm* đường tree-sitter thật phía trên, không xoá đường này.

### 1.2 "8 điểm dispatch" — undercounted, thật ra là 11-13 điểm / 6 file

Đọc lại bằng cách trace một ngôn ngữ Tier-0.5 có sẵn (Ruby) từ đầu đến cuối, số điểm phải sửa để thêm 1 ngôn ngữ đầy đủ:

| # | File:line | Việc |
|---|---|---|
| 1 | `Cargo.toml:43` | version pin workspace (`=x.y.z` EXACT, không phải caret — xem §1.4) |
| 2 | `crates/calm-core/Cargo.toml` (dep) | `optional = true` |
| 3 | `crates/calm-core/Cargo.toml` (`[features]`) | `lang-X = ["dep:tree-sitter-X"]` |
| 4 | `crates/calm-core/Cargo.toml` (`tier0-5 = [...]`) | thêm vào bundle |
| 5 | `crates/calm-server/Cargo.toml` | passthrough |
| 6 | `crates/calm-cli/Cargo.toml` | passthrough (**đây chính là chỗ csharp bị thiếu, đã fix ở đầu file này**) |
| 7 | `indexer/lang_constants.rs::get_lang_constants` | match arm — struct `LangConstants` |
| 8 | `indexer/lang_constants.rs::language_for_extension` | match arm — extension→lang |
| 9 | `indexer/parser.rs::parse_tree` | match arm, `#[cfg(feature = "lang-X")]` |
| 10 | `indexer/parser.rs::is_comment_line` | arm nếu cú pháp comment khác `#`/`//` |
| 11 | `indexer/parser.rs::detect_shallow` + `detect_X` riêng | fallback khi grammar không load |
| 12 | `indexer/parser.rs` test module | `test_tier0_5_grammar_loads_X` (bắt buộc — xem §1.5) |
| 13 | `indexer/parser.rs` test module | `test_X_real_grammar_symbols_and_calls_are_accurate` (bắt buộc — xem §1.5) |

Còn 3 điểm **tuỳ chọn theo độ chính xác muốn có** (Ruby bỏ qua được, nhưng c/cpp/csharp/php có dùng): `branch_node_kinds` (cyclomatic complexity), `decorator_node_kinds`, `extract_type_map_from_tree`'s `binding_kinds` (type inference cho resolver Tier-2).

**Kết luận quan trọng cho Phase A:** 4 trong 6 file ở trên (#1, #2-4, #5, #6) là Cargo feature-flag plumbing — đây là hạn chế cấu trúc của Cargo workspace (mỗi crate tiêu thụ phải khai báo lại feature riêng để passthrough), **không thể gộp lại bằng một registry Rust-side**. Một `LANG_REGISTRY` chỉ có thể gộp #7-13 (source-level dispatch, đúng 1 crate `calm-core`). Việc thêm ngôn ngữ mới **vẫn sẽ luôn cần sửa tối thiểu 4 file Cargo.toml**, dù registry có tồn tại hay không — đây là giới hạn thật, kế hoạch phải nói rõ với user để không kỳ vọng sai.

### 1.3 Resolver — claim của báo cáo đúng nhưng cần 1 sửa nhỏ

`rebuild_graph` (`indexer/pipeline.rs:557-563`) xác nhận đúng: nhận đúng 3 map ngôn ngữ-riêng (`crate_map: &CrateMap`, `psr4: &Psr4Map`, `namespace_map: &NamespaceMap`), gọi từ đúng 2 call site (`pipeline.rs:1416-1425` full index, `1584-1593` incremental).

**Sửa 1 điểm:** báo cáo nói "same-dir tier dùng chung cho mọi ngôn ngữ" — thực tế tier same-dir (`pipeline.rs:778-805`) và same-namespace (`806-827`) sống trong **cùng một hàm** (không copy-paste per-language, đúng phần "generic machinery" báo cáo muốn nói) nhưng có `matches!` guard chỉ *bật* cho `go|java|c|cpp` (same-dir) và `csharp` (same-namespace) — Rust/Python/JS/TS bị loại có chủ đích (comment giải thích: đã resolve qua import_map/type_map ở bước extract). Ý nghĩa cho 9 ngôn ngữ mới ở Phase C: **mặc định KHÔNG được hưởng tier same-dir/same-namespace** trừ khi ta chủ động thêm vào guard đó — cần quyết định per-language khi đo benchmark thấy ambiguous% cao (không nên thêm mù, thêm sau khi có số).

### 1.4 Chính sách pin version — luật cứng đã có, PHẢI tuân theo cho ngôn ngữ mới

`Cargo.toml:29-40` đã ghi rõ và có tiền lệ hồi hộp thật (`tree-sitter-c-sharp` 0.23.1→0.23.5 vẫn "^0.23" nhưng đổi ABI 14→15, bị `parse_tree()`'s `.ok()?` nuốt lỗi âm thầm — chỉ tests `test_tier0_5_grammar_loads_*` bắt được). Runtime hiện tại: `tree-sitter = 0.24` → resolve `0.24.7`, hỗ trợ ABI 13-14. **Luật:** mọi grammar Tier-0.5 mới PHẢI pin EXACT (`=x.y.z`), không dùng caret, và PHẢI có `test_tier0_5_grammar_loads_X` khoá lại ABI đang dùng.

**Rủi ro cụ thể cho danh sách 25 — đã research kỹ thêm 1 vòng (2026-07-10, theo đúng yêu cầu user, verify trực tiếp trên GitHub/crates.io, không suy đoán):**

- `tree-sitter-perl` v1.1.2 dependency THẬT (normal, không phải dev) là **cả 2**: `tree-sitter = "^0.26.3"` VÀ `tree-sitter-language = "^0.1.6"` — khác các grammar hiện đại khác (chỉ cần shim). Perl's Rust wrapper code dùng thẳng API của `tree-sitter` 0.26, không chỉ ABI-shim.
- **Tin tốt, đã verify (giải đáp trực tiếp lo ngại của user "tier cũ chỉ tương thích 0.23x"):** đọc thẳng `api.h` trên GitHub 3 tag — `TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION` = **13** không đổi liên tục từ v0.20.0 → v0.24.7 (runtime hiện tại, `LANGUAGE_VERSION=14`) → v0.25.0 (`LANGUAGE_VERSION=15`) → v0.26.1 (`LANGUAGE_VERSION=15`). Nghĩa là: bump runtime lên 0.25.x/0.26.x **KHÔNG làm rớt** 7 grammar Tier-0.5 đang pin ABI 13/14 (c/cpp/ruby/php/csharp/shell/r) — họ vẫn nằm trong sàn tương thích 13-15 của runtime mới. Đây khác với suy nghĩ ban đầu của cả báo cáo gốc và bản kế hoạch trước — "re-verify ABI cho 7 grammar" không phải rủi ro chính.
- **Rủi ro THẬT, lớn hơn nhiều, đã xác nhận bằng dependency API của crates.io:** `tree-sitter-stack-graphs` 0.10.0 (crate CALM đang dùng cho Tier-0 formal resolution của Python/TypeScript/Java/JavaScript — khác hẳn 7 grammar Tier-0.5) có dependency THẬT `tree-sitter = "^0.26.3"` — mà `tree-sitter-stack-graphs = "^0.24"` (nghĩa là `>=0.24.0, <0.25.0` theo luật semver pre-1.0 của Rust). **0.10.0 (12/2024) là version MỚI NHẤT** — không có version nào mới hơn hỗ trợ tree-sitter 0.25+. Vì `stack-graphs` upstream đã **archived 2025-09-09** (đã biết từ trước, xem [[calm-gortex-adaptation-roadmap]]), **sẽ không có version mới nào của `tree-sitter-stack-graphs` support 0.25/0.26 nữa** — bump runtime để unlock Perl đồng nghĩa phải fork+tự maintain vĩnh viễn `tree-sitter-stack-graphs` (ảnh hưởng cả 4 ngôn ngữ Tier-0 formal đang chạy ổn), không phải "re-verify 7 grammar" như ước tính ban đầu — việc LỚN HƠN đáng kể so với ước tính cũ.
- **Đường thay thế đã cân nhắc:** vendor thẳng `parser.c` của Perl (không dùng crate `tree-sitter-perl` đã publish, chỉ lấy source generate bằng 1 `tree-sitter-cli` cũ hơn 0.25, tự viết wrapper mỏng chỉ cần `tree-sitter-language` shim — đúng kiểu báo cáo gốc gợi ý cho dockerfile/vue/svelte). Khả thi về lý thuyết nhưng tốn công vượt xa budget ~1 ngày/ngôn ngữ của Phase C (không có crate ergonomics, phải tự maintain khi grammar upstream update).
- **Kết luận/khuyến nghị:** swap Perl sang 1 trong 4 phương án dự bị, KHÔNG làm spike bump runtime (chi phí thật đã xác nhận là fork-và-maintain-vĩnh-viễn 1 dependency archived, không phải re-verify ABI đơn giản). Xem §9 Q2 (đã cập nhật).

### 1.5 Test convention `*_real_grammar_symbols_and_calls_are_accurate` — đã có tiền lệ 2 lần, không phải ý mới

Trace git log xác nhận: convention này **ra đời từ `4439d3a`** (2026-07-03, "pin Tier-0.5 grammars to ABI-compatible versions") cho shell/php/csharp/c — **7 ngày trước** bug Ruby. Bug Ruby (`9274a4c`, `call_node_types: "method_call"` → phải là `"call"`) là **lần lặp lại thứ 2** của đúng loại lỗi này (grammar path âm thầm fallback / sai node-kind, zero test bắt được) — không phải lần đầu tiên phát hiện ra cần test này. Kết luận: checklist Phase C bắt buộc test này không phải một ý tưởng rút ra từ 1 lần fix — nó là một quy luật đã tái diễn 2 lần, độ tin cậy cao.

**Sửa 1 điểm nhỏ:** test cho Ruby thực ra được commit **44 phút TRƯỚC** commit fix chính (`8b124e1` lúc 12:49, `9274a4c` lúc 13:33, cách nhau 3 commit khác không liên quan) — bị gộp nhầm vào một commit message không liên quan (`feat(indexer): index markdown ATX headings`). Bài học quy trình: khi làm Phase C, giữ test và fix/feature trong CÙNG một commit rõ ràng, đừng lặp lại kiểu gộp nhầm này.

### 1.6 "AST-dump method" — là quy ước dùng-rồi-xoá, KHÔNG phải tool có sẵn

Xác nhận: không có `[[bin]]` nào tên `dump-ast` hay tương tự, không có hàm `dump_ast`/`to_sexp`/`print_tree` nào tồn tại lâu dài trong repo. Phương pháp thật (`docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md:424`) là viết một `#[test] fn dump_ast_X() { eprintln!("{:#?}", tree.root_node().to_sexp()) }` tạm, chạy `cargo test dump_ast_X -- --nocapture`, đọc output, rồi **xoá test đó** trước khi commit. Kế hoạch Phase B/C phải ghi rõ bước này là "viết-chạy-xoá", không giả định có sẵn công cụ để gọi.

### 1.7 Benchmark `benchmarks/resolution/` — không có oracle, không có Ruby/Python/Rust

Đo `edge_confidence` histogram (`formal/resolved/inferred/textual/ambiguous/unresolved`) trên 8 repo OSS thật (`run_benchmark.py:140-167`) — **không so với ground truth nào cả** (README tự nói rõ). Benchmark có oracle thật (precision/recall so với `rust-analyzer scip`) là `benchmarks/b2_call_graph_quality/`, **chỉ Rust, chỉ self-repo** — không phải cùng benchmark, không nên nhầm 2 cái khi viết corpus cho 9 ngôn ngữ mới ở Phase C. "Benchmark corpus" cho ngôn ngữ mới = thêm 1 dòng vào bảng `benchmarks/resolution/README.md` + 1 entry corpus (đo tier-distribution, không phải accuracy tuyệt đối) — set kỳ vọng đúng cho user.

### 1.8 LSP overlay v2 — tổng quát ở protocol, KHÔNG tổng quát ở binary-discovery

`RustConfig.lsp` (`config.rs:97-108`) là field `.lsp` DUY NHẤT tồn tại — không có `GoConfig.lsp`/`CSharpConfig.lsp`. `LspClient` (`lsp/client.rs`, 432 dòng) đúng là generic (JSON-RPC/Content-Length framing, `lsp_types` structs, không có gì rust-analyzer-specific ngoài 1 default `.unwrap_or("rust")` nhỏ ở `open_file`). NHƯNG:
- `resolve_binary` mà LSP overlay tái dùng (`overlay.rs:28`, thực chất gọi `scip/runner.rs::resolve_binary`, `runner.rs:22-37`) là **100% rust-analyzer-specific** — hardcode literal `"rust-analyzer"`, `rustup which rust-analyzer`, scan `~/.vscode/extensions/rust-lang.rust-analyzer-*`. Không dùng lại được cho gopls/clangd.
- `has_any_rust_files` (`overlay.rs:356-364`) hardcode `WHERE language = 'rust'`.
- `refresh()` (`overlay.rs:86`) hardcode đọc `config.rust.lsp`.
- **Không có `LspProvider` table** như `ScipProvider` (`scip/provider.rs:46-71`) đã có cho SCIP — đây là generalization CẦN LÀM ở Phase D trước khi wire clangd/gopls/kotlin-lsp, không phải "chỉ cần thêm 1 config entry" như báo cáo gốc nói.
- Tin tốt: `formal.rs`/stack-graphs (`resolver/formal.rs:467`) xác nhận chỉ truyền `&str` qua biên crate (không có `tree_sitter::Tree`/`Node` nào xuyên biên) — không có rủi ro version-skew giữa stack-graphs và tree-sitter runtime. `Cargo.lock` xác nhận 1 version `tree-sitter` duy nhất (0.24.7) cho toàn workspace, kể cả qua `tree-sitter-stack-graphs`.
- Provenance (`formal_source TEXT`, `db/schema.rs:270`) đã đúng thiết kế — 3 writer (`pipeline.rs:889-892`='stack_graphs', `scip/ingest.rs:162`='scip', `lsp/overlay.rs:165-167`='lsp') dùng chung `EdgeConfidence::Formal` + field text riêng để phân biệt nguồn, đúng nguyên tắc "không mislabel tier" báo cáo lo ngại — nguyên tắc này ĐÃ được implement từ ADR-0004, không cần làm thêm, chỉ cần theo đúng convention (thêm nguồn mới = thêm 1 string literal, không đổi schema).

### 1.9 CI — không có test-per-feature-flag nào cả

`.github/workflows/ci.yml` chỉ test bundle mặc định (`tier0-5` gộp cả 7 ngôn ngữ) + 1 job tách riêng cho feature `embeddings` (không liên quan ngôn ngữ). Không có job nào build/test `--features lang-X` đơn lẻ. Đây chính xác là lỗ hổng loại đã sinh ra bug calm-cli-thiếu-lang-csharp (báo cáo #1.2) — nếu thêm 9-11 ngôn ngữ mới × 3 crate mà không có check parity, xác suất lặp lại bug này cao. Đưa vào Phase A/E: 1 script/test nhỏ so khớp tên feature `lang-*` giữa 3 file Cargo.toml, không cần build-matrix N-way tốn CI minutes.

---

## 2. Danh sách 25 ngôn ngữ — đề xuất (đã điều chỉnh sau verify)

**Top-12 (formal-tier target):** Python, TypeScript, JavaScript, Rust, Go, Java, C#, C++, C, PHP, Ruby, **Kotlin** *(mới, Phase B)*.

**13 còn lại (good-tier target):** **Swift** *(mới, Phase B)*, SQL *(đã có)*, Shell/Bash *(đã có)*, R *(đã có)*, **Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy** *(7 ngôn ngữ mới + Groovy thay Perl — đã chốt §9 Q2, xem lý do kỹ thuật ở §1.4)*.

→ Việc mới thật sự cần làm: **2 ngôn ngữ Phase B** (Kotlin, Swift) + **9 ngôn ngữ Phase C** (Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, **Groovy**). SQL/Shell/R đã tồn tại, không tính vào effort mới. Perl bị loại khỏi danh sách 25 (không phải "để sau" — chi phí unlock thật là fork+maintain vĩnh viễn `tree-sitter-stack-graphs` đã archived, xem §1.4).

---

## 3. Thứ tự triển khai — đề xuất khác báo cáo gốc, có lý do

Báo cáo gốc: A (registry) → B (Kotlin/Swift) → C (9 ngôn ngữ) → D (formal upgrade) → E (benchmark/CI).

**Đề xuất của kế hoạch này: B → A → C → D → E.** Lý do:
1. Phase B (Kotlin/Swift) là thuần bổ sung (additive-only) — không sửa dispatch của 16 ngôn ngữ hiện có, rủi ro regression ≈ 0. Làm trước để có **1 lần thực chiến thật** "thêm ngôn ngữ end-to-end" trước khi thiết kế registry — tránh thiết kế registry mù rồi phát hiện thiếu field khi làm Phase C.
2. Phase A (registry) là bước rủi ro/blast-radius LỚN NHẤT trong cả kế hoạch (sửa dispatch của toàn bộ 16 ngôn ngữ đang chạy production). Nên làm sau khi đã có thêm 1-2 ngôn ngữ mới (Kotlin/Swift) để biết chắc shape nào thực sự cần, tránh phải sửa registry 2 lần.
3. Cả 2 thứ tự đều hợp lý — đây là judgment call, không phải đúng/sai tuyệt đối. Xem §9 Q1 để chốt.

---

## 4. PHASE B — Kotlin + Swift (làm trước, quick win an toàn)

**B.1 — Xác nhận crate trên crates.io ngay trước khi pin** (memory nói đã verify 2026-07-10 cùng ngày, nhưng phải tự tay re-confirm version + dep-kind — đừng tin lại số cũ mù):
- `tree-sitter-kotlin-ng` — kiểm tra version mới nhất, xác nhận dependency thật là `tree-sitter-language ^0.1` (không phải `tree-sitter` trực tiếp) qua tab Dependencies của crates.io.
- `tree-sitter-swift` — tương tự, xác nhận dep thật không phải `tree-sitter ^0.26+` giống Perl.
- Nếu 1 trong 2 hoá ra có dep thật không tương thích ABI 13-14 → dừng, báo user, không tự bump runtime (xem §1.4).

**B.2 — Thêm dependency + feature (6 file, theo bảng §1.2, dòng 1-6):** pin EXACT version, `lang-kotlin`/`lang-swift` từ `[]` thành `["dep:tree-sitter-kotlin-ng"]`/`["dep:tree-sitter-swift"]`, KHÔNG thêm vào bundle `tier0-5` (giữ tách riêng, đúng pattern hiện tại — 2 ngôn ngữ mới, mới lần đầu có grammar, nên để opt-in riêng ít nhất 1 chu kỳ trước khi bundle mặc định).

**B.3 — AST-dump xác minh (viết-chạy-xoá, §1.6):** viết `#[test] fn dump_ast_kotlin()`/`dump_ast_swift()` tạm trong `parser.rs`, parse vài file mẫu thật (class có method, có call, có receiver `this.`/`self.`), in `to_sexp()`, đối chiếu với các field trong `LangConstants` "kotlin"/"swift" đã có sẵn ở `lang_constants.rs:182-221` (call_node_types, class_node_types, call_function_field, v.v. — coi là **giả định chưa verify**, không phải "viết đúng nhưng cho grammar cũ"). Sửa mọi field sai. Xoá test tạm.

**B.4 — Thêm `parse_tree` match arm** (`indexer/parser.rs:106-137`, theo pattern cfg-gated giống ruby/php ở dòng 115-130) cho kotlin/swift — hiện tại 2 ngôn ngữ này hoàn toàn không có arm (khác các Tier-0.5 khác vốn có arm nhưng tắt bằng cfg).

**B.5 — Test bắt buộc (§1.5 convention):** `test_tier0_5_grammar_loads_kotlin`/`_swift` (khoá ABI) + `test_kotlin_real_grammar_symbols_and_calls_are_accurate`/`test_swift_real_grammar_symbols_and_calls_are_accurate` (khoá call-graph, giữ fix và test **trong cùng 1 commit**, học từ sai lầm nhỏ ở §1.5).

**B.6 — Giữ nguyên fallback shallow** (`detect_kotlin`/`detect_swift`) làm safety net khi ABI lệch trong tương lai — không xoá.

**B.7 — Chạy đủ `cargo test --workspace`, `clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`** trước khi coi Phase B xong.

---

## 5. PHASE A — Đăng ký ngôn ngữ tập trung (registry) — sau khi có kinh nghiệm Phase B

**Phạm vi đúng (đã right-size theo §1.2):** chỉ gộp phần source-level trong `calm-core` (mục #7-13 ở bảng §1.2) — KHÔNG cố gộp phần Cargo feature-flag (mục #1-6, giới hạn cấu trúc Cargo, không gộp được).

**A.1 — Hygiene trước (rẻ, làm ngay, độc lập với A.2):**
- Thêm 1 script/test nhỏ (`scripts/check_lang_feature_parity.sh` hoặc 1 `#[test]` đọc 3 file Cargo.toml bằng `toml` crate) so khớp mọi feature `lang-*` tồn tại đồng thời ở `calm-core`, `calm-server`, `calm-cli` — bắt được đúng loại bug vừa fix ở đầu file này, tự động, trước khi có 9-11 ngôn ngữ mới nhân số lượng chỗ có thể thiếu lên gấp 3-4 lần.

**A.2 — Consolidation (rủi ro cao, làm từng bước, KHÔNG big-bang):**
- Thiết kế `struct LanguageSpec` chứa: `extensions: &[&str]`, `constants: fn() -> LangConstants` (hoặc const), `ts_language: Option<fn() -> tree_sitter::Language>` (None khi không có grammar/flag tắt), `branch_node_kinds`, `decorator_node_kinds`, `comment_style`, `modifier_keywords`, `shallow_detect: fn(&str) -> Option<(String, SymbolKind)>`.
- Migrate **một dispatch function tại một thời điểm** (ví dụ: chỉ `language_for_extension` trước, chạy full test suite xanh, commit; rồi `is_comment_line`, v.v.) — không rewrite 6 hàm cùng lúc. Mỗi bước phải giữ `test_tier0_5_grammar_loads_*` và `*_real_grammar_symbols_and_calls_are_accurate` xanh cho cả 16 ngôn ngữ hiện có (đây là bộ test khoá hành vi hiện tại, dùng làm lưới an toàn cho refactor).
- Cân nhắc dùng macro hoặc `phf`/`once_cell`-built static map để đảm bảo KHÔNG có ngôn ngữ nào "quên" 1 dispatch point — đây chính là root cause của bug Ruby và của việc c/cpp/csharp/php có `branch_node_kinds`/`decorator_node_kinds` còn ruby/shell/r thì không (inconsistency hiện tại, không hẳn là bug nhưng là chính xác loại rủi ro registry nên triệt tiêu).
- **Không làm trong Phase A này** (hoãn, giá trị thấp so với effort): content-based detection kiểu Gortex (`.h` probe C/C++/ObjC, basename map Makefile/Dockerfile, shebang) — không ngôn ngữ nào trong danh sách 25 cần basename/shebang detection thật sự (không có Makefile/Dockerfile trong danh sách), và `.h` probe chỉ có giá trị nếu Objective-C được chọn thay Perl ở §9 Q2 — quyết định sau, không block Phase A.

---

## 6. PHASE C — 9 ngôn ngữ mới, checklist chuẩn 1 ngôn ngữ/lần

Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, **Groovy** (thay Perl — chốt §9 Q2). Mỗi ngôn ngữ theo đúng checklist (rút ra trực tiếp từ Phase B + §1 findings, không phải checklist mới):

1. Re-confirm crate + dep-kind trên crates.io ngay trước khi pin (như B.1) — mỗi ngôn ngữ tự kiểm, đừng tin bảng compat cũ trong memory mù.
2. Thêm entry vào `LanguageSpec` registry (nếu Phase A xong) hoặc 6+ dispatch point cũ (nếu chưa) — pin EXACT version.
3. AST-dump viết-chạy-xoá (§1.6) trên 2-3 file mẫu thật của ngôn ngữ đó (có class/function, có call thường, có call qua receiver/self nếu ngôn ngữ có khái niệm đó).
4. `test_tier0_5_grammar_loads_X` + `test_X_real_grammar_symbols_and_calls_are_accurate` — cùng commit với fix.
5. Đo benchmark tier-distribution (§1.7 — không phải accuracy tuyệt đối): thêm 1 repo OSS nhỏ thật vào `benchmarks/resolution/README.md` + corpus.
6. Nếu ambiguous% cao và ngôn ngữ có cấu trúc thư mục rõ (package/module theo dir) → cân nhắc thêm vào `matches!` guard của same-dir tier (`pipeline.rs:778-805`, xem §1.3) — quyết định SAU khi có số, không thêm mù.
7. `cargo test --workspace` + `clippy -D warnings` + `fmt --check` xanh trước khi merge.

**Ước tính lại effort:** báo cáo gốc nói "~1 ngày/ngôn ngữ" — con số này hợp lý **CHỈ SAU KHI Phase A xong** (registry giảm bước 2 từ 6-13 điểm xuống 1-2 điểm). Nếu làm Phase C trước Phase A (không theo thứ tự đề xuất ở §3), mỗi ngôn ngữ sẽ tốn thêm ~0.5-1 ngày do phải tự tay rải qua nhiều file.

---

## 7. PHASE D — Nâng formal tier cho top-12

**D.0 — (mới, không có trong báo cáo gốc, nhưng bắt buộc trước D.3/D.4):** tổng quát hoá LSP overlay thành `LspProvider` table, mirror đúng shape `ScipProvider` (`scip/provider.rs:46-71`) đã chứng minh hiệu quả:
- `resolve_binary: fn(...)` riêng cho rust-analyzer (giữ nguyên logic cũ) / gopls / clangd.
- `LspConfig` chuyển từ field riêng của `RustConfig` thành 1 field chung theo ngôn ngữ (map hoặc field trên mỗi `XConfig`).
- `has_any_rust_files` → `has_any_lang_files(lang)`.
- `refresh()` nhận provider/lang tham số thay vì hardcode `config.rust.lsp`.
- `lsp_refresh` MCP tool (`calm-server/src/tools/lsp.rs`) update mô tả + wiring cho đa ngôn ngữ.
- Sửa nhỏ: `LspClient::open_file`'s default `.unwrap_or("rust")` → nhận `language_id` thật theo ngôn ngữ.

**D.1 — scip-ruby (Sorbet, Shopify)** provider mới cho Ruby — cảnh báo: Sorbet vốn thiết kế cho codebase có type annotation; cần research độ chính xác thật trên Ruby không có annotation trước khi cam kết "formal tier" cho Ruby.

**D.2 — Kotlin arm** qua `scip-kotlinc`/toolchain `scip-java` đã có provider.

**D.3 — clangd qua LSP overlay** (D.0 xong trước) cho C/C++ — path thực tế thay `scip-clang` (đã xác nhận trước đó bị chặn Bazel+egress trong sandbox).

**D.4 — gopls qua LSP overlay** (D.0 xong trước) làm backstop cho Go (fan-out heuristic yếu, 54.3% ambiguous đã đo trên gin).

---

## 8. PHASE E — Benchmark + CI

- Mở rộng `benchmarks/resolution/` cho mỗi ngôn ngữ Phase B/C (tier-distribution, đúng kỳ vọng theo §1.7).
- Thêm check parity feature-flag (§A.1) vào CI thật (`ci.yml`), không cần build-matrix N-way riêng.
- Sau D.0-D.4: nếu có provider mới đủ ổn định, cân nhắc thêm `#[ignore]`d live integration test theo pattern đã có cho SCIP (`scip-nightly.yml`), nhưng KHÔNG lặp lại tình trạng hiện tại (chỉ Rust từng chạy CI xanh thật — xem `docs/superskills/plans/2026-07-07-eight-lang-formal-tier.md`§10-7) — mỗi provider mới cần ít nhất 1 lần verify CI xanh thật trước khi tuyên bố "formal" trong docs.

---

## 9. Quyết định (2026-07-10)

1. ✅ **Thứ tự Phase: B→A→C→D→E** — chốt. Làm Kotlin/Swift trước (an toàn, additive-only), registry sau khi có kinh nghiệm thật.
2. ✅ **Độ sâu Phase A: full `LanguageSpec` registry refactor** (§5, A.2) — chốt, làm ngay sau Phase B, không chỉ hygiene rẻ.
3. ✅ **Perl — đã research kỹ theo yêu cầu, đã chốt swap sang Groovy:**
   - User đúng khi nghi ngờ "tier cũ chỉ tương thích 0.23x", nhưng cơ chế thật khác: `MIN_COMPATIBLE_LANGUAGE_VERSION` (ABI 13) không đổi từ v0.20.0 tới v0.26.1 — 7 grammar Tier-0.5 hiện tại (ABI 13/14) **an toàn** trước một lần bump runtime.
   - Nhưng `tree-sitter-stack-graphs` 0.10.0 (dùng cho Tier-0 formal Python/TS/Java/JS) pin cứng `tree-sitter = "^0.24"`, không có version mới hơn (upstream `stack-graphs` đã archived 2025-09-09) → bump runtime để unlock Perl = phải fork+tự maintain vĩnh viễn dependency đã archived, ảnh hưởng cả Tier-0 formal tier đang ổn. Chi tiết đầy đủ ở §1.4.
   - **✅ Đã chốt: swap sang Groovy.** Groovy v0.1.2 đã xác nhận tương thích ABI 13-14, không cần bump runtime. Phase C's danh sách 9 ngôn ngữ dùng Groovy thay Perl (xem §2, §6).
