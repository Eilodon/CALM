---
title: "Multi-agent symbol-level lease/reservation — research + design (paper-informed)"
date: 2026-08-02
status: "research + design, KHÔNG phải execution plan — cần fen chốt hướng trước khi task-break xuống PR"
scope: trả lời câu hỏi phiên trước để mở — "nâng cấp gì có giá trị nhất, khác biệt nhất, cho cả người
  dùng lẫn agent" — bằng cách đọc trực tiếp 4 paper arXiv 2026 vừa tìm thấy + đối chiếu với hạ tầng
  CALM đã có (edit_lock, reviewing_symbol, session registry, txn.rs, audit_ledger)
verified_against: "abstract đọc trực tiếp qua WebFetch tại arxiv.org/abs/<id> (KHÔNG phải PDF —
  lần đầu thử PDF cho kết quả một phần bịa đặt, xem §0); code CALM đọc qua mcp__calm__search/source
  tại HEAD hiện tại (sau khi WS-2 Phase 2 đã ship)"
---

# Multi-agent Symbol-level Lease — Research + Design (2026-08-02)

## 0. Cảnh báo về độ tin cậy nguồn — đọc trước khi dùng bất kỳ số liệu nào dưới đây

Thử đọc PDF đầy đủ của 4 paper qua WebFetch trước — **thất bại 3/4** (nội dung nén, không đọc
được), và bản "thành công" duy nhất (MPAC) khi đối chiếu lại với abstract thật (đọc qua trang
`arxiv.org/abs/`) lộ ra **có số liệu bịa** (tool tự sinh "tested up to ~10 agents",
"degrades... beyond ~20 agents" — không tồn tại trong abstract thật). **Toàn bộ nội dung dưới đây
chỉ dựa trên abstract đọc trực tiếp từ trang `arxiv.org/abs/<id>` (HTML, không phải PDF nén)** —
không suy diễn thêm nội dung phần thân paper mà tôi chưa đọc được. Cả 4 đều là **arXiv preprint
2026, chưa peer-review**, phần lớn tác giả đơn/nhóm nhỏ, tự thừa nhận mẫu đánh giá nhỏ. Coi là
**tín hiệu định hướng** (research đang nóng, ai đó đã nghĩ tới đúng vấn đề này), **không phải bằng
chứng đã được kiểm chứng** — đúng kỷ luật đã áp dụng cho các số liệu Gartner/GhostApproval trước
đây trong dự án này.

---

## 1. 4 paper — tóm tắt trung thực (chỉ từ abstract thật)

### 1.1 ATM — "CID-Brokered Pre-Write Admission for Multi-Agent Code Co-Synthesis" (arXiv:2607.00041, Eagl Huang, 2026-06-29, cs.SE/cs.AI)

Framework "AI-Atomic-Framework" — trước khi một **shared mutation** được áp dụng, hệ thống phải
quyết định: intent nào được chạy song song, intent nào cần serialize, intent nào phải fail-closed.
Cơ chế: **Content Identifier (CID) broker** làm "shared-mutation admission subsystem"; write intent
được ánh xạ thành "semantic atoms và bounded regions" qua "adapter-guided atomization"; khi chưa có
atom-map đầy đủ, dùng "virtual atoms" tạm thời để so sánh/route một cách thận trọng. **Điểm quan
trọng nhất:** ghi thật do **một "neutral steward" áp dụng, không phải agent đề xuất tự ghi trực
tiếp**. Đánh giá: 12-scenario deterministic matrix + 3 case thật + benchmark riêng + 1 external-
adopter study 3 tuần — tác giả tự nói rõ "không claim broad comparative superiority."

### 1.2 Claim Plane — "Enforceable Change Intents and Dynamic Scope" (arXiv:2607.21909, Maxim Nikolaev, 2026-07-24, cs.SE)

**Sát nhất với bài toán CALM đang cần.** Coi concurrent code change là bài toán **pre-write
admission**. Mỗi worker khai báo một `ChangeIntent` có version, **base commit chính xác**,
resource có kiểu, dependency, và operation đánh dấu `committed` hoặc `contingent`. Một control
plane xác định (deterministic) admit các intent tương thích, **giới hạn song song trong cùng file
xuống đúng vùng đã khai báo** (không phải khoá cả file), serialize phần overlap chưa rõ, theo dõi
dependency bị invalidate, **fail-closed khi authority mơ hồ**. Cơ chế hay nhất: một mutation
`contingent` **không giữ quyền ghi ngay** — chỉ khi có **lần thử ghi thật đầu tiên** mới kích hoạt
"atomic scope promotion and re-admission against the current active set" (declare rẻ, chỉ escalate
thành lock thật khi thực sự cần ghi). Binding: "capabilities to intent versions, **leases**,
OS-level worktree locks, **monotonic fencing tokens**, and Git-tree provenance." Đánh giá: 6-cặp
CooperBench — self-nói rõ "the sample is intentionally too small for comparative claims" (không
phải benchmark đã kiểm chứng rộng).

### 1.3 MPAC — "Multi-Principal Agent Coordination Protocol" (arXiv:2604.09744, Qian/Fang/Li, 2026-04-10, cs.MA/cs.AI)

Bài toán **khác/khó hơn** CALM cần: agent thuộc **các principal/tổ chức khác nhau** cần phối hợp
trên state chung, không tin nhau mặc định. 5 lớp kiến trúc (Session/Intent/Operation/Conflict/
Governance), 21 loại message, 3 state machine, Lamport-clock causal watermarking, optimistic
concurrency control, capability token liên-tổ-chức. Có implementation tham chiếu Python+TS, tự báo
"95% reduction in coordination overhead, 4.8x wall-clock speedup" so với baseline serialize-qua-
người (số tự công bố, chưa third-party verify).

### 1.4 grite — "Before the Pull Request: Mining Multi-Agent Coordination" (arXiv:2606.19616, Dipankar Sarkar, 2026-06-17, cs.SE/cs.AI/cs.MA)

Khác 3 paper trên — đây là **nghiên cứu thực nghiệm + artifact mở**, không phải chỉ thiết kế lý
thuyết. Xây `grite`: coordination substrate **không cần server trung tâm**, lưu ngay trong git,
event log **append-only, ký số**. Số liệu tự báo cáo (từ chính dataset/harness họ công bố mở, có
thể tái kiểm): tỷ lệ công việc bị làm trùng/xung đột giảm từ **78% xuống 0%**, throughput hữu ích
tăng hơn 3 lần; mọi bản sao log của các agent hội tụ về cùng trạng thái, không mất write nào — trong
khi một tracker dựa file thường **mất write khi ghi đồng thời**; log là artifact mineable, tự phát
hiện được conflicting edits/lock starvation/redundant rediscovery/race-to-close mà lịch sử PR không
thấy được.

---

## 2. Đối chiếu với hạ tầng CALM đã có (đọc trực tiếp code, không suy đoán)

| Khái niệm trong paper | Cái CALM đã có, đã test | File:line |
|---|---|---|
| ATM's "neutral steward áp dụng ghi, không phải agent tự ghi" | **Đã đúng kiến trúc này từ đầu** — mọi write đi qua `edit_lines_impl_gated` trên server, agent không bao giờ ghi thẳng đĩa | `crates/calm-server/src/tools/edit.rs` |
| Claim Plane's "ChangeIntent có base commit chính xác" | `base_digest`/`proposed_digest` (SHA-256, `evidence_digest`) đã có sẵn trong `edit_transactions`, ship hôm nay (WS-1/WS-3) | `crates/calm-core/src/txn.rs`, `crates/calm-core/src/digest.rs` |
| Claim Plane's "monotonic fencing token" | `new_tx_id()` — ID sinh mới cho mỗi transaction, đã test, đã dùng làm khoá chính `edit_transactions` | `crates/calm-core/src/txn.rs:145` |
| Claim Plane's "declared vs contingent, promotion khi ghi thật" | **Chưa có** — nhưng `edit_context` (declare/review) và `edit_lines`/`edit_symbol` (ghi thật) đã là 2 bước tách biệt sẵn, đúng hình dạng cần để gắn "declared→promoted" vào | `guardrails.rs::edit_context`, `edit.rs::edit_lines_impl_gated` |
| "Ai đang xem/sửa symbol nào" cross-session | **Đã có, nhưng thuần advisory** — `SessionSummary.reviewing_symbol` hiện ra qua `session_context.other_active_sessions`, tự động dọn khi session disconnect (`deregistering_a_session_removes_it_from_the_shared_registry`, test thật) | `tools.rs:142`, `common.rs:494`, `tools.rs:4197` |
| grite's "append-only signed log hội tụ, mineable" | **Đã có, đang mồ côi cho mục đích multi-agent** — `audit_ledger` (P0-4) hash-chain, `head_digest`/`verify_chain`, ship hôm nay, hiện chỉ ghi 1 lần mỗi tx transition | `crates/calm-core/src/ledger.rs` |
| MPAC's capability token liên-tổ-chức, consensus vote, Byzantine tolerance | **Không cần** — CALM là 1 trust domain (1 project, 1 daemon, các session hợp tác không đối kháng), không phải nhiều tổ chức không tin nhau | — |

**Kết luận đối chiếu:** CALM đã có ~80% khối xây dựng cần thiết (neutral-steward architecture,
content-digest binding, fencing-token-shaped ID generator, cross-session visibility, hash-chained
ledger, auto-cleanup on disconnect) — **thiếu đúng một mảnh: biến "biết ai đang xem gì" thành "có
thể từ chối/nhường khi hai người cùng muốn ghi cùng chỗ."** Đây chính xác là khoảng trống Claim
Plane mô tả, và CALM không cần copy độ phức tạp của MPAC (không phải multi-org, không cần Byzantine
consensus) — cần bản CALM-shaped, nhẹ hơn nhiều.

---

## 3. Thiết kế đề xuất — `SymbolLease` (CALM-shaped, không copy nguyên paper nào)

### 3.1 Nguyên tắc thiết kế

- **Mặc định TẮT** (`[edit] enforce_symbol_lease = false`), đúng tiền lệ `elicit_hub_confirm` — chỉ
  tạo giá trị khi có ≥2 session thật (điều `other_active_sessions` đã phát hiện được), không đổi
  hành vi single-agent mặc định.
- **Declared rẻ, Active mới thật sự khoá** — đúng insight quan trọng nhất của Claim Plane: gọi
  `edit_context` không nên khoá gì cả (agent review 20 symbol nhưng chỉ sửa 1 không nên chặn ai);
  chỉ khi *thực sự* gọi `edit_lines`/`edit_symbol` mới cần "promotion."
  <br>Cân nhắc rõ: bản thân `edit_context` **không phải declaration miễn phí tuyệt đối** — mỗi lần
  gọi vẫn tốn một INSERT nhỏ (`symbol_leases` state `DECLARED`), nhưng đây là chi phí không đáng kể
  so với chính `edit_context` (đã chạy BFS transitive_bfs, đắt hơn nhiều) — "rẻ" ở đây nghĩa là rẻ
  hơn một `ACTIVE` lease chặn người khác, không phải zero cost tuyệt đối.
- **Fail-closed khi mơ hồ, không phải fail-mute** — đúng kỷ luật ATM + toàn bộ WS-1..3 vừa ship:
  session B cố ghi đúng symbol session A đang `ACTIVE` → từ chối rõ ràng, không âm thầm cho qua.
- **Không dùng consensus/Byzantine (MPAC)** — CALM đã có 1 điểm authority thật (daemon + SQLite +
  `edit_lock` hiện có) — đó chính là "neutral steward." Không cần phát minh distributed agreement.
- **Tái dùng `audit_ledger` cho lease event** — không tạo log song song kiểu grite; mỗi transition
  `DECLARED→ACTIVE→RELEASED/EXPIRED/PREEMPTED` là 1 dòng ledger mới, hưởng nguyên hash-chain đã có.

### 3.2 Schema (mẫu, chưa task-break)

```sql
CREATE TABLE symbol_leases (
  lease_id        TEXT PRIMARY KEY,   -- tái dùng new_tx_id()-shaped generator
  qualified_name  TEXT NOT NULL,
  session_id      TEXT NOT NULL,
  base_digest     TEXT NOT NULL,      -- evidence_digest tại thời điểm declare (WS-3, tái dùng)
  scope           TEXT NOT NULL CHECK (scope IN ('declared','active')),
  state           TEXT NOT NULL CHECK (state IN
                     ('DECLARED','ACTIVE','RELEASED','EXPIRED','PREEMPTED')),
  created_at      REAL NOT NULL,
  expires_at      REAL NOT NULL,      -- soft TTL, không giữ mãi nếu session treo
  released_at     REAL
);
CREATE INDEX idx_symbol_leases_active
  ON symbol_leases(qualified_name) WHERE state = 'ACTIVE';
```

### 3.3 Vòng đời

1. `edit_context(symbol)` → upsert 1 hàng `DECLARED` (session_id, base_digest hiện tại). Không
   chặn ai — thuần thông tin, đúng như `reviewing_symbol` hôm nay, chỉ persist thêm vào DB thay vì
   chỉ in-memory.
2. `edit_lines`/`edit_symbol` thật → thử **promote** `DECLARED` → `ACTIVE`:
   - Không ai khác `ACTIVE` trên đúng `qualified_name` → promote, tiếp tục write bình thường.
   - Session khác đang `ACTIVE` → mã lỗi mới `SYMBOL_LEASED_BY_ANOTHER_SESSION` (cùng hình dạng
     fail-closed với `STALE_CALLER_SET`/`EDIT_CONTEXT_REQUIRED` đã có), trả kèm session nào đang
     giữ + thời điểm hết hạn dự kiến — agent (hoặc con người đứng sau) tự quyết định chờ/báo người
     kia/chọn symbol khác.
3. Write commit xong → `ACTIVE → RELEASED`. Session disconnect → tái dùng đúng cơ chế
   `deregistering_a_session_removes_it_from_the_shared_registry` đã test để tự dọn (`EXPIRED`).
   Soft TTL (vd. vài phút) làm lưới an toàn thứ hai cho session còn kết nối nhưng bị treo — mượn ý
   tưởng `is_idle` (`daemon.rs:232`) làm mẫu, không phải logic mới từ đầu.
4. **Fencing thật:** tại thời điểm write commit, so `lease_id` hiện hành của request với
   `ACTIVE` lease mới nhất trong DB cho đúng `qualified_name` — lệch (đã bị `PREEMPTED` bởi một
   promotion khác xảy ra giữa chừng) → từ chối, dù request "tưởng" mình đã có lease hợp lệ lúc bắt
   đầu. Đây chính là race Claim Plane's "monotonic fencing token" và kinh điển Kleppmann's fencing-
   token pattern nhắm tới.

### 3.4 Việc CỐ TÌNH không làm (so với cả 4 paper)

- Không capability token ký số liên-agent (MPAC) — 1 trust domain, không cần.
- Không distributed consensus/voting (MPAC) — đã có 1 authority thật (daemon).
- Không tự xây git-native log riêng (grite) — tái dùng `audit_ledger` đã có, không nhân đôi cơ chế.
- Không semantic/AST-region-level scope như Claim Plane's "dynamic scope" ngay từ đầu — v1 khoá ở
  mức **symbol** (đơn vị CALM đã resolve chính xác `[line_start,line_end]` sẵn cho mọi tool khác) —
  region-level (2 agent sửa 2 phần khác nhau trong CÙNG 1 hàm) là tinh chỉnh sau, không phải v1.

---

## 4. Việc cần làm TRƯỚC khi có execution plan thật (đúng kỷ luật đã áp dụng cho WS-4)

Đây vẫn là **research + design**, chưa đủ để code ngay — khác WS-2 Phase 2 (đã có threat model
hẹp, đã verify chi phí). Câu hỏi còn mở, cần 1 vòng nữa trước khi task-break:

1. **Kịch bản dùng thật là gì?** Multi-agent trên CALM hôm nay = nhiều session cùng 1 daemon (đã
   có hạ tầng) — nhưng có bao nhiêu người dùng CALM thật sự chạy ≥2 agent song song trên cùng repo
   hôm nay? Chưa có dữ liệu sử dụng thật (khác `boundary_ambiguous_count`-kiểu tín hiệu đo được
   trong chính repo) — nên cân nhắc build ở dạng *đo trước* (log `other_active_sessions` overlap
   thật xảy ra bao nhiêu, qua telemetry đã có OTel) trước khi build cơ chế chặn.
2. **UX khi bị từ chối** — agent nhận `SYMBOL_LEASED_BY_ANOTHER_SESSION` thì làm gì tiếp? Retry
   sau bao lâu? Có nên có tool mới `wait_for_lease`/`request_notify`? Hay chỉ trả lỗi + để agent tự
   quyết (đơn giản hơn, đúng tinh thần "công cụ trả tín hiệu, không tự quyết hộ")?
3. **Region-level (không chỉ symbol-level) có thật sự cần cho CALM's user base, hay symbol-level
   đã đủ 95% trường hợp?** — cần dữ liệu, không đoán.
4. Độ ưu tiên so với WS-4 (provider sandbox) và WS-5 (evidence ledger) khi cả 3 đều đang cạnh tranh
   cùng một "trục khác biệt hoá" (write-trust) — không nên làm cả 3 cùng lúc.

**Khuyến nghị hành động tức thời (rẻ, không cần quyết định lớn trước):** thêm 1 counter đo tần suất
`other_active_sessions` thật sự **overlap** trên cùng file/symbol trong sử dụng thực tế (tái dùng
hạ tầng OTel đã ship) — biến câu hỏi 1 từ "đoán" thành "đo," đúng kỷ luật "profile trước khi build"
đã dùng thành công cho reach-index (`calm-gortex-adaptation-roadmap` memory, mục 8: quyết định
KHÔNG làm dựa trên số đo thật, không phải cảm tính) và p95 (WS-1's benchmark discipline hôm nay).
