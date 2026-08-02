---
title: "3 nghiên cứu sâu — tool surface 30→34, hoàn thiện Write-Safety Beta, thiết kế wiring audit_ledger"
date: 2026-08-02
status: nghiên cứu + thiết kế, con của reconciliation-round2 (N2/N3/N4)
scope: trả lời 3 câu hỏi cụ thể từ reconciliation-round2 — không code, chỉ điều tra + thiết kế +
  khuyến nghị hành động
inputs:
  - docs/plans/2026-08-02-reconciliation-round2.md   # N2 (tool surface), N3 (Write-Safety Beta gate), N4 (ledger mồ côi)
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md # WS-9, WS-2 §5.5
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md # P0-4 gốc
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md # §6 milestone gate
verified_against: HEAD ab0e374, đọc trực tiếp qua mcp__calm__source/search trong phiên này
---

> **[SUPERSEDED — trạng thái hiện tại]** Xem [2026-08-02-phase2-priority-and-ws2-execution-plan.md](2026-08-02-phase2-priority-and-ws2-execution-plan.md) — Phần 1 (N2 fix) dưới đây ĐÃ IMPLEMENTED đúng như khuyến nghị §1.5 (toolset "txn" tách khỏi floor, thêm vào preset edit); Phần 2's "1/6 tiêu chí" nay là 5/6. Nội dung dưới đây giữ nguyên làm bằng chứng thiết kế chi tiết.

# 3 nghiên cứu sâu (2026-08-02)

---

## Phần 1 — Tool surface 30→34: hợp lý hay sai định hướng?

### 1.1 Cấu trúc thật (đọc `toolset.rs` toàn văn, không suy đoán từ tên tool)

CALM **đã có** hạ tầng toolset tinh vi hơn hẳn con số "30 tool phẳng" mà audit A16 mô tả:

- **13 toolset** (`TOOLSET_NAMES`, `toolset.rs:87-101`), mỗi tên ánh xạ 1-1 tới một module
  `#[tool_router]` thật (`toolset_tools()` gọi thẳng router đó — không thể lệch khỏi thực tế).
- **4 preset curated** (`orient`=6 tool, `trace`=10, `edit`=12, `compound`=12) — allowlist tường
  minh, **không** tự động thêm tool mới trừ khi ai đó sửa danh sách.
- **`"full" | ""` → `None`** (không filter gì) — và đây **chính là default** khi chạy `calm serve`
  trần: `lib.rs:75` `serve_stdio()` gọi `serve_stdio_with_preset(..., "full".into())`.
- **`SAFETY_FLOOR_TOOLSETS = ["orient", "edit", "guardrails", "recover"]`** (`toolset.rs:121`) —
  4 toolset này **không thể bị tắt** kể cả khi một session dùng `set_toolset` để tự thu hẹp runtime
  (dynamic toolset feature, 07-28). Đây là tầng nghiêm ngặt nhất trong toàn hệ thống.

### 1.2 4 tool mới nằm ở đâu, và hệ quả

4 tool WS-1 mới (`edit_transaction_status`, `maintenance_status`, `retry_maintenance`,
`repair_consistency`) được đăng ký vào `recover_tool_router` (`recover.rs`) — **một trong 4
toolset floor**. Đọc `file_overview` của `recover.rs` xác nhận: toolset này **trước đó chỉ có 2
tool** — `indexing_status` (hub, caller_count 7) và `session_context` (hub, caller_count **18**) —
cả hai đều đúng nghĩa "stuck-session escape hatch" (comment tại `toolset.rs:109-111` tự giải
thích lý do `recover` phải nằm trong floor). Thêm 4 tool mới = **tăng gấp 3 lần** kích thước của
đúng bucket nghiêm ngặt nhất, không phải một bucket tuỳ chọn.

Điều này có 2 hệ quả cụ thể:
1. Dưới preset `full` (default thật của `calm serve`) — đúng phiên đang chạy tài liệu này — cả 4
   tool luôn hiện diện, không cách nào ẩn qua `set_toolset` vì `recover` ở floor.
2. Dưới 4 preset curated (`orient`/`trace`/`edit`/`compound`) — 4 tool này **không** xuất hiện,
   nhưng đó là **ngẫu nhiên** (do `preset_tools()` không được cập nhật khi thêm tool), không phải
   một quyết định thiết kế có chủ đích được ghi lại ở đâu.

### 1.3 Đối chiếu với intent WS-9 (master plan)

WS-9 (`master-upgrade-plan.md`) nêu rõ: *"public default = ít tool cấp workflow; primitive →
`expert` toolset"*, và xem tool-surface lớn (~30) là chính vấn đề A16 chỉ ra. Quyết định gộp 4 tool
mới vào `recover` (floor, luôn hiện diện) đi **ngược chiều** ý định đó về mặt kết quả — dù người
viết code có lý do thực dụng chính đáng (chính commit/plan tự ghi: *"đã có sẵn trong preset `full`,
không cần đăng ký toolset mới"* — tránh việc phải đăng ký thêm 1 tên trong `TOOLSET_NAMES` +
`calm_core::config::VALID_TOOLSET_NAMES` + quyết định floor-membership).

**Đây có phải "thực thi sai định hướng" không?** Có, nhưng ở mức nhẹ và dễ sửa:
- Không phải lỗi logic hay bug — 4 tool này hoạt động đúng, test đầy đủ, toolsnap đúng.
- Là một **đánh đổi thực dụng** (tái dùng hạ tầng có sẵn, tránh thêm 1 tên toolset) đã vô tình đặt
  sai tầng: 4 tool chẩn đoán cho một hệ thống **đang ở shadow mode** (không ảnh hưởng hành vi thật
  — xem Phần 2) lại được gắn nhãn "escape hatch bắt buộc, không thể tắt" — mức độ cần thiết thật
  sự của chúng hôm nay gần bằng 0 (không gì "kẹt" nếu bỏ qua chúng, vì txn/maintenance chưa enforce
  gì cả).

### 1.4 Cơ hội sửa: chi phí bằng 0 ngay lúc này

Xác nhận qua `git tag --sort=-creatordate` + `git log`: **v0.4.0 được tag TRƯỚC** toàn bộ đợt
WS-1/2/3/13 (`f676ff3`/`7e9d972` đứng trước `636f0bc`...`ab0e374` trong lịch sử). **4 tool mới
chưa từng xuất hiện trong bất kỳ bản release nào** — không MCP client thật nào ngoài phiên
dev hiện tại từng thấy chúng. Đổi vị trí toolset của chúng **bây giờ** không phá compatibility với
ai — chi phí thấp nhất có thể có. Sau khi có một tag `v0.4.1`/`v0.5.0` bao gồm chúng, việc này sẽ
tốn một "deprecation window" thật (đúng nguyên tắc governance §6 master plan).

### 1.5 Khuyến nghị thiết kế

1. **Tách 4 tool này khỏi `recover_tool_router` sang một module/toolset mới**, ví dụ
   `crates/calm-server/src/tools/txn.rs` với `#[tool_router]` riêng, tên toolset `"txn"` — đúng
   tiền lệ đã có (`toolset.rs:1-9` tự ghi nhận nó được "extracted verbatim from tools/common.rs,
   2026-07-28 hotspot split" — tách module vì lý do tổ chức toolset không phải việc mới trong repo
   này). Đăng ký `"txn"` vào `TOOLSET_NAMES` + `calm_core::config::VALID_TOOLSET_NAMES` (2 hằng số
   phải khớp — có test `toolset_names_match_calm_core_valid_toolset_names` tự bắt lệch).
2. **KHÔNG đưa `"txn"` vào `SAFETY_FLOOR_TOOLSETS`** — đây chính là điểm sửa cốt lõi. Một session
   dùng `set_toolset` để thu hẹp có thể hợp lệ không cần 4 tool này.
3. **Cân nhắc thêm `"txn"` vào preset `edit`** (12 tool hiện tại, đã có `edit_context`/`edit_lines`/
   `diff_impact`) — một session đang sửa code có lý do chính đáng muốn kiểm tra trạng thái
   transaction của chính edit vừa làm. **Không** thêm vào `orient`/`trace` (thuần đọc, không ghi).
4. **Gắn điều kiện promote lên floor với chính milestone WS-1 enforce transition** (Phần 2 dưới):
   một khi shadow → enforce thật, `repair_consistency`/`edit_transaction_status` mới thật sự đóng
   vai "khi kẹt, dùng cái này" — lúc đó promote `"txn"` vào floor mới hợp lý, không phải bây giờ.
5. Cập nhật `docs/status.generated.md` (tự sinh lại, không hand-edit) + `preset_tools()` không đổi
   (4 preset curated vẫn không có `"txn"`, đúng ý ban đầu).

**Việc KHÔNG nên làm:** không xoá 4 tool hay gộp chúng làm 1 (chúng phục vụ 4 câu hỏi khác nhau —
trạng thái tx, trạng thái outbox, ép chạy lại, đối chiếu digest — gộp sẽ mất độ chi tiết chẩn
đoán); không chờ WS-9 có plan riêng đầy đủ mới sửa (fix này tự chứa, rẻ, không cần chờ redesign
toàn bộ 34 tool).

---

## Phần 2 — Hoàn thiện triệt để "Write-Safety Beta" (6 tiêu chí, 1/6 đạt)

### 2.1 Nhắc lại 5 tiêu chí còn mở (từ `phase1-p0-execution-plan.md` §6)

| # | Tiêu chí | Việc cần làm |
|---|---|---|
| 2 | `replay_state` khớp cache 100% test suite | Test bao phủ **toàn bộ** fixture hiện có, không chỉ test mới |
| 3 | 0 write path bỏ qua `EditTransaction` | Quyết định + implement **enforce transition** |
| 4 | `critical` risk bị block nếu thiếu approver | Cần định nghĩa "critical" — **chưa tồn tại trong code** |
| 5 | `atomic_write` Fast mode 0 đổi hành vi | Có khả năng đã đạt — cần tick tường minh |
| 6 | p95 `edit_lines` không tăng >10% | **Chưa đo** — và công cụ đề xuất trong plan gốc SAI (xem 2.2) |

### 2.2 Tiêu chí 6 — sửa lại cách đo: `b6_tool_call_efficiency` KHÔNG đo latency

Đọc trực tiếp `benchmarks/b6_tool_call_efficiency/run_benchmark.py::main` — benchmark này đo
**số lượng tool call** (naive N calls vs CI 1 call → `reduction_pct`), hoàn toàn không có
`time.time()`/wall-clock nào trong vòng lặp. Gợi ý "dùng b6" trong `phase1-p0-execution-plan.md`
§6 là **sai công cụ** — b6 không thể trả lời câu hỏi p95 latency dù chạy bao nhiêu lần.

**Thiết kế đo đúng:** không có baseline "tắt shadow mode" sống trong code hiện tại (shadow luôn
bật, không có feature flag) — nghĩa là A/B trong cùng 1 binary là không thể mà không thêm code
throwaway. Cách đo sạch hơn, không đụng production code:
1. Build 2 binary: baseline = checkout `acf2793` (commit ngay trước `7b65acb` — commit đầu tiên
   thêm txn.rs), candidate = HEAD hiện tại.
2. Harness: dùng đúng pattern test hiện có (nhiều test trong `tools.rs` đã dựng `CalmServer` +
   gọi `edit_lines_flow` trực tiếp in-process) — viết 1 bench nhỏ gọi `edit_lines_flow` N=200-500
   lần trên cùng 1 fixture file/repo, đo `std::time::Instant` quanh mỗi call, tính p50/p95/p99.
3. Chạy harness đó trên cả 2 binary (cùng máy, cùng fixture, cùng N), so p95 candidate vs baseline.
4. Lưu ngưỡng vào `thresholds.toml` (đã có sẵn ở root repo, dùng cho gate khác như hotspot_risk —
   tái dùng đúng cơ chế đã có, không phát minh cơ chế gate mới) — ví dụ
   `p95_edit_lines_overhead_pct <= 10.0`.
5. **Dự đoán có cơ sở nhưng KHÔNG thay cho đo thật:** shadow mode thêm ~2-4 lệnh SQLite đồng bộ
   (INSERT `edit_transactions`, INSERT `tx_events` ×2-3, UPDATE state) trên cùng connection đã mở
   sẵn cho base reindex — về bản chất nhỏ so với chi phí parse+reindex một file thật, nhiều khả
   năng dưới 10%. Nhưng đây là giả định, đúng kỷ luật "phải đo, không giả định" mà chính các plan
   trước tự áp cho mình — không tick tiêu chí 6 cho tới khi có số thật.

### 2.3 Tiêu chí 2 — `replay_state` khớp 100% test suite

Cơ chế đã có sẵn (`txn::replay_state`), chỉ thiếu 1 test bao phủ rộng: thêm
`tx_events_replay_matches_cached_state_after_every_fixture` (tên đã đề xuất sẵn trong
`phase1-p0-execution-plan.md` §4.7 nhưng chưa xác nhận đã viết) — chạy toàn bộ fixture
`edit_lines`/`edit_symbol`/`format_files` hiện có trong 1 lượt, thu thập mọi `tx_id` được tạo qua
shadow mode, rồi assert `replay_state(tx_id) == edit_transactions.state` cho từng cái ở cuối. Việc
thuần túy cơ học, rủi ro thấp — nên làm trước enforce transition (nó chính là bài test hồi quy bảo
vệ cho bước enforce).

### 2.4 Tiêu chí 3 — thiết kế enforce transition (thu hẹp phạm vi so với master plan gốc)

Master plan hình dung ban đầu (WS-1 §3): toàn bộ write path do state machine "lái" (block/rollback
theo từng state). Theo đúng kỷ luật đã dùng cho WS-2/WS-4 ("việc đổi hành vi write path thật cần
plan/threat-model riêng, không code cơ hội chủ nghĩa"), phạm vi enforce khả thi nhất cho lần đầu
tiên nên **thu hẹp** thay vì làm toàn bộ state machine:

- **Chỉ enforce ở bước `txn::begin`.** Nếu `txn::begin` lỗi (constraint vi phạm, DB busy quá
  timeout, disk full…) → **từ chối cả thao tác ghi**, trả mã lỗi mới (vd. `TRANSACTION_INIT_FAILED`)
  thay vì tiếp tục ghi file như hiện tại. Lý do chọn đúng điểm này: một lỗi ở `begin` thường phản
  ánh vấn đề hạ tầng thật (DB hỏng/đầy đĩa) mà bản thân `atomic_write` sau đó cũng có nguy cơ thất
  bại tương tự — fail-closed ở đây gần như không có false-positive cost.
- **KHÔNG cố rollback file đã ghi thành công nếu `advance(FileCommitted)` sau đó lỗi.** Tại điểm
  này disk đã đổi thật — "rollback" nghĩa là xoá/khôi phục file, một thao tác nguy hiểm hơn chính
  vấn đề đang cố sửa. Thay vào đó: giữ hành vi hiện tại (warn, không block) **nhưng thêm
  `needs_repair: true` + gợi ý gọi `repair_consistency`** vào response — biến một lỗi âm thầm
  thành một tín hiệu tường minh cho agent/người dùng, không giả vờ đã rollback được.
- Rollout theo đúng 3 bước đã đặt tên (`phase1-p0-execution-plan.md` §4.6 task 4.8): **shadow
  (xong)** → **dual-read** (trả `tx_id` thật trong response `edit_lines`/`edit_symbol`/
  `format_files` — hiện tại theo `edit.rs` đã đọc, `tx_id` được tạo nhưng cần xác nhận đã trả ra
  response hay chỉ nằm nội bộ; nếu chưa, đây là việc cần làm trước enforce) → **enforce (hẹp,
  chỉ `begin`)**.
- Việc này cần văn bản plan riêng (đúng nguyên tắc đã áp cho mọi thay đổi hành vi write path thật)
  trước khi code — không nằm trong phạm vi tài liệu nghiên cứu này.

### 2.5 Tiêu chí 4 — "critical" risk: không có trong code, đừng phát minh khái niệm rỗng

Đọc trực tiếp `risk_level_from_caller_count` (`detail.rs:525-533`): chỉ trả `"high"`/`"medium"`/
`"low"` (ngưỡng >10 caller / >3 / còn lại). `classify_gate` (`edit.rs:2071-2109`) chỉ rẽ nhánh trên
`risk == Some("high")` — **không có "critical" ở đâu trong runtime thật**. Bảng 4 tier
(`low/medium/high/critical`) ở master plan WS-2 §5.5 là **thiết kế trên giấy**, chưa có anchor nào
trong code hiện tại.

**Khuyến nghị: đừng tạo enum variant "critical" rỗng chỉ để tick checkbox.** Thay vào đó, định
nghĩa lại tiêu chí 4 một cách trung thực bằng tín hiệu ĐÃ tồn tại:

> Policy: khi `risk == "high"` **và** `elicit_hub_confirm` đã cấu hình (client hỗ trợ elicitation)
> → **bắt buộc** vòng elicitation (Ask → người duyệt → Approved), không có đường nào khác qua được
> (không phải "reason không rỗng", không phải "confirm:true" đơn thuần). Khi `risk == "high"`
> **và** elicitation **chưa** cấu hình → **block cứng, không có lối thoát** (thành thật là "chưa
> có cách approve độc lập ở cấu hình này", đúng tinh thần T4/Phase-3 đã ghi trong ws2-plan, không
> giả vờ có approval-tier chưa từng tồn tại).

Cách này tái dùng 100% hạ tầng đã ship (elicitation veto, F2), thoả tiêu chí 4 một cách trung
thực cho đúng phạm vi CALM thật sự có (single-operator, không có "independent approver" khái
niệm), và không cần chờ WS-2 Phase 3 (approval-tier đa vai trò) — vốn cố ý defer vì chưa có
scenario cụ thể.

### 2.6 Tiêu chí 5 — tick tường minh

Test `atomic_write_high_assurance_surfaces_permission_failure` (nêu trong `phase1-p0-execution-plan.md`
§3.4) cùng full suite 943+296 pass đã là bằng chứng đủ cho tiêu chí này — chỉ cần đánh dấu `[x]`
tường minh trong checklist, không cần việc mới.

### 2.7 Thứ tự thực thi đề xuất để đóng "Write-Safety Beta"

1. Tiêu chí 2 (test replay 100%) — cơ học, rủi ro thấp, làm trước vì nó là lưới an toàn cho bước 3.
2. Tiêu chí 6 (đo p95 thật, sửa công cụ đo theo §2.2) — đo lường thuần, không đổi code sản phẩm.
3. Tiêu chí 5 — chỉ cần tick, không việc mới.
4. Viết plan riêng cho tiêu chí 3 (enforce `begin`, phạm vi hẹp theo §2.4) → code → test.
5. Tiêu chí 4 (policy elicitation-bắt-buộc cho `risk=="high"`, theo §2.5) — có thể làm song song
   với 4 vì không phụ thuộc lẫn nhau, cùng chạm `edit_lines_impl_gated` nên gộp 1 PR với bước 4 để
   tránh 2 lần sửa cùng hàm.

Sau khi cả 5 bước trên xong → 6/6 tiêu chí đạt → milestone "Write-Safety Beta" thật sự đóng, mở
khoá bắt đầu WS-4 theo đúng roadmap.

---

## Phần 3 — Thiết kế wiring `audit_ledger`/`ledger.rs`

### 3.1 Intent gốc — đã ghi rõ, không cần đoán

Đọc `ledger.rs:1-19` (doc comment đầu module) + `adopt-from-vheatm-plan.md` §P0-4 (dòng 291-298):
ý định đã được viết tường minh **ngay trong code**, không phải suy luận lại:

> *"a durable, tamper-evident channel that runs alongside `AUDIT_TARGET` tracing... that stream
> stays exactly what it is, a SIEM-facing log line; this module is the separate durable store...
> Nothing in this crate calls it yet — wiring a real write path... is a separate, deliberately
> later change."*

Tức là **"module mồ côi" là trạng thái CỐ Ý**, đúng pattern shadow/additive đã dùng 3 lần trước
(digest→txn→maintenance) — không phải ai quên nối dây. Việc còn thiếu là **chọn call site nào**
và **thiết kế payload** — phần plan gốc chưa cụ thể hoá.

### 3.2 Khảo sát toàn bộ call site `AUDIT_TARGET` hiện có (ứng viên wiring)

Grep `AUDIT_TARGET` toàn repo tìm thấy các nhóm:

| Nhóm | File:line | Ý nghĩa | Giá trị ledger hoá |
|---|---|---|---|
| Gate decision (allow/deny) | `edit.rs:987,1027,1057,1138,1287` | Quyết định cho phép/từ chối 1 write attempt, kèm `reason_code` | **Cao** — đúng thứ VHEATM's `tool-receipt.schema.json.decision` ghi lại |
| Elicitation round-trip | `edit.rs:1676,1688` (`hub_elicit_roundtrip`) | Người dùng approve/decline 1 yêu cầu high-risk | **Cao** — bằng chứng "con người đã duyệt", đúng P0-5/A03 |
| Tx lifecycle transition | (không dùng `AUDIT_TARGET` — nằm trong `txn::advance`, ghi `tx_events`) | Mọi state PREPARED→...→DONE/FAILED, đã sequence+actor+reason | **Cao nhất** — đã là single choke-point cho MỌI write path |
| Tool-call cấp cao | `tools.rs:708,728` (`call_tool`) | Log mỗi lời gọi tool | Trung bình — tần suất cao, ít giá trị "provenance của 1 hành động ghi" |
| HTTP/auth | `http.rs:44,95` | Bearer token fail, server start | Thấp cho mục tiêu "write provenance" — thuộc phạm vi WS-10 hơn |
| `set_toolset`/startup | `orient.rs:481`, `common.rs:66,77` | Đổi toolset runtime, phục hồi khởi động | Thấp — không phải hành động ghi nội dung |

### 3.3 Khuyến nghị: wire tại `txn::advance` trước tiên (1 call site, giá trị cao nhất)

Thay vì rải `ledger::append` vào ~10 call site rác trong `tools/edit.rs`, điểm nối tối ưu là
**bên trong chính `txn::advance`** (`calm-core/src/txn.rs`) — vì đây **đã là** điểm duy nhất mọi
transition của mọi write path (edit_lines, edit_symbol, format_files — bất cứ gì dùng txn.rs) đi
qua, theo đúng thiết kế "chỉ hàm này được phép đổi state" đã có sẵn. Lý do chọn đây thay vì các
điểm khác:

1. **1 chỗ sửa, phủ mọi call site tương lai** — bất kỳ write path mới nào dùng `txn::advance` sau
   này tự động được ledger hoá, không cần nhớ thêm dòng `ledger::append` mỗi nơi mới.
2. **Bao phủ cả `Failed`** — transition sang `Failed` mang `error_detail` (lý do thất bại) — đây
   chính xác là loại event "chứng minh việc gì đã xảy ra, kể cả khi xảy ra xấu" mà audit ledger
   hướng tới, mạnh hơn nhiều so với chỉ ghi lại các quyết định gate ở tầng `tools/edit.rs`.
3. **Test rẻ nhất** — chỉ cần mở rộng test hiện có của `txn::advance` trong `txn.rs`, assert sau N
   lần `advance()` thì `audit_ledger` có N dòng chained tương ứng và `verify_chain()` pass — không
   cần dựng lại toàn bộ `edit_lines_impl_gated` fixture.
4. **Đúng khuôn "shadow/additive"**: bọc lời gọi `ledger::append` trong `let _ =` ngay sau khi
   `tx_events` insert thành công bên trong `advance()` — một lỗi ledger không bao giờ làm hỏng hay
   chặn transition thật, đúng chính lời module doc tự đặt ra.

**Thiết kế payload** — mirror chính hàng `tx_events` vừa ghi (không phát minh format mới):
```rust
// Trong txn::advance, ngay sau INSERT tx_events thành công:
let payload = format!(
    "tx_id={tx_id}|seq={sequence}|from={from_state}|to={to_state}|actor={actor}|reason={reason}"
);
let _ = crate::ledger::append(conn, actor, &payload);
```
`actor` truyền thẳng qua — lưu ý hiện tại **mọi** call site `txn::advance` trong `tools/edit.rs`
hardcode `actor = "system"` (xác nhận qua grep trước đó) — ledger sẽ phản ánh trung thực giới hạn
này (chưa phân biệt agent/human), không nên giả lập một actor giả chỉ để log "đẹp" hơn.

### 3.4 Phase 2 (sau, không bundle chung) — gate deny events

Sau khi Phase 1 (txn::advance) chạy ổn định, wire tiếp các điểm `AUDIT_TARGET` "gate decision" ở
`edit.rs:987,1027,1057,1138,1287` — các event này xảy ra **trước khi** một tx được `begin()` (một
write bị từ chối chưa từng tạo transaction), nên Phase 1 không phủ được chúng. Giá trị: chứng minh
"tại sao một write KHÔNG được thực hiện", bổ khuyết cho Phase 1 chỉ chứng minh "việc gì xảy ra với
write ĐÃ được thực hiện".

### 3.5 Phase 3 (tuỳ chọn, giá trị thấp hơn) — elicitation + server-level events

`hub_elicit_roundtrip` (2 site) và nhóm HTTP/toolset/startup — có thể làm sau, không khẩn, giá trị
audit thấp hơn 2 phase trên cho đúng mục tiêu "trusted write authority" mà P0-4 nhắm tới.

### 3.6 Việc chưa cần làm ngay (đừng over-engineer)

- **Không** cần tool MCP mới kiểu `audit_ledger_status`/`verify_ledger` ngay — `verify_chain()` có
  thể gọi tạm qua test/CLI debug; chỉ cần 1 tool thật khi có nhu cầu vận hành cụ thể (vd. gộp vào
  `maintenance_status` hoặc một job định kỳ trong `reconcile_stale_at_startup`).
- **Không** cần actor identity thật (agent id/human id) — đúng theo đúng giới hạn WS-2 Phase 3 đã
  tự nhận (single-operator, chưa có khái niệm actor độc lập).
- **Không** ký (Ed25519/HMAC) — đúng quyết định đã giữ ở `priority-reaudit.md` §6.4 (signing là
  lớp hardening sau, không phải bây giờ).

---

## Kết luận chung

Cả 3 vấn đề đều là **gap thực thi có thể đóng ngay, rủi ro thấp, không cần chờ workstream lớn**:
tool surface sửa bằng 1 lần tách module (Phần 1, chi phí = 0 vì chưa release); Write-Safety Beta
đóng bằng 5 bước cụ thể trong đó việc nặng nhất (enforce transition) đã có phạm vi thu hẹp rõ ràng
(Phần 2); ledger hết mồ côi bằng đúng 1 call site trong `txn::advance` (Phần 3). Không có phát
hiện nào trong 3 nghiên cứu này cho thấy hướng đi ban đầu (WS-9, master plan WS-1/WS-2, P0-4) sai
— chỉ có độ lệch nhỏ giữa ý định và thực thi, đều sửa được mà không cần viết lại thiết kế gốc.
