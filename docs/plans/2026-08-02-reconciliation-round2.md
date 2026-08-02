---
title: "Reconciliation Round 2 — xác minh độc lập 3 doc con trên code thật + roadmap điều chỉnh"
date: 2026-08-02
status: reconciliation pass, con của master plan; xác nhận priority-reaudit.md chính xác + bổ sung 5 finding mới nó chưa nêu
scope: đối chiếu lại MỌI claim "XONG" trong 3 doc con (phase1-p0-execution-plan, priority-reaudit,
  ws2-review-token-execution-plan) với code thật tại HEAD hiện tại + lịch sử commit; điều chỉnh
  roadmap/phasing §5 master plan theo tiến độ thật đã đạt trong 1 phiên 2026-08-02
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md          # nguồn gốc, WS-1..WS-14
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md       # nguồn VHEATM↔CALM
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md           # task breakdown WS-1/2/3
  - docs/plans/2026-08-02-priority-reaudit.md                   # re-audit C1-C5, Tier A/B/C/D
  - docs/plans/2026-08-02-ws2-review-token-execution-plan.md    # F1-F7, Phase 1/2/3
verified_against: |
  HEAD = ab0e374 (git log; commits 7b65acb, 07e97fb, ab0e374 tất cả đã nằm trong HEAD, working
  tree clean lúc audit). Mọi file:line dưới đây đọc trực tiếp qua mcp__calm__search/source trong
  chính phiên này — không copy số liệu từ 3 doc con.
---

# Reconciliation Round 2 (2026-08-02)

## 0. Vì sao cần vòng đọc thứ hai

`priority-reaudit.md` (viết cùng ngày, trước tài liệu này) đã tự nhận **không đọc lại mọi số
liệu VHEATM line-by-line** và tự giới hạn phạm vi. Đây là vòng xác minh độc lập: đọc trực tiếp
code cho **mọi** claim "XONG 2026-08-02" trong cả 3 doc con, để loại khả năng một claim "đã làm"
chỉ đúng trong ý định người viết plan chứ chưa đúng trên đĩa. Kết luận ngắn: **cả 3 doc con đều
chính xác** — không phát hiện claim "XONG" nào sai. Nhưng vòng đọc này tìm ra **5 điểm không nằm
trong phạm vi 3 doc trước** (§2), đủ quan trọng để chỉnh roadmap.

---

## 1. Xác minh độc lập — bằng chứng tự đọc, đối chiếu priority-reaudit.md §2

| Claim (priority-reaudit.md) | Xác minh lại (file:line đọc trong phiên này) | Kết quả |
|---|---|---|
| `audit_ledger` table + `ledger.rs` tồn tại, 8 test | `schema.rs:308 CREATE TABLE IF NOT EXISTS audit_ledger`; `ledger.rs:86,105,128` (`head_digest`/`append`/`verify_chain`); `ledger.rs:250,270` (2 test tamper/delete-gap) | ✅ khớp |
| `edit_transactions`/`tx_events`/`maintenance_jobs` tồn tại, đúng 7-state (không phải 9) | `schema.rs:244,266,286` — 3 `CREATE TABLE` đúng vị trí | ✅ khớp |
| WS-3 `path_policy` wire vào `resolve_repo_path`, default `FollowInternalSymlinks`, 0 đổi hành vi | (đã đọc trực tiếp ở phase1-plan §3.1, không re-đọc lần 3 vì đã có 2 vòng verify trong chính doc đó) | không re-kiểm — chấp nhận, rủi ro thấp |
| WS-13 `qualify-release` mới, `build`/`docker` giờ `needs:` nó | `release.yml:25` (`qualify-release:` job), `:75`,`:232` (2 chỗ `needs: qualify-release`) | ✅ khớp — xác nhận đúng 2 job publish đều bị chặn |
| `status-drift` job mới trong `ci.yml` | `ci.yml:302 status-drift:` | ✅ khớp |
| `docs/status.generated.md` sinh từ toolsnaps, liệt kê tool inventory | Đọc trực tiếp: liệt kê **34 tool**, đúng 4 tool mới (`edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`) nằm trong danh sách | ✅ khớp, nhưng xem N2 §2 |
| WS-2 Phase 1: đóng nhánh `known_caller_qns.is_empty() ⇒ auto-pass` cho `LowConfidence`, mã lỗi `UNCERTAIN_ZERO_CALLER` | `edit.rs:1111-1136` đọc trực tiếp — logic khớp **chính xác** mô tả trong ws2-plan §3.1 kể cả comment giải thích tại chỗ | ✅ khớp |
| WS-1 shadow mode: `txn::begin`/`advance` không chặn write khi lỗi | `edit.rs:598` (`format_files_impl`): `Err(e) => { tracing::warn!("shadow txn::begin failed (non-blocking): {e}"); None }`; y hệt tại `edit.rs:1241` (`edit_lines_impl_gated`); mọi `advance` sau đó đều `let _ =` hoặc match không propagate lỗi | ✅ khớp — xác nhận **thật sự** shadow, không có nhánh nào âm thầm đã enforce |
| F1 (ws2-plan): `incremental_graph_update` không bump `graph_generation_state` | Grep toàn bộ `graph_generation_state` trong `pipeline.rs`: 9 hit, thuộc `rebuild_graph_from_index`, `reindex_all_cancellable_with_phase`, `rebuild_call_site_identity_baseline` (chỉ đọc), `reindex_changed_cancellable`, `reindex_paths`, `snapshot` (chỉ đọc), 1 test. **`incremental_graph_update` không xuất hiện trong danh sách này** | ✅ khớp — xác nhận finding F1 là thật, không phải suy đoán |
| P0-4 ledger "chưa wire vào write path thật nào" | Grep `ledger::append`, `use crate::ledger` toàn repo → **0 kết quả** ngoài chính `ledger.rs` | ✅ khớp — xác nhận module tồn tại nhưng hoàn toàn cô lập (xem N4) |

**Không tìm thấy claim "XONG" nào sai hoặc phóng đại trong vòng đọc này.** Kỷ luật
"verify trước khi code, cite file:line thật" mà 3 doc con tự áp cho mình đã giữ vững.

---

## 2. 5 finding mới — không nằm trong phạm vi 3 doc con

### N1 — Nhầm lẫn tên "task 4.8" giữa hai việc khác nhau

`phase1-p0-execution-plan.md` §4.6 định nghĩa task 4.8 = **"shadow → dual-read → enforce"**
(chuyển đổi hành vi thật của write path) và đánh dấu **CHƯA LÀM**. Nhưng §8 mục 3 và
`priority-reaudit.md` Tier B mục 5 lại gọi **crash-injection suite** (việc khác hẳn — chỉ là viết
test, không đổi hành vi write path) là "task 4.8 — XONG". Cả hai tài liệu đều tự nhận thấy mâu
thuẫn này và chú thích ("đây chỉ là crash-injection suite... transition shadow→enforce KHÔNG nằm
trong scope này") — nhưng việc dùng chung một số thứ tự cho 2 task khác nhau dễ khiến người đọc
lướt (hoặc một agent tương lai chỉ đọc bảng, không đọc chú thích) kết luận nhầm task 4.8 đã xong
toàn bộ. **Khuyến nghị:** trong mọi tham chiếu tương lai, gọi việc còn thiếu là **"WS-1 enforce
transition"** (không phải "task 4.8") để tránh trùng tên.

### N2 — Tool surface: 30 → 34, đúng chiều WS-9 muốn thu hẹp

`docs/status.generated.md` xác nhận **34 tool** hiện tại (audit gốc A16 ghi nhận "~30 tool" là
vấn đề — workflow phân mảnh, agent khó chọn tool). WS-1 vừa thêm đúng 4 tool mới
(`edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`) —
đúng hướng ngược với WS-9's "public default = ít tool cấp workflow hơn". Đây **không phải sai
lầm** (4 tool này là admin/diagnostic, nằm trong toolset `recover`, không phải preset public mặc
định) nhưng là một **debt cụ thể** cần WS-9 xử lý khi tới lượt: khi thiết kế
`orient_repository · search_code · understand_symbol · analyze_change · prepare_edit ·
commit_edit · verify_change · transaction_status` (8 tool workflow-cấp mô tả trong master plan
WS-9), 4 tool WS-1 mới nên gộp vào `transaction_status` ở tầng public, giữ nguyên ở tầng
`expert`/`recover` cho debugging sâu — không nên là 4 tool riêng ở cả hai tầng.

### N3 — "Write-Safety Beta" milestone gate: 1/6 tiêu chí đạt, không phải "gần xong"

Đọc lại checklist §6 của `phase1-p0-execution-plan.md` theo đúng trạng thái hiện tại:

- [x] Crash-injection suite 100 lần/transition — **đạt**.
- [ ] `replay_state` khớp cache cho 100% tx trong toàn bộ test suite — chưa có bằng chứng đã
  chạy check này as a gate (test suite có nhưng không có báo cáo "100%" tường minh).
- [ ] 0 write path bỏ qua `EditTransaction` — **KHÔNG đạt, theo thiết kế**: shadow mode (xác
  nhận N1 + §1 trên) để write tiếp tục **ngay cả khi** `txn::begin` lỗi. Đây là hành vi **cố ý**
  của giai đoạn shadow, không phải lỗi — nhưng nghĩa là gate này chỉ đạt được sau khi có quyết
  định enforce (N1).
- [ ] `critical`-risk edit bị block nếu thiếu approver — **KHÔNG đạt**: WS-2 Phase 3 (approval-tier)
  vẫn design-only, chưa có scenario cụ thể (ws2-plan §5 tự nói vậy).
- [ ] `atomic_write` Fast mode 0 đổi hành vi — có khả năng đạt (test pass) nhưng chưa tick tường
  minh trong chính checklist.
- [ ] p95 latency `edit_lines` không tăng >10% — **chưa đo**, không có benchmark nào chạy trong
  phiên này đối chiếu con số này.

**Kết luận N3:** mặc dù khối lượng code/test rất lớn đã ship trong 1 phiên (WS-1/2/3 + WS-13),
milestone "Write-Safety Beta" mà master plan đặt ở 08–10/2026 **còn 4-5/6 tiêu chí mở**, chủ yếu
vì 2 quyết định có chủ đích chưa đưa ra (enforce transition, approval-tier) chứ không phải vì
thiếu code. Roadmap không nên hạ mốc thời gian gate này chỉ vì khối lượng code ship nhanh — 2
quyết định còn lại có thể chậm hơn nhiều so với việc viết code (giống đúng nguyên tắc "bắt đầu
vội đúng là rủi ro" mà cả 3 doc con đã tự áp dụng cho WS-4).

### N4 — `audit_ledger`/`ledger.rs` là module "mồ côi": xây xong nhưng 0 nơi gọi

Xác nhận bằng grep (§1 trên): không một dòng code nào ngoài `ledger.rs` chính nó gọi
`ledger::append`. Module tồn tại, có schema, có 8 test tự-đủ (self-contained), nhưng **không ai
ghi vào nó** trong luồng thực thi thật — khác với `txn.rs`/`maintenance.rs` (cũng shadow, nhưng
**đã wire** vào `edit_lines_impl_gated`/`format_files_impl` thật). Đây là khoảng cách giữa "built"
và "useful" đáng chú ý nhất còn lại trong nhóm Tier A vừa xong: `ledger::append` cần được gọi từ
đâu đó thật (ứng viên tự nhiên: mỗi `tx_events` insert trong `txn::advance`, hoặc mỗi quyết định
gate `allow/deny` trong `edit_lines_impl_gated` — tuning theo mức chi tiết mong muốn) để
`audit_ledger` bắt đầu tích luỹ giá trị thật thay vì là một bảng rỗng mãi mãi.

### N5 — Vận tốc thực tế đã vượt xa giả định roadmap gốc

Master plan §5 giả định "3–5 kỹ sư... 1 maintainer thì kéo dài đáng kể" cho giai đoạn
08–10/2026 (WS-1 shadow, WS-2, WS-3). Thực tế: **toàn bộ WS-1 task 4.1-4.7, WS-3 task 3.1-3.4,
WS-13 rút gọn, P0-4 ledger, và WS-2 Phase 1** đều ship trong đúng **1 phiên làm việc**
(commit timestamps `7b65acb`/`07e97fb`/`ab0e374` cùng ngày 2026-08-02, cách nhau vài phút). Đây
không có nghĩa toàn bộ roadmap 08/2026-07/2027 co lại tương ứng — các mốc còn lại (WS-4 sandbox,
WS-6 verification pipeline, WS-8 ANN) có bản chất effort khác hẳn (OS-level sandbox, ML eval,
infra mới) không so sánh được tốc độ 1:1 với refactor/module bổ sung có pattern sẵn. Nhưng mốc
**"Write-Safety Beta" (08–10/2026)** nên được viết lại thành hai lớp tách biệt thay vì một ngày:
lớp *"code sẵn sàng"* (đã đạt, 2026-08-02) và lớp *"gate quyết định pass"* (N3, còn mở, không có
ETA rõ vì phụ thuộc quyết định enforce/approval-tier chưa có scenario) — gộp chung một ngày trong
roadmap gốc sẽ đánh lừa người đọc rằng cả hai lớp cùng tiến độ.

---

## 3. Bảng trạng thái WS-1..WS-14 — kế thừa priority-reaudit.md §2, đã xác minh + cập nhật N1-N5

| WS | Trạng thái (đã xác minh 2026-08-02, vòng 2) | Việc còn lại cụ thể |
|---|---|---|
| WS-1 | Shadow mode **XONG và đã xác minh trên code** (không chỉ theo lời doc). Milestone "Write-Safety Beta" **1/6 tiêu chí** (N3) | Quyết định + implement **enforce transition** (đổi tên khỏi "task 4.8", N1); đo p95 latency; benchmark `replay_state` 100% |
| WS-2 | Phase 1 XONG+xác minh (`UNCERTAIN_ZERO_CALLER` sống trên code thật). Phase 2 (caller_set_digest durable, đóng T2 TOCTOU) design-only. Phase 3 (approval-tier) chưa có scenario | Phase 2 cần vòng verify riêng đo chi phí recompute digest (đã tự ghi trong ws2-plan §4) trước khi code; Phase 3 chờ scenario cụ thể |
| WS-3 | XONG + xác minh (digest.rs, path_policy.rs, atomic_write_with) | `openat2 RESOLVE_BENEATH` (Linux TOCTOU) vẫn để PR riêng, cố ý |
| WS-4 | **Chưa động — xác nhận lại**: 0 sandbox/bwrap/seccomp | Cần research+plan doc riêng (Tier C không đổi); tham chiếu `host_qualification.py`/`provider_policy.py` VHEATM đã nêu ở priority-reaudit §6 |
| WS-5 | Chưa làm — **lưu ý phân biệt** với P0-4 `audit_ledger` (N4): WS-5 là evidence lattice cho **call-graph edges** (`edge_evidence`/`resolution_runs`), khác hoàn toàn P0-4 (audit log hành động, không phải nguồn gốc edge) | P1, đúng lịch |
| WS-6 | Chưa làm | P1, đúng lịch |
| WS-7 | Nền có (`graph_generation_state`) nhưng **F1 xác nhận**: tín hiệu này quá thô cho per-symbol staleness qua đường incremental — ảnh hưởng cả WS-7 lẫn WS-2 Phase 2 | Khi lập plan WS-7, phải tính luôn per-symbol/per-edge invalidation signal, không chỉ generation toàn cục |
| WS-8 | Chưa làm | P1, đúng lịch |
| WS-9 | Chưa làm — **N2**: tool surface tăng 30→34 (ngược hướng WS-9 một phần, có lý do) | Khi lập plan, gộp 4 tool WS-1 admin vào `transaction_status` ở tầng public |
| WS-10 | Gần như đã xong theo C3 (quyết định có chủ đích, không phải lỗ hổng) | Không cần hành động khẩn |
| WS-11 | Chưa làm, `edit.lock` vẫn repo-wide | P1, đúng lịch |
| WS-12 | Một phần (`wrap_untrusted`/`scan_text` có) | P1, đúng lịch |
| WS-13 | XONG (rút gọn) + xác minh trên `release.yml`/`ci.yml`/`status.generated.md` thật | v2 (ký số, theo mẫu VHEATM `trust_registry.py`) là hardening sau, không khẩn |
| WS-14 | Chưa làm, cố ý defer | P2, đúng lịch |
| P0-4 (ledger) | Module XONG + xác minh, nhưng **N4: 0 call site thật** — inert | Wire `ledger::append` vào ít nhất 1 write path thật (ứng viên: `txn::advance`) |

---

## 4. Roadmap điều chỉnh — thay thế bảng phasing ở master plan §5

Giữ nguyên cấu trúc P0/P1/P2 và các giai đoạn sau của master plan §5 (chưa có gì làm chúng lỗi
thời); chỉ viết lại **giai đoạn đầu tiên** để phản ánh thực tế đã đạt + tách rõ 2 lớp N3/N5:

| Giai đoạn | Hạng mục | Trạng thái thật (2026-08-02) |
|---|---|---|
| ~~08–10/2026~~ **Đã đạt 2026-08-02 (code)** | WS-1 shadow, WS-2 Phase 1, WS-3, WS-13 rút gọn, P0-4 ledger | Code+test **XONG**. Gate "Write-Safety Beta" **CHƯA đạt** (N3) — còn 2 quyết định (enforce, approval-tier) + 1 benchmark (p95) + 1 wiring (N4) |
| **Kế tiếp, chưa có ETA cố định** | (a) Quyết định + implement WS-1 enforce transition · (b) wire `audit_ledger` vào ≥1 write path (N4) · (c) đo p95 `edit_lines` overhead so baseline | Mở khoá thật sự "Write-Safety Beta" — ưu tiên hơn bắt đầu WS-4, vì đây là hoàn thiện việc đã bắt đầu, effort nhỏ hơn WS-4 nhiều |
| Sau khi (a)-(c) xong | WS-2 Phase 2 (caller_set_digest durable, đóng T2) — cần vòng verify chi phí riêng trước | Chưa bắt đầu, đúng kế hoạch ws2-plan §4 |
| 10/2026–01/2027 (giữ nguyên master plan) | WS-4 provider sandbox (Rust+TS trước), tham chiếu `host_qualification.py`/`provider_policy.py` | Chưa bắt đầu — Tier C, cần research+plan doc riêng trước, KHÔNG bắt đầu code cơ hội chủ nghĩa |
| 12/2026–02/2027 (giữ nguyên) | WS-7 (đã biết thêm F1 — tính per-symbol signal), WS-8 ANN, external benchmark | Chưa bắt đầu |
| 01–04/2027 (giữ nguyên) | WS-6, WS-9 (đã biết N2 — gộp 4 tool mới), WS-10 (gần như đã xong — chỉ audit lại nếu remote-serve thật sự dùng) | Chưa bắt đầu |
| 03–07/2027 (giữ nguyên) | WS-14, chaos+security review | Chưa bắt đầu, đúng kế hoạch |
| song song (giữ nguyên) | WS-5 (evidence lattice — phân biệt rõ với P0-4 ledger, N4 note) | Chưa bắt đầu |

---

## 5. Thứ tự hành động tiếp theo — cập nhật §4 priority-reaudit.md theo việc đã xong

`priority-reaudit.md` §4 đề ra 5 việc; **4/5 đã xong** (P0-4, WS-3 path_policy, WS-13, WS-2
execution-plan-trước-khi-code). Danh sách kế tiếp, theo cùng nguyên tắc (rủi ro thấp/effort tự
chứa trước, việc cần quyết định/threat-model riêng sau, không bắt đầu WS-4 cơ hội chủ nghĩa):

1. **Wire `audit_ledger` vào 1 write path thật (N4).** Effort thấp, module đã xong+test, đúng
   pattern Tier A vừa dùng 4 lần an toàn (digest→txn→maintenance→ledger schema) — chỉ còn thiếu
   1 call site. Không đổi gate logic, không rủi ro cao.
2. **Đo p95 `edit_lines` overhead (N3 mục cuối).** Thuần đo lường, không đổi code — dùng benchmark
   sẵn có (`b6_tool_call_efficiency` theo gợi ý phase1-plan §6) trước/sau khi bật shadow-mode.
3. **Viết quyết định/plan riêng cho WS-1 enforce transition** (đổi tên tránh N1) — đúng kỷ luật
   "rủi ro cao cần verify/threat-model trước khi code" đã áp cho WS-2/WS-4. Đây là việc duy nhất
   trong Tier B thật sự đổi hành vi write path production.
4. **WS-2 Phase 2** (caller_set_digest) — sau khi (3) xong và đã soak, đúng khuyến nghị ws2-plan
   §4 ("Phase 1 nên chạy và ổn định trước").
5. **WS-4 research+plan doc riêng** — vẫn Tier C, vẫn chưa bắt đầu, vẫn cần một phiên riêng không
   bundle — không đổi so với priority-reaudit.md, nhắc lại vì đây là gap nặng nhất còn treo.

---

## 6. Việc KHÔNG nằm trong phạm vi tài liệu này

Không chạy lại benchmark/SLO nào (kể cả p95 đề xuất ở mục 5.2 — chỉ đề xuất, chưa đo). Không đọc
lại VHEATM lần 3 (priority-reaudit.md §6 đã làm đủ cho mục đích hiện tại). Không audit sâu WS-5
đến WS-12 phần chưa làm (không có finding mới thay đổi thứ tự P1/P2 của chúng ngoài F1/N2 đã ghi
ở bảng §3). Không tự ý implement N1-N5 — đây là tài liệu **phân tích + điều chỉnh kế hoạch**
theo đúng yêu cầu, không phải một đợt code mới.
