# CALM — Kế hoạch Formal-tier cho 8 ngôn ngữ còn lại (bản đã audit)

> **Ngày:** 2026-07-08 (cập nhật lần 4) · **Trạng thái:** P0 (P0.1–P0.5) VÀ Phase 1 (P1.1–P1.5) ĐÃ XONG TOÀN BỘ — **P1.5 nay đã xong 100%** (phần "using → namespace" còn lại đã làm xong phiên này, xem mục P1.5). P0.1-P0.3: commit `20f4265`, `40e6b40`, `e0471f9`. P0.4-P0.5: commit `bae5161`. P1.3: `fdf0aaf`. P1.4: `7ba5fb5`. P1.5: `d7178b9` (type_map/ctor) + phiên này (using→namespace, xem §10-4). P1.2: `7b7dec7`. P1.1: cùng phiên, xem lịch sử git cho commit cụ thể. **`benchmarks/resolution/` (§7) ĐÃ XONG** — baseline thật 8/8 ngôn ngữ đo xong, kèm 1 bug crash thật tìm+sửa (C, xem §7). **Phase 2: P2.1 (Go) VÀ P2.4 (Python) ĐÃ XONG** — commit `6603e49`/`9abb9f4`, cả 2 verify thật với binary/npx thật (xem §5's bảng). **Phase 3: P3.3 (SQL) ĐÃ XONG** — commit `22965d2`, verify thật bằng CLI thật (xem §6's mục P3.3). Còn lại P2.2/P2.3/P2.5/P2.6, P3.1, P3.2 CHƯA thực thi. Xem §3/§4/§5/§6/§7/§10-4 để biết chi tiết những gì đã làm; đừng làm lại.
> **Phạm vi:** Go · Java · C# · C · C++ · JavaScript · PHP · SQL (+ Python nâng chuẩn, + Kotlin bonus)
> **Nguồn gốc:** Kế hoạch SCIP-overlay gốc của user + audit codebase & SOTA research phiên 2026-07-07.
> Mọi khẳng định codebase trong file này ĐÃ ĐƯỢC XÁC MINH trên working tree ngày 2026-07-07 — phiên sau không cần re-verify trừ khi file liên quan đã đổi.

---

## 0. Mục tiêu & nguyên tắc

**Mục tiêu:** đưa 8 ngôn ngữ còn lại lên độ chính xác call-graph tối đa theo ceiling từng ngôn ngữ — Formal-tier (compiler/type-checker xác nhận) cho Go/Java/C#/C/C++/JS/PHP/Python, Resolved cho SQL — mà không phá triết lý silent-degrade của CALM (thiếu binary ngoài → vẫn hoạt động, chỉ mất tầng formal).

**4 nguyên tắc thiết kế rút ra từ audit (bắt buộc tuân thủ):**
1. **Đừng copy module `scip/` N lần** — tổng quát hoá thành bảng `ScipProvider` data-driven. Thêm ngôn ngữ = thêm 1 entry bảng.
2. **Sửa trần upgrade-only trước khi mua thêm indexer** — nếu không, dữ liệu compiler-grade mua về sẽ bị vứt đúng ở các call site khó nhất (xem §1.1).
3. **Heuristic tự cường trước, binary ngoài sau** — Tier-1.5 package-scope cho Go/Java/C# chữa gap phổ biến nhất KHÔNG cần tool ngoài; overlay chỉ là tầng nâng cấp.
4. **Indexer nặng không được chạy on-save** — per-language refresh policy + đường nhập `.scip` từ CI.

---

## 1. Sự thật kiến trúc đã xác minh (evidence anchors)

Phiên sau đọc mục này thay vì tự khảo sát lại:

1. **`ingest_occurrences` là upgrade-only** — `crates/calm-core/src/scip/ingest.rs:34`. Chỉ UPDATE `call_edges.edge_confidence='formal'` + rule-out siblings qua `mark_ruled_out_siblings`; KHÔNG BAO GIỜ insert. Test khóa hành vi: `never_downgrades_or_inserts` (ingest.rs:236). Khớp thuần theo `(file, line)` — không dùng cột → UTF-8/UTF-16 giữa các indexer vô hại.
2. **`MAX_CALLEE_CANDIDATES = 20`** — `crates/calm-core/src/indexer/pipeline.rs:20`. Call site tên trùng >20 ứng viên toàn repo, không match cùng file → `Vec::new()` = **0 edge** (pipeline.rs:642-649). Ghép với (1): overlay không bao giờ formal-hoá được các tên phổ biến. Đây là trần recall chính.
3. **`parse_index` dùng `doc.relative_path` nguyên văn** — `crates/calm-core/src/scip/parse.rs:29`. Indexer chạy ở subroot (go.mod lồng) → path lệch → ingest khớp 0 dòng, im lặng.
4. **Overlay chỉ nối vào serve/watcher** — `run_overlay` có đúng 2 call site production: `crates/calm-server/src/watcher.rs:188`, `crates/calm-server/src/lib.rs:195`. `calm index` one-shot KHÔNG có overlay.
5. **`formal.rs` (stack-graphs) chỉ đăng ký python/typescript(+TsxVariant)/java** — không có javascript. "Formal" của stack-graphs là upgrade theo **tập tên per-file** (`formally_resolved.contains(callee)`, pipeline.rs:374-379) — yếu hơn SCIP (khớp (file,line) exact). Hai producer chung nhãn `formal`, chưa phân biệt provenance.
6. **Tier hiện tại:** `resolve_tier1` (conservative.rs:61) = `file_symbols` (1 file) + `import_map` (tên→path) → Resolved; `resolve_tier2` (type_map receiver) → Inferred; stack-graphs → Formal; fan-out >1 target → Ambiguous. `EdgeConfidence` có thêm `Unresolved` (reserved, chưa producer nào dùng).
7. **Lỗ heuristic per-language đã xác minh:**
   - `imports.rs::import_node_types`: PHP, C, C++, C# → `&[]` (rỗng).
   - `lang_constants.rs::assignment_nodes`: thiếu php/c/cpp/csharp.
   - `parser.rs::extract_type_map_from_tree` (parser.rs:1178): chỉ python/ts/rust/go/java; comment ghi rõ "javascript: no static annotations".
   - **PHP `call_node_types` chỉ có `function_call_expression`** (lang_constants.rs, entry "php") → `$obj->method()`, `Foo::bar()`, `new Foo()` KHÔNG được trích làm call site. Phải sửa trước mọi thứ khác của PHP.
8. **Grammar thật đã có:** features default = `["embeddings", "tier0-5", "scip-overlay"]` (cả 3 crate). `tier0-5` = tree-sitter thật cho c, cpp, ruby, php, csharp, shell, r. Kotlin/swift là stub regex.
9. **Fixture `multi_lang_workspace` CHƯA tồn tại.**
10. **Bug nhỏ tiện tay:** `types/mcp_types.ts` khai `EdgeConfidence = "resolved" | "inferred" | "textual"` — thiếu `formal`/`ambiguous` (stale so với types.rs).

---

## 2. Trạng thái công cụ SOTA (đã kiểm chứng web, 07/2026)

| Tool | Version/date | Prereq | Ghi chú |
|---|---|---|---|
| scip-java | v0.13.1, 02/07/2026 (rất active) | JDK + Gradle/Maven/Bazel resolve (mạng lần đầu) | Kèm **Kotlin** (scip-kotlinc) + Scala. Docker image có |
| scip-go | v0.2.7, 05/2026 | Go toolchain, go.mod | **go.work/multi-module "incomplete"** (limitation chính thức) → runner phải tự enumerate module + rebase path |
| scip-dotnet | v0.2.14, 05/2026 | .NET 8 SDK, .sln/.csproj | `scip-dotnet index` |
| scip-typescript | v0.4.0, 10/2025 | Node 18/20, **node_modules đã install** | JS thuần: `--infer-tsconfig`. Repo lớn: `--no-global-caches` |
| scip-clang | active | compile_commands.json | **Chỉ Linux x86_64 + macOS arm64** (không Windows native) |
| scip-php | community (davidrjenni/scip-php) | PHP 8.1+, composer.lock + `vendor/` + autoloader | Nhỏ (18★) nhưng thật, CI + OpenSSF. **Kế hoạch gốc tưởng không tồn tại — sai** |
| scip-python | maintained (fork Pyright) | Node (npm package `@sourcegraph/scip-python`) | Lấp lỗ: Python hiện chưa có formal thật |
| stack-graphs | **ARCHIVED 09/09/2025** | — | Crates vẫn cài được, không fix mới. Có crate `tree-sitter-stack-graphs-javascript` 0.3.0 riêng cho JS (CALM chưa dùng) |
| datafusion-sqlparser-rs | Apache, release đều | pure Rust dep | Syntax-only; body procedure yếu ở vài dialect; **fail nguyên file với dbt/Jinja** |
| Bối cảnh | SCIP → community governance 03/2026 (scip-code org; steering committee Meta/Uber/Sourcegraph) | | Prior art runner: GlitterKill/scip-io (detect→install→run→merge) |

---

## 3. PHASE 0 — Nền tảng (làm TRƯỚC, đúng thứ tự)

### P0.1 — ✅ ĐÃ XONG — Nối overlay vào `calm index` one-shot
- **Commit:** `20f4265` (`feat(cli): wire SCIP overlay into one-shot calm index`).
- **Kết quả thật:** `crates/calm-cli/src/main.rs`'s `Commands::Index` giờ gọi `calm_core::scip::run_overlay` sau khi pipeline + embeddings xong, đúng shape `lib.rs`'s `serve_stdio_with_preset` (match + refresh_caller_counts + tracing::warn khi Err).
- **Test:** `crates/calm-cli/tests/scip_overlay_cli.rs::calm_index_cli_upgrades_a_real_edge_on_the_fixture` (`#[ignore]`, cần rust-analyzer) — xanh. DoD cả 2 nhánh (có/không binary) đã verify thủ công thêm bằng subprocess thật trên bản copy fixture.
- Đừng làm lại — nếu cần sửa, xem file/commit trên.

### P0.2 — ✅ ĐÃ XONG — Path rebase cho indexer chạy ở subroot
- **Commit:** `40e6b40` (`feat(scip): rebase SCIP occurrence paths for indexers run at a subroot`).
- **Kết quả thật:** `parse_index`/`parse_scip_file` (`crates/calm-core/src/scip/parse.rs`) nhận thêm `rebase_prefix: &Path`, join+normalize (`.`/`..` collapse, `/`-separated). Absolute `relative_path` → strip `index.metadata.project_root` (`file://` URI, percent-decode thủ công) rồi mới rebase; project_root không rõ → giữ nguyên absolute (KHÔNG rơi về relative-looking string để tránh trùng path giả). Cả 2 call site production (`run_overlay`, `main.rs`'s `scip-dump`) truyền prefix rỗng — hành vi Rust không đổi, verify lại bằng test `overlay_upgrades_a_real_edge_on_the_fixture` (real rust-analyzer) chạy xanh sau khi sửa.
- **Tests mới:** `rebase_prefix_joins_onto_a_subroot`, `rebase_prefix_normalizes_dot_segments`, `absolute_relative_path_is_stripped_of_project_root_then_rebased`, `absolute_relative_path_with_unknown_project_root_falls_back_unchanged`, `empty_prefix_is_identity_rust_runner_behavior_unchanged`, `file_uri_to_path_decodes_percent_escapes` — tất cả trong `scip/parse.rs`.
- Đừng làm lại. Lưu ý cho Phase 2: khi thêm provider mới (P0.4/P2.x), gọi `parse_scip_file(path, sub_root)` với `sub_root` thật thay vì `Path::new("")`.

### P0.3 — ✅ ĐÃ XONG — Provenance + gated-insert mode + match-rate (đòn bẩy chính xác lớn nhất)
- **Commit:** `e0471f9` (`feat(scip): provenance-aware gated-insert for cap-dropped call sites`).
- **Kết quả thật (khớp cả 5 bước gốc):**
  1. Migration `call_edges.formal_source TEXT` (`db/schema.rs::run_migrations`). Set `'stack_graphs'` bằng 1 UPDATE ngay sau `insert_call_edges_batch` trong `rebuild_graph` (đơn giản hơn thiết kế gốc — không cần thread field mới qua `CallSiteData`/`CallEdge`, vì mọi row `formal` ngay sau fresh-insert chắc chắn đến từ stack-graphs). `ingest_occurrences` set `'scip'` và được phép override `'stack_graphs'` (không override `'scip'` cũ) — implement trong `mark_ruled_out_siblings`'s `is_formal` computation.
  2. Gated insert = `scip/ingest.rs::insert_missing_edges`. **Khác thiết kế gốc một điểm có chủ đích:** thay vì tự map call site → enclosing symbol bằng range-lookup thô (như def-side), nó JOIN thẳng vào bảng `call_sites` (đã có sẵn `enclosing_qn`) — vừa đơn giản hơn, vừa là gate an toàn quan trọng: một SCIP reference thuần túy (type ref, field access) KHÔNG BAO GIỜ có mặt trong `call_sites` (chỉ tree-sitter call expression thật mới có), nên không thể tự tạo edge giả từ non-call reference. Def-side vẫn dùng narrow-range lookup trên `symbols` đúng như thiết kế gốc (`resolve_unique_symbol_at`).
  3. `IngestStats.inserted`/`match_rate` — expose qua `indexing_status`'s `scip_overlay` field (`last_match_rate`/`last_inserted`) qua sidecar `.calm/scip-stats.json` (mirror pattern `scip.cache`). **Cắt phạm vi có chủ đích:** KHÔNG wire vào `fitness_report` (đó là threshold pass/fail gate, thêm 1 ratio diagnostic vào đó là scope creep ngoài effort budget) — `indexing_status` đã là nơi DoD yêu cầu và đủ dùng.
  4. Tests đúng cả 4 tên gốc + `insert_missing_false_skips_the_insert_gate_entirely`.
  5. `types/mcp_types.ts` `EdgeConfidence` sửa đủ 6 variant (`formal|resolved|inferred|textual|ambiguous|unresolved`), không chỉ 2 cái đề xuất.
- **Sửa thêm phát hiện khi làm:** cả 3 call site production của `run_overlay` (`lib.rs`, `watcher.rs`, `main.rs`) trước đó chỉ refresh `caller_count` khi `upgraded>0 || ruled_out>0` — thiếu `inserted>0`, nghĩa là edge mới insert sẽ có `caller_count` stale ngay lập tức. Đã sửa cả 3.
- **Verify trên dữ liệu thật (không chỉ fixture synthetic):** chạy `calm index` với rust-analyzer thật trên fixture → 5 upgraded, 1 ruled_out, **3 inserted** (đúng nhóm cap-dropped mà P0.3 sinh ra để giải), match_rate=0.28 (số hợp lý, không phải 1.0 giả tạo).
- Đừng làm lại.

### P0.4 — ✅ ĐÃ XONG — Tổng quát hoá runner thành `ScipProvider`
- **File mới:** `crates/calm-core/src/scip/provider.rs`. Refactor `scip/mod.rs` (`run_overlay`→`run_overlay_for`, `overlay_status`→`overlay_status_for`, `rust_source_dirty_keys`→`source_dirty_keys`) và `runner.rs` (`run_scip`→`rust_build_command`+`run_indexer`).
- **Kết quả thật (khác thiết kế gốc ở vài điểm có chủ đích — xem lý do dưới mỗi điểm):**
  ```rust
  pub struct ScipProvider {
      pub lang: &'static str,
      pub resolve_binary: fn(Option<&str>, &Path) -> Option<PathBuf>,
      pub build_command: fn(bin: &Path, root: &Path, out: &Path) -> Command,
      pub timeout: Duration,
      pub cache_key: fn(bin: &Path, root: &Path, dirty: &[String]) -> String,
      pub cache_file_name: &'static str,
  }
  ```
  - **Cắt phạm vi có chủ đích:** `marker_files`, `prereqs: &[Prereq]`, `default_policy: RefreshPolicy` KHÔNG có trong struct thật. Lý do: với đúng 1 provider (Rust) tồn tại, các field này sẽ là dead field (không ai đọc) → tự vi phạm `-D warnings` (dead_code) mà không mang lại giá trị thật — hoãn đến khi Phase 2 có provider thứ 2 để xác nhận đúng shape (multi-root discovery, prereq gating, refresh policy) thay vì đoán trước. Thêm field + mở rộng bảng khi đó, không phải viết lại.
  - **`cache_key` gộp thành 1 fn thay vì `CacheSpec` tách lockfile-glob/toolchain-probe:** mỗi provider tự sở hữu toàn bộ cách tính cache key (bao gồm việc dò bao nhiêu file manifest) — generic pipeline không cần biết Go có 2 file (go.mod+go.sum) hay C++ có 1 (compile_commands.json). Rust's `rust_cache_key` (trong `provider.rs`) gọi lại nguyên xi `cache::overlay_cache_key` 4-tham-số đã có sẵn — `cache.rs` và cả 5 test của nó KHÔNG đổi.
  - `run_overlay_for(provider, conn, root, sub_root, cfg, dirty)` — pipeline chung: `enabled` gate → resolve binary → cache key → `runner::run_indexer` (spawn+poll+timeout generic, thay `run_scip`) → `parse::parse_scip_file(tmp, sub_root)` → `ingest::ingest_occurrences`. Cache file: `provider.cache_file_name` (Rust giữ nguyên `"scip.cache"` — không đổi tên để không phá cache của checkout cũ).
  - **KHÔNG làm (hoãn sang Phase 2 khi cần):** multi-root marker-file discovery, `config.rs`'s `GoConfig/JavaConfig/...` (không có consumer nào hôm nay), `RefreshPolicy` enum.
  - Dirty-keys: `source_dirty_keys(conn, langs: &[&str])` (SQL `WHERE language IN (...)`, dùng `rusqlite::params_from_iter` — pattern đã có sẵn ở `tools/common.rs`). `rust_source_dirty_keys` giờ là wrapper 1 dòng.
  - `run_overlay`/`overlay_status`/`rust_source_dirty_keys` giữ **nguyên xi chữ ký công khai** (0 call site nào trong `lib.rs`/`watcher.rs`/`main.rs`/`recover.rs` phải sửa) — wrapper mỏng gọi `*_for(&provider::RUST, ...)`.
- **Verify trên dữ liệu thật:** `calm index` qua binary CLI thật trên bản copy fixture `rust_workspace` → **giống hệt số P0.3**: 5 upgraded, 1 ruled_out, 3 inserted, match_rate=0.2777... — xác nhận 0 thay đổi hành vi qua đường ống mới.
- **Tests:** toàn bộ 32 test trong `scip::*` + `config::scip_config_tests::*` xanh (bao gồm 2 test `#[ignore]` chạy thật với rust-analyzer: `scip::tests::overlay_upgrades_a_real_edge_on_the_fixture` và calm-cli's `scip_overlay_cli.rs::calm_index_cli_upgrades_a_real_edge_on_the_fixture`). Toàn bộ workspace: 496+6+6+108+3 passed, 0 failed. `cargo clippy --workspace --all-targets --features scip-overlay -- -D warnings` sạch (1 lint thật gặp phải: `doc_lazy_continuation` do dòng doc-comment bắt đầu bằng `+` bị hiểu nhầm là markdown list — sửa bằng cách viết lại câu, không `#[allow]`). `cargo fmt --all -- --check` sạch.
- Đừng làm lại.

### P0.5 — ✅ ĐÃ XONG — Fixture `multi_lang_workspace` + CI nightly
- **Vị trí:** `crates/calm-core/tests/fixtures/multi_lang_workspace/` — đúng 8 thư mục như thiết kế gốc (`go/`, `java/`, `csharp/`, `c/`, `cpp/`, `js/`, `php/`, `sql/`), mỗi thư mục là "standard gap" cho ngôn ngữ đó (chi tiết + lý do từng file: xem `multi_lang_workspace/README.md`). Đây là fixture TĨNH (hand-written, không build được) — chưa test nào tham chiếu tới nó (Phase 1/2/3 chưa code); README ghi rõ Phase 1/2/3 nên trỏ `#[ignore]` test riêng vào đây khi cần, không cần sửa gì thêm ở CI.
- **CI:** `.github/workflows/scip-nightly.yml` (file mới, KHÔNG chung với `ci.yml`) — trigger `schedule` (03:00 UTC) + `workflow_dispatch`. **Cắt phạm vi có chủ đích so với thiết kế gốc:** chỉ cài `rust-analyzer` (qua `dtolnay/rust-toolchain`'s `components:`) — KHÔNG cài scip-go/scip-java/scip-dotnet ngay bây giờ, vì chưa provider Phase 2 nào tồn tại để tiêu thụ chúng (cài toolchain Go/JDK+Gradle/.NET SDK cho các binary không ai gọi là phí phút CI + tăng bề mặt lỗi vô ích). Comment trong file ghi rõ: thêm bước cài đặt cho từng Phase 2 provider ngay khi nó hạ cánh; job luôn chạy `cargo test --workspace -- --ignored` nên tự động nhặt test `#[ignore]` mới, không cần sửa workflow lần nữa.
- **Verify:** YAML parse hợp lệ (kiểm bằng `pyyaml`); action versions/pin SHA copy nguyên từ `ci.yml` (không tự chế). Lệnh thật trong job (`cargo test --workspace -- --ignored`) đã chạy LOCAL và xanh (2+1 test thật với rust-analyzer). **Chưa xác nhận trên GitHub Actions thật** (phiên này không push/trigger workflow) — DoD gốc yêu cầu "nightly workflow xanh"; phần local-equivalent đã xanh, phần "chạy thật trên GitHub Actions" cần push lên remote + đợi lịch chạy (hoặc `workflow_dispatch` thủ công) để xác nhận, phiên sau nên làm việc đó nếu cần độ tin cậy cao hơn.
- Đừng làm lại.

---

## 4. PHASE 1 — Zero external deps (song song được, sau P0)

### P1.1 — ✅ ĐÃ XONG — JavaScript stack-graphs (XS→S)
- **Option A đi đúng như dự kiến, không conflict version:** thêm `tree-sitter-stack-graphs-javascript = "0.3"` (workspace + calm-core) — `cargo build` resolve sạch ngay lần đầu, không cần Option B fallback. `formal.rs` thêm `FormalResolver::load_javascript()` (mirror `load_typescript` nhưng đơn giản hơn — xem điểm dưới).
- **Khác dự kiến ở 2 điểm, cả 2 đều xác minh qua source code thật của crate (không đoán):**
  1. Hàm thật là `try_language_configuration()` (KHÔNG có hậu tố `_javascript`) — chỉ đăng ký 1 file type `"js"`. Không có cặp hàm variant kiểu `_typescript`/`_tsx`.
  2. **Không cần `JsxVariant`:** `tree-sitter-javascript` (grammar nền của crate JS) parse JSX ngay trong 1 grammar duy nhất — không có nhập nhằng generic-`<T>`-vs-JSX-tag như TypeScript (lý do TS cần 2 grammar riêng). `ci`'s `lang_constants.rs` đã map `.js|.jsx|.mjs|.cjs` → cùng 1 string `"javascript"` từ trước — nên chỉ cần **1 entry** `FormalLanguageConfig` (`tsx: None`), test xác nhận `.jsx` resolve def/ref bình thường qua cùng entry đó.
  - `builtins.js` của crate này **RỖNG** (0 dòng) — khác Python/TS đều có builtins thật (len/print/Array.isArray...). Không có gì để test "builtin resolve qua formal tier" cho JS.
- **Wire:** chỉ 2 call site SẢN XUẤT thật (`run_indexing_pipeline`, `reindex_changed`) cần `formal.load_javascript()` thêm cạnh `load_typescript()` — 6 "site" còn lại trong ước tính gốc ("~8 site") là test riêng của TypeScript (`test_resolve_tsx_def_ref` etc.), không liên quan đến JS.
- **DoD đạt được ở mức cơ chế (FormalResolver), KHÔNG đạt đúng nguyên văn ở mức pipeline — lý do xác minh thật, không phải thiếu sót:** đã verify bằng `calm index` thật trên fixture JS đơn giản (`run()` gọi `helper()` cùng file) → `edge_confidence='resolved'`, KHÔNG phải `'formal'`. Nguyên nhân: `extract_symbols` của `ci` gom MỌI function declaration cùng file vào 1 `file_symbols` set phẳng bất kể độ sâu lồng nhau → Tier-1 đã "resolved" được các case intra-file trước khi formal tier (stack-graphs, chỉ hoạt động per-file, không cross-file) có cơ hội nâng cấp gì thêm — và vì `builtins.js` rỗng, không có case kiểu `Array.isArray` (TS) để ép ra transition "textual→formal" thật. Mức "def/ref → formal" ĐÃ được chứng minh đúng ở tầng `FormalResolver::resolve_file()` (4 test mới trong `formal.rs`, gọi thẳng không qua Tier-1) — đây chính là cơ chế P1.1 cần lắp, chỉ là pipeline hiện tại không có input nào lộ ra transition đó cho JS cụ thể.
- **Lưu ý:** upstream archived — đây là giải pháp giữ chỗ; đường dài là P3.2 (scip-typescript, cũng phủ JS). KHÔNG đầu tư viết .tsg mới.
- **Verify:** `test_load_javascript`, `test_resolve_simple_javascript_def_ref`, `test_resolve_javascript_does_not_resolve_undefined_name`, `test_resolve_javascript_def_ref_in_jsx_file` (formal.rs, tất cả gọi `resolve_file` trực tiếp) + `test_javascript_formal_resolver_wired_into_pipeline` (pipeline.rs, integration — mirror đúng tinh thần pragmatic của `test_formal_tier_upgrades_textual_python_call` đã có sẵn cho Python, cùng lý do). Toàn bộ workspace 526 test xanh, clippy `-D warnings` sạch, fmt sạch.
- Đừng làm lại.

**→ Phase 1 (P1.1-P1.5) ĐÃ XONG TOÀN BỘ.**

### P1.2 — ✅ ĐÃ XONG — PHP heuristics (S/M) — ĐÚNG THỨ TỰ
1. **Call extraction — ✅.** `lang_constants.rs`'s `"php"` entry: thêm `member_call_expression`, `scoped_call_expression`, `nullsafe_member_call_expression`, `object_creation_expression` vào `call_node_types`. **Phát hiện kiến trúc thật (lớn hơn dự kiến của bullet ⚠️ gốc):** `call_function_field` là 1 string DÙNG CHUNG cho MỌI node kind trong 1 ngôn ngữ — không đủ để biểu diễn PHP cần 2 field khác nhau (`function_call_expression`="function", còn lại="name") cùng lúc. Đã thêm field mới `LangConstants.call_function_field_by_kind: &[(&str, &str)]` (override theo node kind, rỗng cho mọi ngôn ngữ khác) + sửa `walk_calls` tra cứu field đó trước khi fallback về `call_function_field`. `object_creation_expression`'s callee ("Foo" trong `new Foo()`) hoá ra KHÔNG phải field nào cả (xác nhận qua dump AST thật) — là positional child kind="name" — `walk_calls` có thêm 1 fallback nữa: nếu field lookup thất bại, tìm child đầu tiên có `kind() == field_name`.
   - **Bug thật phát hiện khi verify (không nằm trong kế hoạch gốc, ảnh hưởng CẢ Java):** `parser.rs::split_receiver_callee` chỉ nhận `.`/`::`, chưa từng nhận PHP's `->`. Đã sửa y hệt P1.4's C/C++ fix (rightmost giữa `.`/`->` thắng, `::` giữ ưu tiên thấp hơn). **Phát hiện thêm:** Java's `method_invocation` cũng tách "object"+"name" làm 2 field riêng (xác nhận qua dump) — nghĩa là MỌI call Java có receiver (`obj.method()`) trước đây CHƯA TỪNG có receiver được trích ra (luôn `None`), dù đã có type_map từ trước — field-lookup fallback mới trong `walk_calls` (tìm child theo "object"/"scope" khi field chính không có) sửa gap này cho CẢ Java, không chỉ PHP.
2. **imports.rs — ✅.** 4 node kind riêng biệt xác nhận qua grammar thật: `require_expression`, `include_expression`, `include_once_expression`, `require_once_expression` (không có wrapper chung) + `namespace_use_declaration`. `require`/`include` route qua nhánh relative-import có sẵn của `resolve_module_to_path` (prefix `"./"`, xử lý riêng trường hợp `__DIR__ . '/x.php'` có `/` đầu để tránh `.//x.php`) — y hệt pattern `parse_c_include`. `use App\Service\Foo (as Bar)?;` → `module_name` giữ nguyên dạng `\`-separated, `imported_names` = alias hoặc segment cuối.
3. **PSR-4 — ✅.** Module mới `psr4.rs` (`Psr4Map`, mirror `CrateMap::build`'s pattern: đọc file thật 1 lần/run, xây map, truyền xuyên suốt pipeline) — đọc `composer.json`'s `autoload`+`autoload-dev` `.psr-4`, longest-prefix-first. Threading thật: `run_indexing_pipeline`/`reindex_changed` → `rebuild_graph` → `resolve_import_targets` → `resolve_module_to_path` (tham số mới `psr4: &Psr4Map`, nhánh PHP-only chạy TRƯỚC generic dotted-scan vì scan đó không tách `\`).
4. **type_map — ✅.** `extract_type_map_from_tree`/`binding_names_and_type` thêm `"php" => &["simple_parameter", "property_declaration"]`. `simple_parameter`'s field `type` (`named_type`) đã là text thuần ("Foo", không cần unwrap như C's struct_specifier); field `name` là `variable_name` cần strip `$`. `property_declaration` chia sẻ 1 type qua nhiều `property_element` con (như Java/C#). Ctor inference `$x = new Foo();` qua `php_constructor_type` (mirror `csharp_constructor_type`, đơn giản hơn vì PHP không có khái niệm "declaration keyword" tách biệt reassignment — 1 nhánh `assignment_expression` là đủ, không cần 2 nhánh như Rust).
- **Cắt phạm vi có chủ đích:** `imported_names` của require/include CỐ Ý để rỗng (như C's `#include`) — nghĩa là gọi 1 hàm top-level định nghĩa trong file được require KHÔNG được Tier-1 nâng lên "resolved" qua `import_map` (require/include paste toàn bộ file, không bind 1 tên cụ thể nào biết trước). Cạnh đó, edge thật vẫn được tạo đúng qua `rebuild_graph`'s `by_name` (cơ chế tách biệt khỏi tier nhãn) — chỉ có NHÃN CONFIDENCE là không đạt "resolved" cho trường hợp này, không phải edge bị thiếu. Không xây thêm 1 pre-pass "hàm nào định nghĩa trong file nào" cho use case hẹp này.
- **Verify:** ~20 test mới trải đều parser.rs/imports.rs/pipeline.rs/psr4.rs, xác nhận qua real grammar dump ở từng bước (PHP, và Java bonus). Test tổng hợp `test_php_p1_2_end_to_end` (pipeline.rs) chạy cả 4 bước trên 1 project nhỏ: require_once + use+PSR-4 đều resolve đúng `import_edges.to_path`; `$this->helper->run()` qua typed property resolve đúng lớp (không fan-out sang lớp trùng tên method), confidence đúng `"inferred"`; `Foo::bar()` qua use+PSR-4 resolve đúng qua scoped type path. Toàn bộ workspace 521 test xanh, clippy `-D warnings` sạch, fmt sạch.
- **DoD gốc:** đạt đủ ngoại trừ phần require_once → "resolved" ở Tier-1 label (xem cắt phạm vi trên) — edge vẫn đúng, chỉ nhãn confidence khác dự kiến gốc.
- Đừng làm lại.

### P1.3 — ✅ ĐÃ XONG (V1) — Tier-1.5 package-scope cho Go/Java/C#/C/C++ (S mỗi ngôn ngữ) — quick-win giá trị nhất
- **V1 (làm trước, không schema change):** trong `rebuild_graph` candidate selection (`pipeline.rs:642-649` cũ), chèn bậc ưu tiên **same-dir** giữa `same_file` và global fan-out, áp cho `go|java|c|cpp` — khớp đúng thiết kế gốc. Nếu có ứng viên cùng thư mục (so bằng `Path::parent()`) → chỉ lấy chúng, bất kể `t.len()` so với `MAX_CALLEE_CANDIDATES`. **"Header/impl pairing theo basename cho c/cpp" thu gọn thành same-dir equality thuần** (không xây cơ chế basename-pairing riêng) — c/cpp trong thực tế hầu hết đã để .h/.c cùng thư mục nên same-dir literal đã bao phủ trường hợp phổ biến; pairing thật sự khác-thư-mục (vd `include/` riêng `src/`) hoãn sang V2/Phase 2 nếu đo thấy cần. Rust/Python/JS/TS bị loại rõ ràng khỏi tier này (đã có import_map/type_map riêng, thêm vào sẽ là pass thừa/có thể sai).
- **V2 (chưa làm — sau khi đo):** nâng confidence — pre-pass build `package_symbols` (Go: dir+package clause; Java: dir; C#: bảng namespace→symbols) đưa vào `FileContext`, `resolve_tier1` check thêm → `Resolved`. C#: cần trích `namespace_declaration`/`file_scoped_namespace_declaration` per-file lúc index (lưu vào bảng phụ hoặc derive từ qualified_name).
- **Lý do:** ngữ nghĩa Go thật (package = compilation unit); safety net khi binary ngoài vắng; baseline đo giá trị cộng thêm của overlay.
- **Verify:** 3 test mới (`test_go_same_directory_call_resolves_not_fanned_out`, `test_java_same_package_call_resolves_not_fanned_out`, `test_c_same_directory_call_resolves_not_fanned_out`, `pipeline.rs`) — mỗi test dựng 2 thư mục với hàm/method trùng tên, xác nhận caller CHỈ resolve về ứng viên cùng thư mục, KHÔNG fan-out sang thư mục kia. **Đã xác nhận cả 3 test THẬT SỰ fail khi tắt tạm nhánh same-dir** (temp `if false` guard, chạy lại, restore) — tránh false-positive "test pass nhưng không exercise code mới". Toàn bộ 499 test workspace xanh (496+3 mới), clippy `-D warnings` sạch, fmt sạch.
- **DoD gốc đạt đủ:** fixture go same-package cross-file → 1 edge đúng target (không fan-out/không rỗng); java static-call cùng package tương tự (test C thêm ngoài DoD gốc cho chắc, cpp không test riêng vì cùng nhánh code y hệt c).
- Đừng làm lại.

### P1.4 — ✅ ĐÃ XONG — C/C++ heuristics (S)
- `imports.rs`: `preproc_include` cho c/cpp — `#include "x.h"` (`parse_c_include`) route qua nhánh relative-import có sẵn của `resolve_module_to_path` (prefix `"./"` nếu path chưa bắt đầu `./`/`../`) thay vì basename-scan riêng — tái dùng resolver JS/TS đã có, không cần code mới ở pipeline.rs. `<...>` system headers bị bỏ qua (trả `None`). `imported_names` cố ý để rỗng — `#include` không bind một tên cụ thể nào (khác Python/JS import theo tên); giá trị thật là `import_edges` row cho `dependencies`/hub graph.
- `extract_type_map_from_tree` + `binding_names_and_type`: thêm nhánh `"c" | "cpp"` cho `declaration`/`field_declaration`, unwrap `pointer_declarator`/`reference_declarator`/... đệ quy về `identifier`/`field_identifier` (hàm mới `innermost_c_declarator_identifier`). Sửa thêm: `type` field khi là `struct_specifier`/`class_specifier`/`union_specifier`/`enum_specifier` (vd `struct Shape *s;`) phải lấy riêng field `name` con ("Shape"), không lấy nguyên text node ("struct Shape") — nếu không type_map sẽ không bao giờ khớp `class_context` nào.
- **Bug thật phát hiện khi làm (không nằm trong kế hoạch gốc, là tiền đề bắt buộc để P1.4 có tác dụng):** `parser.rs::split_receiver_callee` chưa từng nhận diện `->` (chỉ `.`/`::`) — `s->area()` trước đây bị tách sai thành callee="s" (cắt tại byte đầu tiên không phải ident), KHÔNG có receiver — nghĩa là mọi call qua `->` (cách gọi method phổ biến nhất của C/C++ với con trỏ) chưa từng tới được Tier-2 dù `type_map` có tốt đến đâu. Đã sửa: `->` xếp cùng bậc ưu tiên với `.` (rightmost-wins giữa 2 cái), `::` vẫn giữ nguyên ưu tiên thấp hơn y hệt trước (không phá turbofish `foo.bar::<T>()` của Rust).
- **Verify:** 5 test mới — `c_include_quoted_header`, `cpp_include_quoted_header_preserves_subdir_and_dotdot`, `c_include_system_header_yields_no_import` (imports.rs) + `test_cpp_pointer_member_call_resolves_via_field_type` (pipeline.rs, xác nhận THẬT SỰ fail khi tắt tạm nhánh `->`, giống P1.3). `test_field_type_map_resolves_same_language_cross_module_method_call` (Rust, đã có sẵn) vẫn xanh — xác nhận sửa `split_receiver_callee` không phá turbofish/existing dot-chain. Toàn bộ workspace 502+ test xanh (1 lần flaky không liên quan: `db::instance_lock::...` — pass lại khi chạy riêng/`--test-threads=1`, do tranh chấp file-lock giữa test song song, không phải regression). Clippy `-D warnings` sạch, fmt sạch.
- Đừng làm lại.

### P1.5 — ✅ ĐÃ XONG TOÀN BỘ (kể cả "using → namespace") — C# heuristics (S/M + remainder)
- **Phần type_map/ctor (phiên trước):**
  - `extract_type_map_from_tree`/`binding_names_and_type`: thêm `"csharp" => &["parameter", "variable_declaration"]`. Phát hiện qua dump AST thật: `field_declaration` VÀ `local_declaration_statement` đều chỉ bọc đúng 1 node `variable_declaration` (có field `type` trực tiếp) — khớp field/local bằng CÙNG MỘT node kind, không cần 2 nhánh riêng như bullet gốc gợi ý (`field_declaration`). `parameter` tự có field `type`+`name`, rơi vào nhánh generic có sẵn, không cần code riêng.
  - `csharp_constructor_type` + `csharp_declarator_initializer` (mirror `rust_constructor_type`): `var x = new Foo();` — `var`'s field `type` là text literal "var" (không phải type thật) → nhánh generic cố ý coi như rỗng (ty="") để nhường cho block suy luận ctor riêng đọc `object_creation_expression`'s field `type` trực tiếp (đơn giản hơn Rust — không cần unwrap `scoped_identifier`). Initializer của `variable_declarator` không có tên field riêng (xác nhận qua dump) → lấy bằng vị trí (node ngay sau token `=`).
  - **Lưu ý quan trọng phát hiện khi verify:** P1.3's same-dir tier KHÔNG bao gồm `csharp` (chỉ go|java|c|cpp, xem P1.3 — tiêu đề mục P1.3 gốc liệt kê C# nhưng bullet chi tiết gốc "áp cho go|java|c|cpp" thì không, code khớp đúng bullet chi tiết). C# dựa hoàn toàn vào type_map/Tier-2 (đúng ngữ nghĩa hơn — resolve theo type khai báo, không phải theo thư mục) thay vì proxy thư mục kém tin cậy hơn cho C# (namespace C# không bắt buộc khớp thư mục như Go).
  - **Verify:** 4 test — `csharp_field_and_local_share_one_type_across_declarators`, `csharp_var_infers_type_from_constructor` (parser.rs) + 1 case thêm vào `test_type_map_all_languages` + `test_csharp_field_type_and_var_ctor_resolve_via_declared_type` (pipeline.rs). Xác nhận THẬT SỰ fail khi tắt tạm `"csharp"` khỏi `binding_kinds` (giống P1.3/P1.4).
- **Phần "using → namespace" (phiên này, 2 phần độc lập, rủi ro thấp hơn thiết kế gốc):**
  - **Trace thật trước khi code phát hiện gap cụ thể hơn mô tả gốc:** C# không có toán tử static-access riêng (`.` dùng chung cho `helper.Greet()` VÀ `Helper.Greet()`) — `receiver_is_type_path` chỉ được set ở nhánh `::` của `split_receiver_callee` (dùng chung mọi ngôn ngữ), KHÔNG BAO GIỜ set ở nhánh `.` — nên `Helper.Greet()` chưa từng được nhận diện là type-qualified. Tier-2 (`resolve_tier2`) tra `type_map.get("Helper")` như thể "Helper" là tên BIẾN — sai, nó là tên class — nên Tier-2 cũng miss. Trước khi sửa: confidence rơi về `Textual`, `target_class=None`, `rebuild_graph` phải dùng `by_name` fan-out theo tên trần "Greet" toàn repo — đúng "may mắn" với fixture 1-Helper, nhưng `Ambiguous`/sai ngay khi có 2 method cùng tên `Greet` bất kỳ đâu trong repo C#.
  - **Phần A — sửa tại chỗ, C#-only, KHÔNG cần pre-pass** (`extract_file_data`, pipeline.rs): sau khi Tier-1 lẫn Tier-2 đều miss, thêm nhánh `lang == "csharp" && is_type_like(receiver)` → `confidence = Inferred`, `target_class = Some(receiver)`. Quét gọn 1 chỗ, KHÔNG đụng `split_receiver_callee`/`is_type_like` dùng chung (chỉ bump `is_type_like` lên `pub(crate)` để pipeline.rs gọi được) — zero blast radius sang Go/Java/Python/... Ghi chú rõ: Java có gap tương tự y hệt (`Math.max()`) nhưng KHÔNG sửa ở đây — ngoài phạm vi P1.5.
  - **Phần B — `NamespaceMap` pre-pass (module mới `csharp_namespace.rs`, mirror `CrateMap::build`) + threading vào `rebuild_graph`:** **lệch có chủ đích so với gợi ý gốc** — gợi ý gốc nói "truyền vào `extract_file_data`"; sau khi trace, chọn threading vào `rebuild_graph` (đúng điểm `crate_map`/`psr4` đã dùng sẵn) thay vì `extract_file_data`, vì lý do: `crate_map`/`psr4` xác nhận pattern "build sau vòng extract_file_data song song, chỉ ảnh hưởng `import_edges.to_path`" đã chạy tốt — ép namespace map vào `extract_file_data` đòi hỏi đảo thứ tự pipeline (parse-all-trước-rồi-extract), rủi ro cao hơn hẳn cho cùng một kết quả cuối. `NamespaceMap::build` quét mọi `.cs` file thật (`namespace_declaration`/`file_scoped_namespace_declaration`, cả 2 dạng xác nhận qua dump AST thật), xây `namespace -> Vec<file>`. API: `resolve(ns)` (single-winner-or-none cho `import_edges.to_path` — namespace trải ≥2 file thì để `None`, KHÔNG đổi schema `to_path` thành multi-valued như lo ngại của bản kế hoạch gốc) và `contains(ns, path)` (dùng cho candidate narrowing). Threading: `run_indexing_pipeline`/`reindex_changed` build `NamespaceMap` cạnh `crate_map`/`psr4` → `rebuild_graph` (tham số mới) → (1) `resolve_import_targets`/`resolve_module_to_path` (nhánh C# mirror nhánh PSR-4 của PHP, cho `to_path`), (2) closure `same_namespace` mới trong candidate-narrowing của `rebuild_graph` (song song `same_dir` của P1.3, chỉ áp cho `csharp`) — build `from_path -> HashSet<using namespace>` từ `import_edges` đã có sẵn trong transaction (1 SELECT rẻ, KHÔNG cần cột mới), lọc `by_name_class` candidates theo namespace khớp `using`; lọc còn đúng 1 candidate → nâng `effective_confidence` từ tier gốc lên `Resolved` (bằng chứng cross-file thật từ `namespace` declaration, không phải suy luận).
  - `imports.rs`: thêm `"csharp" => &["using_directive"]` vào `import_node_types` + `parse_csharp_using` (text-based, mirror `parse_c_include`/`parse_php_require` — `imported_names=[]` vì `using` không bind 1 tên cụ thể). Dump AST thật xác nhận 3 dạng cùng share node kind `using_directive`: `using X;` (namespace thường, xử lý), `using static X;` (import static members — bỏ qua, `None`), `using X = Y;` (type alias, có field `name` — bỏ qua, `None`).
  - **Verify:** 6 test `csharp_namespace.rs` (mirror style `psr4.rs`) + 4 test `imports.rs` (`csharp_using_plain_namespace`, `csharp_using_dotted_namespace`, `csharp_using_static_yields_no_import`, `csharp_using_alias_yields_no_import`) + 3 test tích hợp `pipeline.rs`: `test_csharp_type_qualified_static_call_resolves_via_target_class` (case cơ bản, xác nhận `resolved` nhờ namespace-confirm), `test_csharp_same_class_name_in_different_namespaces_disambiguated_by_using` (case thật của gap — 2 class `Helper` khác namespace, chỉ 1 được `using`, xác nhận đúng target + KHÔNG `Ambiguous`), `test_csharp_using_resolves_import_edge_to_path_when_unambiguous` (namespace 1 file → `to_path` set; namespace 2 file → `to_path` NULL). Cả phần A lẫn phần B đều xác nhận THẬT SỰ fail khi tắt tạm nhánh tương ứng (`if false` guard, chạy lại, restore) — đúng convention của các mục P1.x trước. Toàn bộ workspace 567+108+3 test xanh (tăng từ 563 lib test trước đó), `cargo clippy --workspace --all-targets --features scip-overlay -- -D warnings` sạch, `cargo fmt --all -- --check` sạch.
  - **Cắt phạm vi có chủ đích (chưa làm, ghi lại cho phiên sau nếu cần):** gap `receiver_is_type_path` tương tự cho Java (`Math.max()`) — không sửa; `import_edges.to_path` vẫn single-valued (quyết định thay cho "cần nghĩ lại shape" của bản kế hoạch gốc, xem lý do ở Phần B); `using static X;`/`using X = Y;` (alias) không được `NamespaceMap`/`import_edges` xử lý.
- Đừng làm lại — cả 2 phần P1.5 đã xong.

---

## 5. PHASE 2 — SCIP providers (độc lập nhau, chia song song; cần P0.2–P0.4 xong)

Mỗi provider = 1 entry bảng + probe prereq + integration test nightly trên fixture. Shape chung: auto-detect (ScipConfig 3 trạng thái y Rust), silent no-op khi thiếu binary/prereq, log info khi `enabled=Some(true)` mà thiếu.

| # | Provider | Markers | Invoke | Cache key inputs | Prereq/policy | Ghi chú |
|---|---|---|---|---|---|---|
| P2.1 | go | ✅ **XONG (V1 single-module)** — commit `6603e49`. `provider::GO` (`crates/calm-core/src/scip/provider.rs`) + `go_resolve_binary`/`go_build_command`/`go_toolchain_fingerprint` (`runner.rs`) + `GoConfig` (`config.rs`) + `run_go_overlay_and_log` (`mod.rs`), wired vào đúng 3 call site production (lib.rs/watcher.rs/main.rs) cạnh Rust. **Verify thật bằng scip-go thật** (`go install github.com/scip-code/scip-go/cmd/scip-go@latest` — LƯU Ý module path đã đổi từ `sourcegraph/scip-go`, xem §2) trên fixture 2-package cross-call: `calm index` → 2 edge upgrade `formal`/`formal_source=scip`, match_rate=0.67; lần chạy 2 cache-skip đúng, không re-invoke. **Chưa làm (V2, hoãn có chủ đích):** multi-module `go.work` enumerate (P0.4's `ScipProvider` vẫn chưa có field marker/multi-root — Go V1 chỉ single go.mod ở root, đúng như phần lớn checkout thật). Toàn bộ workspace 658 test xanh (527+2 mới), clippy `-D warnings` sạch, fmt sạch. Đừng làm lại V1. | ~~`scip-go --output {out}` tại module dir~~ → thật: `scip-go index --module-root {root} --output {out} --quiet` | hash(go.mod+go.sum) + `go version` + dirty .go trong module | Go toolchain; policy OnSave/MinInterval ok (nhẹ) | Multi-module (`go.work`) CHƯA xử lý — V2 |
| P2.2 | java | `pom.xml`/`build.gradle(.kts)`/`settings.gradle` | `scip-java index --output {out}` | build files + lockfiles + JDK version | JDK + build resolve (mạng lần đầu). **Policy: OnDemand/MinInterval(15m+)** — full build, KHÔNG on-save. Docs: khuyến nghị Docker `sourcegraph/scip-java` cho CI | Giữ stack-graphs Java làm fallback. **Bonus: Kotlin/Scala free** — thêm ext mapping khi bật |
| P2.3 | csharp | `*.sln`/`*.csproj` | `scip-dotnet index` | csproj/sln + packages.lock.json + `dotnet --version` | .NET 8 SDK; policy MinInterval | |
| P2.4 | python | ✅ **XONG** — commit `9abb9f4`. `provider::PYTHON` + `python_resolve_binary`/`python_build_command`/`python_binary_version`/`python_toolchain_fingerprint` (`runner.rs`) + `PythonConfig` (`config.rs`) + `run_python_overlay_and_log` (`mod.rs`), wired vào đúng 3 call site production. `resolve_binary` probe CẢ binary lẫn `npx` đúng như cột "Prereq" gốc yêu cầu — `npx --yes @sourcegraph/scip-python` khi không có binary global. **Bug thật phát hiện qua thực nghiệm:** scip-python crash toàn bộ (`TypeError` trong `normalizeNameOrVersion`) nếu thiếu CẢ `--project-name` LẪN `--project-version` và thư mục không phải git repo — `python_build_command` luôn truyền cả 2 (placeholder cố định cho version, giá trị không quan trọng, chỉ cần tồn tại). **Verify thật bằng scip-python thật qua npx** trên fixture cross-module (`main.py` gọi `pkg.helper.helper()`) → 1 edge upgrade `formal`/`formal_source=scip`, match_rate=0.50; lần chạy 2 cache-skip đúng. Cùng tồn tại với stack-graphs formal tier có sẵn của Python qua cơ chế provenance P0.3 (scip override stack_graphs, không cần code mới). Toàn bộ workspace xanh, clippy sạch, fmt sạch. Đừng làm lại. | ~~`scip-python index . --output {out}`~~ → thật: `scip-python index --cwd {root} --project-name {name} --project-version 0.0.0 --output {out} --quiet` | hash(requirements.txt) + hash(pyproject.toml) + `python3 --version` + dirty .py | npm package (cần node) — ✅ probe cả binary lẫn `npx` | Nâng Python lên formal THẬT (hiện chỉ stack-graphs archived) — ĐÃ XONG |
| P2.5 | php | `composer.json` **và** `vendor/autoload.php` tồn tại | `vendor/bin/scip-php` (ưu tiên) hoặc global | composer.lock + `php -v` | Không autoload → silent skip. Community tool → docs ghi rõ | Nâng ceiling PHP lên Formal (kế hoạch gốc sai ở điểm này) |

**P2.6 — Ops surface (bắt buộc kèm Phase 2):**
- CLI `calm scip run [--lang <l>]` + MCP tool `scip_refresh` — chạy tay indexer nặng.
- **`calm index --scip-file <path> [--sub-root <p>]`** — nhập `.scip` build sẵn từ CI (giải bài CI sandbox không mạng; pattern chuẩn ngành). Chỉ parse+ingest, bỏ qua runner.
- Refresh policy trong config: `[languages.java.scip] policy = "on_demand" | "min_interval:900" | "on_save"`, default theo provider table.
- `indexing_status`/`fitness_report`: per-language {edges theo confidence, overlay match_rate, last_run, binary_found}.

---

## 6. PHASE 3 — Effort cao

### P3.1 — C/C++ → scip-clang (L)
- `ClangConfig { scip: ScipConfig, compile_commands: Option<String> }`; auto-detect `compile_commands.json` ở root/`build/`; absent → silent no-op.
- Invoke: `scip-clang --compdb-path={cc} --index-output-path={out}` (+ giới hạn `-j`).
- **Platform gate:** chỉ Linux x86_64/macOS arm64 — probe OS trước, nơi khác silent skip + docs. Docs: `CMAKE_EXPORT_COMPILE_COMMANDS=ON`, không tự chạy CMake; Make → gợi ý `bear`.
- DoD: fixture c + compile_commands → formal; cpp virtual call → formal (nhờ P0.3 insert nếu textual không có candidate).

### P3.2 — JS/TS → scip-typescript (M)
- Markers: `package.json` + (`tsconfig.json` hoặc infer) + **`node_modules/` tồn tại** (không thì silent skip). Invoke: `scip-typescript index [--infer-tsconfig] [--yarn-workspaces|--pnpm-workspaces]`; repo lớn: `--no-global-caches`, NODE_OPTIONS heap.
- Cache: lockfile (package-lock/yarn.lock/pnpm-lock) + version. Policy MinInterval.
- Quan hệ với stack-graphs: chạy sau → provenance `scip` override `stack_graphs` (P0.3). Đường thoát dần khỏi upstream archived cho cả TS lẫn JS.

### P3.3 — ✅ ĐÃ XONG — SQL → sqlparser (commit `22965d2`)
- **Module mới** `crates/calm-core/src/indexer/sql.rs` (không ép vào khung LangConstants/tree-sitter, đúng thiết kế gốc). Dep: `sqlparser` (tên crates.io thật của `datafusion-sqlparser-rs`, xác nhận qua `cargo info`).
- Extension mapping: `"sql" => Some("sql")` trong `language_for_extension` — ✅.
- **Symbols — ✅ đúng như thiết kế:** CREATE TABLE/VIEW/MATERIALIZED VIEW/PROCEDURE/FUNCTION/TRIGGER/INDEX → rows trong `symbols` (kind: Struct cho table/view/matview/index, Function cho procedure/function/trigger).
- **Edges — ✅.** view/proc → bảng trong FROM/JOIN (`edge_kind='reference'`); proc → proc qua CALL/EXEC[UTE] (`edge_kind='call'`). Confidence `resolved` khi target cùng file (mirror `ConservativeResolver::resolve_tier1`'s file_symbols check — KHÔNG dùng schema-qualification để phân biệt 2 schema cùng tên bảng, cắt phạm vi có chủ đích cho V1); cross-file → `textual`, vẫn ra edge thật qua `rebuild_graph`'s global by-name fallback sẵn có (y hệt pattern PHP require/include). **Đã thêm cột `edge_kind TEXT NOT NULL DEFAULT 'call'` vào CẢ `call_sites` và `call_edges`** (không chỉ `call_edges` như thiết kế gốc — cần ở `call_sites` để thread giá trị qua `rebuild_graph`), threading đầy đủ qua `CallSiteData`→`CallEdge`→`insert_call_edges_batch`, VÀ xuyên tới `callers`/`callees` MCP tools (`CallerEntry`/`CalleeEntry` + SELECT trong `trace.rs`/`guardrails.rs`/`inspect.rs`) — đây chính là mục đích cột tồn tại, không chỉ nằm im trong DB. `types/mcp_types.ts` cập nhật theo.
- **Robustness — ✅.** Statement splitter tự viết (state machine, không dùng `sqlparser::Parser::parse_sql` cho cả file — đã xác nhận thực nghiệm: 1 statement lỗi làm hỏng TOÀN BỘ batch, mất cả statement hợp lệ trước đó) tôn trọng `'...'`/`"..."` quoting, `--`/`/* */` comment, và dollar-quoted body (`$$...$$`/`$tag$...$tag$`) — `;` bên trong body KHÔNG bị coi là ranh giới statement. Statement `sqlparser` không parse được (xác nhận thực nghiệm: MySQL/T-SQL `CREATE PROCEDURE ... BEGIN...END` không có `AS` FAIL dưới `GenericDialect`) → fallback regex header scan, vẫn ra symbol. **Tham chiếu (FROM/JOIN/CALL/EXEC) dùng regex quét text thô CHO MỌI statement** (không chỉ fallback dbt/Jinja) thay vì đi bộ AST `Query` — quyết định có chủ đích: body procedure là đúng phần khác biệt nhiều nhất giữa các dialect, regex đồng nhất mạnh hơn AST không đồng nhất. dbt/Jinja file → 0 symbol, KHÔNG crash (verify thật).
- Same-language filter trong rebuild_graph không cản SQL→SQL — không cần đổi, đúng dự đoán gốc.
- **Verify thật bằng CLI thật** (không chỉ unit test): fixture 8-object (`users`, `orders`, 2 view, 2 function, 1 index, 1 trigger) → cả 8 symbol đúng kind, cả 6 edge đúng confidence/edge_kind. Fixture 2-file riêng xác nhận cross-file reference resolve qua global fallback (`textual`, vẫn có edge thật). Fixture dbt/Jinja xác nhận không crash. 13 unit test mới trong `sql.rs` + 1 test end-to-end (`test_sql_p3_3_end_to_end`, pipeline.rs, verify đúng DoD gốc từng chữ). Toàn bộ workspace xanh, clippy `-D warnings` sạch, fmt sạch.
- **DoD gốc đạt đủ và vượt:** fixture schema.sql → ≥1 file_index row ✅, symbol `users` (table) + `get_user` (proc) ✅, view→table edge `resolved` ✅ — verify thật bằng CLI, không chỉ test giả định.
- **Cắt phạm vi có chủ đích (chưa làm):** `[languages.sql] dialect = "generic"` config field (chỉ dùng `GenericDialect` cứng, chưa cho chọn dialect khác) — không có consumer nào cần ngay; index→table reference edge (CREATE INDEX ... ON table) không tạo — plan gốc không yêu cầu; `path`/`transitive_bfs`/`dependencies` tools CHƯA surface `edge_kind` (chỉ `callers`/`callees` — đúng 2 tool plan gốc nêu tên "callers/path" nhưng path để lại cho theo dõi sau, effort cao hơn do BFS đa bước).
- Đừng làm lại.

---

## 7. Benchmark & telemetry (xuyên suốt, bắt đầu từ P0.3)

- ✅ **`benchmarks/resolution/`** — ĐÃ XONG (2026-07-07, cùng phiên với việc audit file này). Corpus
  thật: go=gin, java=spring-petclinic (nhẹ, đúng nhánh gợi ý), csharp=eShopOnWeb, c=redis, cpp=fmt,
  js=express, php=monica, sql=sakila (mirror đa dialect). Output JSON đúng schema gốc
  `{lang, edges_total, tier_histogram, formal_pct, overlay_match_rate, wall_time}` — xem
  `benchmarks/resolution/results.json` (mỗi lần chạy) và README's bảng cho số baseline đầu tiên.
  **`overlay_match_rate` = `null` cho mọi ngôn ngữ, có chủ đích** (chưa Phase 2 provider nào tồn
  tại — không phải `0.0`, tránh nhầm với "provider chạy nhưng không giúp được gì").
  **Phát hiện quan trọng nhất của lần đo này: một bug crash thật** — index redis (C, `server.h`
  ~4700 dòng) crash toàn bộ với `UNIQUE constraint failed: symbols.qualified_name`, không sinh ra
  số nào. Root cause (xác nhận bằng bisection nhị phân + debug print tạm): C's symbol extractor
  coi mỗi lần *nhắc tên* một struct forward-declare làm tham số con trỏ là 1 symbol occurrence
  riêng; khi 1 dòng nhắc cùng tên struct 2 lần (2 tham số cùng kiểu, vd
  `moduleTypeCopyFunc(struct redisObject *fromkey, struct redisObject *tokey, ...)`), dedup nội bộ
  của `extract_file_data` (`pipeline.rs`) — vốn chỉ thử **một lần** hậu tố `#{line_start}` — đụng
  lại chính nó ở lần thử thứ 2, để lại 1 INSERT lỗi UNIQUE constraint không được xử lý, crash toàn
  bộ lần index (không riêng file đó). **Đã sửa**: vòng lặp dedup giờ thử hậu tố tăng dần cho đến
  khi thật sự unique. Test hồi quy `test_c_same_line_triple_name_collision_does_not_crash_indexing`
  (`pipeline.rs`) tái tạo tối thiểu pattern này, xác nhận crash y hệt nếu tắt fix. 527 test workspace
  xanh, clippy/fmt sạch sau fix. **Chưa sửa** (ngoài phạm vi, ghi rõ trong benchmark's README): root
  cause đầu tiên (coi type reference là symbol) vẫn còn — số `symbols_total`/`ambiguous` của C/C++
  trong benchmark này có nhiễu thật chưa định lượng được.
  **Phát hiện số liệu quan trọng thứ nhì**: `ambiguous` (không phải `textual`) là tier chiếm áp đảo
  ở hầu hết ngôn ngữ (go 54.3%, cpp 92.5%, js 66.7%) — xác nhận trực tiếp bằng số đo thật rằng
  `MAX_CALLEE_CANDIDATES=20` fan-out (§1.2) là trần chính xác cần Phase 2 lấp, không phải điều suy
  luận lý thuyết. `formal_pct=0.0%` mọi ngôn ngữ (đúng kỳ vọng — chưa Phase 2 provider nào tồn tại).
  SQL=0 symbols thật (P3.3 chưa làm, `.sql` chưa có trong `language_for_extension`), không phải lỗi.
  Đừng làm lại phần harness — muốn đo lại (vd sau khi Phase 2 provider đầu tiên hạ cánh), chạy lại
  `benchmarks/.venv/bin/python benchmarks/resolution/run_benchmark.py` trực tiếp.
- DoD tổng mỗi ngôn ngữ = fixture xanh **và** formal_pct/resolved_pct trên repo chuẩn đạt ngưỡng thống nhất (đặt sau lần đo baseline đầu — **đã có baseline thật ở trên**; gợi ý mục tiêu: Go/Java/C# formal ≥60% call edges nội-repo khi indexer có mặt — vẫn cần Phase 2 provider để đo được `formal_pct` > 0).

## 8. Rủi ro & guardrails

1. **Binary ngoài vắng mặt** → silent no-op (giữ nguyên triết lý); docs "cài X để đạt độ chính xác tối đa" + `indexing_status` hiển thị binary_found.
2. **Indexer = chạy build tool của repo** (Gradle/MSBuild/composer thực thi code tuỳ ý) → docs security note + off-switch per-language (`enabled=false`); cân nhắc yêu cầu opt-in tường minh cho java/csharp trên repo lạ.
3. **Heavy indexer trong watcher** → refresh policy (P2.6); tuyệt đối không nối scip-java/scip-clang vào on-save.
4. **Monorepo path lệch** → P0.2 bắt buộc trước Phase 2; match_rate thấp = tín hiệu path lệch.
5. **stack-graphs archived** → không đầu tư .tsg mới; kế hoạch thoát = P3.2 + P2.4.
6. **scip-php/scip-go community-grade** → nightly CI trên fixture + benchmark repo trước khi mặc định auto=on; có thể ship `enabled=None` (auto) nhưng docs ghi maturity.
7. **SQL động (string-concat, ORM)** → giới hạn cố hữu static analysis; ngoài scope, ghi docs.
8. **PHP ceiling:** Formal chỉ khi scip-php chạy được (cần vendor/); heuristic P1.2 là floor Resolved.

## 9. Thứ tự thực thi khuyến nghị (dependency graph)

```
P0.1 ✅ → P0.2 ✅ → P0.3 ✅ → P0.4 ✅ → P0.5 ✅   (P0 XONG TOÀN BỘ)
P1.1 ✅ ∥ P1.2 ✅ ∥ P1.3 ✅ ∥ P1.4 ✅ ∥ P1.5 ✅ (100%, xem ghi chú)   (PHASE 1 XONG TOÀN BỘ)
P2.1 ✅ (Go, V1 single-module, commit `6603e49`) ∥ P2.2 ∥ P2.3 ∥ P2.4 ✅ (Python, commit `9abb9f4`) ∥ P2.5 → P2.6   (P2.2/P2.3/P2.5/P2.6 mở)
sau P2: P3.1 ∥ P3.2   (P3.1/P3.2 vẫn mở)
P3.3 (SQL) ✅ XONG (commit `22965d2`) — độc lập, không chặn P3.1/P3.2
Benchmark harness: ✅ XONG (2026-07-07) — xem §7, kết quả baseline đầu tiên đã đo thật, 8/8 ngôn ngữ
P1.5's "using→namespace" nửa còn lại: ✅ XONG (§10-4) — không còn mục P1.x nào mở
```

Effort tổng ước lượng: P0 ≈ 1.5–2 tuần-người (P0.1-P0.3 đã xong trong 1 phiên); P1 ≈ 1–1.5 tuần; P2 ≈ 2–3 tuần (song song hoá tốt); P3 ≈ 2–3 tuần. SQL độc lập ≈ 1 tuần.

## 10-4. Điểm dừng phiên 2026-07-08 — sau khi P1.5's "using → namespace" xong (MỚI NHẤT — đọc mục này trước §10-3)

**Đã làm (P1.5 remainder — C# "using → namespace", toàn bộ):** xem mục P1.5 ở §4 (đã cập nhật ✅ đầy đủ ở đó — không lặp lại chi tiết ở đây). Việc này KHÔNG đụng SCIP providers — thuần Rust, zero external deps, làm độc lập với nhánh Phase 2 đang dở (Java/C#/PHP scip-*).

**Quyết định kiến trúc quan trọng nhất (lệch có chủ đích so với bản kế hoạch gốc — đọc trước khi động vào vùng này lần sau):** bản kế hoạch gốc (viết trong mục P1.5 ở phiên trước) đề xuất truyền `NamespaceMap` vào `extract_file_data`, đòi hỏi đảo thứ tự pipeline (parse-namespace-map-trước-rồi-mới-extract-file-data-song-song). Sau khi trace code thật (không chỉ đọc lại đề xuất cũ), quyết định KHÔNG làm vậy — threading `NamespaceMap` vào `rebuild_graph` (đúng điểm `crate_map`/`psr4` đã dùng, không đảo pipeline) đạt cùng kết quả với rủi ro thấp hơn hẳn, vì `rebuild_graph` đã chạy sau khi mọi file được parse+persist và đã có `call_sites`/`import_edges` sẵn để dùng. Nếu phiên sau cần sửa vùng này, đọc kỹ đoạn "Phần B" trong mục P1.5 trước khi cân nhắc đổi lại thiết kế.

**Bug thật phát hiện qua trace code (không nằm trong mô tả gốc của gap):** C# không có toán tử static-access riêng biệt — `.` dùng chung cho cả `helper.Greet()` (instance) lẫn `Helper.Greet()` (static). `receiver_is_type_path` (cờ quyết định một call có phải type-qualified hay không) chỉ được set ở nhánh `::` của `split_receiver_callee` — nhánh `.` không bao giờ set nó, bất kể receiver trông có giống tên Type (viết hoa) đến đâu. Hệ quả: MỌI static call kiểu `Type.Method()` trong C# trước phiên này đều bị coi là instance call, tier-2 tra `type_map.get("Type")` như tên biến (sai — nó là tên class) → miss → confidence rơi về `Textual` + fan-out toàn repo theo tên method trần. Lưu ý cho phiên sau: Java có gap giống hệt (`Math.max()` cũng đi qua nhánh `.`) nhưng KHÔNG được sửa ở đây — ngoài phạm vi P1.5, cần 1 việc riêng nếu muốn làm.

**Verify thật (không chỉ giả định):** 3 test tích hợp mới trong `pipeline.rs`, mỗi test dựng project C# thật trong temp dir và chạy `run_indexing_pipeline` thật (không mock) — bao gồm 1 test dựng ĐÚNG kịch bản gap: 2 class cùng tên `Helper` ở 2 namespace khác nhau, chỉ 1 được `using`, xác nhận resolve đúng target + không `Ambiguous`. Cả phần A (target_class fix) lẫn phần B (namespace narrowing) đều xác nhận THẬT SỰ fail khi tắt tạm nhánh tương ứng rồi restore lại — đúng convention P1.x. Toàn bộ workspace xanh (567 lib test, tăng từ 563 — 4 test mới ở `pipeline.rs` cộng dồn với việc 1 test cũ đổi assertion từ `inferred` sang `resolved`), `cargo clippy --workspace --all-targets --features scip-overlay -- -D warnings` sạch, `cargo fmt --all -- --check` sạch.

**Không còn việc P1.x nào mở.** Việc tiếp theo thuần là ưu tiên giữa các nhánh Phase 2/3 đang dở — xem §10-3 ngay dưới đây (danh sách ưu tiên ở đó vẫn đúng, chỉ bỏ P1.5 khỏi mọi danh sách "còn mở" vì đã xong).

## 10-3. Điểm dừng phiên 2026-07-08 — sau khi P2.4 (Python) xong (đọc §10-4 ở trên trước, mục này giữ nguyên làm lịch sử phiên trước)

**Đã làm (P2.4 — Python, toàn bộ):** xem §5's bảng, dòng P2.4 (đã cập nhật ✅ đầy đủ ở đó — không lặp lại ở đây). Commit `9abb9f4`.

**Phát hiện quan trọng nhất (bug thật, không phải suy đoán):** `scip-python` (thử qua `npx @sourcegraph/scip-python@0.6.6`) **crash toàn bộ** với `TypeError: Cannot read properties of undefined (reading 'indexOf')` trong `normalizeNameOrVersion` nếu CẢ `--project-name` LẪN `--project-version` đều thiếu và thư mục target không phải git repo (không có gì để tự suy ra version). Thử riêng từng cờ: chỉ `--project-name` KHÔNG đủ, vẫn crash — phải có CẢ HAI. `python_build_command` luôn truyền cả 2 tường minh (name = basename thư mục root, version = placeholder cố định `"0.0.0"`) để né hoàn toàn lỗi này, bất kể target có phải git repo hay không.

**Kiến trúc đáng chú ý (khác Go):** `resolve_binary` phải hỗ trợ CẢ standalone binary lẫn `npx` proxy (đúng yêu cầu cột "Prereq" gốc) — vì hàm `fn` pointer trong `ScipProvider` không capture closure được, `python_build_command`/`python_binary_version` phải tự kiểm tra `bin.file_name() == "npx"` lúc runtime để quyết định có chèn tên package `@sourcegraph/scip-python` vào args hay không. Xem `runner.rs::is_npx`.

**Verify thật bằng scip-python thật (qua npx, không phải fixture giả):** fixture Python cross-module (`main.py` import+gọi `pkg.helper.helper()`) → `calm index` → 1 edge upgrade `formal`/`formal_source='scip'`, match_rate=0.50; chạy lại → cache-skip đúng, không re-invoke npx (tránh phí thời gian download/check lại mỗi lần).

**Tiếp theo (không đổi so với §10-2, chỉ bỏ P2.4 khỏi danh sách vì đã xong):**
1. P3.2 (JS/TS, scip-typescript) — Node/npm đã có, `@sourcegraph/scip-typescript@0.4.0` resolve được trên registry nhưng CHƯA thử cài/chạy thật — nên thử trước khi code, như đã làm với Go/Python.
2. P2.2 (Java) — toolchain có nhưng tải release scip-java bị chặn trong sandbox này; cần tìm đường khác (coursier, Docker image `sourcegraph/scip-java`) trước khi bắt đầu code.
3. P2.3 (C#) — bỏ qua trong sandbox này (không có .NET SDK); không chặn các nhánh khác.
4. P2.6 (ops surface) — làm sau khi có ≥2 provider Phase 2 thật (đã có Go+Python) — có thể bắt đầu bất kỳ lúc nào từ đây, không còn lý do hoãn "tránh đoán trước shape" nữa vì đã có 2 case thật để tham chiếu.
5. P3.1 (scip-clang) — chưa thử; platform gate Linux x86_64 thoả trong sandbox này, chưa thử toolchain (`compile_commands.json`).

## 10-2. Điểm dừng phiên 2026-07-08 — sau khi P3.3 (SQL) xong

**Đã làm (P3.3 — SQL, toàn bộ, không có phần "V1 partial"):**
- `crates/calm-core/src/indexer/sql.rs` — module mới hoàn chỉnh, xem chi tiết đầy đủ ở §6/P3.3 (đã cập nhật ✅ trong mục đó — đọc ở đó, không lặp lại ở đây).
- Cột `edge_kind` thêm vào CẢ `call_sites` VÀ `call_edges` (thiết kế gốc chỉ nói `call_edges` — `call_sites` cần thêm để thread giá trị qua `rebuild_graph`), xuyên suốt tới `callers`/`callees` MCP tools.
- **Verify thật bằng CLI thật** (build binary, chạy `calm index` thật trên 3 fixture riêng — 8-object schema chính, 2-file cross-reference, dbt/Jinja robustness) — không chỉ dựa vào unit test.
- Toàn bộ workspace xanh sau mỗi bước, clippy `-D warnings` sạch, fmt sạch.
- Commit: `22965d2` (`feat(indexer): standalone SQL indexer via sqlparser (P3.3)`).
- Đừng làm lại.

**Bug tự bắt trong lúc làm (đáng lưu ý cho phiên sau viết regex nhiều dòng):** dùng `\` line-continuation bên trong raw string Rust (`r"...\<newline>..."`) KHÔNG hoạt động như C — raw string không xử lý escape, nên `\` + newline trở thành 2 ký tự literal trong pattern, làm regex compile "thành công" nhưng match sai (hoặc panic tuỳ pattern). Sửa bằng `concat!(r"...", r"...")` để nối nhiều dòng an toàn. Xem `sql.rs::create_header_regex`/`reference_regex` cho pattern đúng.

**Tiếp theo (không đổi thứ tự ưu tiên so với §10 dưới, chỉ bỏ P3.3 khỏi danh sách vì đã xong):**
1. **P2.4 (Python, scip-python)** hoặc **P3.2 (JS/TS, scip-typescript)** — cả 2 chỉ cần Node/npm (đã xác nhận có ở sandbox này), chưa thử chạy thật — nên thử cài+chạy thật trước khi code, như đã làm với Go/SQL.
2. P2.2 (Java) — toolchain có nhưng tải release scip-java bị chặn trong sandbox này; cần tìm đường khác (coursier, Docker image `sourcegraph/scip-java`) trước khi bắt đầu code.
3. P2.3 (C#) — bỏ qua trong sandbox này (không có .NET SDK); không chặn các nhánh khác.
4. P2.6 (ops surface) — làm sau khi có ≥2 provider Phase 2 thật để tránh đoán trước shape config (đúng tinh thần P0.4).
5. P3.1 (scip-clang) — chưa thử; cần `compile_commands.json`, platform gate Linux x86_64/macOS arm64 (sandbox này là Linux x86_64 — khả thi về platform, chưa thử toolchain).

## 10. Điểm dừng phiên 2026-07-08 — sau khi P2.1 (Go) xong (đọc §10-2 ở trên trước, mục này giữ nguyên làm lịch sử phiên trước)

**Môi trường sandbox này (không phải mọi môi trường):** Go 1.24.7 + network tới `proxy.golang.org` sẵn có → `go install github.com/scip-code/scip-go/cmd/scip-go@latest` cài và chạy được ngay (lưu ý: org đã đổi từ `sourcegraph/scip-go` sang `scip-code/scip-go`, khớp §2's ghi chú "SCIP → community governance 03/2026"). Node 22 + npm cũng có → `@sourcegraph/scip-typescript@0.4.0` và `@sourcegraph/scip-python@0.6.6` resolve được trên registry (chưa thử cài/chạy thật) → P2.4/P3.2 khả thi kỹ thuật, chưa verify. JDK 21+Gradle+Maven có sẵn nhưng tải release scip-java từ GitHub bị 403 qua proxy trong sandbox này (không thử coursier/Docker) → P2.2 ma sát cao hơn ở đây. Không có .NET SDK → P2.3 không khả thi trong sandbox này. `davidrjenni/scip-php`'s module path Go không đúng như kỳ vọng khi thử `go install` trực tiếp → P2.5 cần điều tra thêm.

**Đã làm (P2.1 — Go, V1 single-module):**
- `crates/calm-core/src/scip/provider.rs::GO` — entry thứ 2 trong bảng `ScipProvider`, xác nhận thiết kế P0.4 tổng quát hoá đúng thật (không cần sửa `mod.rs`/`runner.rs`'s core logic).
- `runner.rs::go_resolve_binary`/`go_build_command`/`go_toolchain_fingerprint`/`GO_SCIP_TIMEOUT` (180s, cao hơn Rust's 120s — Go compiler-driven typecheck chậm hơn LSP incremental).
- `config.rs::GoConfig` (mirror `RustConfig`, cùng `ScipConfig` 3-trạng thái).
- `mod.rs::run_go_overlay_and_log` — helper gộp refresh+log, tránh copy ~15 dòng lần thứ 3 ở mỗi trong 3 call site production (`lib.rs`, `watcher.rs`, `main.rs`) — chỉ 1 hàm mới cần sửa nếu provider thứ 3 xuất hiện.
- **Verify thật bằng scip-go thật** (không chỉ fixture giả): fixture Go 2-package (`main.go` gọi `pkg.Helper()`) → `calm index` → 2 edge upgrade thành `formal`/`formal_source='scip'`, match_rate=0.67; chạy `calm index` lần 2 (không đổi gì) → cache-skip đúng, KHÔNG re-invoke `scip-go`. Test `PRAGMA table_info`/`SELECT * FROM call_edges` xác nhận trực tiếp qua sqlite3 (python3, vì `sqlite3` CLI không có trong sandbox).
- 2 test mới: `go_explicit_off_is_a_noop_even_when_scip_go_is_on_path` (mod.rs), `go_binary_runs_rejects_a_nonexistent_path` (runner.rs) — mirror đúng 2 test tương ứng của Rust.
- Toàn bộ workspace: 658 test xanh (527+2 mới +... — bản build trước P2.1 đã ở 656 do 2 test go thêm vào 526 gốc, khớp; xem log thật trong commit), `cargo clippy --workspace --all-targets --features scip-overlay -- -D warnings` sạch, `cargo fmt --all -- --check` sạch.
- Commit: `6603e49` (`feat(scip): Go SCIP provider via scip-go (P2.1)`).
- Đừng làm lại V1.

**Cắt phạm vi có chủ đích (V2, chưa làm):**
- `go.work` multi-module enumerate — P0.4's `ScipProvider` chưa có field marker-file/multi-root discovery; Go V1 chỉ chạy 1 lần ở project root (đúng cho phần lớn checkout single-module thật, đúng tinh thần "hoãn field tới khi có 2nd/3rd case xác nhận shape" của P0.4's chính nó).
- P2.6 (ops surface: `calm scip run`, `--scip-file` CI ingest, refresh policy config, per-language `indexing_status`/`fitness_report`) — chưa làm, vẫn dùng chung sidecar `scip-stats.json` với Rust (ghi đè lẫn nhau nếu cả 2 provider cùng chạy 1 lúc — bug nhỏ có sẵn từ P0.4, không phải P2.1 gây ra, nên fix khi làm P2.6).

**Tiếp theo (ưu tiên theo môi trường thật, không chỉ theo bảng gốc):**
1. **P3.3 (SQL)** — độc lập hoàn toàn, không cần toolchain ngoài nào, effort thấp nhất trong các lựa chọn còn lại.
2. **P2.4 (Python, scip-python)** hoặc **P3.2 (JS/TS, scip-typescript)** — cả 2 chỉ cần Node/npm (đã xác nhận có), chưa thử chạy thật — nên thử cài+chạy thật trước khi code, như đã làm với Go.
3. P2.2 (Java) — toolchain có nhưng tải release bị chặn trong sandbox này; cần tìm đường khác (coursier, Docker image `sourcegraph/scip-java`) trước khi bắt đầu code.
4. P2.3 (C#) — bỏ qua trong sandbox này (không có .NET SDK); không chặn các nhánh khác.
5. P2.6 (ops surface) — làm sau khi có ≥2 provider Phase 2 thật để tránh đoán trước shape config (đúng tinh thần P0.4).

## 10.1 Điểm dừng phiên 2026-07-07 (Phase 0+1 xong) — LỊCH SỬ, xem §10 ở trên cho trạng thái hiện tại

Phase 0 (P0.1-P0.5) và Phase 1 (P1.1-P1.5, P1.5 partial) đã xong và verify đầy đủ (build/test/clippy/fmt xanh sau MỖI mục, không chỉ ở cuối). Mỗi mục Phase 1 đã có commit riêng (P1.3 `fdf0aaf`, P1.4 `7ba5fb5`, P1.5 `d7178b9`, P1.2 `7b7dec7`; P1.1 xem git log). Toàn bộ workspace ở lần verify cuối: 526 test, 0 fail.

Phát hiện kiến trúc quan trọng từ phiên này, áp dụng cho MỌI ngôn ngữ còn lại (Phase 2/3):
- `LangConstants.call_function_field` (1 string/ngôn ngữ) không đủ khi 1 ngôn ngữ có nhiều node kind gọi hàm với field khác nhau — đã tổng quát bằng `call_function_field_by_kind`. Kiểm tra field thật của MỌI node kind mới trước khi thêm vào `call_node_types`, đừng giả định giống node đã có.
- `split_receiver_callee` giờ nhận `.`/`->`/`::` (trước chỉ `.`/`::`) — có thể còn ngôn ngữ khác dùng separator khác chưa phát hiện (vd Kotlin `?.`, Swift chưa xem qua receiver thật).
- `walk_calls` giờ có fallback "tìm child theo kind khi field lookup thất bại" — hữu ích cho các node kind PHP-style tách object/scope+name làm 2, hoặc node có callee là positional child không field.
- Luôn dump AST thật (temporary `#[test]` + `eprintln!`, xoá sau khi verify) trước khi viết code dựa trên giả định field/node-kind — phiên này bắt được ít nhất 4 giả định sai (PHP object_creation_expression, C# variable_declaration initializer field, C struct_specifier text, Java method_invocation receiver) chỉ nhờ dump thật thay vì đoán.

Không còn phụ thuộc kỹ thuật nào chặn bất kỳ nhánh nào bên dưới — lựa chọn tiếp theo thuần là ưu tiên, không phải trình tự bắt buộc:
1. **Phase 2 provider đầu tiên** (khuyến nghị Go — `scip-go` đơn giản nhất, không cần build-tool network resolve như Java/C#) — có thể cắm thẳng vào bảng `ScipProvider` (P0.4 đã tổng quát hoá) mà không cần sửa lại `mod.rs`/`runner.rs`, chỉ thêm 1 entry + config `GoConfig` khi cần.
2. **P3.3 (SQL)** — độc lập hoàn toàn, effort M-L riêng.
3. ~~Benchmark harness~~ ✅ ĐÃ XONG (2026-07-07) — xem §7 cho kết quả baseline thật trên 8 ngôn ngữ +
   1 bug crash thật tìm thấy (C, đã sửa). Phát hiện chính: `ambiguous` (fan-out), không phải
   `textual`, là trần chính Phase 2 cần lấp — xác nhận bằng số đo thật, không phải suy luận.
4. Xác nhận `.github/workflows/scip-nightly.yml` chạy xanh thật trên GitHub Actions (push + đợi lịch hoặc `workflow_dispatch` thủ công) — chưa phiên nào verify tương đương ngoài local.
5. P1.5's "using→namespace-to-files" nửa còn lại (namespace không map trực tiếp thư mục cho C#) — cần pre-pass kiến trúc mới, xem ghi chú trong mục P1.5.
