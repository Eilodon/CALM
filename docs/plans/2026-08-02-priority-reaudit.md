---
title: "Priority Re-Audit — đối chiếu lại master plan + adopt-plan với code thật sau khi WS-1 hoàn thành"
date: 2026-08-02
status: audit + revised priority list, con của master plan
scope: xác minh lại toàn bộ workstream WS-1..WS-14 (master plan) trên HEAD hiện tại, sửa những
  claim đã lỗi thời/sai, xếp lại thứ tự ưu tiên theo giá trị thật / rủi ro thật / effort thật
inputs:
  - docs/plans/2026-08-01-calm-master-upgrade-plan.md      # đọc lại toàn văn 2026-08-02
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md   # đọc lại toàn văn 2026-08-02
  - docs/plans/2026-08-02-phase1-p0-execution-plan.md       # WS-1/2/3 execution, đã cập nhật §4.1c/§3.1
audited_state:
  calm_head: nhánh main, sau khi WS-1 task 4.1-4.7 merge trong phiên này (chưa commit khi viết doc)
verification: mọi file:line dưới đây đọc trực tiếp qua mcp__calm__source/search/callers tại HEAD
  hiện tại trong phiên này — không copy lại số liệu cũ từ hai plan doc nguồn
---

> **[SUPERSEDED — trạng thái hiện tại]** Xem [2026-08-02-phase2-priority-and-ws2-execution-plan.md](2026-08-02-phase2-priority-and-ws2-execution-plan.md) — bảng §2 dưới đây ("WS-1 shadow mode, task 4.8 chưa làm") đã lỗi thời từ commit 1830328 (enforce đã ship). Nội dung dưới đây giữ nguyên làm bằng chứng xác minh chi tiết (C1-C5, Tier A/B/C/D).

# Priority Re-Audit (2026-08-02)

## 0. Vì sao audit lại

Master plan (2026-08-01) liệt kê WS-1..WS-14 dựa trên một lần đọc code tại thời điểm đó. Từ đó
tới giờ: WS-1 đã làm xong hoàn toàn (task 4.1-4.7, xem execution plan), và trong quá trình làm,
**hai claim gốc của chính master plan bị phát hiện sai/lỗi thời** (bootstrap() đã tự refresh,
resolve_repo_path đã chặn symlink escape — xem §1). Nếu không audit lại, danh sách ưu tiên tiếp
theo sẽ dựa trên tiền đề sai. Đây là vòng đọc-verify thứ tư trong cùng chuỗi phiên (sau §4.1b,
§4.1c, §3.1 của execution plan), lần này quét rộng qua toàn bộ 14 workstream thay vì một hàm.

---

## 1. Correction log — claim gốc sai hoặc đã lỗi thời (xác minh lại trên code thật)

| # | Claim gốc | Nguồn | Thực tế xác minh lại | Hệ quả |
|---|---|---|---|---|
| C1 | `recover_and_rerun` cần tự gọi lại `run_all_coalesced`/`embed_pending` lúc startup | phase1 plan bản nháp đầu | `crates/calm-server/src/lib.rs::bootstrap` (dòng 99-494, xác nhận qua `callers()` là **duy nhất** điểm vào của cả 3 launch path) đã tự chạy 1 lượt SCIP-overlay + embedding đầy đủ, không điều kiện, mỗi lần khởi động cho tiến trình sở hữu `instance_lock`. Gọi lại là dư/trùng lặp | Đã sửa thành `maintenance::reconcile_stale_at_startup` (chỉ sửa trạng thái, không tự chạy refresh) — xem execution plan §4.1c. **XONG** |
| C2 | "Chưa có policy symlink tường minh" ở path containment (WS-3 §3.1 gốc, A06) | master plan A06, adopt-plan | `crates/calm-server/src/tools/edit.rs::resolve_repo_path` (dòng 2138-2162, **6 caller xác nhận** qua `callers()`: `edit_lines_flow`, `edit_symbol_flow` x2, `format_files_impl`, `edit_lines_impl_gated`, `insertion_hunk_for`) đã `canonicalize()` + `starts_with(root)` — chặn **cả** `..`-traversal **và** symlink-escape, message lỗi ghi rõ cả hai. Ca tấn công chính đã bị chặn sẵn | path_policy (task 3.3/3.4) hạ từ "đóng lỗ hổng đang mở" xuống "defense-in-depth + policy configurability" — xem execution plan §3.1 |
| C3 | Bearer token so sánh không constant-time là lỗ hổng chưa xử lý (A05 → WS-10) | master plan A05 | `crates/calm-server/src/http.rs:71-77` — comment tại chỗ, gắn nhãn `[Task 3.4b / audit FM2]`: **đã cân nhắc và quyết định có chủ đích** không dùng constant-time compare, vì đây là "coarse remote-dev-only gate... not a substitute for TLS", không phải phòng thủ trước timing side-channel khi kẻ tấn công đã quan sát được latency mạng. Không phải một lỗ hổng bị bỏ sót — là một quyết định đã ghi lại | WS-10 hạ mức khẩn cấp mạnh — phần duy nhất master plan nêu cho WS-10 đã có câu trả lời rồi |
| C4 | `indexing_status` chỉ phát 1 `indexing_phase` đơn trị (P0-2, `common.rs:503`) | adopt-plan P0-2 | Đọc `IndexingStatusOutput` (`recover.rs:683-779`) hiện tại: đã có `scip_overlays` (per-language, không chỉ Rust), `watcher` (tách khỏi index phase), `embeddings_status`/`embeddings_error` riêng, `graph_mode`, `identity_migration`, `external_proofs` theo status. Đa chiều **đáng kể** đã tồn tại | P0-2 thu hẹp: phần thật sự thiếu chỉ còn **snapshot_id content-addressed** để bind vào WS-2 review-token (TOCTOU) — không phải "đa chiều hoá" từ đầu như mô tả gốc |
| C5 | Schema `edit_transactions.state` có 9 giá trị gồm `PROOFS_PENDING`/`EMBEDDINGS_PENDING` | master plan WS-1 SQL mẫu | Đã sửa còn 7 state (bỏ 2 cái trên) từ vòng đọc §4.1b của execution plan, lý do: 2 job đó không có cận trên thời gian rõ ràng, ép vào chuỗi tx sẽ sai mô hình | Đã note trong execution plan; nhắc lại ở đây để không ai coi SQL mẫu gốc trong master plan là đặc tả cuối |

**Ý nghĩa chung của 5 correction này:** cả 5 đều làm giảm mức khẩn cấp của một hạng mục so với
mô tả gốc (WS-10 gần như không cần làm gì thêm; WS-3 hạ từ "lỗ hổng" xuống "hardening"; P0-2 thu
hẹp phạm vi còn thiếu). Không có correction nào theo chiều ngược lại (phát hiện thêm lỗ hổng mới
nặng hơn) trong vòng audit này — nhưng xem §2 WS-4, vẫn là gap nặng nhất chưa động tới.

---

## 2. Trạng thái thật từng workstream (đối chiếu code, không phải đọc lại prose)

| WS | Tên | Trạng thái thật | Bằng chứng |
|---|---|---|---|
| WS-1 | Durable edit transaction + maintenance outbox | **XONG (shadow mode) — task 4.1-4.7/4.8** | `txn.rs`, `maintenance.rs`, wire vào `edit_lines_impl_gated`/`format_files_impl`, 4 MCP tool, startup hook — tất cả trong phiên này, 926+294+3 test pass. Task 4.8 (shadow→enforce) **chưa làm**, đúng theo kế hoạch (cần crash suite thật trước) |
| WS-2 | State-bound review token | **CHƯA LÀM — gap thật, đã xác nhận** | `edit.rs` (đọc trong phiên này, dòng ~1011-1088): `FRESHNESS_WINDOW_CALLS=200` theo call-count, `known_caller_qns.is_empty() ⇒ !reason.is_empty()` (auto-pass) vẫn y nguyên |
| WS-3 | Crypto hash + FS hardening | **PHẦN LỚN XONG** — `evidence_digest`+`atomic_write_with` xong; `path_policy` chưa làm nhưng đã hạ mức khẩn cấp (C2) | digest.rs, edit.rs (WriteAssurance) |
| WS-4 | Provider sandbox + supply-chain | **CHƯA ĐỘNG TỚI GÌ CẢ** — xác nhận: 0 kết quả cho `sandbox`/`bwrap`/`seccomp`/`namespace` trong `scip_overlay.rs` | grep rỗng |
| WS-5 | Provenance lattice / evidence ledger | Chưa làm (P1, đúng lịch) | — |
| WS-6 | Verification pipeline nhiều tầng | Chưa làm (P1, đúng lịch) | — |
| WS-7 | Dependency-aware incremental + snapshot | Nền có sẵn (`graph_generation_state`, `schema.rs:147`) nhưng invalidation-graph/work-estimator chưa làm | — |
| WS-8 | ANN hybrid search | Chưa làm (P1, đúng lịch) | — |
| WS-9 | Workflow MCP + trust boundary | Chưa làm (P1, đúng lịch) | — |
| WS-10 | HTTP transport hardening | **Gần như không cần làm gì** — xem C3; loopback-default + policy ở CLI đã có theo docstring `serve_http` | http.rs |
| WS-11 | Concurrency: per-path lock/lease | Chưa làm — `edit.lock` vẫn repo-wide (P1, đúng lịch) | — |
| WS-12 | Taint propagation | Một phần — `wrap_untrusted`/`scan_text` có, taint propagation xuyên response chưa | security.rs |
| WS-13 | Release qualification + doc-drift | **XONG (rút gọn) 2026-08-02** — `qualify-release` job mới trong `release.yml` (`build`/`docker` giờ `needs:` nó — trước đây publish không gate gì cả); `scripts/gen-status.sh` sinh `docs/status.generated.md` từ toolsnaps+Cargo features, `--check` mode + job `status-drift` mới trong `ci.yml` | release.yml, ci.yml, scripts/gen-status.sh |
| WS-14 | Multi-repo | Chưa làm, cố tình defer P2 | — |
| P0-4 | Ledger hash-chain (`audit_ledger`, không trùng WS-1) | **XONG 2026-08-02** — `crates/calm-core/src/ledger.rs` mới (append/head_digest/verify_chain), bảng `audit_ledger` trong schema.rs, 8 test (bao gồm `ledger_detects_tampering_via_broken_hash_chain`, `ledger_detects_a_deleted_row_via_prev_hash_gap`, `ledger_replays_to_same_head_digest`). Shadow/additive — chưa wire vào write path thật nào, đúng kế hoạch (bước wire là việc sau, không thuộc Tier A #1) | crates/calm-core/src/ledger.rs |

Lưu ý P0-4 **không phải** cùng thứ với WS-1's `tx_events`: `tx_events` là log per-transaction,
mỗi event content-addressed (`EVT-sha256(...)`) nhưng **không chain** `prev_hash → event_hash`
xuyên toàn bộ log như `provenance.py:60-61` — sửa/xoá một dòng `tx_events` không tự động làm gãy
một chuỗi hash toàn cục kiểm tra được. Đây là 2 lớp khác nhau, cả hai đều có giá trị.

---

## 3. Xếp lại ưu tiên — theo giá trị thật / rủi ro thật / effort thật, không theo P0/P1/P2 gốc

Nguyên tắc xếp: (a) gap có bằng chứng thật trên code (không phải "runtime hypothesis") xếp trên
gap chưa xác minh được; (b) effort tự chứa (self-contained), rủi ro thấp, dùng lại đúng pattern
"shadow/additive" vừa được chứng minh an toàn 3 lần trong phiên này (digest.rs → txn.rs →
maintenance.rs) được ưu tiên trước việc phải sửa logic gate đang sống; (c) việc cần một vòng
threat-model/plan riêng trước khi viết code (theo đúng kỷ luật đã dùng cho WS-1/2/3) không nên bắt
đầu code ngay cả khi mức nghiêm trọng cao, vì bắt đầu vội đúng là thứ gây regression.

### Tier A — làm tiếp ngay, rủi ro thấp, effort tự chứa, giá trị rõ

1. **P0-4 ledger hash-chain** — ~~`crates/calm-core/src/ledger.rs` mới, bảng `audit_ledger`~~
   **XONG 2026-08-02.** Lý do
   xếp đầu: đúng pattern đã dùng an toàn 3 lần liên tiếp trong phiên này — module mới, content-
   addressed, additive/shadow, không đụng gate logic đang sống, không đụng write path thật (chỉ
   ghi thêm 1 dòng log có thể verify). Effort thấp hơn hẳn WS-2/WS-4. Giá trị: biến `calm_audit`
   từ log một chiều thành chuỗi chống giả mạo thật — đúng mảnh "durable provenance 6.0/4.5" scorecard
   chỉ ra là điểm yếu nhất.
2. **WS-3 nốt phần path_policy** — ~~task 3.3/3.4~~ **XONG 2026-08-02.**
   `crates/calm-core/src/path_policy.rs` mới (3 mode: `RejectSymlinks`/`FollowInternalSymlinks`/
   `AllowExternalSymlinksWithApproval`, component-walk kiểu `sandbox.py` cho mode reject), 11 test.
   `resolve_repo_path` (edit.rs) refactor để delegate qua module này dưới
   `FollowInternalSymlinks` — verify **0 đổi hành vi observable**: mã lỗi/message
   `READ_FAILED`/`PATH_ESCAPES_PROJECT_ROOT` giữ nguyên byte-for-byte, cả 294 test calm-server
   (bao gồm `edit_lines_rejects_symlink_escaping_project_root`) + 943 test calm-core đều pass,
   `diff_impact` xác nhận `resolve_repo_path` `signature_changed:false`. `AllowExternalSymlinksWithApproval`
   cố tình fail-closed (`NeedsApproval`) vì chưa có cơ chế approval — chưa wire mode này hay
   `RejectSymlinks` vào call site nào, để dành cho WS-2.
3. **WS-13 rút gọn** — ~~`qualify-release` CI job + `status.generated.md`~~ **XONG 2026-08-02.**
   `qualify-release` sống trong `release.yml` (không phải `ci.yml`) vì `needs:` không bắc cầu
   được giữa 2 workflow file khác nhau — trước đây `build`/`docker` publish không `needs:` gì cả
   (một tag push đi thẳng tới build+publish, 0 xác minh). Job mới lặp lại 6 check đã có ở `ci.yml`
   (fmt/clippy -D warnings/test/audit/stack-graphs corpus/fitness-check/status-drift/SDK interop)
   trên đúng commit được tag, `build` và `docker` giờ `needs: qualify-release`. `status-drift` job
   mới thêm vào `ci.yml` (không phải chỉ release.yml) để bắt drift ngay từ PR, sớm hơn publish
   time — `scripts/gen-status.sh` sinh `docs/status.generated.md` từ `__toolsnaps__/*.snap` (annotation
   `readOnlyHint`/`idempotentHint` có sẵn, đã được `tool_schemas_match_committed_snapshots` giữ đúng)
   + Cargo `[features]`, verify: chạy `--check` trên chính output vừa sinh (0 drift), sinh lại có chủ
   đích 1 dòng lạ rồi xác nhận `--check` bắt được (exit 1), rồi phục hồi. Không đụng code sản phẩm,
   không thêm dependency (chỉ `jq`, đã dùng sẵn trong `otel-http-features` job). YAML parse-validated
   qua `python3 -c "import yaml"` cho cả 2 file (không có `actionlint` trong môi trường này để lint sâu
   hơn — rủi ro còn lại là cú pháp GH Actions runtime-only, chấp nhận được ở effort/risk mức Tier A).

### Tier B — giá trị P0 thật, nhưng cần chuẩn bị/threat-model riêng trước khi code

4. **WS-2 state-bound review token** (P0-5 adopt-plan) — **Phase 1 XONG 2026-08-02**, xem
   `docs/plans/2026-08-02-ws2-review-token-execution-plan.md`. Plan doc riêng viết trước (đúng yêu
   cầu), 7 finding xác minh (F1-F7) thu hẹp phạm vi đáng kể so với master plan gốc (không cần HMAC
   signing — cùng process issue+verify — xem F7), rồi mới code: đóng nhánh
   `known_caller_qns.is_empty() ⇒ auto-pass` cho case `UncertainZeroCallerReason::LowConfidence`
   cụ thể (không phải mọi zero-caller — EntryPoint/TestOnly có giải thích cấu trúc, giữ nguyên,
   phát hiện này tìm ra NGAY LÚC code nhờ 1 test cũ fail). Escape hatch qua elicitation
   (Ask→Approved) đã có, tái dùng nguyên. Mã lỗi mới `UNCERTAIN_ZERO_CALLER`. 943+296 test pass,
   clippy+fmt sạch. Phase 2 (caller_set_digest durable) + Phase 3 (approval-tier độc lập) vẫn
   design-only, chưa code — xem plan doc §4/§5.
5. **WS-1 task 4.8 (crash-injection suite thật) — XONG 2026-08-02.** `crates/calm-cli/src/bin/
   txn_crash_harness.rs` (subprocess thật, tự `libc::raise(SIGKILL)` xác định ngay sau bước
   `--crash-after` chỉ định — không timing/race) + `tests/txn_crash_injection.rs` (driver, verify
   disk/tx_events/replay_state/recover_incomplete nhất quán sau crash). 3 transition Phase 1 thật
   sự đạt tới (`Prepared`/`FileCommitted`/`IndexCommitted`), 100 lần/transition thật (300
   subprocess+SIGKILL thật, ~13s), job CI riêng `txn-crash-injection`. Milestone gate execution
   plan §6 item 1 giờ pass. Lưu ý: đây chỉ là *crash-injection suite* (đúng scope task 4.8 được
   yêu cầu) — bản thân transition shadow→enforce (đổi hành vi `edit_lines_impl_gated` để thật sự
   block/rollback theo transaction outcome) KHÔNG nằm trong scope này, vẫn để dành cho một quyết
   định/plan riêng sau (5 mục còn lại của milestone gate §6 vẫn chưa pass).

### Tier C — mức nghiêm trọng cao nhất theo raw severity, nhưng effort/độ phức tạp không tương xứng để bắt đầu cơ hội chủ nghĩa

6. **WS-4 provider sandbox.** Đây là gap **nặng nhất còn mở thật sự** (arbitrary code execution
   qua `npx`/build-tool invocation, xác nhận 0 sandboxing nào tồn tại) — nhưng cũng là hạng mục
   effort lớn nhất (OS-level sandbox primitive, cross-platform, network policy, resource limit).
   Không nên bắt đầu code cơ hội chủ nghĩa trong một phiên đang làm dở WS-1/2/3 — cần một vòng
   nghiên cứu+plan riêng (giống hệt cách execution plan đã làm cho WS-1/2/3) trước khi viết dòng
   code đầu tiên. Xếp Tier C không phải vì không quan trọng, mà vì bắt đầu vội đúng là rủi ro.

### Tier D — giữ nguyên P1/P2 như master plan, không có correction nào thay đổi thứ tự

WS-5, WS-6, WS-7 (phần còn lại), WS-8, WS-9, WS-11, WS-12 (phần còn lại), WS-14 — không có phát
hiện mới nào trong audit này làm đổi thứ tự P1/P2 mà master plan đã đặt.

---

## 4. Khuyến nghị hành động tức thời

Thứ tự thực thi đề xuất cho phiên/nhóm phiên kế tiếp, bám đúng nguyên tắc đã thống nhất từ đầu
("phần đã verify/rủi ro thấp thì làm luôn, phần rủi ro cao thì verify kỹ trước"):

1. P0-4 ledger — làm ngay, cùng rhythm với WS-1 vừa xong.
2. WS-3 path_policy (3.3/3.4) — làm ngay sau, default mode `follow_internal_symlinks`.
3. WS-13 rút gọn (`qualify-release` + `status.generated.md`) — làm ngay sau, thuần CI/tooling.
4. Dừng, viết một execution plan riêng cho WS-2 (mức chi tiết như đã làm cho WS-1) **trước khi**
   chạm vào `edit_lines_impl_gated`'s gate logic — không code trực tiếp từ mô tả master plan.
5. WS-4 — cần một research+plan doc riêng, không bundle vào phiên đang chạy.

---

## 6. VHEATM re-scan (2026-08-02, +54 commit kể từ 9303d87 / +40 kể từ lần đọc trước e181e3d
     — HEAD hiện tại `eb01817`)

Theo yêu cầu "nghiên cứu lại VHEATM trước khi tiến hành theo thứ tự đã đề xuất" — `git log`/
`git diff --stat` trực tiếp trên `/home/ybao/B.1/VHEATM`, không suy đoán từ commit message.

**5 file MỚI hoàn toàn** kể từ lần đọc trước (~2300 dòng): `host_qualification.py` (421),
`trust_registry.py` (327), `signer_service.py` (249), `host_attestation.py` (214),
`provider_policy.py` (41). Cộng `evaluation.py` (+297), `supply_chain.py` (+133), `judge.py` (+132)
lớn hẳn đáng kể. Tóm lại: VHEATM đã chuyển từ "policy decision + HMAC token" (đúng thứ adopt-
plan gốc đã đọc) sang **một chuỗi signed-attestation thật** (Ed25519, external signer service
tách khỏi process chính để cô lập private key).

### 6.1. Pattern mới đáng giá nhất — `host_qualification.py` (trực tiếp trả lời cái mẠ master
plan tự gọi là điều kiện chặn GA cho WS-4: "host isolation chưa production-qualified")

Đọc toàn văn (`src/vheatm_control/host_qualification.py`, 421 dòng). Nguyên tắc thiết kế đáng
mượn **về concept**, không phải copy code Python:
- Chạy **probe thật** (bubblewrap hard-stop timeout, nhiều sample) thay vì giả định sandbox
  đáng tin. Backend không có sẵn → **mọi** sample `"unavailable"`, không âm thầm bỏ qua.
- `validate_host_qualification_run()` **tự dẫn xuất** `status`/`reference_monitor_status` từ
  raw observations rồi so với giá trị được claim — record bị từ chối nếu claim “complete” mà
  không có đủ observed evidence. **Đúng hệt nguyên tắc đã dùng cho `txn.rs::replay_state`**
  (state phải dẫn xuất được từ log, không được assert trực tiếp) — giờ áp cho host/sandbox
  qualification thay vì edit lifecycle.
- `evidence_state: "unverified"` — tách rõ “đã chạy và ra dữ liệu” khỏi “đã được xác minh độc
  lập” — khớp đúng tinh thần “đừng gọi tier yếu là compiler proof” của chính master plan §7.
- `host_identity_digest` cố tình **loại bỏ** hostname/username — chỉ digest os/arch/python-impl,
  “digest is only an evidence binding, not a fingerprint API” (comment gốc). Chi tiết privacy
  đáng giữ nếu CALM sau này làm provider/host qualification tương tự cho WS-4.

→ **Hệ quả:** WS-4 khi được lập plan riêng (Tier C) nên tham chiếu trực tiếp file này làm mẫu
cho “provider/sandbox qualification probe” thay vì nghĩ lại từ đầu — nhưng **không** đổi Tier C
thành Tier A/B: effort vẫn lớn (OS sandbox primitive, cross-platform), chỉ là giờ đã có tham
chiếu thiết kế tốt hơn khi bắt đầu.

### 6.2. `provider_policy.py` — mẫu cho “provider manifest pinned” của WS-4

Allowlist YAML schema-valid, key theo `provider_id`+`provider_version`, mỗi entry có
`qualification_state` (có thể `revoked`), `validate_provider_binding()` đỏi runtime
endpoint/config_digest/adapter_profile phải khớp **chính xác** policy đã pin, không thì từ
chối. Đây chính là phiên bản đã tinh chỉnh của ý tưởng “provider manifest pinned” master plan
đã phác ngay từ đầu WS-4 (YAML mẫu `provider_id`/`artifact_digest`/... trong master plan §WS-4)
— đáng dùng làm tham chiếu trực tiếp khi WS-4 viết schema thật.

### 6.3. `judge.py` — “độc lập” giờ bắt buộc bind vào provider policy, không chỉ nhãn khác nhau

`build_blind_packet` giờ gọi `validate_provider_binding()` — một judge được coi "independent"
phải là một provider đã được allowlist/pin canonical, không chỉ có `provider_id` string khác.
Đáng áp cho WS-2's approval-principal tier (`independent_human/policy-bot` ở mức High) — khi
WS-2 có execution plan riêng, nên định nghĩa "independent approver" theo binding canonical
tương tự, không chỉ "khác actor_id".

### 6.4. `trust_registry.py` + `signer_service.py` — hạ tầng ký Ed25519 + signer tách process

Trust registry ký Ed25519 với validity window + key rotation; signer service chạy ngoài process
qua Unix socket transport để private key không bao giờ sống chung process với code xử lý
request. Đây là **hạ tầng nặng hơn hẳn** những gì adopt-plan gốc mô tả cho WS-2 (HMAC đơn giản)
và WS-13 (RG registry không ký). CALM chưa có gì tương đương (không key management, không
signer service) — kéo pattern này vào ngay bây giờ sẽ làm Tier A's WS-13 "rút gọn" phình to ra
thành một dự án ký số riêng. **Giữ nguyên quyết định:** WS-13 Tier A vẫn chỉ là gom CI job +
sinh status, không ký gì cả — signing infrastructure là một lớp hardening **sau**, riêng, cho
WS-2/WS-13 phiên bản sau này, giống đúng cách VHEATM tự xây nó tăng dần (RG registry trước,
ký sau nhiều tháng) chứ không làm một lần.

### 6.5. Kết luận cho thứ tự Tier A/B/C

**Thứ tự Tier A đã đề xuất (P0-4 ledger → WS-3 path_policy → WS-13 rút gọn) KHÔNG đổi** —
không file nào trong 5 file mới này chạm vào phạm vi của 3 việc đó ở quy mô đã scope (P0-4 vẫn
là hash-chain đơn giản kiểu `provenance.py:60-61` cũ, không phải ký Ed25519; WS-3 không liên
quan đến provider/signer; WS-13 Tier A cố tình rút gọn không bao gồm ký). Giá trị thật sự của
lần đọc lại này nằm ở **Tier B/C** (WS-2, WS-4, và một phiên bản WS-13 “v2” sau này) — giờ đã
có tham chiếu thiết kế cụ thể, đã kiểm chứng bởi chính VHEATM, thay vì phải thiết kế từ đầu
khi tới lượt.

---

## 7. Việc KHÔNG nằm trong audit này

Audit này không chạy lại benchmark/metric nào (precision/recall, p95 latency, v.v. — mọi con số
SLO trong master plan §6 vẫn chưa đo). Không audit lại VHEATM-side (3 lỗ VHEATM phải tự đóng —
adopt-plan §8) vì nằm ngoài phạm vi repo CALM. Không đánh giá lại WS-5..WS-9/11/12/14 chi tiết vì
không có finding mới thay đổi thứ tự P1/P2 của chúng trong vòng đọc này. Không đọc sâu toàn
bộ 5 file mới của VHEATM ở mức line-by-line (§6) — đủ để nắm pattern và xác nhận không đổi
Tier A, chưa đủ để làm spec chi tiết cho WS-2/WS-4 — việc đó để dành khi các workstream đó
đến lượt làm plan riêng.
