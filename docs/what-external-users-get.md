# CALM MCP — Users khác thực sự nhận được gì?

> Tài liệu này mô tả chính xác những gì một user **cài CALM qua `npx @eilodon/calm-mcp` / npm / MCP Registry / GitHub Release** nhận được — phân biệt rõ với phần chỉ tồn tại trong môi trường dev của chính repo CALM. Mọi claim dưới đây đã được verify trực tiếp từ code (file:line), không suy từ docs/comment (những thứ này đôi khi tự thân đã lỗi thời — xem mục "Bài học phương pháp luận" ở cuối).
>
> **Chốt tại**: commit `5a3d03f` (2026-07-15), version `0.3.0` — **đã tag, đã release, đã verify live** (npm, GitHub Release, MCP Registry, `npx` smoke test đều xác nhận `--hooks` hoạt động trong binary published thật). Mục 6 dưới đây mô tả tính năng này với tư cách đã ship, không còn là "sắp có".

---

## 1. Cài đặt & phân phối

| Gì | Thực tế |
|---|---|
| Cách cài | `npx -y @eilodon/calm-mcp serve`, hoặc `calm setup --npx` tự ghi entry này vào `.mcp.json`/`.cursor/mcp.json`/`.vscode/mcp.json` |
| Cơ chế npm | `npm/calm-mcp/package.json` chỉ là wrapper mỏng — `optionalDependencies` trỏ tới 3 package theo platform (`linux-x64`/`linux-arm64`/`darwin-arm64`), `bin/calm-mcp.js` spawn binary thật + forward SIGINT/SIGTERM/SIGHUP. Không compile từ source, giống cơ chế esbuild/swc. |
| Binary release | `.github/workflows/release.yml:17-26` cross-compile đúng **3 target**: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (qua `cross`), `aarch64-apple-darwin` (native trên `macos-14`). Chưa có Windows/Linux gnu/macOS x64 trong ma trận thật (có 1 workflow thử nghiệm riêng, chưa gắn vào release). |
| Xác thực binary | `SHA256SUMS` **+ `actions/attest-build-provenance`** (Sigstore/Fulcio-backed) — thêm từ commit `636d003` (2026-07-15). **Verify sống** trên asset `v0.3.0` (`gh` 2.96.0, CLI hệ thống 2.45.0 quá cũ để có subcommand này): `gh attestation verify calm-x86_64-unknown-linux-musl.tar.gz -R Eilodon/CALM` exit 0, digest SHA256 trong payload khớp digest file tải về. Image container/GHCR ký riêng bằng `cosign` keyless — 2 cơ chế khác nhau, không hợp nhất (chủ đích, comment trong code giải thích). |
| MCP Registry | `server.json` (`io.github.Eilodon/calm-mcp`), publish qua `mcp-publisher` + GitHub OIDC (`.github/workflows/publish-mcp-registry.yml`) |
| `calm init` (không cờ) | Chỉ tạo `.calm/config.json` — không đụng AGENTS.md, không đụng hooks |
| Bootstrap lần đầu | Tự thêm `.calm/` vào `.gitignore`, spawn thread index nền, DB tại `.calm/index.db` (`crates/calm-server/src/lib.rs::bootstrap`) |
| Embedding model | `minishlab/potion-code-16M` (distilled từ CodeRankEmbed), nhúng vào binary qua `include_bytes!`. **Không dùng Git LFS** (đã gỡ 2026-07-12) — `build.rs` fetch + xác minh SHA256 từ HuggingFace lúc **compile** (trên máy build CI), cache lại, nhúng thẳng. Runtime user cuối 100% offline; có `allow_network_fallback` (mặc định `true`) cho phép tải lại 1 lần nếu asset nhúng bị hỏng — không phải cờ `--offline` tường minh. |
| Daemon dùng chung | **Mặc định bật** trên Linux/macOS (không phải opt-in như code comment cũ nói) — `scripts/mcp-launcher.sh:134-142`: khi launcher được gọi không kèm tham số thêm (đúng cách npm/plugin gọi), tự chọn `calm connect` (nhiều client share 1 process). Opt-out tường minh bằng `CI_MCP_LAUNCHER_NO_DAEMON=1`. |

---

## 2. 29 tool MCP, chia 8 giai đoạn workflow

Xác nhận bằng grep toàn bộ `#[tool(name = "...")]` trong `crates/calm-server/src/tools/`:

**Điều hướng**: `search`, `locate`, `file_overview`, `symbol_info`, `source`, `understand`, `symbols_batch`
**Sức khỏe repo**: `repo_overview`, `hotspots`, `fitness_report`
**Sửa code**: `edit_lines`, `edit_symbol`, `format_files`
**Gate an toàn**: `edit_context`, `diff_impact`
**Graph**: `callers`, `callees`, `dependencies`, `path`
**Bảo mật/test**: `scan_text`, `test_gap_hotspots`
**Pattern debt**: `pattern_debt_register`, `pattern_debt_status`
**Bộ nhớ**: `remember`, `recall`
**Phục hồi**: `indexing_status`, `session_context`
**Refresh overlay**: `scip_refresh`, `lsp_refresh`

### Toolset/preset — 2 tầng, không phải 1 danh sách phẳng

- **13 toolset chi tiết** (module-domain): `trace`, `locate`, `orient`, `memory`, `guardrails`, `recover`, `scip`, `lsp`, `security`, `testgap`, `inspect`, `edit`, `patterndebt`
- **5 preset cross-cutting cũ**: `full` (mặc định, 29 tool), `orient`, `trace`, `edit`, `compound`
- Cú pháp composable: `--preset "trace,security"` (hợp), `--preset "full,-edit"` (trừ) — token không nhận diện được = **hard error**, không âm thầm cấp full access

---

## 3. Lớp an toàn hóa việc agent tự sửa code — lợi thế cạnh tranh thật

- **Chặn ghi đè stale**: `edit_lines`/`edit_symbol` bắt buộc `expected_hash` từ lần đọc trước; hash lệch → reject
- **Khóa cross-process thật**: `flock` trên `.calm/edit.lock` (crate `fs4`) — chặn 2 process CALM khác nhau (vd Cursor + Claude Code) cùng sửa 1 repo, cùng pass hash-check, ghi đè âm thầm lẫn nhau
- **Chặn symlink/path traversal**: `resolve_repo_path` canonicalize + `starts_with(root)`, có test filesystem thật với symlink thật
- **Sanitize — 2 hệ thống độc lập trong `sanitize.rs`**:
  1. **Redact credential**: PEM key, `sk-`/`rk-` token, GitHub PAT, AWS key, JWT, Slack token, `Authorization: Bearer`, URL-embedded credential, env-style assignment
  2. **Phát hiện prompt-injection** (không sửa, chỉ cảnh báo qua `content_warning`): giả ChatML (`<|im_start|>`), `[INST]`/`[SYS]`, giả role marker `system:`, giả tag `</tool_result>`, phrasing "ignore previous instructions", jailbreak/persona-override, exfiltration phrasing, zero-width Unicode, một số biến thể tiếng Việt (19 pattern có nhãn)
- **`scan_text`**: quét cùng bộ heuristic injection cho nội dung KHÔNG qua index (kết quả WebFetch/WebSearch, báo cáo subagent...) — điểm mù mà `content_warning` của `source` không phủ tới
- **SIGTERM watchdog**: `libc::alarm()` kernel-level 10 giây (không phải async timer — timer async không kích hoạt được vì thread đọc stdio của rmcp bị block, đã verify bằng `/proc/<pid>/task/*/wchan`)
- **File permission 0600**: socket daemon, log daemon, `audit.log`, `memory.key`

---

## 4. Coverage ngôn ngữ — chính xác **24**, không phải 12/13

Đếm trực tiếp `LanguageSpec` entries trong `crates/calm-core/src/indexer/lang_constants.rs` = 24. Markdown và SQL **không tính** — cả hai bị loại khỏi registry một cách cố ý, có test đảm bảo 2 danh sách không giao nhau.

| Tier | Ngôn ngữ | Trong binary release? |
|---|---|---|
| Tier-0 (luôn bật) | Python, Rust, Go, JavaScript, TypeScript, Java | ✅ Có |
| Tier-0.5 mặc định (`tier0-5` bundle) | C, C++, Ruby, PHP, C#, Shell, R | ✅ Có |
| Tier-0.5 opt-in (feature flag riêng từng cái) | Kotlin, Swift, Scala, Dart, Lua, Elixir, Haskell, OCaml, Zig, PowerShell, Groovy | ❌ **Không** — grammar thật tồn tại trong code nhưng không compile vào binary release; user npx chỉ nhận `shallow_detect` (regex/line-scan, không call-graph) |

**Tổng: 6 + 7 = 13 ngôn ngữ có call-graph đầy đủ mặc định; 11 ngôn ngữ nữa parse được nhưng cần build lại với `--features lang-X`; 24 tổng cộng.**

Ngoài 24 này:
- **Markdown**: line-scan ATX-heading riêng (`#`...`######`), không có symbol extraction thật
- **SQL**: parser `sqlparser` riêng (grammar thật, không phải tree-sitter), có bảng/view/procedure nhưng không có call-graph
- **`.txt`**: hoàn toàn không được nhận diện — vô hình với indexer như file ảnh
- **Solidity/Circom/Move/Cairo/Vyper/TOML**: "recognized nhưng unparsed" — có row trong DB (path/hash) nhưng 0 symbol

### SCIP overlay (formal, compiler-grade)
**9 provider, phủ 12 ngôn ngữ**: `rust-analyzer` (Rust), `scip-go` (Go, kể cả multi-module `go.work`), `scip-python` (Python), `scip-typescript` (JS+TS), `scip-java` (Java + Kotlin trong cùng 1 pass), `scip-dotnet` (C#), `scip-php` (PHP), `scip-ruby`/Sorbet (Ruby), `scip-clang` (C+C++). Mỗi provider tự dò binary, im lặng bỏ qua nếu máy không có toolchain — zero behavior change.

### LSP overlay (bổ sung, không thay thế SCIP)
Chỉ 3: `rust-analyzer` (live-session, khác export SCIP batch), `gopls` (Go), `clangd` (C/C++).

### Benchmark thật (không phải số liệu đoán)
`benchmarks/resolution/` — Kotlin 89.6% rơi vào tier `ambiguous`, OCaml 86.3% tương tự, Dart cho symbol nhưng **0 call edge** (giới hạn grammar đã biết, không phải bug), `inferred%` = 0.0% cho toàn bộ 11 ngôn ngữ Phase B/C (Tier-2 type inference mới chỉ wire cho Tier-0).

---

## 5. Hướng dẫn workflow cho user ngoài

- **`calm_workflow` MCP Prompt**: luôn có sẵn trong mọi binary, gọi bất kỳ lúc nào (không cần cờ gì) — trả về đúng 8-stage workflow rút gọn
- **`get_info().with_instructions()`**: push tự động lúc `initialize` handshake — **mọi MCP client** đều nhận được dòng "Call the `calm_workflow` prompt first..." mà không cần làm gì (verify sống: chuỗi này đã hiện ra làm "MCP Server Instructions" ngay đầu phiên hội thoại này)
- **`calm init --agents-md`** (opt-in, tắt mặc định): ghi bản **rút gọn** (~700 ký tự, 8 dòng, hằng số `CALM_WORKFLOW_GUIDE`) vào AGENTS.md của project đích, bọc marker `<!-- calm:workflow:start/end -->`, idempotent — **không phải** bản AGENTS.md 17KB đầy đủ mà repo CALM tự dùng cho chính nó

---

## 6. `calm init --hooks` — đã ship trong `v0.3.0` (2026-07-15)

`calm init --hooks[=nudge|enforce|off]` scaffold một hook Claude Code **generic** (`.claude/hooks/calm-hooks.sh`, khác hoàn toàn với `calm-nudge.sh` nội bộ của chính repo CALM) vào project của user khác, với:

- Mặc định `nudge` (chỉ nhắc, không bao giờ chặn) — phải gõ tường minh `--hooks=enforce` mới nâng lên chặn thật (`exit 2`)
- Framing **best-effort rõ ràng**, nói thẳng cách bypass cụ thể (ghi đè `.calm/hooks.mode`, xóa script, sửa settings.json) ngay trong output cài đặt — không overclaim "unbypassable"
- `calm doctor` báo trạng thái thật (cross-check mode file + settings.json wiring + script tồn tại — không tin 1 chiều)
- Downgrade mode để lại dấu vết trong `.calm/audit.log` + thông báo 1 lần, không im lặng

**Đã verify live**, không chỉ "code xong": commit `fc45ab7` → push → CI xanh (`ci.yml`) → tag `v0.3.0` → `release.yml` xanh cả 7 job → `npm view @eilodon/calm-mcp version` trả về `0.3.0` → `npx -y @eilodon/calm-mcp@0.3.0 init --help` in đúng flag `--hooks` trong binary published thật. Xem `docs/superskills/specs/2026-07-15-calm-hooks-transparent-reactivation.md` để biết chi tiết thiết kế + trạng thái implementation.

---

## 7. Ranh giới rõ — cái gì KHÔNG bao giờ đi kèm user khác

Verify bằng grep `include_str!`/`include_bytes!` toàn bộ `crates/` + đọc `npm/calm-mcp*/package.json`'s `"files"` field (chỉ `["bin"]` hoặc `["calm"]`):

- **`.claude/hooks/calm-nudge.sh`** (67KB, cơ chế enforce nội bộ mà chính repo CALM dùng để tự dev CALM) — không bao giờ ship, không có cờ nào đưa nó ra ngoài
- **`docs/pattern-debt-registry.yaml`** — dữ liệu debt-tracking riêng của repo này; tool `pattern_debt_register`/`pattern_debt_status` (2 trong 29 tool) hoạt động generic trên bất kỳ repo nào, nhưng file YAML cụ thể này không ship
- **`.claude/skills/`** (super-skills: VHEATM, adr-commit, using-super-skills, tdd-verified...) — phương pháp luận dev riêng của team làm CALM, không phải bản sắc sản phẩm CALM, không liên quan gì tới việc cài CALM MCP

---

## 8. Rough edge cần biết

- **CI SCIP nightly**: root cause (Composer advisory chặn `scip-php`) đã vá, workflow đã tái cấu trúc thành job riêng từng ngôn ngữ (`.github/workflows/scip-nightly.yml`, commit `d299f03`/`636d003`) — lần chạy cron đầu tiên với cấu trúc mới (2026-07-15 05:35 UTC) đã xanh (`gh run list --workflow=scip-nightly.yml`, `conclusion: success`), sau 5 lần fail liên tiếp trước đó. 1 lần xanh chưa chứng minh hết flaky, nhưng root cause đã được xác nhận đúng bằng bằng chứng thật, không còn là "đã vá nhưng chưa thấy chạy".

---

## Bài học phương pháp luận (rút ra từ quá trình verify tài liệu này)

Tài liệu phân tích *trước* bản này (viết cách đây vài phiên) sai ở 2 chỗ đáng chú ý, cả hai đều do tin docs/comment thay vì đọc thẳng cơ chế:
1. Đếm ngôn ngữ mặc định (13) tưởng là tổng, bỏ sót 11 ngôn ngữ Phase B/C feature-gated → tổng thật là 24
2. Tin comment cũ trong `daemon.rs` nói daemon "opt-in" — trong khi `scripts/mcp-launcher.sh` (script thực thi thật) đã tự chuyển mặc định từ 2026-07-11

Ngay cả `docs/architecture.md` — sửa ngay trong ngày viết tài liệu này — cũng có 1 dòng sai (embedding model "qua Git LFS", trong khi LFS đã gỡ). **Nguyên tắc áp dụng xuyên suốt tài liệu này**: mọi con số/claim cụ thể đều verify trực tiếp bằng grep/test/build/`gh run list`, không lấy từ mô tả — kể cả mô tả "mới sửa hôm nay".
