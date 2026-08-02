---
title: "Phase 2 — ưu tiên phần việc còn tồn đọng + kế hoạch thực thi chi tiết (WS-2 Phase 2, WS-4 research-kickoff) + hợp nhất trạng thái"
date: 2026-08-02
status: "UPDATE 2026-08-02 (same day): WS-2 Phase 2 SHIPPED (§5, code+tests+clippy+fmt all green); WS-1 criterion 6 (p95) decision CLOSED by milestone owner — accepted the ~15%/23% floor (§4). Original: priority ranking + 1 execution-ready plan (WS-2 Phase 2) + 1 research-kickoff scoping (WS-4, KHÔNG phải execution plan) + consolidated state, thay thế vai trò 'nguồn trạng thái hiện tại' của 7 doc con bên dưới"
supersedes_for_current_state:
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md
  - docs/plans/2026-08-02-priority-reaudit.md
  - docs/plans/2026-08-02-reconciliation-round2.md
  - docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md
  - docs/plans/2026-08-02-ws1-enforce-and-critical-risk-execution-plan.md
  - docs/plans/2026-08-02-ws2-review-token-execution-plan.md
  - docs/plans/2026-08-02-shadow-txn-connection-consolidation-plan.md
note_on_superseding: >
  7 doc trên KHÔNG bị xoá hay di chuyển — chúng là nhật ký xác minh (file:line thật, số đo thật)
  có giá trị lịch sử, đúng kỷ luật "verify trước khi code" mà cả 7 doc tự áp cho mình. Tài liệu
  này chỉ thay thế vai trò "hỏi trạng thái hiện tại ở đâu" — mỗi doc trên đã được gắn banner trỏ
  về đây. Khi cần chi tiết một quyết định cũ (vd. vì sao Tier 1 connection-consolidation không đủ),
  vẫn đọc doc gốc.
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md
  - docs/plans/2026-08-02-priority-reaudit.md (Tier A/B/C/D framework, kế thừa nguyên tắc xếp hạng)
  - docs/plans/2026-08-02-reconciliation-round2.md (N1-N5, 2 trong 5 đã đóng — xem §2)
  - docs/plans/2026-08-02-ws2-review-token-execution-plan.md §4/§5 (Phase 2/3 sketch gốc)
  - docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md Phần 1 (N2 fix design, đã ship)
verified_against: "HEAD 1830328 (4 commit chưa push so với origin/main), đọc trực tiếp qua
  mcp__calm__search/source/repo_overview/fitness_report trong phiên này — không copy số liệu cũ"
---

# Phase 2 — Ưu tiên + kế hoạch thực thi (2026-08-02, vòng tiếp theo)

> Tài liệu này trả lời 3 việc: (1) trong các phần còn tồn đọng sau đợt WS-1/2/3/13, phần nào nên
> làm trước và vì sao; (2) kế hoạch chi tiết, đọc-code-thật để thực thi phần ưu tiên cao nhất
> (WS-2 Phase 2) — đủ chi tiết để bắt tay code ngay; (3) với phần ưu tiên cao về mức nghiêm trọng
> nhưng effort quá lớn để lập kế hoạch thực thi ngay (WS-4), tài liệu chỉ scope câu hỏi nghiên cứu,
> đúng kỷ luật "không bắt đầu code cơ hội chủ nghĩa" mà chính đợt trước đã tự đặt ra cho WS-4.

---

## 0. Vì sao viết lại thay vì chỉ nối thêm vào 1 trong 7 doc cũ

7 doc con ngày 2026-08-02 đã tự tích luỹ 2 vòng correction log chồng lên nhau (`priority-reaudit.md`
sửa master plan, `reconciliation-round2.md` sửa `priority-reaudit.md`) và **chính chúng cũng đã bắt
đầu lỗi thời** kể từ commit `1830328` (ship sau khi cả 7 doc được viết) — xem §2 correction log dưới.
Thay vì viết correction log vòng 3 chồng lên vòng 2, tài liệu này gộp: trạng thái mới nhất + ưu tiên
+ kế hoạch tiếp theo, một chỗ.

---

## 1. Trạng thái WS-1..14 — xác minh lại lần thứ 3, tại HEAD hiện tại

| WS | Trạng thái | Việc còn lại |
|---|---|---|
| WS-1 | **5/6 tiêu chí Write-Safety Beta đạt.** Chỉ còn tiêu chí 6 (p95) — xem §4 | Quyết định milestone-owner (§4), không phải code |
| WS-2 | Phase 1 xong. Phase 2 (TOCTOU `caller_set_digest`) **chưa code, đã design** | **→ Kế hoạch chi tiết §3 dưới** |
| WS-3 | Xong | `openat2 RESOLVE_BENEATH` (Linux TOCTOU) vẫn cố tình để riêng |
| WS-4 | Chưa động — gap nghiêm trọng nhất còn mở | **→ Research-kickoff scoping §5 dưới (không phải execution plan)** |
| WS-5,6,7,8,11,12,14 | Chưa làm, P1/P2 đúng lịch, không đổi thứ tự | — |
| WS-9 | Chưa làm ở mức thiết kế lớn — nhưng debt cụ thể N2 (tool-surface) đã **đóng** (xem §2) | Còn lại là redesign 34→8 tool mức workflow, P1 đúng lịch |
| WS-10 | Coi như xong (quyết định có chủ đích) | — |
| WS-13 | Xong | v2 (ký số) là hardening sau, không khẩn |

---

## 2. Correction log — 3 điểm 7 doc con đã lỗi thời kể từ khi viết

Đọc trực tiếp code tại HEAD hiện tại (`1830328`), không copy lại:

| # | Claim cũ | Nguồn | Thực tế hiện tại | File:line vừa đọc |
|---|---|---|---|---|
| P1 | "WS-1 shadow mode XONG, task 4.8 (shadow→enforce) CHƯA làm", "Milestone 1/6 tiêu chí" | `priority-reaudit.md` §2, `reconciliation-round2.md` N3 | **Enforce đã ship.** `txn::begin` fail giờ abort write thật (không còn "log rồi tiếp tục") | `TRANSACTION_INIT_FAILED` sống ở [edit.rs:1357](../../crates/calm-server/src/tools/edit.rs#L1357); `high_risk_needs_independent_review` chặn cứng high-risk khi không có elicitation ở [edit.rs:1206-1260](../../crates/calm-server/src/tools/edit.rs#L1206) |
| P2 | "`ledger.rs` là module mồ côi — 0 nơi gọi `ledger::append`" | `reconciliation-round2.md` N4 | **Đã wire.** `txn::advance` giờ gọi `ledger::append` qua SAVEPOINT trong cùng transaction | `append_ledger_in_savepoint` gọi `crate::ledger::append` ở [txn.rs:262](../../crates/calm-core/src/txn.rs#L262) |
| P3 | "4 tool WS-1 mới nằm trong `recover` (floor toolset, không tắt được) — ngược hướng WS-9" | `reconciliation-round2.md` N2, phân tích + khuyến nghị ở `toolsurface-writesafety-ledger-research.md` Phần 1 | **Đã sửa đúng như khuyến nghị.** Toolset `"txn"` mới, tách khỏi `recover`, **không** nằm trong `SAFETY_FLOOR_TOOLSETS`, được thêm vào preset `edit` | [toolset.rs:109](../../crates/calm-server/src/tools/toolset.rs#L109) (`"txn"` trong `TOOLSET_NAMES`), [toolset.rs:130](../../crates/calm-server/src/tools/toolset.rs#L130) (`SAFETY_FLOOR_TOOLSETS` không có `"txn"`), [toolset.rs:55-58](../../crates/calm-server/src/tools/toolset.rs#L55) (4 tool trong preset `edit`, comment trỏ đúng về doc thiết kế) |

**Không tìm thấy claim "XONG"/"chưa làm" nào khác bị sai** trong vòng đọc lại lần này — 3 điểm trên
là tiến triển kể từ khi 7 doc con được viết, không phải lỗi của chúng tại thời điểm viết.

---

## 3. Xếp ưu tiên phần còn tồn đọng

Nguyên tắc kế thừa nguyên bản từ `priority-reaudit.md` §3 (đã chứng minh đúng qua 1 vòng thực thi
thật): (a) gap có bằng chứng code thật ưu tiên hơn gap runtime-hypothesis; (b) effort tự chứa +
pattern đã chứng minh an toàn ưu tiên hơn việc phải sửa gate logic đang sống; (c) việc cần
threat-model riêng trước khi code thì viết plan riêng trước, không code cơ hội chủ nghĩa dù mức
nghiêm trọng cao.

1. **WS-2 Phase 2 (TOCTOU `caller_set_digest`)** — ưu tiên cao nhất để **code ngay**. Lý do: design
   đã có sẵn từ `ws2-review-token-execution-plan.md` §4, threat model (T2) đã precise-scoped, tái
   dùng nguyên `evidence_digest` (WS-3, đã có sẵn, đã test) + cột schema đã tồn tại sẵn nhưng chưa ai
   ghi (`edit_transactions.review_token_id` — dù thiết kế cuối dùng bảng khác, xem §3.2 dưới) — đúng
   pattern "shadow/additive, không đụng gate logic đang chạy theo cách phá vỡ nó" đã dùng an toàn 4
   lần liên tiếp (digest.rs → txn.rs → maintenance.rs → path_policy.rs) trong đợt trước.
2. **WS-1 tiêu chí 6 (p95) — quyết định, không phải code** — cần chủ sở hữu milestone (fen) chốt
   trước khi "Write-Safety Beta" có thể coi là đóng chính thức. Xem §4, khuyến nghị cụ thể.
3. **WS-4 research-kickoff (KHÔNG code)** — bắt đầu **ngay** phần nghiên cứu (không phải
   implementation) vì đây là gap raw-severity cao nhất còn mở (arbitrary code execution qua
   `npx`/build-tool không sandbox) — bắt đầu sớm phần *câu hỏi cần trả lời* không rủi ro, vì chưa
   viết dòng code sản phẩm nào. Xem §5.
4. **WS-2 Phase 3 (approval-tier)** — giữ nguyên trạng thái chờ, như `ws2-review-token-execution-plan.md`
   §5 đã tự quyết định: cần kịch bản sử dụng cụ thể (team mode? CI-triggered agent?) trước khi có gì
   để lập kế hoạch — không có thay đổi nào trong audit này tạo ra kịch bản đó.
5. **WS-5, WS-6, WS-7, WS-8, WS-9 (redesign lớn), WS-11, WS-12, WS-14** — giữ nguyên P1/P2, không
   có phát hiện mới nào trong 2 vòng audit gần đây làm đổi thứ tự.

---

## 4. WS-1 tiêu chí 6 (p95) — ĐÃ CHỔT 2026-08-02: chấp nhận floor

> **Quyết định (2026-08-02, milestone owner):** chọn phương án **(a)** dưới đây — chấp nhận floor
> ~15%/23%. `phase1-p0-execution-plan.md` §6 đã cập nhật: target đổi từ "≤ 10%" thành floor đo
> được, tiêu chí 6/6 đóng. Không cần code thêm cho điểm này. Lập luận đã dẫn tới quyết định
> giữ nguyên ở dưới để tham chiếu sau này (vd. nếu một buổi nature-of-baseline đổi — vd. sau
> khi tối ưu `reindex_paths` độc lập với WS-1 — đáng đo lại từ đầu thay vì giả định floor
> này cố định mãi mãi).

`shadow-txn-connection-consolidation-plan.md` đã làm hết những gì hợp lý về mặt kỹ thuật: root
cause tìm ra chính xác (~4ms txn/ledger cost cố định trên baseline ~28ms `reindex_paths`-dominated,
≈+15% p50 / +23% p95), 2 tier tối ưu đã áp dụng (connection consolidation + SAVEPOINT ledger merge +
`advance_many` batching), Tier 3 (ledger `head_digest` lookup) đã bị loại trừ bằng đo thật (~0.1ms,
không đáng kể). **Không còn lever rẻ nào được tìm thấy.**

Hai lựa chọn thật sự, cả hai đều hợp lệ về mặt kỹ thuật:

- **(a) Chấp nhận floor ~15%/23%** — sửa target trong `phase1-p0-execution-plan.md` §6 từ "≤10%"
  thành một con số phản ánh chi phí cố định thật của durable transaction + hash-chained audit
  (đánh đổi write-safety lấy latency là một trade-off có chủ đích, không phải regression).
- **(b) Đầu tư thêm để hạ ~4ms** — hướng duy nhất chưa thử: gộp `atomic_write`'s fsync với transaction
  commit ở tầng OS (nếu khả thi), hoặc defer ledger-mirror sang async outbox thay vì đồng bộ trong
  savepoint (đánh đổi: ledger không còn "cùng transaction với file write" — mất chính guarantee mà
  P0-4 tồn tại để có).

**Khuyến nghị:** (a) — root cause đã hiểu rõ, effort để đóng hoàn toàn tiêu chí 6 sẽ phải hy sinh
đúng guarantee mà toàn bộ WS-1 vừa mới xây (ledger đồng bộ với transaction). Nhưng đây là quyết định
sản phẩm, cần fen chốt, không phải việc tôi tự quyết được.

---

## 5. WS-2 Phase 2 — SHIPPED 2026-08-02 (kế hoạch thực thi chi tiết, đã code)

> **Trạng thái: ĐÃ SHIP cùng ngày viết plan này.** Implement đúng như thiết kế §5.3 dưới đây,
> không lệch: `EditContextReview.caller_set_digest` field mới (`tools.rs`), `caller_set_digest`
> helper (`common.rs`, kết hợp BTreeSet dedupe+sort), `caller_symbol_set` fresh-query helper
> (`edit.rs`, cùng cạnh `all_caller_edges_confident`), gate logic mới trong
> `edit_lines_impl_gated` với mã lỗi mới `STALE_CALLER_SET`. 2 test mới
> (`caller_set_digest_mismatch_forces_stale_review_even_within_freshness_window`,
> `caller_set_digest_matches_when_nothing_changed_no_behavior_change`) + toàn bộ 306 test
> `calm-server` (303 cũ + 3 mới tính cả 2 test mới ở trên) pass, `clippy -D warnings` sạch,
> `cargo fmt --check` sạch, `diff_impact` xác nhận blast radius đúng như kỳ vọng (2 signature
> thay đổi, mỗi cái 1 call site duy nhất, cả hai đã được cập nhật). Một lưu ý triage thật sự từ
> quá trình code (không phải thiết kế sai, chỉ là chi tiết thực thi): edit sửa
> `record_edit_context_review`'s call site trong `guardrails.rs::edit_context` tự nó kích hoạt
> CHÍNH gặng WS-1 Change A vừa ship (`edit_context` là hub >10 caller, risk=high, môi trường
> này không cấu hình elicitation) — CALM từ chối edit qua chính `edit_lines`/`edit_symbol`
> của nó, đúng thiết kế. Fen duyệt thủ công edit đó (native `Edit`, không phải bypass —
> đường fallback chính thức khi CALM từ chối mà con người đã duyệt), tất cả các edit còn lại
> đều qua CALM `edit_lines`/`edit_symbol` bình thường. Thiết kế gốc bên dưới giữ nguyên làm
> tài liệu tham chiếu đúng những gì đã code.

### 5.1 Threat model (T2, kế thừa nguyên từ `ws2-review-token-execution-plan.md` §2, không mở rộng)

**Trong phạm vi:** caller-set của một symbol đã được `edit_context` review thay đổi (qua một edit
incremental ở file khác) **giữa** lúc review và lúc `edit_lines`/`edit_symbol` thật sự ghi — trong
cửa sổ `FRESHNESS_WINDOW_CALLS=200`. Đồng hồ call-count không thấy được thay đổi này vì
`incremental_graph_update` (đường đi thật của phần lớn edit) không bump `graph_generation_state`
(F1, đã xác nhận lại — xem §5.5 "không đổi so với lần đọc trước").

**Ngoài phạm vi:** mọi thứ Phase 1 đã đóng (T1); ký số / cross-process verification (F7, vẫn không
cần — `edit_context` và `edit_lines`/`edit_symbol` vẫn chạy chung 1 process); T3/T4 như Phase 1 đã
vẽ ranh giới.

### 5.2 Trạng thái code hiện tại (đọc trực tiếp phiên này, không phải trích lại doc sáng nay)

- `EditContextReview` struct — [tools.rs:155-165](../../crates/calm-server/src/tools.rs#L155):
  in-memory, session-scoped (không phải bảng DB), field hiện có: `at` (tool-call counter),
  `caller_qns` (tối đa 5, đã cap), `risk_level`.
- Nơi ghi: `record_edit_context_review` —
  [common.rs:369-386](../../crates/calm-server/src/tools/common.rs#L369) — nhận `caller_qns: &[String]`
  đã đầy đủ (chưa cap) từ call site, tự `.take(5)` bên trong.
- Call site duy nhất: `edit_context` —
  [guardrails.rs:380-384](../../crates/calm-server/src/tools/guardrails.rs#L380) — biến `callers`
  (full, untruncated — comment tại chỗ tự xác nhận "Uses the full, untruncated `callers` computed
  above") đã có sẵn ngay tại điểm gọi, **trước** khi bị cap.
- Nơi đọc + freshness check: `edit_lines_impl_gated` —
  [edit.rs:1091-1098](../../crates/calm-server/src/tools/edit.rs#L1091):
  ```rust
  let mut known_caller_qns: Vec<String> = Vec::new();
  for t in &pre_touched {
      match self.edit_context_review(&t.qualified_name) {
          Some(r) if now.saturating_sub(r.at) <= FRESHNESS_WINDOW_CALLS => {
              known_caller_qns.extend(r.caller_qns);
              ...
  ```
  Đây là **toàn bộ** cơ chế freshness hiện tại — thuần call-count, không có gì so sánh nội dung.
- `evidence_digest` (WS-3, SHA-256, dùng cho trust boundary) đã sẵn sàng tái dùng —
  `calm_core::digest::evidence_digest`, đã dùng ở `edit_lines_impl_gated`
  ([edit.rs:1374-1375](../../crates/calm-server/src/tools/edit.rs#L1374)) cho file content — cùng
  hàm, khác input.
- Index hỗ trợ: `idx_call_edges_to ON call_edges(to_symbol)` —
  [schema.rs:46](../../crates/calm-core/src/db/schema.rs#L46) — truy vấn "ai gọi symbol X" (chính
  là truy vấn cần recompute digest) đã có index, cùng shape truy vấn `edit_context` đang chạy mỗi
  lần gọi (không phải truy vấn mới, không phải chi phí mới về loại).

### 5.3 Thiết kế

**Không tạo bảng DB mới cho Phase 2** (khác gợi ý ban đầu trong `ws2-review-token-execution-plan.md`
§4 — "New table `prepared_reviews`") — vì review vẫn là in-memory/session-scoped ở Phase 1
(`EditContextReview` trong `tools.rs`, không phải DB), thêm 1 field vào struct có sẵn là thay đổi
nhỏ hơn hẳn, và Phase 2 không cần review sống sót qua restart (đó sẽ là một yêu cầu mới, không có
trong threat model T2). Nếu sau này cần review bền qua restart, đó là một quyết định riêng, không
bundle vào Phase 2.

1. **`EditContextReview`** (tools.rs:155) thêm field `caller_set_digest: String` — digest SHA-256
   của full `callers` list (sort theo `symbol` để deterministic, join bằng `\n`, qua
   `evidence_digest`), tính tại `guardrails.rs:380` **trước** khi gọi `record_edit_context_review`
   (dùng `callers` — biến đã có sẵn, full list, chưa cap).
2. **`record_edit_context_review`** (common.rs:369) thêm tham số `caller_set_digest: String`,
   lưu nguyên vào struct — không đổi hành vi field `caller_qns` (vẫn cap 5, vẫn dùng cho citation
   như Phase 1).
3. **`edit_lines_impl_gated`** (edit.rs:1091 loop) — sau khi pass call-count freshness check hiện
   tại (giữ nguyên làm pre-filter rẻ, không query DB), recompute digest fresh: query `call_edges
   WHERE to_symbol = ?` cho `t.qualified_name` (cùng shape query guardrails.rs đã chạy), build cùng
   thuật toán digest (sort + join + `evidence_digest`), so với `r.caller_set_digest` đã lưu.
   - Khớp ⇒ tiếp tục như hiện tại (không đổi hành vi quan sát được).
   - Lệch ⇒ coi như **chưa review** cho mục đích freshness — cùng shape fail-closed với
     `uncertain_empty_caller_needs_review`/`high_risk_needs_independent_review` đã có: mã lỗi mới
     `STALE_CALLER_SET` (không tái dùng `REASON_NOT_GROUNDED` — nguyên nhân khác, thông điệp khác:
     "caller set đã đổi kể từ review, không phải reason thiếu căn cứ").
4. **`graph_generation`** — ghi kèm vào `EditContextReview` làm **metadata chẩn đoán** (đúng F1 kết
   luận: quá thô để làm tín hiệu chính) — hiển thị trong lỗi `STALE_CALLER_SET` để debug, không phải
   điều kiện pass/fail.

### 5.4 Việc KHÔNG đổi

- `FRESHNESS_WINDOW_CALLS=200` vẫn là pre-filter đầu tiên, rẻ, không đổi ngưỡng.
- `cites_token`/citation logic của nhánh `!known_caller_qns.is_empty()` (Phase 1, F4) — không đụng.
- Không có bảng DB mới, không có migration mới.
- Không có tool mới — vẫn extend gate logic tại chỗ, đúng quyết định Phase 1 đã đưa ra (§6 doc gốc).

### 5.5 Việc cần làm trước khi merge (không skip)

- **Đo chi phí thật**, không chỉ suy luận từ index — dùng đúng harness đã dùng cho p95 (N=200
  in-process `edit_lines_flow` calls, `std::time::Instant`), so sánh **có** vs **không** digest
  recompute trên baseline hiện tại (đã có txn/ledger overhead rồi, đây là overhead **cộng thêm**
  cần đo riêng, không gộp vào con số +15%/+23% đã chốt ở §4). Kỳ vọng phân tích: nhỏ (1 query đã
  index + 1 lần hash trên list ngắn, tính từ mức `evidence_digest` đo được ở §4 cho 1 file content
  ~28-31ms baseline hoàn toàn khác class so với 1 danh sách caller thường <20 phần tử) — nhưng
  "khả năng nhỏ" không thay thế số đo thật, đúng kỷ luật đã áp dụng suốt đợt trước.
- Xác nhận lại F1 (`incremental_graph_update` không bump `graph_generation_state`) **chưa đổi** kể
  từ lần đọc trước — nếu ai đó đã sửa nó trong lúc này thì thiết kế §5.3 mục 4 cần xem lại (chỉ ảnh
  hưởng metadata chẩn đoán, không ảnh hưởng digest chính — rủi ro thấp nếu bỏ sót).
- Test tối thiểu: `caller_set_digest_mismatch_forces_stale_review_even_within_freshness_window`
  (thay đổi caller set qua incremental edit giữa 2 lần gọi, xác nhận `STALE_CALLER_SET`, không phải
  silent pass); `caller_set_digest_matches_when_nothing_changed_no_behavior_change` (regression
  guard cho case phổ biến nhất — không đổi hành vi quan sát được khi caller set không đổi).
- `diff_impact` trên `edit_lines_impl_gated`/`record_edit_context_review`/`EditContextReview` sau
  khi sửa — xác nhận `signature_changed` đúng như kỳ vọng (struct đổi field = có, hành vi gate =
  không đổi cho case caller-set-không-đổi).

### 5.6 Done khi

`known_caller_qns` không còn "khớp" một review đã stale về nội dung chỉ vì còn trong cửa sổ
call-count — reason cited đúng caller nhưng caller đó đã bị xoá khỏi caller set thật giữa lúc review
và lúc ghi sẽ bị từ chối, không silent-pass.

---

## 6. WS-4 — Research-kickoff scoping (KHÔNG phải execution plan)

**Vì sao không viết execution plan ngay dù đây là gap nghiêm trọng nhất:** đúng nguyên tắc
`priority-reaudit.md` Tier C đã tự đặt ra và giữ vững suốt cả đợt — effort lớn (OS-sandbox primitive,
cross-platform, network policy) khởi động vội trong lúc chưa có threat model đầy đủ là chính rủi ro
gây regression, không phải sự thận trọng thừa.

**Câu hỏi nghiên cứu cần trả lời trước khi có thể viết execution plan thật** (chưa trả lời trong
audit này — cần một phiên riêng):

1. **Phạm vi thật của "provider" cần sandbox** — liệt kê chính xác từng process CALM tự spawn ra
   ngoài (SCIP indexer theo ngôn ngữ, `npx`, package-manager, compiler plugin) qua
   `crates/calm-core/src/scip/runner.rs`/`provider.rs` — audit này **chưa** làm việc đọc đó, chỉ xác
   nhận (qua grep) rằng 0 sandbox nào tồn tại, chưa liệt kê đủ danh sách provider cần bọc.
2. **Primitive sandbox nào khả thi cross-platform** — Linux có `bwrap`/namespaces, macOS/Windows
   không có tương đương trực tiếp; cần quyết định: chấp nhận platform gap (Linux-only enforcement,
   fallback cảnh báo trên macOS/Windows) hay tìm primitive khác (container, WASM sandbox cho
   provider hỗ trợ compile-to-WASM, v.v.) — đây là quyết định kiến trúc lớn, không nên đoán trước.
3. **Tham chiếu thiết kế đã xác định (kế thừa từ `priority-reaudit.md` §6, CHƯA re-verify trong
   audit này — đọc lại VHEATM ở `/home/ybao/B.1/VHEATM` nằm ngoài phạm vi CALM repo)**:
   `host_qualification.py` (probe thật thay vì giả định sandbox đáng tin — "unavailable" khi backend
   thiếu, không âm thầm bỏ qua), `provider_policy.py` (allowlist pinned theo `provider_id`+
   `provider_version`, `validate_provider_binding()` từ chối khi runtime lệch policy đã pin),
   `sandbox.py` (`SandboxExecutor` component-walk, fail-closed khi thiếu policy broker). **Việc cần
   làm ở phiên nghiên cứu tiếp theo:** đọc lại 3 file này ở VHEATM HEAD hiện tại (đã trôi xa hơn từ
   lần đọc `eb01817` trước đó) trước khi copy bất kỳ pattern nào, không tin lại số liệu cũ.
4. **Cost/benefit của "ingest prebuilt signed SCIP từ CI" (ưu tiên 1 trong 4 theo master plan WS-4)**
   so với sandbox provider tại chỗ — vì đây là lựa chọn rẻ hơn nhiều (né hoàn toàn vấn đề sandbox
   local) cho phần lớn use-case CI, cần đánh giá riêng trước khi đầu tư vào sandbox local.

**Việc CÓ THỂ làm ngay, rủi ro ~0, không cần chờ nghiên cứu xong:** viết riêng 1 doc
`docs/plans/<date>-ws4-provider-sandbox-research.md` khi có phiên dành riêng cho việc này, bắt đầu
bằng câu hỏi 1 (liệt kê chính xác danh sách process CALM spawn ra ngoài — việc đọc code thuần, không
quyết định kiến trúc nào). Đây là bước hợp lý tiếp theo cho WS-4, không phải bây giờ trong tài liệu
này.

---

## 7. Bản đồ tài liệu — sau khi có tài liệu này

```
docs/plans/2026-08-01-calm-master-upgrade-plan.md        ← nguồn chiến lược gốc, KHÔNG đổi
docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md     ← nguồn xác minh VHEATM↔CALM, KHÔNG đổi
docs/plans/2026-08-01-vheatm-tier2-findings-remediation.md ← track riêng (SCIP encoding), ĐÃ XONG,
                                                               không liên quan WS-1..14, giữ nguyên
docs/plans/2026-08-02-phase2-priority-and-ws2-execution-plan.md  ← TÀI LIỆU NÀY, nguồn trạng thái
                                                                     + ưu tiên + kế hoạch hiện hành
docs/plans/2026-08-02-{phase1-p0-execution-plan,priority-reaudit,reconciliation-round2,
  toolsurface-writesafety-ledger-research,ws1-enforce-and-critical-risk-execution-plan,
  ws2-review-token-execution-plan,shadow-txn-connection-consolidation-plan}.md
  ← 7 doc lịch sử, mỗi doc đã gắn banner trỏ về tài liệu này, giữ nguyên nội dung làm bằng chứng
    xác minh chi tiết (số đo p95, threat model gốc, F1-F7, C1-C5, N1-N5) khi cần tra lại
```

Lần tới cần biết "trạng thái hiện tại là gì" → đọc tài liệu này. Lần tới cần biết "vì sao quyết định
X được đưa ra, số đo Y đến từ đâu" → đọc doc lịch sử tương ứng qua bản đồ trên.
