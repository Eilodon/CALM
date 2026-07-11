---
title: 4 tính năng lấy cảm hứng từ DPS-Superskills cho CALM — Spec + audit-design
date: 2026-07-11
author: ybao (qua phiên chat với Claude)
SPEC_APPROVED: true
SPEC_ESCALATION: false
ESCALATION_FINDING: ""
process_note: >
  Adapted invocation — spec này không đi qua `brainstorming` trước; nó là
  bản chính thức hoá của 1 phân tích chat-based đã có đủ nội dung tương
  đương (rationale, tradeoff, adopt-verdict) cho cả 4 mục. audit-design
  chạy trực tiếp trên nội dung này thay vì trên output của brainstorming.
  Compressed ceremony, không compressed evidence discipline.
---

# 4 tính năng lấy cảm hứng từ DPS-Superskills cho CALM

Nguồn: nghiên cứu `/home/ybao/B.1/DPS-Superskills` (1 MCP server + skill
methodology khác) đối chiếu với CALM's own source, tìm cơ chế transferable.
4 mục dưới đây đã qua vòng "verify trước khi recommend" (đọc source thật,
không suy luận) ở phiên phân tích trước; giờ audit thêm 1 lớp risk trước
khi quyết định implement.

---

## Spec — 4 tính năng đề xuất

### #1 — Pattern-debt / duplicate-code tracker

Tool mới `pattern_debt_register(anchor_symbol, note)` / `pattern_debt_status(topic)`,
dùng `search(kind="similar")` (`crates/calm-core/src/search.rs::search_similar`,
796-850) để tìm code trùng lặp ngữ nghĩa, thay cho `grep_cmd` tĩnh của
DPS-Superskills. Anchor lưu bằng `symbol_qn` (không phải raw `path+line`,
vì `chunk_at` — `embedding.rs:451-483` — resolve theo `path+line` chính
xác, dễ trỏ sai sau khi file bị sửa).

### #3 — Ambient injection của memory notes vào `edit_context`/`locate`

Thêm field `related_notes` vào `EditContextOutput` (`guardrails.rs:628-676`,
cùng pattern với `co_changed_files` đã có), lấy từ 1 hàm mới
`notes_for_path(conn, path)` join `project_memory_refs.ref_path = path`
(bảng đã có, `schema.rs:137-144`). Notes tự động xuất hiện, không cần
agent tự gọi `recall()`.

### #4 — MCP tool capability annotations + static lethal-trifecta assertion

Khai báo `rmcp::model::ToolAnnotations` (readOnlyHint/destructiveHint) cho
toàn bộ tool set (hiện tại: 0 chỗ dùng `ToolAnnotations` trong repo — xác
nhận qua grep). Thêm 1 unit test assert không tool nào set đồng thời
network + untrusted-content-exposure + destructive. Dựa trên xác nhận: embedding
model vendored-in-binary theo default (`lib.rs:508-554`), daemon dùng Unix
socket không TCP (`daemon.rs:39-97`), 0 axum/TcpListener/SSE trong repo.

### #5 — Structural (session-state) confirm gate cho `edit_symbol`/`edit_lines`

Thay vì free-text `reasoning` (như DPS-Superskills' `execution_pipeline.ts:76-100`,
gameable bằng keyword-stuffing), gate dựa trên việc `edit_context` đã thực
sự được gọi cho đúng symbol trong session hiện tại — field mới
`edit_context_reviewed: HashMap<String, i64>` trong `session_log`, tách
khỏi `explored_symbols` chung (hiện dùng chung bởi 8 tool khác nhau, xác
nhận qua grep `track_symbol`, nên không đủ để biết riêng `edit_context` đã
chạy chưa). Gate tại `edit.rs:248`.

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-11 | trigger: NORMAL (adapted, no prior brainstorming spec) -->

**Tier:** 2 (Production) baseline — **escalate cục bộ lên Tier 3 cho #5**:
subsystem `session_log`/daemon đã có lịch sử incident thật (WAL 3.8GB do
orphaned child process, SIGTERM shutdown hang, cross-process edit race —
cả 3 đều sống trong đúng vùng code #5 định mở rộng), nên áp "past incident"
trigger của Tier 3 riêng cho mục này.
**Date:** 2026-07-11

### Failure Modes

**#1 Pattern-debt tracker**
1. **Anchor silently lost sau rename/refactor, báo "0 remaining" (= false "resolved") thay vì "anchor not found"** — nếu tool không phân biệt rõ 2 trạng thái này — HIGH — mitigation in plan: NO (chưa thiết kế trạng thái `anchor_lost` tường minh)
2. **Embedding staleness giữa lúc file vừa sửa và lúc reindex nền chạy xong** — `search_similar`/`SearchOutput` hiện không có field freshness nào (khác hẳn `edit_context.index_freshness.stale_callers` đã có cho graph edges) — MEDIUM-HIGH — mitigation in plan: NO
3. **Nhồi structured fields (anchor, baseline count, resolution_trigger) vào `project_memory.content` free-text làm ô nhiễm FTS5 search của `recall()` thông thường** — MEDIUM — mitigation in plan: NO (đề xuất gốc "tái dùng project_memory, không cần bảng mới" bị rút lại sau audit — xem Assumptions)

**#3 Ambient notes injection**
1. **Staleness ở cấp file (không phải symbol) làm noise áp đảo tín hiệu đúng ở chính nhóm file quan trọng nhất — hub file** (175 hub symbol trong chính repo CALM) — vì match theo `ref_path` không theo range, 1 note cũ về file sẽ bám theo mọi symbol trong file đó mãi mãi — HIGH — mitigation in plan: NO
2. **Thay đổi silent lên 2 tool có caller_count/coreness cao nhất hệ thống** (`edit_context` caller_count=7, `locate` caller_count=6, cả 2 đều là `core_symbols`) — nếu code JOIN mới lỗi/chậm, ảnh hưởng lan ra toàn bộ Stage 2/5 của mọi user, không chỉ riêng feature memory — MEDIUM-HIGH — mitigation in plan: NO (chưa nêu rõ yêu cầu fail-open)
3. **Mở lại bề mặt prompt-injection mà CALM đã chủ động phòng ở `source`/`understand` (`content_warning`/`scan_text`) nhưng chưa che cho `remember`/`recall` content** — biến 1 lần đọc chủ động (`recall()`, agent đã được huấn luyện cảnh giác) thành 1 injection point tự động, không thể bỏ qua, trên đúng 2 tool gọi nhiều nhất — MEDIUM — mitigation in plan: NO

**#4 Capability annotations**
1. **Tuyên bố "no network" chỉ đúng ở tầng MCP-tool, không đúng ở tầng process capability thật** — CALM có LSP-overlay/SCIP subprocess (rust-analyzer, gopls, clangd, scip-go...) nằm ngoài phạm vi đã audit; các process con này tự nó có thể có network access CALM không kiểm soát trực tiếp — nếu claim "local-only" không nêu rõ ngoại lệ này, đây đúng dạng "aspirational claim không match reality" mà chính Gotchas của audit-design đã cảnh báo — HIGH (cho độ tin cậy của tuyên bố, không phải cho rủi ro kỹ thuật trực tiếp) — mitigation in plan: NO
2. **Unit test tĩnh chỉ bắt được tool đã khai annotation sai, không bắt được tool mới bị quên khai annotation hoàn toàn** (default = không set cờ nào = luôn pass assertion) — tạo cảm giác an toàn giả — MEDIUM — mitigation in plan: NO
3. **Annotation là thông tin cho client (MCP spec: informational, client không bắt buộc enforce), không phải cơ chế chặn ở server** — khác hẳn DPS-Superskills' runtime rejection thật — MEDIUM — mitigation in plan: NO (chấp nhận được nếu ghi rõ đây là "khai báo minh bạch", không phải "enforcement")

**#5 Structural confirm gate**
1. **Gate chỉ chứng minh `edit_context` ĐÃ ĐƯỢC GỌI, không chứng minh agent ĐÃ ĐỌC kết quả** — agent có thể gọi rỗng (spam call, bỏ qua response) rồi edit ngay sau — đây là 1 gaming vector khác, chi phí ngang với keyword-stuffing của DPS-Superskills, chỉ khác hình thức — **đây là finding quan trọng nhất của toàn bộ audit này** — HIGH — mitigation in plan: NO (thiết kế gốc coi đây là điểm mạnh, thực ra chưa đóng được)
2. **`session_log` reset khi daemon restart/reconnect** (đã là limitation biết trước cho `session_context`, AGENTS.md Stage 8) — nhưng #5 nâng nó từ "gây khó chịu khi mất định hướng" thành "chặn hẳn thao tác ghi" — cùng 1 limitation, hệ quả nặng hơn hẳn ở use-case mới — MEDIUM — mitigation in plan: NO
3. **UX của lỗi mới (phải gọi tool khác trước, không chỉ thêm 1 param) dễ gây thrash nếu message không nêu chính xác symbol/tool cần gọi** — MEDIUM — mitigation in plan: PARTIAL (đã gợi ý tái dùng format `suggested_next`, chưa specify đầy đủ)

### Layer Signals

- **L1 Logic (#1)**: chưa verify `chunk_at`'s sliding-window chunk boundary có luôn align với symbol boundary sau khi re-resolve theo `symbol_qn` hay không — comment trong `embedding.rs:444-450` gợi ý chunk là window trượt (overlapping), không phải 1-chunk-1-symbol cứng — chiến lược "re-resolve by symbol" trong thiết kế gốc **chưa được chứng minh**, chỉ mới suy luận.
- **L2 Concurrency (#1)**: 2 agent cùng đăng ký debt-entry cho 2 bug khác nhau nhưng auto-slug trùng nhau → upsert theo `topic` unique sẽ ghi đè âm thầm.
- **L2 Concurrency (#5)**: xem Failure Mode 1 — trọng tâm của toàn bộ audit.
- **L3 Data (#1, #3)**: `migrate_add_column` helper đã có sẵn (`schema.rs:384-403`) nên schema change cơ học an toàn, nhưng #3 cần thêm index theo `ref_path` (hiện chỉ có index theo `topic`) và phải verify path *normalization* của `ref_path` khớp chính xác format `c.path` mà `edit_context`/`locate` dùng — lệch format sẽ khiến JOIN luôn trả 0 dòng một cách âm thầm ("trông như xong, thực ra không làm gì").
- **L5 Security (#3, #4)**: xem Failure Mode tương ứng ở trên.
- **L6 Observability (#1, #3, #5)**: cả 3 đều thiếu tín hiệu phân biệt "không có gì để báo" vs "cơ chế báo bị hỏng" (anchor_lost/join-mismatch/gate-state-missing) — cùng 1 lớp lỗi lặp lại ở 3 nơi khác nhau, đáng để xử lý bằng 1 quy ước chung thay vì 3 giải pháp riêng lẻ.
- **L7 Cross-cutting**: không có rate-limit/regulated-data liên quan (tool local).

### Assumptions to Verify

- **ASSUMED** (#4): "CALM hoàn toàn local-only, không network" — chỉ đúng ở tầng MCP tool surface đã audit; **chưa verify** khả năng network của LSP/SCIP subprocess (rust-analyzer/gopls/clangd/scip-go). Phải verify trước khi viết bất kỳ claim nào ra SECURITY.md/README.
- **ASSUMED** (#4): quyền của Unix socket file (`set_socket_perms`) — chưa đọc implementation thật, chỉ thấy tên hàm. Trên máy multi-user, nếu socket world-accessible thì "local-only = an toàn" có lỗ hổng khác (access boundary, không phải network).
- **ASSUMED** (#1): "không cần bảng mới, tái dùng `project_memory`" — **RÚT LẠI sau audit**: nên dùng bảng `pattern_debt` riêng cho structured field, tránh ô nhiễm FTS5 của `recall()` thường (Failure Mode 3).
- **ASSUMED và SAI như phát biểu ban đầu** (#5): "structural gate không thể bị game bằng chữ" — đúng là không gameable *bằng chữ*, nhưng gameable bằng *hành động rỗng* (gọi mà không đọc) — cùng mức độ dễ như DPS-Superskills' vấn đề gốc, chỉ khác hình thức.
- **ASSUMED** (#3): staleness cấp file "đủ dùng" — audit cho thấy đây là điểm yếu nghiêm trọng nhất của #3, đặc biệt ở hub file.

### Abductive Hypotheses

1. **#3 × #5 coupling**: nếu #3 (ambient notes) không fail-open và `edit_context` bắt đầu lỗi do bug ở `notes_for_path`, thì #5 (gate yêu cầu `edit_context` đã chạy thành công) sẽ khiến agent **không bao giờ** vượt được gate — 1 bug ở tính năng tiện ích (#3) leo thang thành hard-block toàn bộ khả năng edit (#5). Đây là lý do yêu cầu "fail-open" của #3 không còn là nice-to-have, mà là hard dependency một khi #5 tồn tại.
2. **#4's framing quá hẹp**: phân tích gốc đếm "CALM chỉ có 2 write tool" để kết luận lethal-trifecta risk gần 0 — chỉ đúng ở tầng *MCP tool*, bỏ sót tầng *process capability* thật (LSP/SCIP subprocess = đúng loại `process.spawn` mà chính DPS-Superskills liệt vào denylist mặc định). Model rủi ro cần mở rộng phạm vi trước khi tự tin kết luận "gần như miễn phí".

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->

- **#1 Pattern-debt tracker**: **PASS WITH FLAGS** — sửa thiết kế lưu trữ (bảng riêng, không nhồi vào `project_memory`), thêm trạng thái `anchor_lost` tường minh, verify chunk/symbol boundary alignment thật trước khi code, thiết kế status-check là on-demand (không periodic — tránh O(n×KNN) ở repo lớn).
- **#3 Ambient notes injection**: **HOLD** — cơ chế cốt lõi (match theo file) cần thiết kế lại trước khi viết code: hoặc thu hẹp phạm vi hiển thị (vd. chỉ note tạo *sau* lần cuối hub file đó bị coi là "đã review", hoặc rank theo recency + cảnh báo rõ "note về file, không chắc về đúng symbol"), bắt buộc fail-open, bắt buộc route qua `scan_text`/`content_warning` trước khi splice vào response.
- **#4 Capability annotations**: **PASS WITH FLAGS** — được làm (annotation + test có giá trị thật, chi phí thấp), nhưng **KHÔNG được dùng làm căn cứ cho bất kỳ tuyên bố "local-only/an toàn tuyệt đối" nào ra ngoài** cho tới khi verify xong 2 mục ASSUMED (LSP/SCIP network surface, socket permission) — tách rõ "chúng tôi khai báo minh bạch" khỏi "chúng tôi đã chứng minh an toàn".
- **#5 Structural confirm gate**: **HOLD** — cơ chế gốc (thuần structural) không đóng được lỗ hổng "gọi rỗng, không đọc". Khuyến nghị thiết kế lại theo hướng **hybrid**: vẫn yêu cầu `edit_context` đã chạy (structural) **CỘNG THÊM** yêu cầu `reason` tham chiếu đúng ít nhất 1 caller/tín hiệu **thật** mà chính `edit_context` đó đã trả về (server tự verify khớp với dữ liệu đã có, không phải regex đoán từ khóa chung chung) — vừa khó game hơn cả 2 phương án ban đầu (CALM's structural-only và DPS-Superskills' free-text), vừa tận dụng đúng lợi thế CALM có mà DPS-Superskills không có: ground-truth data để đối chiếu.

**Kết luận chung**: #4 an toàn để triển khai sớm (effort thấp, giá trị rõ, chỉ cần kỷ luật khi phát ngôn ra ngoài). #1 triển khai được sau khi sửa 3 điểm đã nêu. #3 và #5 — 2 mục tưởng "rẻ nhất" trong phân tích ban đầu — hoá ra cần thiết kế lại phần lõi trước khi viết bất kỳ dòng code nào; đây chính xác là giá trị của việc audit trước khi implement thay vì audit sau.

---

## Red Flags check (áp theo audit-design's Red Flags — Never)

- Đã đọc toàn bộ nội dung phân tích gốc trước khi audit — không audit dựa trên tóm tắt.
- Không có 2 section Risk Assessment trùng nhau trong file này.
- Không có failure mode nào viết mơ hồ kiểu "có thể có vấn đề với X" — mỗi mode đều có cơ chế cụ thể + file:line khi có.
- 2/4 mục (#3, #5) mang finding HIGH chưa có mitigation trong plan → **không** đánh PASS cho 2 mục đó — đánh đúng HOLD.

---

## Verification kết quả — 2 mục Assumptions của #4 (2026-07-12)

### #4a — LSP/SCIP subprocess network surface

**Verdict: claim "local-only" SAI ở 1 điểm cụ thể, có thể sửa — không phải sai lan toả.**

Đọc trực tiếp toàn bộ `crates/calm-core/src/scip/runner.rs` build-command
functions + `crates/calm-core/src/lsp/provider.rs`, cộng grep
`GOPROXY|CARGO_NET_OFFLINE|npm_config_offline|GOFLAGS` toàn repo (0 kết quả):

- **Đường network thật, do chính CALM chọn (không phải kế thừa môi trường)**:
  `js_build_command` (`runner.rs:312-327`) và `python_build_command`
  (`runner.rs:197-219`) — khi `is_npx(bin)` đúng — chạy
  `npx --yes @sourcegraph/scip-typescript` / `npx --yes @sourcegraph/scip-python`.
  `--yes` là cờ tự nhận do CALM chủ động thêm, khiến `npx` **tự động tải
  package từ npm registry** nếu chưa cache sẵn — không hỏi, không cảnh báo.
  Đây là network call thật, xảy ra khi user bật SCIP overlay cho TS/JS hoặc
  Python mà chưa từng cài `@sourcegraph/scip-typescript`/`scip-python` cục bộ.
- **Đường network gián tiếp, kế thừa từ toolchain của user, CALM không set/unset gì**:
  `rust_build_command`/`go_build_command`/`java_build_command`/`clang_build_command`
  (`runner.rs:42-51,116-128,394-403,804-821`) không truyền cờ liên quan
  network nào cả — sạch ở tầng lời gọi của CALM — nhưng rust-analyzer/gopls/
  scip-java khi tự chạy vẫn có thể trigger `cargo`/`go`/Maven fetch nếu
  dependency cache của project chưa đầy đủ. CALM không set `CARGO_NET_OFFLINE`/
  `GOPROXY=off` để chặn — hoàn toàn phụ thuộc môi trường sẵn có của user.
- Java's `scip-java` resolve (`runner.rs:378-385`) chỉ là PATH-probe đơn
  thuần — không tự launch qua coursier trong code path này (khác với suy
  đoán ban đầu trong ghi chú nội bộ trước đó — đã đính chính).

**Kết luận dùng để viết SECURITY.md/README (bản chính xác, không aspirational)**:
"CALM's MCP server process itself makes no network calls; the default
embedding model ships vendored, no network required. When LSP-overlay/
SCIP-indexer features are enabled for TypeScript/JavaScript or Python, the
`npx`-launched indexer may fetch a package from the npm registry if not
already cached — this is the one network-reach path under CALM's direct
control. Other language indexers (Rust/Go/Java/C++) invoke no
network-related flags, but the underlying toolchain (cargo/go/Maven) may
itself reach the network if the project's own dependency cache is
incomplete — this is inherited from the user's environment, not added or
suppressed by CALM."

**Fix rẻ nếu muốn đóng hẳn lỗ hổng #1 (npx auto-install)**: đổi
`js_build_command`/`python_build_command` sang fail-closed — bỏ `--yes`,
để `npx` từ chối khi package chưa cài, trả lỗi rõ ràng ("scip-typescript
not installed — run `npm install -g @sourcegraph/scip-typescript`") thay
vì âm thầm tải về. Effort: đổi 2 dòng, có thể làm cùng đợt với #4's
annotation/test work.

### #4b — Unix socket file permission

**Verdict: sạch, không có caveat.** `set_socket_perms` (`daemon.rs:358-362`)
set quyền `0o600` — chỉ owner đọc/ghi được. Trên máy multi-user, process
của user khác **không** connect được vào socket. Xác nhận đóng hoàn toàn
mục ASSUMED này, không cần sửa gì.

---

## Revised Designs (post-audit) — #3 và #5

### #3 v2 — Ambient notes, thu hẹp phạm vi theo `is_hub`

Đóng cả 3 finding HOLD ban đầu bằng 4 thay đổi cụ thể:

1. **Specificity-gating cho hub file** (đóng Failure Mode 1 — noise ở hub
   file): tái dùng `hub_hit`/`is_hub` đã tính sẵn trong `edit_context`/
   `locate`. Với file **không phải hub** — giữ nguyên match theo `ref_path`
   (file-level, như thiết kế gốc — noise thấp vì file nhỏ/đơn mục đích).
   Với file **là hub** — thêm điều kiện lọc: chỉ giữ note nếu `note.content`
   chứa chuỗi tên symbol đang thao tác (string-contains đơn giản, không
   cần symbol-level ref capture mới trong schema). Field response thêm
   `specificity: "symbol" | "file"` để agent tự biết mức độ liên quan,
   thay vì đoán qua văn phong.
2. **Fail-open bắt buộc** (đóng Failure Mode 2): `notes_for_path` lookup
   theo đúng precedent đã có ở `capture_refs` (`memory.rs:34-38`,
   "best-effort... a failure here shouldn't fail the note itself") — lỗi
   DB/query ở bước này không bao giờ làm `edit_context`/`locate` trả về
   `ToolOutcome::error`; chỉ `related_notes` rỗng + optional 1 dòng note
   nội bộ để debug.
3. **Route qua content-scan trước khi splice** (đóng Failure Mode 3): note
   nào bị flag bởi cùng heuristic `scan_text` đã dùng cho `source`/
   `understand` thì **bị loại khỏi ambient injection** (không hiển thị tự
   động) nhưng **vẫn còn nguyên** khi agent chủ động gọi `recall()` — giữ
   đúng ranh giới "tự động = ngưỡng tin cậy cao hơn, chủ động = agent đã
   được huấn luyện cảnh giác" mà AGENTS.md Stage 3 đã thiết lập cho
   `source`.
4. **Test path-normalization** (đóng L3 risk): 1 regression test assert
   `ref_path` (lưu bởi `capture_refs`/`store_refs`) và `path` mà
   `edit_context`/`locate` dùng để `track_file` là cùng 1 dạng chuẩn hoá —
   tránh lớp lỗi "JOIN luôn 0 dòng, trông như xong nhưng không làm gì".

Field shape cuối: `related_notes: Vec<{ topic, excerpt(≤160 chars),
staleness, specificity }>`, cap **2** note (giảm từ 2-3 ban đầu — ưu tiên
ít nhiễu hơn ít nhiều), sort theo `specificity="symbol"` trước, sau đó
recency.

### #5 v2 — Hybrid structural + content-grounded gate

Đóng đúng finding quan trọng nhất của audit (gọi rỗng, không đọc, vẫn qua
được gate thuần structural):

1. **`edit_context` lưu fingerprint thật vào session_log khi chạy**, không
   chỉ đánh dấu "đã explore": `edit_context_reviewed: HashMap<qualified_name,
   ReviewRecord>` với `ReviewRecord { at: tool_call_index, caller_qns: Vec<String>
   (top 5 theo confidence), risk_level }` — tái dùng dữ liệu `edit_context`
   đã tính sẵn, không thêm query mới.
2. **`EditSymbolParams`/`EditLinesParams` thêm field `reason: Option<String>`**,
   chỉ bắt buộc khi chạm hub/high-risk.
3. **Gate 2 lớp tại `edit.rs:248`**:
   - Không có `ReviewRecord` cho đúng symbol trong session hiện tại →
     `EDIT_CONTEXT_REQUIRED` (khác `CONFIRM_REQUIRED` cũ — lỗi rõ ràng hơn:
     "chưa gọi, không phải chưa confirm").
   - Có review nhưng `reason` không chứa **bất kỳ tên caller thật** nào
     trong `caller_qns` đã lưu → `REASON_NOT_GROUNDED`, kèm gợi ý 3 tên
     caller đầu tiên để agent biết cần nhắc tới cái gì — không phải đoán
     từ khoá chung chung như thiết kế DPS-Superskills gốc.
   - Symbol hub nhưng 0 caller thật (hub vì lý do cấu trúc khác) → fallback
     duy nhất còn lại: chỉ yêu cầu `reason` không rỗng — chấp nhận đây là
     điểm yếu còn sót (không có fact thật để đối chiếu), nhưng phạm vi hẹp,
     không phải lỗ hổng chung.
4. **Freshness window**: nếu `ReviewRecord.at` cách lần edit hiện tại quá xa
   (vd. > 200 tool-call cùng session, dùng lại đúng counter `session_log.tool_calls`
   đã có), coi như stale, bắt gọi lại `edit_context` — tránh review 1 lần ở
   đầu session dài rồi dùng mãi trong khi callers thật đã đổi.
5. **Phạm vi multi-agent — quyết định rõ, không để ngỏ**: `edit_context_reviewed`
   sống **per-connection**, giống hệt `explored_symbols` hiện tại
   (`CalmServer::for_connection()`, `common.rs:68-73` đã cố ý tách
   `session_log` riêng mỗi connection). Agent B không "thừa hưởng" việc
   agent A đã gọi `edit_context` để tự động qua gate — đây là lựa chọn chủ
   động, khớp đúng tinh thần thiết kế gốc của trường này, không phải lỗ hổng
   bỏ sót.
6. **Error UX tái dùng `suggested_next`** đã có sẵn khắp nơi trong codebase
   (`suggested("edit_context", ..., args:{symbol})`) thay vì format lỗi mới.

Thiết kế này đóng được gap "chỉ chứng minh đã gọi, không chứng minh đã đọc"
vì `reason` phải chứa dữ kiện **chỉ xuất hiện trong đúng response
`edit_context` của session này** — một agent thực sự bỏ qua response thì
không có cách nào biết cần gõ gì vào đó, khác hẳn cách agent có thể đoán
đúng từ khoá chung ("schema", "rollback"...) trong thiết kế gốc của
DPS-Superskills.

---

## Implementation Status (2026-07-12)

Cả 4 mục đã implement + test theo đúng thiết kế đã sửa sau audit (không phải bản gốc trước audit). Build sạch, 164 test calm-server + 656 test calm-core đều pass.

- **#4 — DONE.** `ToolAnnotations` trên toàn bộ 26 tool (`crates/calm-server/src/tools/*.rs`), 2 test trạng thái trại (`no_tool_combines_open_world_and_destructive_capability`, `every_tool_declares_annotations` — `crates/calm-server/src/tools.rs`). `npx --yes` → `--no-install` (fail-closed) ở `crates/calm-core/src/scip/runner.rs::js_build_command`/`python_build_command`, đóng đúng lỗ hổng network đã xác minh ở #4a.
- **#1 — DONE.** Bảng `pattern_debt` riêng (`crates/calm-core/src/db/schema.rs`), tool `pattern_debt_register`/`pattern_debt_status` (`crates/calm-server/src/tools/patterndebt.rs`), anchor theo `anchor_qualified_name` (không phải path+line), trạng thái `anchor_lost` tường minh, ngưỡng similarity 0.75 (`PATTERN_DEBT_SIMILARITY_THRESHOLD`) đóng gap "current_count không bao giờ về 0" phát hiện qua test. 3 test round-trip/anchor-lost/embeddings-not-ready.
- **#3 v2 — DONE.** `notes_for_path` (`crates/calm-core/src/memory.rs`) + `CalmServer::related_notes` (`crates/calm-server/src/tools/common.rs`) đã wire vào `edit_context`/`locate`, đúng specificity-gating (hub file cần note nhắc tên symbol) + fail-open + lọc qua `injection_warning`. 4 test (non-hub file-level, hub symbol-gating, injection-filtering, locate wiring).
- **#5 v2 — DONE.** `EditContextReview`/`edit_context_reviewed` trong `SessionLog` (per-connection, `crates/calm-server/src/tools.rs`), gate 3 lớp trong `edit_lines_impl` (`crates/calm-server/src/tools/edit.rs`): `EDIT_CONTEXT_REQUIRED` → `CONFIRM_REQUIRED` → `REASON_NOT_GROUNDED`, freshness window 200 tool-call. `reason` field thêm vào `EditLinesParams`/`EditSymbolParams`. 4 test mới (3-layer gate rewrite, grounded-vs-generic reason, per-connection isolation) cộng `edit_lines_requires_confirm_for_hub_symbol` viết lại hoàn toàn.

**Chưa làm** (ngoài scope phiên này): cập nhật `types/mcp_types.ts` (TS type mirror, không có test ràng buộc với Rust struct nên không chặn build/test); cập nhật README/AGENTS.md mô tả 4 tool mới cho user-facing docs.