---
title: "Phase 1 (P0) Execution Plan — WS-1 durable transaction + WS-2 review token + WS-3 crypto/FS hardening"
date: 2026-08-02
status: execution-ready detail plan, con của master plan
scope: biến §3 WS-1/WS-2/WS-3 của master plan thành task breakdown thực thi được — module/schema/signature cụ thể, thứ tự PR, test, rollout
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md   # nguồn — §3 WS-1..WS-3, §5 roadmap giai đoạn 08-10/2026
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md # nguồn xác minh VHEATM↔CALM, §5 P0-1..P0-6
  - /home/ybao/B.1/VHEATM (đọc trực tiếp source, không lấy prose làm bằng chứng)
audited_state:
  calm_head: e42a5d0 (branch main, kế thừa 7477721 — chỉ thêm 2 doc, code chưa đổi)
  vheatm_audited_by_master_plan: 9303d87
  vheatm_head_now: e181e3d (+14 commit "fix:"/"docs:" kể từ 9303d87 — xem §1)
verification: mọi file:line CALM trong tài liệu này đọc trực tiếp qua mcp__calm__source/search tại calm_head hiện tại; mọi trích dẫn VHEATM đọc trực tiếp source tại vheatm_head_now
---

# Phase 1 (P0) Execution Plan — WS-1 + WS-2 + WS-3

> Tài liệu này **không lặp lại** phần "vấn đề/thiết kế đích" đã có trong master plan §3 —
> nó bắt đầu từ đó và đi xuống một tầng: **module nào, hàm nào, chữ ký gì, migration nào,
> PR nào trước/sau, test nào, rollout theo bước nào.** Đọc kèm master plan §3 WS-1/WS-2/WS-3
> và §5 (roadmap 08–10/2026 → "Write-Safety Beta").

---

## 0. Việc đã làm để soạn tài liệu này (để người review biết mức tin cậy)

1. Đọc lại toàn văn cả hai plan doc vừa pull (`2026-08-01-calm-master-upgrade-plan.md`,
   `2026-08-01-calm-adopt-from-vheatm-plan.md`).
2. Đối chiếu VHEATM tại `/home/ybao/B.1/VHEATM` — **không dùng lại số liệu cũ trong hai file
   trên**, đọc trực tiếp `lifecycle.py`, `provenance.py`, `sandbox.py`, `providers.py`,
   `tool_broker.py`, `evaluation.py`, `report_validator.py` và 4 schema
   (`approval-token`, `tool-receipt`, `validation-receipt`, `supply-chain-attestation`) tại
   HEAD hiện tại (`e181e3d`), vì HEAD đã đi xa hơn HEAD được hai file trên audit (`9303d87`).
3. Đối chiếu lại các file:line CALM được hai file trên trích dẫn cho WS-1/WS-2/WS-3, qua
   `mcp__calm__source`/`search` tại CALM HEAD hiện tại — mọi trích dẫn dưới đây là **số dòng
   thật vừa đọc**, không copy từ hai file nguồn.

---

## 1. VHEATM đã trôi xa hơn — điều này củng cố kế hoạch, không làm nó lỗi thời

`git log 9303d87..HEAD` bên VHEATM = 14 commit, toàn bộ là `fix:`/`docs: ADR` cho đúng những
mảng CALM định mượn pattern: `sandbox.py` (+163/-... dòng — "bind sandbox backend to authorized
execution"), `evaluation.py` (+147 dòng — "bind RG measurements to canonical methods"),
`providers.py`, `provenance.py`, `report_validator.py`, `pilot.py`, `judge.py`. Tức là VHEATM
đang tự hardening đúng lớp mà hai plan doc đề xuất CALM mượn — pattern càng lúc càng cứng, không
phải đang rã ra. Tôi đã đọc lại các file này ở HEAD mới nhất (không phải bản đã audit), nên mọi
trích dẫn VHEATM trong tài liệu này là **bản hiện hành**, gồm cả những phần *không* đổi
(`lifecycle.py`, `provenance.py`, 4 schema — byte-for-byte khớp mô tả trong hai file gốc) và
những phần *có* đổi (`sandbox.py`, `providers.py`, `tool_broker.py` — mô tả dưới đây phản ánh
bản mới, mạnh hơn bản đã audit).

**Phát hiện đáng chú ý khi đọc `tool_broker.py` (chưa được nêu chi tiết trong hai file gốc):**
`ToolBroker._validate_configuration()` (dòng 458–478) **từ chối khởi động** nếu policy không
đúng "hiến pháp" tối thiểu: `default_decision == "unknown"`, `fail_safe == true`,
`tools.default == "deny"`, `egress.default == "deny"`, `human_approval.reusable == false`. Đây
là **fail-closed kiểm ở constructor**, không phải kiểm ở từng call site — một pattern đáng mượn
riêng cho CALM (xem §5.4 dưới).

---

## 2. Thứ tự triển khai 3 workstream — vì sao WS-3 → WS-1 → WS-2

Master plan liệt kê WS-1/WS-2/WS-3 song song (cùng P0), nhưng có phụ thuộc dữ liệu một chiều:

```
WS-3 (evidence_digest SHA-256 + atomic_write hardening)
   │  cung cấp: hàm digest dùng ở mọi nơi khác; atomic_write mới mà WS-1 sẽ gọi
   ▼
WS-1 (edit_transactions + maintenance_jobs + txn.rs state machine)
   │  cung cấp: tx_id, graph_generation_before/after, state — là "state thật" mà
   │  WS-2 cần bind review token vào
   ▼
WS-2 (ReviewToken bind graph_generation + caller_set_digest + tx state)
```

Làm ngược thứ tự (vd. build ReviewToken trước) sẽ phải bind vào state chưa tồn tại rồi sửa lại
sau. Khuyến nghị: **3 PR tuần tự, mỗi PR merge xong mới bắt đầu PR sau**, không làm song song
trên cùng branch vì cả ba đều chạm `crates/calm-server/src/tools/edit.rs`.

---

## 3. WS-3 chi tiết — crypto hash + FS hardening

### 3.1. Trạng thái hiện tại (đọc trực tiếp, 2026-08-02)

- `hash_content` — `crates/calm-core/src/indexer/pipeline.rs:121-130`. FNV-1a 64-bit,
  `pub`, **is_hub:true, caller_count 41**. Comment tại chỗ đã tự giải thích lý do dùng FNV thay
  vì `DefaultHasher`: *"DefaultHasher is explicitly not stable across Rust versions/platforms
  ... FNV-1a has a fixed, documented algorithm"*. → **Không đụng vào hàm này** (đúng anti-goal
  cả hai plan doc): nó là hub, sửa nó kéo theo edit_context/rủi ro lan rộng không cần thiết cho
  WS-3.
- `atomic_write` — `crates/calm-core/src/edit.rs:477-515`. Xác nhận đúng A06:
  - dòng 483: `tmp_path` = `.{file_name}.ci-edit-{PID}.tmp` — **PID-based, không random, không
    `O_EXCL`** (hai process trùng PID sau reuse, hoặc hai request cùng process/thread pool, có
    thể đụng tên).
  - dòng 494–497: `File::create(&tmp_path)` (= `O_CREAT|O_TRUNC`, không `O_EXCL`) rồi
    `sync_all()` — chỉ fsync **file**, không fsync **thư mục cha** sau `rename()` (dòng 508).
    Trên crash ngay sau rename nhưng trước khi thư mục cha tự flush (không đảm bảo timing),
    một số filesystem có thể mất liên kết tên→inode mới dù nội dung file đã sync.
  - dòng 500–507: permissions preservation là **best-effort** — comment tại chỗ giải thích rõ
    đây là quyết định có chủ đích (audit F5, không fail write vì permission mismatch) — WS-3
    **giữ nguyên hành vi mặc định**, chỉ thêm cờ high-assurance để surface lỗi khi cần.
- **SỪA 2026-08-02, đọc lại trước khi implement task 3.3/3.4 (vòng đọc bắt buộc thứ ba trong
  cùng phiên, đúng yêu cầu "rủi ro cao phải verify kỹ"):** claim ở trên ("chưa có policy
  symlink tường minh") **không hoàn toàn đúng.** Đã đọc trực tiếp
  `crates/calm-server/src/tools/edit.rs::resolve_repo_path` (dòng 2138-2162, gọi từ CẢ
  `edit_lines_impl_gated` **và** `format_files_impl` — xác nhận qua `callers()`, không phải
  suy đoán): hàm này `std::fs::canonicalize()` full path (resolve **mọi** symlink component +
  `..`), rồi kiểm `real.starts_with(&root)` — từ chối nếu escape, message lỗi còn nói rõ
  "via `..` traversal or a symlink". Tức là: **ca tấn công chính** (symlink/`..` đưa write ra
  ngoài project root) **đã được chặn sẵn**, không phải một lỗ hổng đang mở.

  Phần **thật sự còn thiếu** (thu hẹp lại so với bản nháp gốc):
  1. **TOCTOU**: `canonicalize()` trả về một `real` path an toàn TẠI THờI ĐIỂM kiểm tra —
     không có gì ngăn một component trở thành symlink escape **sau** kiểm tra nhưng **trước**
     `File::open`/`rename()` thật trong `atomic_write`. Đây mới là chỗ pattern VHEATM
     `sandbox.py::SandboxExecutor.run()` (walk từng component, không canonicalize-rồi-tin) thực
     sự khác biệt — nhưng cần `openat2(RESOLVE_BENEATH)` (Linux-only) để đóng thật sự, không
     phải portable Rust std thuần; bản nháp gốc đã tự xếp đây là PR riêng, vẫn giữ nguyên
     quyết định đó.
  2. **Không có policy surface cấu hình được** — hành vi hiện tại thực chất là cứng
     `follow_internal_symlinks` (symlink được theo, chỉ từ chối nếu kết quả escape root),
     không có cách chuyển sang `reject_symlinks` (nghiêm hơn — từ chối mọi symlink dù không
     escape) hay `allow_external_symlinks_with_approval` (lỏng hơn, cần gắn elicitation —
     WS-2/human-approval, chưa có ở Phase 1).
  3. Hardlink không được xét riêng (rủi ro thấp hơn nhiều — không thể escape filesystem qua
     hardlink, `atomic_write`'s rename-based swap vẫn an toàn với nó).

  **Hệ quả cho ưu tiên**: task 3.3/3.4 vẫn đáng làm (defense-in-depth + policy configurability
  thật sự có giá trị, và là nền cho `allow_external_symlinks_with_approval` sau này gắn vào
  WS-2), nhưng **không còn là đóng một lỗ hổng đang mở** như mô tả gốc — mức độ khẩn cấp thấp
  hơn đáng kể so với cách đặt vấn đề ban đầu. Default mode khi wire (task 3.4) NÊN giữ
  `follow_internal_symlinks` để 0 thay đổi hành vi observable so với `resolve_repo_path` hiện
  tại — `path_policy` là một module tái cấu trúc + mở rộng policy surface của chính logic
  `resolve_repo_path` đã có, không phải thêm một lớp kiểm tra chồng lên nó.
- 2 call site cần đổi cùng lúc với `atomic_write` (giữ chữ ký tương thích ngược để không phải
  sửa cả hai cùng PR nếu không muốn): thực chất là **1** điểm nối, không phải 2 —
  `format_files_impl` (`edit.rs:533`) và `edit_lines_impl_gated` (`edit.rs:770`, `edit_symbol`
  dùng chung) **cả hai đều gọi chung `resolve_repo_path`** (xác nhận qua `callers()`) — wire
  `path_policy` vào đúng hàm đó là đủ cho cả hai, không cần sửa 2 nơi riêng biệt. (Số dòng
  574/1075 trong bản nháp gốc đã lệch so với HEAD hiện tại sau các edit WS-1 ở trên.)

### 3.2. Thiết kế cụ thể

**a) `evidence_digest` — module mới, không sửa `pipeline.rs`:**

```rust
// crates/calm-core/src/digest.rs (mới)
use sha2::{Digest, Sha256};

/// Trust-boundary content digest — SHA-256, dùng cho receipt/ledger/review-token
/// identity (WS-1/WS-2/WS-5). KHÔNG thay `hash_content` (FNV) — đó vẫn là stale-write
/// guard/cache key. Domain-separated bằng prefix để một SHA-256 dùng sai chỗ (vd. lẫn
/// với git blob hash) không âm thầm khớp.
pub fn evidence_digest(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("sha256:{:x}", hasher.finalize())
}
```
Chọn **SHA-256 qua `sha2`**, không phải BLAKE3: VHEATM's `provenance.py:sha256_digest` /
`tool_broker.py:_canonical_digest` / mọi schema (`^[a-f0-9]{64}$`) đều SHA-256. Nếu §4 kiến
trúc dual-authority (CQR/CER receipt băng qua ranh giới CALM↔VHEATM) thành hiện thực, digest
trùng thuật toán nghĩa là **không cần lớp dịch** ở biên. Nếu benchmark sau này cho thấy SHA-256
là bottleneck thật (không phải giả định), đổi sang BLAKE3 là một hàm nội bộ, không phải thay
đổi kiến trúc — nhưng đừng làm trước khi có số đo.
Kiểm tra `Cargo.toml` xem `sha2` đã là dependency chưa (nhiều khả năng có sẵn qua chain khác);
nếu chưa, thêm vào `calm-core/Cargo.toml`, không thêm `openssl` (tránh link C).

**b) `atomic_write` — sửa tại chỗ (không phải hub, caller_count 5, rủi ro thấp):**

Thay đổi tối thiểu, giữ chữ ký `pub fn atomic_write(path: &Path, content: &str) -> io::Result<()>`
để 2 call site không cần sửa; hành vi mới:
1. Tên temp: thay PID bằng nonce ngẫu nhiên đủ dài (không cần thêm crate `rand` — kết hợp
   `std::time::SystemTime::now()` nanosecond + một `static AtomicU64` counter trong process là
   đủ chống collision trong cùng tiến trình lẫn khác tiến trình cùng thời điểm).
2. Mở bằng `OpenOptions::new().write(true).create_new(true)` — đây **chính là `O_EXCL`** trên
   mọi platform std hỗ trợ, không cần syscall thô. Nếu `create_new` fail vì tên đã tồn tại
   (collision cực hiếm), retry với nonce mới, tối đa N lần trước khi trả lỗi.
3. Sau `rename()` thành công: `#[cfg(unix)]` mở `File::open(dir)` rồi `.sync_all()` — fsync thư
   mục cha. `#[cfg(not(unix))]` no-op (Windows không có khái niệm này qua std) — comment tại chỗ
   giải thích, khớp ghi chú "(nơi hỗ trợ)" trong master plan.
4. Thêm tham số mode (qua wrapper mới, không phá chữ ký cũ):
   ```rust
   pub enum WriteAssurance { Fast, HighAssurance }
   pub fn atomic_write_with(path: &Path, content: &str, assurance: WriteAssurance) -> io::Result<()>
   ```
   `atomic_write` cũ gọi `atomic_write_with(path, content, WriteAssurance::Fast)` để hành vi mặc
   định không đổi; `HighAssurance` biến permission-preservation-failure thành `Err` thay vì
   best-effort-bỏ-qua. WS-1's transaction commit path sẽ gọi `HighAssurance`.

**c) Path containment policy — module mới `crates/calm-core/src/path_policy.rs`:**

3 mode đúng master plan (`reject_symlinks` / `follow_internal_symlinks` /
`allow_external_symlinks_with_approval`), nhưng cách thực thi portable mượn **nguyên** pattern
đã đọc ở VHEATM `sandbox.py::SandboxExecutor.run()` (dòng 304-320 bản hiện hành): walk **từng
component** của path tương đối, `.is_symlink()` từng bước, từ chối nếu path rời khỏi root sau
`.resolve(strict=true)`. Trên Linux có thể nâng cấp lên `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)`
sau (cần crate `rustix` hoặc raw `libc` — quyết định thêm dependency này để riêng ở một PR nhỏ,
không bundle vào WS-3 chính, vì nó chỉ là hardening thêm trên nền non-Linux fallback đã đủ an
toàn).

### 3.3. Task breakdown (PR-sized)

| # | Task | File | Phụ thuộc |
|---|---|---|---|
| 3.1 | ~~`evidence_digest` + unit test collision-domain-separation~~ **XONG** | `crates/calm-core/src/digest.rs` | không |
| 3.2 | ~~`atomic_write_with` + `WriteAssurance`, giữ `atomic_write` tương thích~~ **XONG** | `crates/calm-core/src/edit.rs` | 3.1 |
| 3.3 | ~~`path_policy` 3-mode, portable component-walk~~ **XONG 2026-08-02** — `openat2` (Linux TOCTOU hardening) vẫn để PR riêng như dự định | `crates/calm-core/src/path_policy.rs` | không |
| 3.4 | ~~Wire `path_policy` vào `resolve_repo_path`~~ **XONG 2026-08-02** — 1 điểm nối duy nhất (không phải 2, xem §3.1), default `FollowInternalSymlinks`, 0 đổi hành vi observable (verify: mã lỗi/message giữ nguyên, `diff_impact` báo `signature_changed:false`, 943+294 test pass) | `crates/calm-server/src/tools/edit.rs::resolve_repo_path` | 3.3 |

### 3.4. Test

- `evidence_digest_is_stable_and_collision_domain_separated` (khác input → khác digest; cùng
  input → cùng digest; prefix `sha256:` cố định).
- `atomic_write_rejects_temp_name_collision_and_retries` (giả lập tên đã tồn tại).
- `atomic_write_high_assurance_surfaces_permission_failure` (chmod read-only owner, kỳ vọng
  `Err` ở `HighAssurance`, `Ok` best-effort ở `Fast`).
- Fault injection theo master plan §3 WS-3: symlink swap race (TOCTOU giữa check và write),
  `..` traversal, Unicode NFC/NFD trùng tên, case-insensitive collision (macOS/Windows),
  hardlink count>1, read-only fs, network fs (NFS — chỉ smoke test nếu CI có runner phù hợp).
- **Không đổi** test hiện có của `hash_content`/`atomic_write` cũ (giữ 100% backward compat).

### 3.5. Done
`evidence_digest` dùng được trong WS-1/WS-2; `atomic_write` mặc định hành vi cũ 100% (0 thay
đổi observable trừ tên temp file); `HighAssurance` mode có test riêng; path policy chặn được
toàn bộ fault-injection case trên.

---

## 4. WS-1 chi tiết — durable edit transaction + maintenance outbox

### 4.1. Khoảng trống so với schema DDL trong master plan

Schema `edit_transactions` trong master plan (§3 WS-1) có cột `state` với `CHECK (state IN (...))`
— đây là **validation shape**, không phải **transition graph**. Nếu chỉ có CHECK constraint,
không gì ngăn code ghi thẳng `state = 'DONE'` bỏ qua các bước trung gian. VHEATM's
`lifecycle.py::ALLOWED_TRANSITIONS` (dict tường minh, đã đọc lại ở HEAD hiện tại — không đổi so
với bản audit) là điểm khác biệt cốt lõi: **transition được validate qua một hàm duy nhất
(`transition()`), không có cách nào khác để đổi `state`.** Task breakdown dưới đây thêm phần
này mà master plan mới chỉ mô tả ở mức nguyên tắc ("Pattern VHEATM... state phải replay được").

Đồng thời thêm một bảng `tx_events` (không có trong DDL gốc của master plan) để mỗi transition tự
thân là event có thể replay — nếu không, "replay được" chỉ là lời hứa vì `edit_transactions.state`
đơn thuần là một cột `UPDATE`-able. Đây trực tiếp mượn `AuditLifecycle.events` +
`from_document()` (đã đọc `lifecycle.py:84-127`, không đổi so với bản audit).

### 4.1b. Đọc-verify SCIP overlay + embedding refresh (2026-08-02) — sửa lại §4 gốc

**Đây là vòng đọc bắt buộc trước khi viết code, theo đúng yêu cầu "rủi ro cao phải verify kỹ".**
Đã đọc trực tiếp phần còn lại của `edit_lines_impl_gated` sau `atomic_write` (`tools/edit.rs:
1075-1211`), `scip_overlay.rs::run_all_coalesced`/`run_one` (dòng 42-63, 314-338), và
`embedding.rs::embed_pending`/`embed_pending_chunks` (dòng 606-627, 683-703). Kết quả: **giả định
ban đầu ở §4.2/§4.4/§4.6 (job theo từng path) sai mô hình**, phải sửa trước khi implement.

**Những gì đọc được:**
1. Base reindex (`indexer::pipeline::reindex_paths`, dòng 1120-1124) là **đồng bộ**, chạy trong
   cùng call — đúng như master plan mô tả.
2. SCIP overlay refresh sau đó là `std::thread::spawn` **fire-and-forget trần** (dòng 1166-1173)
   gọi `crate::scip_overlay::run_all_coalesced(&root, &db)`. Hàm này **không nhận path** — nó
   re-scan *toàn bộ* overlay mỗi lần, coalesce bằng 2 `AtomicBool` module-level
   (`OVERLAY_IN_FLIGHT`/`OVERLAY_RERUN`, in-memory, mất khi process restart). Không có khái niệm
   "job cho path X" ở tầng này — đơn vị công việc là "một lượt overlay toàn repo", không phải
   from-file.
3. Background embedding (dòng 1189-1211) cũng `std::thread::spawn` trần, gọi
   `embedding::embed_pending`/`embed_pending_chunks` — hai hàm này **tự idempotent/resumable**:
   quét thẳng row còn thiếu embedding trong DB, gọi lại bao nhiêu lần từ bất kỳ đâu cũng an toàn,
   không cần biết "lần trước dừng ở đâu".
4. **Lỗ hổng durability thật sự** (đã có bằng chứng runtime, không phải suy đoán) nằm ngay tại
   comment ở `edit.rs:1152-1165`: *"Root cause of the formal tier silently dying after every
   CALM-tool edit (observed live 2026-07-10: 0 formal edges in a DB whose sidecar recorded 2863
   upgrades 30 minutes earlier)"*. Cơ chế `std::thread::spawn` hiện tại đã fix phần lớn triệu
   chứng đó, nhưng **không có bản ghi bền nào nói "còn nợ 1 lượt refresh"**: nếu process chết
   giữa lúc reindex commit và trước khi thread đó chạy xong (hoặc trước khi kịp spawn), formal-edge
   tier/embedding cứ stale tới khi có edit khác hoặc daemon restart — watcher tự nó không cứu
   được vì hash đã khớp nên `reindex_changed` thành no-op (chính comment tại chỗ xác nhận).

**Hệ quả cho thiết kế (sửa §4.2/§4.3/§4.4/§4.6 so với bản nháp trước):**
- `maintenance_jobs` phải là **singleton toàn cục theo `job_kind`** (`dedupe_key = job_kind`,
  không có path) — khớp đúng cơ chế coalesce-toàn-repo đã có sẵn, không phát minh lại per-path
  job mà 2 subsystem này không có.
- **Không đụng vào logic nội bộ** `scip_overlay.rs`/`embedding.rs` — cả hai đã đúng, idempotent,
  không cần viết lại. Việc cần làm chỉ là **bọc durable quanh lời gọi hiện có**: ghi 1 row
  `queued` (INSERT OR IGNORE theo dedupe_key — tự coalesce ở tầng DB, y hệt tinh thần
  `OVERLAY_IN_FLIGHT`) trước khi spawn thread, đánh dấu `done` sau khi thread chạy xong; ở startup
  quét mọi row còn `queued`/`running` từ phiên trước và kích lại đúng hàm hiện có
  (`run_all_coalesced`/`embed_pending`).
- **Tx lifecycle không nên block chờ 2 job này** — chúng không có cận trên thời gian rõ ràng
  (comment tại chỗ nói rust-analyzer batch run có thể mất "~20s"), nên `ProofsPending`/
  `EmbeddingsPending`/`VerifyPending` như một chuỗi tuần tự bên trong `edit_transactions.state`
  (bản nháp trước) là sai — một transaction "xong" (`Done`) ngay khi disk+base-index nhất quán và
  bền; durability của scip/embed refresh là mối quan tâm **tách biệt**, theo dõi qua
  `maintenance_jobs` global, không phải một trạng thái tx phải chờ.

### 4.1c. Đọc-verify `bootstrap()` (2026-08-02, trước khi implement task 4.6) — sửa lại §4.3/§4.5 gốc

**Vòng đọc bắt buộc thứ hai trong cùng phiên**, lần này cho "cái gì chạy lúc server khởi động",
trước khi viết `recover_and_rerun` như bản nháp §4.3 mô tả. Đã đọc trực tiếp
`crates/calm-server/src/lib.rs::bootstrap` (dòng 99-494) — hàm DUY NHẤT cả 3 launch path
(`calm-cli/src/main.rs::main`, `daemon.rs::serve_unix_daemon`, `lib.rs::serve_stdio_with_preset`)
gọi tới, xác nhận qua `callers()`.

**Phát hiện:** `bootstrap()` đã tự chạy MỘT LƯỢT SCIP-overlay + embedding ĐẦY ĐỦ, KHÔNG ĐIỀU KIỆN,
mỗi lần server khởi động thành công — không chỉ khi có job dang dở. Cụ thể: dòng 445-453 gọi
`bootstrap_embeddings()` (→ `embed_pending`/`embed_pending_chunks`) nếu `semantic.enabled`; dòng
455-463 gọi `scip_overlay::run_all()` nếu `index_ok` — cả hai chạy cho MỌI tiến trình thắng
`instance_lock` (tiến trình "thua" chỉ serve read-only, không tự chạy lượt này, nhưng nó cũng
không sở hữu `maintenance_jobs` ghi nhận job dang dở). Điều này **mạnh hơn** những gì
`recover_and_rerun` bản nháp §4.3 định làm (chỉ resume job còn 'queued'/'running').

**Hệ quả (sửa §4.3/§4.5/§4.6 so với bản nháp trước):**
- **Không viết `recover_and_rerun` gọi lại `run_all_coalesced`/`embed_pending` ở startup** — làm
  vậy là chạy trùng lặp (race) với chính lượt `bootstrap()` sắp tự chạy cho tiến trình sở hữu
  `instance_lock`, không giúp thêm gì. Thay bằng `maintenance::reconcile_stale_at_startup(conn)`:
  chỉ **sửa trạng thái** — mọi row còn 'queued'/'running' tại đúng thời điểm `new_with_preset` chạy
  chắc chắn thuộc về một tiến trình TRƯỚC đó (tiến trình hiện tại chưa kịp enqueue gì), nên chuyển
  thẳng sang 'failed' kèm `last_error` giải thích lý do — không tự sửa disk/index (đã có
  `bootstrap()` lo phần đó cho tiến trình sở hữu lock).
- **Điểm nối**: `CalmServer::new_with_preset` (`tools/common.rs`, gọi từ `bootstrap()` dòng 106) —
  hàm khởi tạo DUY NHẤT mọi launch path đi qua, chạy đúng một lần mỗi tiến trình, trước khi
  `bootstrap()` spawn indexer thread. `txn::recover_incomplete` chạy ở đây chỉ để LOG (không tự
  sửa `edit_transactions.state` — sai nếu đoán nhầm một tx `FileCommitted` thật ra đã ghi file
  thành công, chỉ chưa kịp `IndexCommitted`); `repair_consistency` (task 4.7) mới là nơi so khớp
  digest thật trên disk.
- `retry_maintenance(job_kind)` (task 4.7) là ngoại lệ được phép gọi lại refresh thật
  (`run_all_coalesced`/`embed_pending`) — đây là hành động **tường minh, hiếm, do người/agent yêu
  cầu**, không phải tự động ở startup, nên không có rủi ro chạy trùng với `bootstrap()`.

### 4.2. Schema (đã sửa theo phát hiện §4.1b)

```sql
-- Thêm vào run_migrations() theo đúng convention project: migrate_add_column cho cột đơn lẻ,
-- execute_batch cho bảng mới — theo mẫu graph_generation_state (schema.rs:147-151) và
-- external_proofs (schema.rs:107-125).

CREATE TABLE IF NOT EXISTS edit_transactions (
  tx_id        TEXT PRIMARY KEY,          -- ULID (time-sortable), KHÔNG content-addressed:
                                            -- một transaction là ý định hành động, chưa có nội
                                            -- dung cố định tại PREPARED. base_digest/
                                            -- proposed_digest mới là content-address (WS-3).
  project_id   TEXT NOT NULL,
  path         TEXT NOT NULL,
  base_digest       TEXT NOT NULL,        -- evidence_digest() của nội dung TRƯỚC edit
  proposed_digest   TEXT NOT NULL,        -- evidence_digest() của nội dung ĐỀ XUẤT
  review_token_id   TEXT,                  -- FK logic tới WS-2 token, nullable (self-commit tier)
  state        TEXT NOT NULL DEFAULT 'PREPARED',
  temp_path    TEXT,
  graph_generation_before INTEGER,
  graph_generation_after  INTEGER,
  created_at   REAL NOT NULL,
  updated_at   REAL NOT NULL,
  error_code   TEXT,
  error_detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_edit_transactions_state ON edit_transactions(state);
CREATE INDEX IF NOT EXISTS idx_edit_transactions_path ON edit_transactions(path);

-- Replay log — mirror AuditLifecycle.events. `state` ở bảng trên là CACHE của
-- "replay(tx_events WHERE tx_id=?) → state cuối", không phải nguồn sự thật độc lập.
CREATE TABLE IF NOT EXISTS tx_events (
  event_id     TEXT PRIMARY KEY,          -- "EVT-" || sha256(canonical(payload)) — mirror
                                            -- provenance.py::expected_journal_event_id
  tx_id        TEXT NOT NULL REFERENCES edit_transactions(tx_id) ON DELETE CASCADE,
  sequence     INTEGER NOT NULL,          -- 1-based, contiguous, unique per tx_id
  from_state   TEXT NOT NULL,
  to_state     TEXT NOT NULL,
  actor        TEXT NOT NULL,             -- "system" | "agent:<session_id>" | "human:<id>"
  reason       TEXT NOT NULL,
  occurred_at  REAL NOT NULL,
  UNIQUE(tx_id, sequence)
);

-- SỬA theo §4.1b: singleton toàn cục theo job_kind, KHÔNG path, KHÔNG tx_id bắt buộc —
-- một job bao phủ mọi tx đã commit từ lần chạy trước tới giờ, đúng mô hình
-- run_all_coalesced/embed_pending thật (re-scan toàn bộ, không phải theo-yêu-cầu-của-1-edit).
CREATE TABLE IF NOT EXISTS maintenance_jobs (
  job_id       TEXT PRIMARY KEY,
  job_kind     TEXT NOT NULL,             -- 'scip_refresh' | 'embed_refresh' (v1 — chỉ 2 kind
                                            -- này có lời gọi fire-and-forget cần bọc durable)
  dedupe_key   TEXT NOT NULL UNIQUE,      -- = job_kind — tối đa 1 job 'queued' mỗi kind tại một
                                            -- thời điểm (INSERT OR IGNORE tự coalesce, mirror
                                            -- OVERLAY_IN_FLIGHT nhưng bền qua restart)
  state        TEXT NOT NULL DEFAULT 'queued', -- 'queued' | 'running' | 'done' | 'failed'
  triggered_by_tx_id TEXT,                -- tx GẦN NHẤT làm job này queued lại — chỉ để chẩn
                                            -- đoán/log, KHÔNG phải quan hệ chặn (không FK CASCADE)
  attempts     INTEGER NOT NULL DEFAULT 0,
  available_at REAL NOT NULL,
  lease_owner  TEXT,
  lease_expires_at REAL,
  last_error   TEXT,
  last_completed_at REAL
);
CREATE INDEX IF NOT EXISTS idx_maintenance_jobs_available ON maintenance_jobs(state, available_at);
```

### 4.3. Module `crates/calm-core/src/txn.rs` — chữ ký cụ thể (đã sửa theo §4.1b)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    // SỬA theo §4.1b: bỏ ProofsPending/EmbeddingsPending khỏi chuỗi tx — chúng là job
    // toàn cục không cận trên thời gian, không phải bước tx phải chờ. VerifyPending giữ lại
    // làm điểm neo cho WS-6 (verification pipeline) sau này, nhưng ở Phase 1 chưa có gì tạo ra
    // nó — advance thẳng IndexCommitted -> Done là đường duy nhất thật sự dùng tới.
    Prepared, FileCommitted, IndexCommitted,
    VerifyPending, Done, Failed, RolledBack,
}

impl TxState {
    fn as_str(self) -> &'static str { /* "PREPARED" | ... */ }
    fn from_str(s: &str) -> Option<Self> { /* inverse */ }
}

/// Mirror lifecycle.py::ALLOWED_TRANSITIONS — nguồn sự thật DUY NHẤT cho transition hợp lệ.
/// Không có cách nào khác trong code base để đổi `edit_transactions.state`.
fn allowed_next(from: TxState) -> &'static [TxState] {
    use TxState::*;
    match from {
        Prepared        => &[FileCommitted, Failed],
        FileCommitted   => &[IndexCommitted, Failed, RolledBack],
        // Phase 1: IndexCommitted -> Done là đường thật sự dùng. VerifyPending chỉ tồn tại
        // làm chỗ neo transition hợp lệ cho WS-6 gắn vào sau — không ai advance tới nó ở Phase 1.
        IndexCommitted  => &[VerifyPending, Done, Failed],
        VerifyPending   => &[Done, Failed],
        Done | Failed | RolledBack => &[],
    }
}

pub struct EditTransaction { pub tx_id: String, pub state: TxState, /* ... */ }

pub fn begin(
    conn: &Connection, project_id: &str, path: &str,
    base_digest: &str, proposed_digest: &str,
) -> rusqlite::Result<EditTransaction> { /* INSERT edit_transactions + tx_events seq=1 (created→PREPARED) */ }

/// Chỉ hàm này được phép đổi `state`. Trả lỗi typed nếu transition không nằm trong
/// `allowed_next` — mirror LifecycleError.
pub fn advance(
    conn: &Connection, tx_id: &str, to: TxState, actor: &str, reason: &str,
) -> Result<(), TxnError> { /* validate allowed_next, ghi tx_events, UPDATE state cache */ }

/// Startup recovery: quét mọi tx chưa DONE/FAILED/ROLLED_BACK, đối chiếu digest thật trên disk.
pub fn recover_incomplete(conn: &Connection) -> rusqlite::Result<Vec<EditTransaction>> { }

/// Replay toàn bộ tx_events của một tx_id, trả state dẫn xuất — dùng để TỰ KIỂM TRA cột
/// `state` cache không bị lệch khỏi log (chạy trong CI/test, và trong `repair_consistency`).
pub fn replay_state(conn: &Connection, tx_id: &str) -> rusqlite::Result<TxState> { }
```

```rust
// crates/calm-core/src/maintenance.rs (mới, tách khỏi txn.rs — §4.1b: đây KHÔNG phải một
// bước trong vòng đời tx, là một concern độc lập).

/// Ghi (hoặc coalesce vào) một job 'queued' cho `kind`. INSERT OR IGNORE trên `dedupe_key = kind`
/// — nếu đã có job 'queued'/'running' cho kind này, gọi lại là no-op (đúng ngữ nghĩa
/// OVERLAY_IN_FLIGHT/OVERLAY_RERUN nhưng bền qua restart). Gọi TRƯỚC khi spawn thread hiện có.
pub fn enqueue(conn: &Connection, kind: MaintenanceKind, triggered_by_tx_id: Option<&str>) -> rusqlite::Result<()> { }

/// Đánh dấu job 'done' (hoặc 'failed' + last_error) sau khi thread hiện có chạy xong —
/// KHÔNG thay logic run_all_coalesced/embed_pending, chỉ bọc quanh lời gọi.
pub fn mark_completed(conn: &Connection, kind: MaintenanceKind, result: Result<(), &str>) -> rusqlite::Result<()> { }

/// SỬA theo §4.1c: KHÔNG gọi lại run_all_coalesced/embed_pending — bootstrap()
/// tự làm việc đó rồi cho tiến trình sở hữu instance_lock. Chỉ sửa trạng thái
/// mọi job còn 'queued'/'running' tại đúng lúc process này khởi động (chắc
/// chắn stale — process này chưa kịp enqueue gì) thành 'failed'.
pub fn reconcile_stale_at_startup(conn: &Connection) -> rusqlite::Result<Vec<MaintenanceJob>> { }

/// Explicit, người/agent yêu cầu — override cả job đang 'queued'/'running'.
/// Dùng bởi retry_maintenance (task 4.7), caller tự spawn refresh thật sau đó.
pub fn force_requeue(conn: &Connection, kind: MaintenanceKind) -> rusqlite::Result<()> { }
```

### 4.4. Tích hợp vào write path hiện có (đã sửa theo §4.1b)

Điểm nối chính xác (đã đọc dòng thật, 2026-08-02, kể cả phần sau `atomic_write` mà lần trước
chưa đọc — `tools/edit.rs:1075-1211`):

```
1. base_digest = evidence_digest(current_disk_bytes)   // đọc trước khi ghi
2. proposed_digest = evidence_digest(&new_content)
3. tx = txn::begin(conn, project_id, &path, &base_digest, &proposed_digest)?
4. atomic_write_with(&full_path, &new_content, WriteAssurance::HighAssurance)
     .map_err(|e| { txn::advance(conn, &tx.tx_id, Failed, "system", &e.to_string())?; e })?
5. txn::advance(conn, &tx.tx_id, FileCommitted, "system", "atomic_write succeeded")?
6. base reindex (đường hiện có tại dòng 1120-1124, synchronous) trong cùng DB transaction với
   bump graph_generation → txn::advance(.., IndexCommitted, ..)
7. txn::advance(.., Done, "system", "base index committed, disk+index consistent")? — KHÔNG
   chờ scip/embed refresh (§4.1b). tx coi là xong ở đây.
8. Trước khi spawn 2 thread hiện có (dòng 1166-1173 SCIP, dòng 1189-1211 embed):
   maintenance::enqueue(conn, ScipRefresh, Some(&tx.tx_id))? / enqueue(.., EmbedRefresh, ..)?
   — thread bên trong gọi maintenance::mark_completed(..) ở cuối, KHÔNG đổi logic
   run_all_coalesced/embed_pending.
9. Trả `tx_id` trong response (field mới, không phá schema cũ) — client có thể poll
   `edit_transaction_status(tx_id)` nếu cần.
```
`format_files_impl` (`edit.rs:574`) áp cùng pattern nhưng `action_class = semantics_preserving_transform`
(khớp taxonomy P0-3 trong adopt-plan §5) — verify_change tier có thể bỏ qua vì semantics-preserving.

### 4.5. MCP tool mới — **XONG 2026-08-02** (đăng ký trong `recover.rs`/`recover_tool_router`,
không cần toolset mới — router đó đã nằm trong preset `full` và toolset `recover` sẵn)

- `edit_transaction_status(tx_id)` — đọc `edit_transactions` qua `txn::get` (mới) +
  `txn::replay_state`; gợi `repair_consistency` nếu state = Failed.
- `maintenance_status()` — đọc `maintenance::all_jobs` (mới, không chỉ pending) —
  state/attempts/last_completed_at/last_error mỗi kind; gợi `retry_maintenance` nếu có job failed.
- `retry_maintenance(job_kind)` — **SỬA theo §4.1c**: `maintenance::force_requeue` (mới, override
  cả 'running') rồi gọi THẬT `run_all_coalesced`/`embed_pending` inline (không backgrounded) —
  đây là hành động tường minh do agent yêu cầu nên được phép chạy refresh thật, khác với
  startup hook (không bao giờ tự chạy refresh thật — xem dưới).
- `repair_consistency(path | tx_id)` — chạy `replay_state`, đối chiếu digest hiện tại trên disk
  với `proposed_digest`; nếu lệch → đánh dấu cần re-scan, không tự sửa im lặng. `path` resolve qua
  `txn::latest_for_path` (mới).
- Startup hook (`CalmServer::new_with_preset`, không phải daemon.rs) — **SỬA theo §4.1c**: gọi
  `recover_incomplete` (tx, chỉ LOG) + `maintenance::reconcile_stale_at_startup` (job, SỪA trạng
  thái → 'failed', KHÔNG gọi lại refresh thật — `bootstrap()` tự làm việc đó rồi), log qua
  `calm_audit` tracing target hiện có (`crates/calm-server/src/telemetry.rs`).

### 4.6. Task breakdown (PR-sized, sau khi WS-3 merge — đã sửa theo §4.1b)

| # | Task | Ghi chú |
|---|---|---|
| 4.1 | ~~Migration: `edit_transactions`, `tx_events`, `maintenance_jobs`~~ | **XONG 2026-08-02** |
| 4.2 | ~~`txn.rs`: `TxState` (7 state), `allowed_next`, `begin`, `advance`, `replay_state`, `recover_incomplete`~~ | **XONG 2026-08-02**, + `get`/`latest_for_path` thêm ở task 4.7 |
| 4.3 | ~~`maintenance.rs` MỚI: `enqueue`/`mark_running`/`mark_completed`/`pending_jobs`~~ | **XONG 2026-08-02** — không có `recover_and_rerun` như bản nháp gốc, xem §4.1c |
| 4.4 | ~~Wire `txn.rs` vào `edit_lines_impl_gated` + `format_files_impl` — shadow mode~~ | **XONG 2026-08-02** |
| 4.5 | ~~Wire `maintenance.rs` quanh 2 lời gọi `std::thread::spawn` hiện có~~ | **XONG 2026-08-02** |
| 4.6 | ~~`recover_incomplete` (tx, log-only) + `maintenance::reconcile_stale_at_startup` (job, SỪA theo §4.1c) + startup hook trong `CalmServer::new_with_preset`~~ | **XONG 2026-08-02** — test `startup_hook_reconciles_a_stale_maintenance_job_left_by_a_previous_process` (tools.rs) thay cho kill -9 thật, cùng lý do đã nêu ở mục 3 §8 |
| 4.7 | ~~MCP tools: `edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`~~ | **XONG 2026-08-02** — `recover.rs`/`recover_tool_router`, 7 test mới trong `tools.rs`, 4 toolsnap mới commit cùng |
| 4.8 | Chuyển từ shadow → dual-read (status tool đọc journal thật) → enforce (write path fail nếu `txn::begin` lỗi) | **CHƯA LÀM** — milestone gate riêng, xem §6; cần quyết định riêng về lifecycle enforce trước khi làm, không phải incremental nữa |
### 4.7. Test (fault injection — mở rộng, không thay master plan)
Ngoài danh sách master plan đã nêu (kill mọi transition, corrupt temp, disk-full, SQLite busy,
permission fail, watcher race, provider timeout), thêm cụ thể:
- `advance_rejects_out_of_order_transition` (vd. PREPARED → DONE thẳng phải lỗi).
- `tx_events_replay_matches_cached_state_after_every_fixture` — chạy trên toàn bộ fixture
  test hiện có của `edit_lines`/`edit_symbol`/`format_files`, không chỉ test mới.
- `recover_incomplete_finds_tx_left_at_file_committed_after_kill_-9`.

---

## 5. WS-2 chi tiết — state-bound review token

### 5.1. Trạng thái hiện tại (đọc trực tiếp, 2026-08-02)

- `FRESHNESS_WINDOW_CALLS: u64 = 200` — `crates/calm-server/src/tools/edit.rs:929`. Freshness
  đo bằng **số lần gọi tool trong session** (`self.session_tool_calls()`), không bind digest.
- `cites_token(reason, needle)` — `crates/calm-server/src/tools/edit.rs:2084-2105`. Word-boundary
  substring match, xác nhận đúng A02: bất kỳ token nào đứng riêng (không dính chữ khác) trong
  `reason` là pass, không chứng minh gì về việc caller thật sự được đọc.
- Nhánh "any non-empty reason passes" khi `known_caller_qns.is_empty()` — nêu trong cả hai plan
  doc ở `edit.rs:995-996`/`:990-1005`; đây là dòng gọi `cites_token` trong gate logic quanh
  `FRESHNESS_WINDOW_CALLS` (929-962), không phải định nghĩa hàm (2084) — hai vị trí khác nhau
  trong cùng file, không mâu thuẫn với trích dẫn gốc.

### 5.2. Thiết kế — mượn trực tiếp `tool_broker.py` (đã đọc lại HEAD hiện tại, không đổi)

Khác với hai plan doc mô tả (đúng nhưng ở mức khái niệm), đây là ánh xạ **hàm-đối-hàm** cụ thể
sau khi đọc `tool_broker.py`:

| VHEATM (`tool_broker.py`, dòng hiện tại) | CALM tương đương cần viết |
|---|---|
| `request_digest()` dòng 32-35 — digest TOÀN BỘ request | `review_scope_digest()` — digest toàn bộ `ReviewTokenPayload` trước khi ký |
| `action_digest()` dòng 38-42 — digest phần "operational fields", tách khỏi requester/request_id | Không cần tách ở CALM (chưa multi-tenant) — có thể bỏ qua bước này ở v1 |
| `expected_approval_token_id()` dòng 45-47 — `"APR-" + sha256(unsigned_fields)` | `expected_review_token_id()` — `"RVT-" + evidence_digest(unsigned_fields)` |
| `_verify_approval()` dòng 413-456 — check requester/tool_class/exact_scope/request_digest khớp, timestamp hợp lệ, HMAC verify, token_id khớp lại | `verify_review_token()` — check target_path/target_digest/graph_generation/caller_set_digest khớp state THẬT tại thời điểm `commit_edit`, không chỉ tại thời điểm phát hành |
| `TokenLedger`/`InMemoryTokenLedger`/`DirectoryTokenLedger` (single-use, dòng 116-146) | `consume_review_token(tx_id, token_id)` — dùng chính bảng SQLite (WS-1 đã có `edit_transactions.review_token_id`), UNIQUE constraint đóng vai trò ledger, không cần file riêng |
| `_validate_configuration()` dòng 458-478 — fail-closed **ở constructor** | `ReviewTokenIssuer::new()` từ chối khởi tạo nếu risk-tier policy thiếu approver-class cho Critical |

### 5.3. `ReviewTokenPayload` — chữ ký cụ thể (Rust, khớp field đã có sẵn để tận dụng)

```rust
pub struct ReviewTokenPayload {
    pub token_id: String,                 // "RVT-<sha256 hex>", tính SAU khi có đủ field còn lại
    pub target_path: String,
    pub target_digest: String,            // evidence_digest() — WS-3
    pub target_symbol_ids: Vec<String>,
    pub source_range_digest: String,
    pub graph_generation: i64,            // đọc từ graph_generation_state (đã có, schema.rs:147)
    pub caller_set_digest: String,        // evidence_digest(sorted qualified_names đã trả về)
    pub evidence_policy_digest: String,
    pub provider_health_digest: String,   // từ snapshot đa chiều (WS-7, tạm placeholder nếu WS-7 chưa xong)
    pub risk_level: String,               // low|medium|high|critical — tái dùng phân loại risk hiện có
    pub required_approver_class: String,  // self|structured|independent_human|human_plus_verification
    pub issued_at: f64,
    pub expires_at: f64,
    pub nonce: String,
}
```
`commit_edit`/`edit_lines_impl_gated` verify **lại** `graph_generation` và `caller_set_digest`
so với state hiện tại (không phải state lúc `prepare_edit`/`edit_context` phát token) — đây
chính là đóng TOCTOU §3.2 kiến trúc dual-authority của adopt-plan, áp dụng nội bộ trong CALM
trước khi có VHEATM ở đầu kia.

### 5.4. `acknowledged_positions` thay `cites_token` làm đường chính

`edit_context` trả về danh sách evidence **có index** (caller list đã đánh số). Gate mới chấp
nhận `acknowledged_positions: Vec<u32>` — vị trí thực sự được agent trích dẫn trong structured
evidence (không phải substring reason tự do). `cites_token` **giữ làm fallback tương thích
ngược** cho client cũ chưa gửi `acknowledged_positions` — nhưng nhánh `known_caller_qns.is_empty()
⇒ auto-pass` (A02's lỗ hổng chính) phải đổi thành: caller-set rỗng vẫn bắt buộc
`acknowledged_positions` khớp `caller_set_digest = evidence_digest(empty_set)` tường minh —
không còn "reason không rỗng là đủ".

### 5.5. Approval-principal tier (mượn thẳng từ `_evaluate_write`/policy tổng của VHEATM)

| risk_level | required_approver_class | Điều kiện auto +1 tier (mượn ý "reference monitor preflight failed" của `sandbox.py::_probe`) |
|---|---|---|
| low | self | — |
| medium | structured (evidence có `acknowledged_positions` khớp) | provider degraded → medium |
| high | independent (human hoặc policy-bot khác agent) | graph incomplete → high |
| critical | human + verification suite (WS-6, chưa có ở Phase 1 — tạm chặn cứng: critical luôn cần `elicit_hub_confirm=true` cho tới khi WS-6 xong) | luôn +1 nếu provider_health_digest bất thường |

### 5.6. Task breakdown (PR-sized, sau khi WS-1 merge — cần `graph_generation`/`tx_id` tồn tại)

| # | Task |
|---|---|
| 5.1 | `ReviewTokenPayload` struct + `expected_review_token_id` + `review_scope_digest` |
| 5.2 | `edit_context` trả evidence có index + `query_receipt_id` tạm thời (chưa cần CQR đầy đủ của WS-5, chỉ cần digest ổn định) |
| 5.3 | Gate mới nhận `acknowledged_positions`, giữ `cites_token` fallback song song (2 nhánh, log riêng tỷ lệ dùng nhánh nào) |
| 5.4 | `verify_review_token` — re-check `graph_generation`/`caller_set_digest` tại `commit_edit`, không chỉ tại phát hành |
| 5.5 | Approval-tier bảng §5.5, wire `elicit_hub_confirm` bắt buộc cho `critical` |
| 5.6 | Xoá nhánh `known_caller_qns.is_empty() ⇒ auto-pass`, thay bằng acknowledgment tường minh cho caller-set rỗng |

### 5.7. Test
- `empty_caller_set_still_requires_positional_acknowledgment` (nêu trong cả hai plan doc — giữ nguyên tên).
- `token_rejected_when_graph_generation_changed_between_issue_and_commit`.
- `token_rejected_when_replayed` (dùng lại `edit_transactions.review_token_id` UNIQUE làm ledger).
- `acknowledgment_binds_to_returned_set_digest`.
- `critical_risk_without_independent_approver_is_blocked_not_auto_passed`.

---

## 6. Milestone gate "Write-Safety Beta" — tiêu chí đo được (cụ thể hoá master plan §5/§6)

Master plan đặt gate này ở 08–10/2026 với mô tả định tính ("crash suite 0 divergence không-
journal; mọi high-risk edit dùng state-bound token"). Cụ thể hoá thành checklist pass/fail:

- [x] **XONG 2026-08-02.** Crash-injection suite (kill -9 tại mọi `TxState` Phase 1 thật sự đạt
      tới — `Prepared`/`FileCommitted`/`IndexCommitted`; `VerifyPending`/`RolledBack` không
      producer/caller thật nào trong Phase 1, xem §4.6 task 3 giải thích) chạy 100 lần mỗi
      transition trên Linux CI (`ci.yml` job `txn-crash-injection`), 0 trường hợp disk thay đổi
      mà không có `tx_events` row tương ứng —
      `crates/calm-cli/tests/txn_crash_injection.rs` + `src/bin/txn_crash_harness.rs`.
- [x] **XONG 2026-08-02** (docs/plans/2026-08-02-toolsurface-writesafety-ledger-research.md#2.3,
      #2.6). `replay_state(tx_id)` khớp `edit_transactions.state` cache cho 100% tx trong toàn bộ
      test suite hiện có — `shadow_tx_replay_state_matches_cached_state_across_edit_lines_edit_symbol_and_format_files`
      (`crates/calm-server/src/tools.rs`) chạy cả 3 write path thật (edit_lines/edit_symbol/
      format_files) trên 1 DB chung rồi assert cho MỌI tx_id được tạo ra, không chỉ 1 kịch bản.
- [x] **XONG 2026-08-02** (docs/plans/2026-08-02-ws1-enforce-and-critical-risk-execution-plan.md
      §2 "Change B"). `txn::begin` failure giờ **abort thẳng write attempt** thay vì tiếp tục im
      lặng, ở cả `edit_lines_impl_gated` (mã lỗi mới `TRANSACTION_INIT_FAILED`) lẫn
      `format_files_impl` (per-file `status: "error"`, không abort cả batch). Test:
      `edit_lines_aborts_when_txn_begin_fails`,
      `format_files_skips_one_file_when_txn_begin_fails_without_aborting_the_batch` (cả hai ép lỗi
      thật bằng cách chmod read-only file DB, không phải giả lập). Phạm vi cố ý hẹp hơn hình dung
      gốc của master plan (chỉ enforce ở bước `begin`, không phải toàn bộ state machine) — xem lý
      do ở execution plan §2.2. Các `advance()` sau `begin` vẫn non-blocking như cũ (disk đã đổi,
      "rollback" lúc đó rủi ro hơn lỗi đang cố sửa) — `needs_repair` hint UX cho case đó **cố ý
      chưa làm** (nice-to-have, không phải điều kiện gate).
- [x] **XONG 2026-08-02** (docs/plans/2026-08-02-ws1-enforce-and-critical-risk-execution-plan.md
      §1 "Change A", đã sửa lại §0 tài liệu đó: elicitation veto **đã** áp dụng cho mọi
      `risk=="high"` khi elicitation cấu hình từ trước — lỗ hổng thật chỉ nằm ở nhánh
      `ElicitGate::Off`, hẹp hơn phác thảo ban đầu). `risk=="high"` + không có elicitation cấu
      hình → **block cứng** (mã lỗi mới `HIGH_RISK_REQUIRES_INDEPENDENT_REVIEW`), kể cả khi
      `confirm:true` + `reason` cite đúng 1 caller thật (case trước đây pass được). Xác nhận
      `bridge_downgrade_eligible` **không thể** đồng thời true với `risk=="high"` (đọc trực tiếp
      `edit.rs:942-943`: `risk.as_deref() != Some("high")` là một phần điều kiện của chính nó) —
      không có xung đột với nhánh bridge-downgrade. Test:
      `high_risk_edit_off_elicitation_is_blocked_even_with_confirm_and_grounded_reason`,
      `high_risk_edit_can_pass_via_elicitation_ask_then_approved` (round-trip Ask→Approved). Không
      phát minh tier "critical" mới — tái dùng `risk=="high"` đã có sẵn, đúng khuyến nghị
      toolsurface-writesafety-ledger-research.md §2.5.
- [x] **XONG (đã đạt từ trước, tick lại 2026-08-02).** `atomic_write` mặc định (`Fast` mode) — 0
      thay đổi hành vi observable, xác nhận bởi toàn bộ 945 test `calm-core` + 301 test
      `calm-server` pass không đổi; `HighAssurance` mode có test riêng
      (`atomic_write_high_assurance_surfaces_permission_failure`) chứng minh nó surface lỗi mà
      `Fast` nuốt.
- [ ] **ĐO LẠI 2026-08-02 (sau Tier 1+2), VẪN KHÔNG ĐẠT nhưng đã tiến rất gần.** Tier 1 (gộp
      connection) + Tier 2 Option A+B (docs/plans/2026-08-02-shadow-txn-connection-consolidation-
      plan.md §3/§5) đều đã IMPLEMENT. Tier 2: `txn::advance` giờ gộp `ledger::append` vào CHÍNH
      transaction của nó qua `SAVEPOINT` (thay vì commit riêng thứ 2) — cắt mỗi `advance()` từ 2
      commit xuống 1, không đổi granularity state machine; `txn::advance_many` (mới) batch cùng
      một state transition qua nhiều tx_id độc lập vào 1 transaction, dùng trong
      `format_files_impl`'s final advance phase. **Cố tình KHÔNG** gộp 2 state khác nhau (vd
      IndexCommitted+Done) của CÙNG 1 tx_id — phát hiện qua đọc lại `txn_crash_injection.rs`: làm
      vậy sẽ phá guarantee mà criterion 1 (crash-injection suite) đang test (`IndexCommitted` sẽ
      không còn là checkpoint durable độc lập được nữa). Đo lại đúng phương pháp cũ (`git worktree`
      tại `acf2793`, N=200, 3 lần chạy mỗi bên, **đo cả baseline lẫn Tier1+2 lại từ đầu trong cùng 1
      khung thời gian** để tránh lệch do tải máy trôi dạt — bài học rút ra: tái sử dụng số baseline
      cũ từ lần đo Tier 1 sẽ cho kết quả sai lệch, vì tải máy đã đổi giữa 2 lần đo dù cùng 1 commit):
      baseline mới p50 ≈31.4ms (34.96/30.91/28.33), p95 ≈53.6ms (58.51/57.04/45.27). Sau Tier 1+2:
      p50 ≈36.16ms (34.53/38.33/35.63), p95 ≈65.8ms (71.57/61.67/64.06). **Overhead: p50 ~+15%, p95
      ~+23%** — giảm mạnh từ mức ~+41%/+41% của riêng Tier 1 (và ~+31-35%/+50-100% trước Tier 1),
      xác nhận giả thuyết số lượng commit (9→6 commit/edit) là yếu tố chính. **Vẫn chưa đạt ngưỡng
      ≤10%** nhưng đã tiến sát.

      **ĐÀO SÂU TIẾP 2026-08-02 (lần 4) — đã tìm ra nguyên nhân chính xác của phần overhead còn
      lại, không còn là ẩn số.** Viết probe thứ 2 (component-breakdown, cùng kỷ luật thêm-đo-xoá) đo
      riêng từng phase trong đúng chuỗi `edit_lines_impl_gated` thực hiện. Phát hiện: `reindex_paths`
      một mình chiếm avg 112.8ms/94.8% tổng — nhưng min=20.3ms, max=**17.97 GIÂY** (1 lần index đầu
      tiên của file mới trong DB trống làm lệch trung bình hoàn toàn; steady-state thực tế chỉ
      ~20-25ms/lần). Đây chính là lý do `mean` (~120-130ms) luôn cao hơn hẳn `p50`/`p95` (~30-70ms)
      ở MỌI lần đo trước đó — không phải do background thread contention như đoán ban đầu, mà do
      đúng 1 outlier khổng lồ này (percentile của 200 mẫu miễn nhiễm với 1 giá trị bất thường, mean
      thì không). Với steady-state đã tách outlier: `reindex_paths` (~20-25ms) vốn ĐÃ là chi phí
      chính của 1 lần edit, kể cả TRƯỚC WS-1 (WS-1 không đụng vào reindex_paths) — nó giải thích cho
      p50 baseline ~28-31ms, KHÔNG PHẢI phần overhead WS-1 thêm vào. Phần overhead thật sự = toàn bộ
      phần còn lại: `open_writer` ≈1.2ms + `txn::begin` ≈1.5ms + `advance→FileCommitted` ≈0.5ms +
      `maintenance::enqueue` ≈0.1ms + `advance→IndexCommitted+Done` ≈0.8ms ≈ **~4.1ms/edit** —
      4.1ms trên baseline ~28ms = **~+14.6%**, khớp gần như chính xác với con số đo được ~+15%.
      **Phần chênh lệch không còn là bí ẩn — chính xác là ~4ms này.** Điều này đồng thời loại (c):
      `maintenance::enqueue` đo thực tế chỉ ~0.1ms, không đáng kể; và giới hạn trần cho (b) Tier 3:
      toàn bộ `advance()` (gồm cả ledger append) đã đo dưới 1ms, nên bỏ SELECT `head_digest` chỉ có
      thể tiết kiệm một phần rất nhỏ của mức dưới-1ms đó — khó có khả năng đủ để đạt ngưỡng ≤10%.
      **Kết luận thực tế:** ~15%/~23% overhead giờ đã được quy về chính xác, không còn lever rủi ro
      thấp nào khác trong phạm vi đã khảo sát có thể đóng nốt khoảng cách này. Còn lại 2 lựa chọn
      (chưa quyết trong phiên này, là quyết định của người sở hữu milestone, không phải code
      change): (i) chấp nhận ~15% là sàn thực tế cho tính năng durability/audit-trail này ở mức chi
      phí baseline hiện tại, hoặc (ii) xem lại chính ngưỡng ≤10% — ngưỡng này tương đương ngân sách
      tuyệt đối chỉ ~2.8ms (10% của baseline ~28ms) cho TOÀN BỘ đường ghi journal+ledger mỗi edit,
      một con số rất chặt cho một audit trail durable, hash-chained, crash-recoverable, giờ đã đo
      được thay vì chỉ giả định.

      Chi tiết đầy đủ + toàn bộ test mới (3 test cho savepoint invariant + advance_many) + số liệu
      component-breakdown: xem "Tier 2 implementation & measurement results" và "Remaining-gap root
      cause" trong file plan đó.

Bench script đã dùng (không giữ lại trong test suite — thêm tạm, đo, rồi xoá cả 2 bên mỗi lần):
N=200 lần `edit_lines` liên tiếp (đổi số trả về tăng dần trên cùng 1 dòng mỗi lần), đo
`std::time::Instant` quanh mỗi lời gọi `server.edit_lines(...)` in-process (không qua MCP
JSON-RPC), sort rồi lấy phần tử tại vị trí p50/p95. Worktree baseline tại `acf2793` (commit ngay
trước `7b65acb`, commit đầu tiên thêm `txn.rs`), build+chạy độc lập, không đụng working tree chính
đang có các thay đổi chưa commit của phiên này. Confound đã ghi nhận: mean (~120-130ms) cao hơn
nhiều so với p50 (~30-45ms) cả 2 bên — nhiều khả năng do background thread `run_all_coalesced`
(scip-overlay, default-on feature) chạy 200 lần chồng lấn trong ~25s tranh CPU với phép đo — ảnh
hưởng đều cả 2 bên nên p50 vẫn là số đáng tin nhất, p95 nên coi là nhiễu cho tới khi hiểu rõ hơn
tương tác này.

Chỉ khi cả 6 mục pass mới coi Phase 1 xong và mở khoá WS-4 (provider sandbox, giai đoạn kế tiếp
theo roadmap master plan §5).

---

## 7. Rủi ro triển khai riêng cho Phase 1 (bổ sung master plan §6)

- ~~`maintenance_jobs` outbox đụng vào code refresh async hiện có mà tài liệu này CHƯỌC đọc chi
  tiết~~ — **ĐÃ ĐỌC 2026-08-02, xem §4.1b.** Rủi ro thực tế thấp hơn dự đoán ban đầu: cả
  `run_all_coalesced` lẫn `embed_pending`/`embed_pending_chunks` đều không cần sửa nội bộ,
  chỉ cần bọc durable quanh lời gọi hiện có. Rủi ro còn lại chuyển sang: (a) `enqueue`/
  `mark_completed` phải thật sự race-safe dưới nhiều edit đồng thời (INSERT OR IGNORE +
  UNIQUE(dedupe_key) là đủ hay cần transaction riêng — cần test concurrent), (b)
  `recover_and_rerun` ở startup gọi `run_all_coalesced`/`embed_pending` thật — nếu repo lớn,
  việc này có thể kéo dài startup time, cần đo thời gian thật trước khi coi là an toàn chạy
  đồng bộ ở startup hay phải background-off.
- **Shadow-mode có thể che giấu lỗi thật nếu không có alerting.** `txn::begin`/`advance` lỗi
  trong shadow mode theo thiết kế **không được** chặn write — nhưng phải log đủ để không âm
  thầm mất transaction trong nhiều tuần trước khi ai đó nhìn lại.
- **WS-2 phụ thuộc `graph_generation` (đã có, schema.rs:147) nhưng chưa phụ thuộc snapshot đa
  chiều (WS-7, P1)** — `provider_health_digest` trong `ReviewTokenPayload` sẽ tạm là digest rỗng/
  placeholder cho tới khi WS-7 tồn tại. Ghi rõ trong code (comment + có thể một `TODO(WS-7)`
  tường minh) để không ai tưởng field này đã có ý nghĩa thật.
- **3 PR tuần tự cùng chạm `tools/edit.rs`** — rủi ro merge-conflict nếu có PR khác đang sửa
  file này song song trong lúc triển khai; khuyến nghị thông báo trước cho người maintain khác
  (nếu có) trước khi bắt đầu 4.4/5.3 (đã đổi số task theo §4.6 mới).

---

## 8. Việc cần làm ngay (tuần đầu tiên)

1. ~~Merge WS-3 task 3.1+3.2~~ — **XONG 2026-08-02**: `crates/calm-core/src/digest.rs` +
   `WriteAssurance`/`atomic_write_with` trong `edit.rs`, 904/904 test pass, clippy `-D warnings`
   sạch, `cargo fmt` sạch. Task 3.3/3.4 (`path_policy` + wire 2 call site) CHƯA làm — việc kế
   tiếp hợp lý nếu muốn xong trọn WS-3 trước khi sang WS-1.
2. ~~Đọc chi tiết `scip_overlay.rs`/embedding refresh path trước khi viết task 4.5~~ — **XONG
   2026-08-02, xem §4.1b.** Kết luận: schema `maintenance_jobs` đổi từ per-path sang per-kind
   singleton; `TxState` bỏ `ProofsPending`/`EmbeddingsPending` khỏi chuỗi tx. §4.2-§4.6 đã cập
   nhật theo phát hiện này.
3. ~~Viết fixture crash-injection harness...~~ — **XONG 2026-08-02 (DB-state only), sau đó
   nâng lên I/O thật XONG 2026-08-02 (task 4.8, xem dưới).** Giai đoạn đầu: thay OS-level
   `SIGKILL` subprocess (như `sigterm_shutdown.rs`) bằng test atomic-rollback thật
   (`begin_is_atomic_with_its_seq_1_event` ép `ROLLBACK` giữa 2 câu lệnh) + test
   interrupted-sequence (`replay_state_reflects_an_interrupted_sequence`,
   `pending_jobs_finds_a_job_stuck_at_running_after_a_simulated_crash`) — đủ để verify
   `txn.rs`/`maintenance.rs` thuần DB-state, chưa chạm I/O thật.

   **Task 4.8 (crash-injection suite thật, I/O + SIGKILL) — XONG 2026-08-02:**
   `crates/calm-cli/src/bin/txn_crash_harness.rs` (bin mới, không ship trong `release.yml`) —
   subprocess thật chạy 1 chu trình `txn::begin` → `atomic_write` thật → `advance(FileCommitted)`
   → `advance(IndexCommitted)` (giả lập, không chạy reindex thật — xem doc comment giải thích tại
   sao) → `advance(Done)`, tự `libc::raise(SIGKILL)` ngay sau bước được chỉ định qua
   `--crash-after` (self-raise xác định, không phải kill từ ngoài theo timing — loại bỏ hoàn toàn
   race điều kiện timing). `crates/calm-cli/tests/txn_crash_injection.rs` là driver: verify cho
   mỗi lần crash — (a) process thật sự chết bởi SIGKILL, (b) disk khớp chính xác với những gì
   crash point đó ngụ ý, (c) `edit_transactions.state` (cache) == `txn::replay_state` (derive
   thuần từ `tx_events`), (d) disk đổi ⇒ luôn có `tx_events` row `FILE_COMMITTED` tương ứng
   (đúng invariant cốt lõi milestone gate đòi), (e) `txn::recover_incomplete` tìm thấy transaction
   này — nối trực tiếp bằng chứng của suite này với chính startup-recovery hook đã wire trong
   `common.rs::new_with_preset` phiên này.

   3 transition thật sự có thể đạt tới trong Phase 1 (`Prepared`/`FileCommitted`/
   `IndexCommitted` — `VerifyPending` chưa có producer tới WS-6, `RolledBack` không caller thật
   nào request), mỗi transition test **100 lần thật** (bug tìm thấy giữa chừng: DB path ban đầu
   chỉ khoá theo iteration, không theo step, khiến 2 crash-point khác nhau share nhầm cùng 1 DB
   file — sửa bằng cách khoá theo `{step}-{iteration}`). Biến thể `#[ignore]` (300 subprocess
   spawn thật, ~13s, quá chậm cho `cargo test --workspace` thường xuyên) + biến thể nhanh 5
   lần/transition luôn chạy trong suite thường. Job CI riêng `txn-crash-injection` mới trong
   `ci.yml` chạy biến thể `--ignored` trên mọi push/PR vào main.
   Sau đó **ĐÃ LÀM hết task 4.1-4.4 trong §4.6**: migration (`edit_transactions`, `tx_events`,
   `maintenance_jobs`) + `txn.rs` (`TxState` 7-state, `begin`/`advance`/`replay_state`/
   `recover_incomplete`) + `maintenance.rs` (`enqueue`/`mark_running`/`mark_completed`/
   `pending_jobs`, UPSERT atomic sau khi 1 test tự viết bắt được bug coalesce-vĩnh-viễn) + wire
   **shadow-mode** vào `edit_lines_impl_gated` thật (`tools/edit.rs`) — mọi lời gọi `txn::`
   bọc `if let Ok(..)`/`let _ =`, không bao giờ đổi kết quả edit thật. Verify: 919/919 test
   `calm-core` + 286/286 test lib `calm-server` + 3/3 `watcher_integration` (bao gồm
   `concurrent_edit_write_and_watcher_reindex_does_not_lock_or_go_stale` — đúng bài lo nhất)
   đều pass, clippy `-D warnings` sạch, `cargo fmt` sạch. ~~`format_files_impl` (edit.rs:574)
   CHƯA wire — việc kế tiếp. `maintenance.rs` CHƯA wire quanh 2 spawn thật~~ — **XONG 2026-08-02**
   (task 4.4b/4.5, cùng bài verify trên: 926/926 + 294/294 + 3/3, clippy/fmt sạch).
4. ~~Chốt quyết định SHA-256-qua-`sha2`~~ — **XONG**: `sha2 = { workspace = true }` đã tồn tại
   sẵn trong `calm-core/Cargo.toml` (thêm trước đó cho `memory.rs`'s HMAC integrity feature) —
   không cần thêm dependency mới, không cần quyết định lại.
5. ~~Startup recovery hook (task 4.6) + 4 MCP tool mới (task 4.7)~~ — **XONG 2026-08-02.** Đọc
   lại `crates/calm-server/src/lib.rs::bootstrap` trước khi viết (xem §4.1c) — phát hiện nó đã tự
   chạy 1 lượt SCIP-overlay + embedding đầy đủ mỗi lần khởi động cho tiến trình sở hữu
   `instance_lock`, nên `recover_and_rerun` gọi lại refresh thật ở bản nháp gốc là dư/trùng lặp.
   Thay bằng `maintenance::reconcile_stale_at_startup` (chỉ sửa trạng thái 'queued'/'running' →
   'failed', không tự chạy refresh) + `txn::recover_incomplete` (chỉ log) trong
   `CalmServer::new_with_preset` — điểm khởi tạo duy nhất mọi launch path đi qua. 4 MCP tool mới
   (`edit_transaction_status`, `maintenance_status`, `retry_maintenance`, `repair_consistency`)
   thêm vào `recover.rs`/`recover_tool_router` (đã có sẵn trong preset `full`, không cần đăng ký
   toolset mới); `retry_maintenance` là nơi DUY NHẤT được phép gọi lại refresh thật (hành động
   tường minh do agent yêu cầu). Verify: 926/926 test `calm-core` + 294/294 test lib
   `calm-server` (thêm 7 test tool mới + 1 test startup-hook 2-tiến-trình-giả-lập) + 3/3
   `watcher_integration` pass, clippy `-D warnings` sạch, `cargo fmt` sạch, 4 file toolsnap mới
   (`crates/calm-server/src/__toolsnaps__/{edit_transaction_status,maintenance_status,
   retry_maintenance,repair_consistency}.snap`) tự sinh + đã soát qua. WS-1 coi như xong hẳn theo
   §4.6 task 4.1-4.7 (chỉ còn task 4.8 — chuyển shadow→enforce, milestone gate riêng §6, KHÔNG
   làm trong đợt này). Việc kế tiếp hợp lý: WS-3 task 3.3/3.4 (`path_policy`, độc lập với WS-1) —
   rủi ro cao hơn (đổi hành vi write path thật nếu wire sai default mode) nên cần verify kỹ
   trước khi wire vào 2 call site thật, theo đúng yêu cầu ban đầu.