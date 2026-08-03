---
title: "WS-6 Execution Plan — first slice: opt-in Rust semantic verification, VERIFY_PENDING made real"
date: 2026-08-03
status: plan + implemented (same session)
scope: turn `TxState::VerifyPending` from "a legal but never-produced transition"
  into a real, working (if narrow) path, following the exact discipline this repo
  already used for WS-1/2/3 -- write the plan before touching `edit_lines_impl_gated`'s
  live gate logic, scope the first slice as small as possible, ship it opt-in/off-by-
  default so it cannot regress anyone's existing behavior.
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md   # WS-6 §, full 5-tier design
  - docs/plans/2026-08-02-priority-reaudit.md            # WS-6 = Tier D, "no change to
    P1 order", i.e. still correctly scheduled, not urgent -- this plan does NOT
    contradict that; it picks the cheapest slice of it precisely because it's cheap,
    not because it's suddenly urgent
  - crates/calm-core/src/txn.rs                          # TxState::VerifyPending
    already a legal `allowed_next` target from IndexCommitted; comment there says
    "kept as a valid transition target now so WS-6 can start emitting it later"
verified_against: HEAD 91fd0dc (workspace 0.5.0) -- CALM's own MCP tools were
  disconnected for this session; grounded via native Read/Grep/Bash instead
---

# WS-6 Execution Plan — first slice

## 0. Vì sao chỉ làm 1 lát mỏng, không phải cả WS-6

Bản mô tả gốc (master plan) là hệ thống 5 tầng (Fast/Semantic/Security/Deep/Runtime) +
3 tool MCP mới (`verify_change`/`verification_status`/`findings_for_change`) + SARIF
ingest + validation-receipt schema. Đó là đúng bức tranh cuối, nhưng bắt đầu code từ
mô tả đó trực tiếp là chính xác thứ WS-1/2/3 đã dạy team này KHÔNG làm ("không bắt đầu
cơ hội chủ nghĩa"). Lát cắt đầu tiên ở đây cố tình nhỏ nhất có thể mà vẫn:

1. Biến `VERIFY_PENDING` từ "không bao giờ được produce" thành "được produce thật, có
   thể tái tạo, có test" — đúng khoảng trống tôi đã chỉ ra ở phiên trước.
2. Không đổi hành vi mặc định của bất kỳ ai — toàn bộ tính năng nằm sau 1 config flag
   default `false`.
3. Có giá trị thiết thực ngay cả ở dạng nhỏ nhất: agent có thể hỏi "edit vừa rồi có
   thật sự cargo check sạch không?" thay vì chỉ tin `applied: true`.

## 1. Phạm vi lát cắt này

**Có:**
- 1 config flag mới: `[verification] rust_check_on_write` (default `false`).
- Khi bật VÀ file bị sửa là `.rs`: `edit_lines`/`edit_symbol` advance transaction tới
  `VerifyPending` thay vì thẳng tới `Done` (transition này **đã hợp lệ** trong
  `allowed_next`, chỉ chưa từng có caller nào emit nó).
- 1 tool mới: `verify_change(tx_id)` — đúng tên tool master plan đã đặt. Tìm
  `Cargo.toml` gần nhất chứa file đó (đi ngược thư mục, cùng chiến lược
  `format::detect_rust_edition` đã dùng), chạy `cargo check --manifest-path ...`,
  advance transaction tới `Done` (sạch) hoặc `Failed` (có lỗi — **không rollback file
  trên disk**, đúng triết lý đã ghi trong `txn.rs`: sau khi disk đã đổi, rollback rủi
  ro hơn là chấp nhận trạng thái có thể phát hiện được qua `repair_consistency`).
- Chạy inline, không background — cùng precedent với `retry_maintenance` đã có
  ("Runs the real refresh inline... explicit recovery, not routine use").

**Không có trong lát cắt này (anti-goals, không phải quên):**
- Không làm 4 tầng còn lại (Security/Deep/Runtime) — chỉ Fast (đã có,
  `validate_syntax_diff`) + Semantic (mới, chỉ `cargo check`).
- Không làm ngôn ngữ nào khác ngoài Rust — `verify_change` trên file không phải `.rs`
  trả về rõ ràng `tier: "unsupported"`, không giả vờ đã kiểm tra.
- Không SARIF ingest, không validation-receipt schema riêng, không
  `verification_status`/`findings_for_change` (2 tool còn lại của master plan) — chưa
  có nhu cầu cụ thể để justify thêm 2 tool nữa ngay bây giờ.
- Không đổi `edit_lines` mặc định (flag off) — không session nào bị ảnh hưởng trừ khi
  tự bật.
- Không thêm timeout/job-queue cho `cargo check` — nếu nó chạy lâu, đó là chi phí đã
  biết trước của một hành động on-demand, tường minh, không phải trong hot path.

## 2. Vì sao thiết kế này an toàn để làm ngay (không cần thêm 1 vòng threat-model)

Khác với WS-2/WS-4 (đổi gate logic đang sống, hoặc cần OS-level sandbox mới), lát cắt
này:
- Zero behavior change khi flag off (mặc định) — verify bằng test `rust_check_on_write
  defaults to false, existing edit_lines flow unchanged`.
- Chỉ thêm, không sửa, state machine đã có (`allowed_next` không đổi).
- Verification "Failed" là thông tin, không phải rollback — khớp nguyên tắc đã ghi
  trong `txn.rs`'s module comment (best-effort sau khi disk đã đổi).
- `cargo check` chạy trên workspace CỦA CHÍNH DỰ ÁN agent đang sửa — không có gap bảo
  mật mới nào WS-4 (provider sandbox) chưa từng có: agent vốn đã chạy `cargo
  build`/`cargo test` trực tiếp qua Bash rồi.

## 3. Thiết kế cụ thể

### 3.1 `crates/calm-core/src/verify.rs` (mới)
- `is_verifiable_rust_file(path) -> bool`
- `find_nearest_cargo_toml(file_path, project_root) -> Option<PathBuf>` (giống hệt
  chiến lược `format::detect_rust_edition`)
- `run_cargo_check(manifest_path) -> Result<CargoCheckResult, String>` — spawn
  `cargo check --manifest-path <path> --message-format=short`, phân loại pass/fail
  qua exit code, cap 40 dòng stderr diagnostic.

### 3.2 `crates/calm-core/src/config.rs`
- `VerificationConfig { rust_check_on_write: bool }`, default `false`, thêm field
  `verification: VerificationConfig` vào `Config`.

### 3.3 `crates/calm-server/src/tools/edit.rs`
- Tại chỗ advance `IndexCommitted -> Done` hiện tại (dòng ~1618-1641): khi
  `config.verification.rust_check_on_write` bật VÀ `is_verifiable_rust_file(path)`,
  advance tới `VerifyPending` thay vì `Done`, trả `tx_id` với gợi ý gọi
  `verify_change`. Khi tắt (mặc định) hoặc file không phải `.rs`: hành vi CŨ giữ
  nguyên 100%.

### 3.4 `crates/calm-server/src/tools/txn.rs`
- Tool mới `verify_change(tx_id)` trong `txn_tool_router`:
  - `tx` không tồn tại → `TX_NOT_FOUND`.
  - `replay_state` không phải `VerifyPending` → trả kết quả rõ ràng "nothing to
    verify" (không phải lỗi) kèm lý do (đã `Done` vì verification tắt/không áp dụng,
    hoặc `Failed`/`RolledBack`).
  - `VerifyPending` + file `.rs` → chạy `run_cargo_check`, advance `Done`/`Failed`,
    trả `tier`, `verified`, `diagnostics`, `command`.
  - `VerifyPending` + file không xác định được ngôn ngữ hỗ trợ → `tier: "unsupported"`
    (phòng thủ; luồng bình thường không nên tới được đây vì §3.3 chỉ route file
    `.rs` vào `VerifyPending`).

### 3.5 `crates/calm-server/src/tools/toolset.rs`
- Thêm `"verify_change"` vào preset `"edit"` (cùng nhóm 4 tool `txn` khác).

## 4. Test

- Config default `false` → hành vi `edit_lines` không đổi (transaction vẫn advance
  thẳng tới `Done`).
- Config `true` + file `.rs` hợp lệ → transaction qua `VerifyPending`, `verify_change`
  trả `verified: true`, advance `Done`.
- Config `true` + file `.rs` có lỗi cú pháp/type → `verify_change` trả `verified:
  false`, advance `Failed`, **file trên disk vẫn giữ nguyên nội dung đã ghi** (không
  rollback).
- `verify_change` trên `tx_id` không tồn tại → lỗi rõ ràng.
- `verify_change` trên `tx_id` đã `Done` (vì flag tắt) → phản hồi "nothing to verify",
  không phải lỗi mơ hồ.
- `find_nearest_cargo_toml`/`is_verifiable_rust_file` unit test riêng, cô lập trong
  tempdir (không đụng target dir của chính workspace CALM — tránh tranh chấp lock với
  `cargo test` đang chạy).

## 5. Rollout

Không cần "shadow → dual-read → enforce" như WS-1 vì bản thân flag đã LÀ shadow mode
(default off = không ai bị ảnh hưởng cho tới khi tự bật). Không cần milestone gate
riêng — tiêu chí "Done" của lát cắt này đơn giản: `cargo test` xanh, `verify_change`
hoạt động đúng trên cả 2 nhánh (pass/fail), flag off không đổi bất kỳ hành vi quan sát
được nào.

## 6. Việc để lại cho lát cắt kế tiếp (không phải bây giờ)

- Semantic tier cho ngôn ngữ khác Rust (Python/`mypy`, TypeScript/`tsc`, Go/`go
  vet`, …).
- Security tier (Semgrep targeted rules).
- `verification_status`/`findings_for_change` nếu có nhu cầu cụ thể phát sinh.
- Quyết định có nên bật `rust_check_on_write` mặc định `true` cho chính CALM repo hay
  không — để riêng, cần đo chi phí latency thật trước (giống cách team đã làm cho
  WS-1's p95 overhead).
