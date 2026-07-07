# CALM — Kế hoạch Formal-tier cho 8 ngôn ngữ còn lại (bản đã audit)

> **Ngày:** 2026-07-07 · **Trạng thái:** P0.1–P0.3 ĐÃ XONG (commit `20f4265`, `40e6b40`, `e0471f9` trên `main`) — P0.4 trở đi CHƯA thực thi. Xem §3 để biết chi tiết những gì đã làm; đừng làm lại.
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

### P0.4 — Tổng quát hoá runner thành `ScipProvider`
- **File mới:** `crates/calm-core/src/scip/provider.rs`; refactor `scip/mod.rs::run_overlay`, `runner.rs`, `config.rs`.
- **Thiết kế:**
  ```rust
  pub struct ScipProvider {
      pub lang: &'static str,              // "rust", "go", ...
      pub marker_files: &'static [&'static str],   // ["go.mod"], ["pom.xml","build.gradle","build.gradle.kts"], ...
      pub binary_names: &'static [&'static str],   // ["scip-go"]; rust giữ probe đặc thù (rustup/VS Code)
      pub invoke: InvokeSpec,              // args template: {root} {out}; rust = ["scip","{root}","--output","{out}"]
      pub cache_inputs: CacheSpec,         // lockfile globs + toolchain probe cmd ("go version", "java -version", ...)
      pub prereqs: &'static [Prereq],      // CompileCommands | VendorAutoload | NodeModules | DotnetSdk...
      pub timeout: Duration,               // rust 300s; java/clang cao hơn
      pub default_policy: RefreshPolicy,   // OnSave | MinInterval(Duration) | OnDemand
  }
  ```
  - `run_overlay_for(provider, conn, repo_root, sub_root, cfg, dirty)` — pipeline chung: resolve binary → cache key (per sub_root!) → run → parse(rebase=sub_root) → ingest. Cache file: `.calm/scip-{lang}-{hash(sub_root)}.cache`.
  - **Multi-root discovery:** scan marker files (bounded depth, tôn trọng ignore-dirs của indexer) → chạy per sub-root. Go: nếu có `go.work` thì lấy danh sách module từ đó.
  - `config.rs`: generalize `[languages.<lang>] scip = {...}` — thêm `GoConfig/JavaConfig/CsharpConfig/...` cùng shape `RustConfig { scip: ScipConfig }` (+ field riêng: `clang.compile_commands`, `sql.dialect`). `ScipConfig` giữ nguyên (đã tổng quát).
  - Dirty-keys: generalize `rust_source_dirty_keys` → `source_dirty_keys(conn, lang_exts)`.
- **DoD:** Rust đi qua provider table, toàn bộ test scip cũ xanh, không đổi hành vi. Effort: **M**. **Làm xong mới bắt đầu Phase 2.**

### P0.5 — Fixture `multi_lang_workspace` + CI nightly
- **Vị trí:** `crates/calm-core/tests/fixtures/multi_lang_workspace/` — mỗi ngôn ngữ một dự án mini:
  - `go/`: go.mod + `helper.go`/`main.go` cùng package (gap chuẩn); `java/`: Maven tối giản, static call cùng package không import; `csharp/`: .csproj + call qua namespace; `c/`: helper.c/main.c + compile_commands.json tối giản; `cpp/`: 1 virtual method call; `js/`: package.json + require + call; `php/`: composer.json + require_once + `$obj->method()`; `sql/`: schema.sql (CREATE TABLE users + CREATE VIEW tham chiếu + 1 procedure CALL).
- **CI:** job **nightly** (không per-PR) cài rust-analyzer/scip-go/scip-java/scip-dotnet, chạy integration tests đánh dấu `#[ignore]` bằng `--ignored`. Per-PR chỉ chạy phần không cần binary ngoài.
- **DoD:** fixture commit + nightly workflow xanh. Effort: **S-M**.

---

## 4. PHASE 1 — Zero external deps (song song được, sau P0)

### P1.1 — JavaScript stack-graphs (XS→S)
- **Option A (khuyến nghị):** thêm dep `tree-sitter-stack-graphs-javascript = "0.3.0"` (workspace); `formal.rs` thêm `load_javascript()` mirror `load_typescript` (crate JS xử lý CommonJS require); wire tại mọi nơi gọi `load_typescript` (dùng `callers` tool để liệt kê ~8 site). `.jsx` → kiểm tra `language_for_extension`; nếu cần grammar variant thì mirror cơ chế `TsxVariant`.
- **Option B (fallback nếu version conflict):** đăng ký khoá `"javascript"` trỏ cùng SGL/builtins đã build cho TS.
- **Lưu ý:** upstream archived — đây là giải pháp giữ chỗ; đường dài là P3.2 (scip-typescript). KHÔNG đầu tư viết .tsg mới.
- **DoD:** fixture js def/ref → edge `formal` (`formal_source='stack_graphs'`).

### P1.2 — PHP heuristics (S/M) — ĐÚNG THỨ TỰ
1. **Call extraction trước tiên** (`lang_constants.rs` entry "php"): thêm `member_call_expression`, `scoped_call_expression`, `nullsafe_member_call_expression`, `object_creation_expression` vào `call_node_types`. ⚠️ Các node này dùng field `name` cho callee (khác `function_call_expression` dùng field `function`) — kiểm tra walker trích call trong parser.rs có hỗ trợ per-node-kind field không; nếu không, thêm mapping nhỏ (tiền lệ: Java dùng `"name"`). Verify node-kind names bằng grammar thật: parse thử fixture qua test.
2. `imports.rs`: thêm nhánh `"php"` — bắt require/include (kiểm tra node kind thật trong tree-sitter-php; dự kiến `require_expression`/`include_expression` hoặc unary dạng `require_once_expression`) + `namespace_use_declaration` (use). Xử lý `require_once __DIR__ . '/x.php'` (binary concat với `__DIR__`) — pattern phổ biến nhất thực tế.
3. **PSR-4:** parse `composer.json` → `autoload.psr-4` (namespace prefix→dir); resolve `use App\Service\Foo;` → `<dir>/Service/Foo.php` nếu file tồn tại → vào `import_map`. Đây là đường resolve chính cho PHP hiện đại (require thủ công hiếm).
4. `assignment_nodes` += `"php" => ["assignment_expression"]`; `extract_type_map_from_tree` thêm nhánh php: typed properties (PHP 7.4+), param type hints, `$x = new Foo()` constructor inference (mirror `rust_constructor_type` → `php_constructor_type`).
- **DoD (fixture php):** require_once → `resolved`; `$this->helper->run()` với typed property → `inferred`; class autoload qua use+PSR-4 → `resolved`.

### P1.3 — Tier-1.5 package-scope cho Go/Java/C#/C/C++ (S mỗi ngôn ngữ) — quick-win giá trị nhất
- **V1 (làm trước, không schema change):** trong `rebuild_graph` candidate selection (pipeline.rs:642-649), chèn bậc ưu tiên **same-dir** giữa `same_file` và global fan-out, áp cho `go|java|c|cpp` (+ header/impl pairing theo basename cho c/cpp): nếu có ứng viên cùng thư mục → chỉ lấy chúng. Diệt fan-out noise + cho scip-go/scip-java thứ để upgrade.
- **V2 (sau khi đo):** nâng confidence — pre-pass build `package_symbols` (Go: dir+package clause; Java: dir; C#: bảng namespace→symbols) đưa vào `FileContext`, `resolve_tier1` check thêm → `Resolved`. C#: cần trích `namespace_declaration`/`file_scoped_namespace_declaration` per-file lúc index (lưu vào bảng phụ hoặc derive từ qualified_name).
- **Lý do:** ngữ nghĩa Go thật (package = compilation unit); safety net khi binary ngoài vắng; baseline đo giá trị cộng thêm của overlay.
- **DoD:** fixture go same-package cross-file → 1 edge đúng target (không fan-out/không rỗng); java static-call cùng package tương tự.

### P1.4 — C/C++ heuristics (S)
- `imports.rs`: `preproc_include` cho c/cpp — `#include "x.h"` match theo basename trong repo (ưu tiên cùng dir); bỏ qua `<...>` system headers.
- `extract_type_map_from_tree`: nhánh c/cpp — `declaration` (type_identifier + declarator/pointer_declarator), field_declaration struct → mở Tier-2 cho `var->method()`/`var.method()`.

### P1.5 — C# heuristics (S/M)
- ⚠️ "Thêm `using` vào import_node_types" KHÔNG đủ: `import_map` là tên→file, `using System.Text` là namespace. Cần bảng **namespace→files** (từ namespace_declaration khi index) rồi resolve using qua đó.
- `csharp_constructor_type`: `var x = new Foo(...)` (mirror rust_constructor_type); type_map: `parameter`, `field_declaration`, local declaration có kiểu tường minh.

---

## 5. PHASE 2 — SCIP providers (độc lập nhau, chia song song; cần P0.2–P0.4 xong)

Mỗi provider = 1 entry bảng + probe prereq + integration test nightly trên fixture. Shape chung: auto-detect (ScipConfig 3 trạng thái y Rust), silent no-op khi thiếu binary/prereq, log info khi `enabled=Some(true)` mà thiếu.

| # | Provider | Markers | Invoke | Cache key inputs | Prereq/policy | Ghi chú |
|---|---|---|---|---|---|---|
| P2.1 | go | `go.mod` (enumerate qua `go.work` nếu có) | `scip-go --output {out}` tại module dir | hash(go.mod+go.sum) + `go version` + dirty .go trong module | Go toolchain; policy OnSave/MinInterval ok (nhẹ) | Multi-module TỰ xử lý (upstream incomplete); mỗi module một run + rebase P0.2 |
| P2.2 | java | `pom.xml`/`build.gradle(.kts)`/`settings.gradle` | `scip-java index --output {out}` | build files + lockfiles + JDK version | JDK + build resolve (mạng lần đầu). **Policy: OnDemand/MinInterval(15m+)** — full build, KHÔNG on-save. Docs: khuyến nghị Docker `sourcegraph/scip-java` cho CI | Giữ stack-graphs Java làm fallback. **Bonus: Kotlin/Scala free** — thêm ext mapping khi bật |
| P2.3 | csharp | `*.sln`/`*.csproj` | `scip-dotnet index` | csproj/sln + packages.lock.json + `dotnet --version` | .NET 8 SDK; policy MinInterval | |
| P2.4 | python | `pyproject.toml`/`setup.py`/`requirements.txt` | `scip-python index . --output {out}` | lockfile + `python --version` | npm package (cần node) — probe cả binary lẫn `npx` | Nâng Python lên formal THẬT (hiện chỉ stack-graphs archived) |
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

### P3.3 — SQL → datafusion-sqlparser-rs (M-L, độc lập hoàn toàn — chạy song song bất kỳ lúc nào)
- **Module mới** `crates/calm-core/src/indexer/sql.rs` (không ép vào khung LangConstants/tree-sitter). Dep: `datafusion-sqlparser-rs`.
- Extension mapping: `"sql" => Some("sql")` trong `language_for_extension`.
- **Symbols:** CREATE TABLE/VIEW/MATERIALIZED VIEW/PROCEDURE/FUNCTION/TRIGGER/INDEX → rows trong `symbols` (kind: Struct cho table, Function cho proc/fn...).
- **Edges:** view/proc → bảng trong FROM/JOIN; proc → proc qua CALL/EXEC. Confidence `resolved` khi khớp tên (schema-qualified ưu tiên) trong repo. ⚠️ Thêm cột `edge_kind TEXT DEFAULT 'call'` vào call_edges (giá trị `'reference'` cho FROM/JOIN) để `callers`/`path` không trình bày JOIN như lời gọi hàm — quyết định schema, làm cùng migration P0.3 cho đỡ 2 lần migrate.
- **Robustness:** split per-statement (tôn trọng `$$` bodies); statement fail parse → bỏ qua statement đó, không bỏ file; file chứa `{{ }}`/`{% %}` (dbt/Jinja) → fallback shallow-scan regex (`FROM x`, `CALL x`) confidence `textual`. Dialect: `[languages.sql] dialect = "generic"` (postgres/mysql/mssql/...).
- Same-language filter trong rebuild_graph không cản SQL→SQL — không cần đổi.
- DoD: fixture schema.sql → ≥1 file_index row, symbol `users` (table) + `get_user` (proc), view→table edge `resolved`.

---

## 7. Benchmark & telemetry (xuyên suốt, bắt đầu từ P0.3)

- `benchmarks/resolution/`: harness clone repo OSS pinned tag mỗi ngôn ngữ — go: gin; java: guava (hoặc spring-petclinic cho nhẹ); csharp: eShopOnWeb; c: redis; cpp: fmt; js: express; php: monica (hoặc 1 plugin WP); sql: sakila. Chạy `calm index` (± providers) → JSON `{lang, edges_total, tier_histogram, formal_pct, overlay_match_rate, wall_time}`.
- DoD tổng mỗi ngôn ngữ = fixture xanh **và** formal_pct/resolved_pct trên repo chuẩn đạt ngưỡng thống nhất (đặt sau lần đo baseline đầu; gợi ý mục tiêu: Go/Java/C# formal ≥60% call edges nội-repo khi indexer có mặt).

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
P0.1 ✅ → P0.2 ✅ → P0.3 ✅ → P0.4 ⬜ → P0.5 ⬜   (tuần tự, nền tảng — dừng ở P0.3, xem banner đầu file)
sau P0: P1.1 ∥ P1.2 ∥ P1.3 ∥ P1.4 ∥ P1.5  (song song)
sau P0.4: P2.1 ∥ P2.2 ∥ P2.3 ∥ P2.4 ∥ P2.5 → P2.6
sau P2: P3.1 ∥ P3.2
P3.3 (SQL): bất kỳ lúc nào sau P0.3 (cần cột edge_kind) — CÓ THỂ BẮT ĐẦU NGAY, không phụ thuộc P0.4/P0.5
Benchmark harness: dựng ngay sau P0.5, đo baseline trước Phase 2 để chứng minh giá trị overlay
```

Effort tổng ước lượng: P0 ≈ 1.5–2 tuần-người (P0.1-P0.3 đã xong trong 1 phiên); P1 ≈ 1–1.5 tuần; P2 ≈ 2–3 tuần (song song hoá tốt); P3 ≈ 2–3 tuần. SQL độc lập ≈ 1 tuần.

## 10. Điểm dừng phiên này (2026-07-07)

P0.4 chưa bắt đầu — cân nhắc lại trước khi làm: nó là refactor thuần (KHÔNG đổi hành vi, per DoD gốc), giá trị chỉ hiện ra khi Phase 2 có provider thứ 2 thật để cắm vào bảng `ScipProvider`. Xây abstraction với đúng 1 case (Rust) có rủi ro đoán sai shape. 3 lựa chọn cho phiên sau, xem `docs/superskills/session-state-2026-07-07-04.md` để biết chi tiết bối cảnh:
1. Làm P0.4 + P0.5 trước (đúng thứ tự gốc của kế hoạch).
2. Bỏ qua P0.4, làm thẳng 1 provider Phase 2 cụ thể (vd Go) trên shape hiện tại của Rust, tổng quát hoá sau khi có 2 case thật.
3. P3.3 (SQL) độc lập hoàn toàn, có thể làm bất kỳ lúc nào không cần chờ P0.4/P0.5.
