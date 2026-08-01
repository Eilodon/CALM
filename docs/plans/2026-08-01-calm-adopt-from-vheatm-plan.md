---
title: "CALM nên học gì từ VHEATM — báo cáo xác minh + kế hoạch áp dụng"
date: 2026-08-01
mode: research + plan (không tạo artifact visual)
scope: đối chiếu HEAD thực tế của hai repo Eilodon/CALM và Eilodon/VHEATM
evidence_base: đọc trực tiếp source (file:line), không lấy prose/spec/comment làm bằng chứng
verdict_on_source_analysis: đúng ~85–90% — mọi finding lớn được xác nhận trên code thật; một số phần cần thu hẹp phạm vi
related: docs/plans/2026-07-14-calm-vheatm-full-audit.md, docs/comparison.md
---

# CALM nên học gì từ VHEATM

> **Một câu tổng kết vận hành.**
> VHEATM định nghĩa *nghĩa vụ chứng minh* và phát hành *authorization có giới hạn*;
> CALM tạo *live code evidence*, tái kiểm tra trạng thái *tại thời điểm hành động*, và
> thực thi thay đổi; VHEATM chỉ tuyên bố *completion* sau khi xác minh *action receipt*
> và postconditions. Cả hai bên đều có quyền **fail-closed**, nhưng không bên nào được
> dùng một "allow" để bypass invariant của bên còn lại.

Báo cáo này trả lời đúng câu hỏi được giao: **cái gì có giá trị từ VHEATM mà CALM nên
học hỏi và áp dụng.** Nó gồm ba phần:

1. **Xác minh** bài phân tích gốc trên code thật (bảng finding có `file:line`).
2. **Danh mục năng lực VHEATM** mà CALM còn thiếu, kèm cơ chế cụ thể để mượn.
3. **Kế hoạch triển khai theo phase** cho CALM, mỗi hạng mục có thay đổi cụ thể, contract/schema,
   test, và tiêu chí hoàn thành.

---

## 0. Định vị lại ranh giới (bản đã hiệu chỉnh)

Luận điểm gốc "CALM là data/action plane, VHEATM là governance/control plane" **đúng hướng**
nhưng tuyệt đối hoá quá mức. Phiên bản chính xác hơn, có bằng chứng:

- **CALM** = *code-intelligence data plane* kiêm *local reference monitor* cho các thao tác
  do chính CALM thực hiện. CALM đã có phân loại rủi ro, machine gate phân tầng, audit logging
  và taxonomy confidence — nó **không** thuần "action plane không có policy".
- **VHEATM** = *audit authority*, *evidence-governance* và *qualification plane*. Nhưng ở HEAD
  hiện tại VHEATM **không còn thuần decision-only**: nó đã có sandbox reference-monitor và
  provider transport adapter (xem §3.3). ToolBroker vẫn decision-only ở cấp broker
  (`src/vheatm_control/tool_broker.py:165` — *"The broker returns policy decisions only. It
  never executes, writes, performs…"*), nhưng repo tổng thể đã có action boundary.

Hệ quả cho tích hợp: sẽ có **hai lớp quyết định**. Phải quy định rõ lớp nào có thể *cho phép*,
lớp nào chỉ có thể *phủ quyết* (xem §5, dual authority).

---

## 1. Bảng xác minh finding (đối chiếu code thật)

Ký hiệu: ✅ xác nhận đúng · ⚠️ đúng nhưng cần thu hẹp/điều chỉnh · ❌ sai.

| # | Luận điểm gốc | Trạng thái | Bằng chứng trên code |
|---|---------------|-----------|----------------------|
| 1.1 | MCP report validator của VHEATM yếu hơn CLI | ✅ | `src/vheatm_control/mcp_server.py:70-76` gọi thẳng `validate_report_semantics(...)`. CLI `report_validator.py:462-491` chạy **JSON Schema registry** (`Draft202012Validator` + `_schema_registry`), dựng `build_bundle(root)["bundle_root"]`, chạy `load_and_route()` → `canonical_selection`, rồi mới `validate_report_semantics(..., canonical_selection=…, bundle_root=…)`. MCP bỏ cả bốn bước. |
| 1.2 | Release gates chưa nằm trên publish path | ✅ | `.github/workflows/release.yml`: `pypi-publish` chỉ `needs: build`; `mcp-registry-publish` `needs: pypi-publish`; `docker` `needs: verify-version`. Không job nào chạy `vheatm-release-gates`. Entry point tồn tại: `pyproject.toml` → `vheatm-release-gates = "vheatm_control.evaluation:main"`, với RG-00…RG-15 ở `evaluation.py:53-70`. `ci.yml` cũng chỉ chạy `vheatm-validate` + `pytest`, và release **không** `needs` CI. |
| 1.3 | Docs drift của VHEATM là thật | ✅ | `README.md:124` — *"The module registry currently uses `pilot` coverage"* trong khi `modules/registry.yaml:3` — `gate_owner_coverage: complete`. `docs/architecture.md:57` vẫn liệt kê *"Split the legacy `SKILL.md` into a compact router plus validated modules"* trong "Next implementation slices" dù đã có **22 module** (`ls modules/*/` = 22, registry entries = 22) và `SKILL.md` đã là router gọn. |
| 1.4 | CALM lexical grounding chưa phải evidence grounding | ✅ | `crates/calm-server/src/tools/edit.rs:990-1005`: `cites_token` là word-boundary lexical match trên `short`/`last_two`/`qn`. Nhánh không có known caller: `known_caller_qns.is_empty()` ⇒ chỉ cần `!reason.is_empty()` là qua. Không chứng minh caller body đã đọc, contract đã kiểm, hay response shape được giữ. |
| 1.5 | FNV phù hợp stale-write guard, không phù hợp provenance | ✅ | `crates/calm-core/src/indexer/pipeline.rs:114-130`: `hash_content` là FNV-1a 64-bit, `pub` để `edit.rs` tái dùng làm **stale-write conflict guard**. `edit.rs:12,135,220,333,405` dùng đúng cho before/after range. Không đủ collision-resistance cho adversarial evidence. |
| 1.6 | CALM audit log chưa phải provenance chain | ✅ | `crates/calm-server/src/telemetry.rs:1-10`: `AUDIT_TARGET = "calm_audit"` là *structured, SIEM-ingestible* **tracing stream** — không phải append-only authenticated ledger, không content-addressed, không signed receipt, không replayable. |
| 2.1 | Mô tả full write-gate hơi tuyệt đối hoá | ⚠️ | Đúng: có tier nhẹ. `edit.rs:860-922` — bridge-only + risk≤medium + mọi caller edge confident ⇒ `GateRequirement::ConfirmOnly` (`edit.rs:1745`), bỏ `EDIT_CONTEXT_REQUIRED`/`REASON_NOT_GROUNDED`. Human elicitation opt-in: `edit.rs:1342-1349` `if !cfg.elicit_hub_confirm { … }`. Base reindex synchronous nhưng SCIP overlay async (`crates/calm-server/src/scip_overlay.rs` — coalesced/concurrent). |
| 2.2 | CALM có >2 write path (inventory drift) | ✅ | `edit_lines` tự mô tả *"The only write-capable tool in calm"* (`edit.rs:20`, và `__toolsnaps__/edit_lines.snap`) — nhưng `edit_symbol` **và** `format_files` (`edit.rs:473-497`) cũng ghi file. `format_files` *"Deliberately does NOT run the hub/high-risk gate"* (semantics-preserving transform). |
| 2.3 | "VHEATM chưa có action adapter" đã lỗi thời một phần | ✅ | `src/vheatm_control/sandbox.py:65-93,321,328` — reference monitor bind `policy_decision`+`tool_receipt`+`action_digest` trước backend action, fail-closed khi *"reference monitor has no policy broker"*. `providers.py:30-117` — `https_json_transport` fail-closed, `_validate_network_request` bắt buộc HTTPS + `scope: workspace:`. Host namespace isolation **chưa** production-qualified (sandbox.py tự ghi nhận). |
| 2.5 | Legacy-source provenance yếu nhưng không như mô tả | ⚠️ | Extracted corpus **có** trong canonical bundle: `src/vheatm_control/bundle.py:36` include `docs/VHEATM-bản gốc tham khảo/vheatm-ultimate/**/*`, content-addressed file-level (`_sha256` per entry, `canonical_bundle_root`). Nhưng archive `.skill` **không có trong repo** (`find . -name "*.skill"` = rỗng); `modules/registry.yaml:95-99` chỉ khai báo `sha256`/`size_bytes`. Test `tests/test_registry_integrity.py:56` chỉ kiểm mutation của fingerprint, không tái tạo byte hash. |
| 2.6 | Taxonomy đúng schema, chưa đủ về production semantics | ✅ | `crates/calm-core/src/types.rs:32-58`: đủ 6 biến thể. `Unresolved` (`types.rs:44-57`) — *"Reserved for a future producer — nothing constructs this yet"*. Vậy chỉ 5/6 được sản xuất thật. |
| 8.1 | CALM CI/release mạnh; embeddings có network path | ✅ | `.github/workflows/ci.yml`: `cargo fmt --check`, `clippy … -D warnings` trên nhiều feature matrix, `cargo audit`, stack-graphs regression corpus, all-language build, `js-client-interop` (TS SDK). `release.yml`: `SHA256SUMS`, `attest-build-provenance`, cosign keyless, npm platform packages. Nhưng `crates/calm-core/src/config.rs:657` — `allow_network_fallback: true` mặc định ⇒ embeddings có HuggingFace Hub fetch (`embedding.rs:159-236`). "Local-first" ≠ "air-gapped mặc định". |
| 8.2 | VHEATM có operational drift ngoài docs | ✅ | `pyproject.toml [project.urls]` trỏ `github.com/vheatm/VHEATM` trong khi repo là `Eilodon/VHEATM`. `fastmcp==4.0.0b1` (prerelease pin). |
| 8.3 | Token estimator không phải bug | ✅ | `src/vheatm_control/module_router.py:55-56` — `max(1, (len(data) + 2) // 3)`, comment *"Conservative tokenizer-independent proxy used only for disclosure budgets."* Deliberate deterministic admission-control approximation, không phải correctness bug. |

**Kết luận xác minh:** không finding lớn nào bị bác bỏ. Ba điểm cần thu hẹp (2.1, 2.5, 2.6) đã
được phản ánh trong bảng và trong §4.

---

## 2. Cái gì VHEATM làm đúng mà CALM còn thiếu (danh mục năng lực nguồn)

Đây là "hàng hoá" thực sự CALM nên mượn. Mỗi mục nêu **cơ chế VHEATM** (đã xác minh) và **khoảng
trống tương ứng ở CALM**.

### 2.A. Content-addressed, immutable evidence records (SHA-256)
- **VHEATM:** `src/vheatm_control/provenance.py` — `_content_id(prefix, value)` = `SHA-256` của
  canonical bytes (`provenance.py:47-48`); `expected_source_id`/`expected_claim_id`/
  `expected_validation_receipt_id` khiến **ID = nội dung**. Đổi nội dung ⇒ đổi ID (immutable
  đúng nghĩa). `verify_source_content` (`provenance.py:222-224`) kiểm lại digest.
- **CALM:** identity nội bộ dựa FNV + SQLite rowid, tái tạo được sau reindex. Không có record
  bất biến content-addressed cho query/edit evidence.

### 2.B. Replayable lifecycle state machine
- **VHEATM:** `src/vheatm_control/lifecycle.py` — `AuditLifecycle` với `ALLOWED_TRANSITIONS`,
  `transition()` bắt buộc `actor`+`reason`, gắn `sequence`, và `from_document()` **replay** event
  để dẫn xuất `state`. Report validator từ chối nếu `lifecycle_state` ≠ state replay được
  (`report_validator.py:404-420`).
- **CALM:** vòng đời một edit là ngầm định trong một tool call; không có state machine
  disk_applied → … → complete có thể replay/audit độc lập.

### 2.C. Authenticated append-only journal (hash chain)
- **VHEATM:** `provenance.py:60-61` — `_expected_event_hash` băm event (trừ chính `event_hash`),
  `expected_journal_event_id` (`provenance.py:56`) ⇒ chuỗi event content-addressed, chống chèn/sửa.
- **CALM:** `calm_audit` là log stream một chiều — mất một dòng không phát hiện được.

### 2.D. Typed brokered action receipt
- **VHEATM:** `schemas/tool-receipt.schema.json` — `id: ^TRC-[A-F0-9]{64}$`, bind
  `request_id`, `request_digest`, `tool_class`, `decision`, `action_digest`, `approval_token_id`,
  `recorded_at`. Receipt buộc phải khớp action digest + policy decision (sandbox.py:85-90).
- **CALM:** `edit_lines`/`edit_symbol` trả về `current_hash` + reindex status, nhưng **không** có
  receipt bất biến bind quyết định gate ↔ digest before/after ↔ postcondition.

### 2.E. Single-use signed approval token (cho remote/privileged)
- **VHEATM:** `schemas/approval-token.schema.json` — `token_id: ^APR-[A-F0-9]{64}$`,
  `exact_scope: ^workspace:`, `request_digest`, `expires_at`, `nonce`, `single_use: true`,
  `signature: {algorithm: hmac-sha256, key_id, value}`.
- **CALM:** human veto là boolean opt-in (`elicit_hub_confirm`), không có token phạm vi hẹp, hết
  hạn, dùng một lần, ký mật mã cho remote mode.

### 2.F. Taint model: "tainted đến khi có validation receipt"
- **VHEATM:** source có `taint_state`; claim "verified" trên tainted source **bắt buộc** một
  validation receipt hợp lệ (`report_validator.py:291-317`, `validate_claim_trust`). Epistemic
  status tách khỏi confidence.
- **CALM:** "grounded reason" là lexical acknowledgment, không có bước validation tách tainted →
  cleared, không tách epistemic status khỏi edge confidence.

### 2.G. Evidence-gated release qualification (barrier trước publish)
- **VHEATM:** RG-00…RG-15 (`evaluation.py:53-70`) là contract điều kiện phát hành từ evidence đã
  verify (determinism 1000 runs, mutation rejection, recall CI, ASR upper-CI, supply-chain…).
- **CALM:** CI rất mạnh nhưng **không có một job qualification chặn publish** tổng hợp các ngưỡng
  thành một cửa duy nhất mà `release.yml` phải `needs`.

### 2.H. Deterministic byte-budget disclosure control
- **VHEATM:** ngân sách disclosure tính bằng byte, tokenizer-independent, **replayable** — routing
  canonical không phụ thuộc vendor tokenizer (`module_router.py:55-56`).
- **CALM:** đáng mượn cho bất kỳ ngân sách ngữ cảnh nào cần replay xác định.

### 2.I. Doc drift gate (generated status)
- **VHEATM (bài học ngược):** chính VHEATM đang bị drift (1.3/8.2) vì đếm module & trạng thái
  migration bằng tay ở nhiều nơi. Bài học cho CALM: **sinh** trạng thái từ nguồn khả thi hành, CI
  chỉ kiểm không-drift.

---

## 3. Kiến trúc tích hợp CALM ↔ VHEATM (những gì thiếu trong bài gốc)

### 3.1. Vấn đề hai authority — luật hợp thành fail-closed
Khi VHEATM `allow` nhưng CALM local gate nói `EDIT_CONTEXT_REQUIRED` hoặc snapshot đã stale, ai
thắng? Nguyên tắc:

> VHEATM authorization là **necessary nhưng không sufficient**. CALM local gate luôn có quyền
> phủ quyết. Không "allow" nào được bypass invariant của executor.

Bảng chân trị (monotonic fail-closed):

```
VHEATM deny     → deny
VHEATM unknown  → block
CALM deny       → deny
CALM stale      → block + replan
chỉ khi cả hai allow → execute
```

Không cho CALM tự diễn giải policy VHEATM; không cho VHEATM thay thế risk gate CALM.

### 3.2. Flow prepare–authorize–execute–verify (đóng TOCTOU)
Có khoảng trống TOCTOU giữa lúc VHEATM xét evidence và lúc CALM ghi:

```
1. CALM capture snapshot S
2. CALM trả query/context receipt Q bound với S           (CQR-…)
3. VHEATM validate Q, dẫn xuất plan P
4. VHEATM phát one-time authorization A bound S + P + action digest chính xác   (APR-…)
5. CALM PREPARE: verify S còn hiện hành, verify range hash kỳ vọng, tái đánh giá local gate
6. CALM EXECUTE atomically
7. CALM phát action receipt R: A-id · before/after SHA-256 · parse result ·
   base reindex result · overlay states · postcondition                        (CER-…)
8. VHEATM validate R, dẫn xuất lifecycle/completion
```

Nếu snapshot đổi giữa (4) và (5): `SNAPSHOT_MISMATCH` → authorization consumed/invalidated →
capture evidence mới → replan. **Không** "refresh tự động rồi dùng authorization cũ".

### 3.3. Bất đối xứng adapter của VHEATM (điều kiện tích hợp)
VHEATM đã có sandbox + provider adapter (§2.3), nhưng **chưa có** write/filesystem reference
monitor giàu code-semantics ngang CALM, và host isolation chưa qualified. Đây chính là **lý do
kiến trúc** để dùng CALM làm executor/evidence-provider thay vì VHEATM tự dựng lại multi-language
graph.

---

## 4. Các điều chỉnh bắt buộc so với cách mô tả gốc (đã xác minh)

1. **Đừng mô tả full gate của CALM như áp dụng tuyệt đối cho mọi hub edit.** Có tier
   `ConfirmOnly` cho bridge-only hub confident (edit.rs:860-922).
2. **Human elicitation của CALM là opt-in, không mặc định** (`elicit_hub_confirm`, edit.rs:1342-1349).
3. **Base reindex synchronous, nhưng SCIP/formal overlay có thể async** ⇒ "reindex xong" ≠ "mọi
   enrichment fresh". Snapshot phải tách trạng thái từng overlay (scip_overlay.rs).
4. **VHEATM đã có sandbox + provider action adapter** (§2.3) — không còn thuần decision-only ở
   cấp repo; nhưng host isolation vẫn là external qualification blocker.
5. **Legacy corpus thật sự nằm trong canonical bundle** (bundle.py:36); chỉ byte-hash của archive
   `.skill` là chưa tái tạo được (archive không có trong repo).
6. **`Unresolved` của CALM là designed state, chưa phải actively produced** (types.rs:44-57).
7. **Đừng gộp hai trục "resolution confidence" và "trust".** Một `formal` edge từ provider stale
   hoặc sai build flags **không** tự nhiên thành validated audit evidence. Giữ metadata đa chiều
   (xem §6, CALM P0-2).
8. **Structured evidence acknowledgment không chứng minh "agent đã hiểu".** Server chỉ chứng minh
   được: *evidence đã trả về và một tập evidence ID cụ thể đã được acknowledge.* Đặt tên đúng:
   **lexically/positionally acknowledged reason**, không phải "grounded/understood".

---

## 5. Kế hoạch áp dụng cho CALM (theo phase, có contract + test)

Nguyên tắc xuyên suốt, khớp `AGENTS.md` của CALM: **không thêm autonomous write/execution mới**;
tất cả receipt/ledger là *observability + provenance*, không nới lỏng gate hiện có; giữ FNV cho
concurrency, thêm SHA-256 chỉ cho provenance identity (dual-hash, không thay toàn bộ).

### CALM P0 — nền provenance & epistemic (giá trị cao nhất, rủi ro thấp)

#### P0-1. Dual-hash: thêm `evidence_digest` (SHA-256) bên cạnh `fast_hash` (FNV)
- **Mục tiêu:** provenance identity chịu được adversarial, giữ nguyên hiệu năng cache/stale-write.
- **Thay đổi:** `crates/calm-core/src/indexer/pipeline.rs` — giữ `hash_content` (FNV, đổi tên khái
  niệm thành `fast_hash` trong doc), thêm `evidence_digest(&str) -> String` dùng SHA-256 (crate
  `sha2`). Dùng cho receipt/ledger, **không** thay cột `file_index.hash`.
- **Contract:**
  ```
  fast_hash:       FNV-1a 64  # concurrency / cache / stale-write guard (giữ nguyên)
  evidence_digest: SHA-256    # provenance / receipt identity
  ```
- **Test:** `evidence_digest_is_stable_and_collision_domain_separated`; các test hash_content hiện
  có (edit.rs:525+) **không đổi**.
- **Done:** receipt mới dùng SHA-256; gate hiện tại 0 thay đổi hành vi.

#### P0-2. Snapshot contract đa chiều (thay `indexing_phase` đơn trị)
- **Mục tiêu:** giữ được sự bất định thay vì nén thành một boolean `ready`.
- **Hiện trạng:** `crates/calm-server/src/tools/common.rs:503` chỉ phát một `indexing_phase`
  (`phase_str()`) + `embeddings_status`.
- **Thay đổi:** mở rộng `session_context`/orientation thành:
  ```yaml
  snapshot_id: CGS-<sha256>            # content-address của tập digest dưới
  git_commit: <sha>
  workspace_state:
    tracked_dirty_digest: <sha256>
    untracked_digest: <sha256>
  index:
    base_state: ready | building_edges | parsing | scanning | failed
  providers:
    scip_overlay: { state: ready|refreshing|stale|unsupported }
    lsp_overlay:  { state: ready|refreshing|stale|unsupported }
    embeddings:   { state: ready|refreshing|offline_unavailable }
  coverage:
    rust: formal_partial
    dart: symbols_only
  limitations:
    dynamic_dispatch: partial
    runtime_reachability: unsupported
  ```
- **Nguồn dữ liệu đã tồn tại:** phase (`common.rs`), embeddings status (`embed_status_str`), scip
  overlay coalescing (`scip_overlay.rs`), coverage per-language (indexer).
- **Test:** `snapshot_reports_overlay_states_independently_of_base_index`.
- **Done:** overlay đang refresh không bị báo là `ready`.

#### P0-3. Durable query/edit receipts (content-addressed)
- **Mục tiêu:** mọi write path phát receipt bất biến, bind digest before/after + snapshot +
  postcondition.
- **Thay đổi:** module mới `crates/calm-core/src/receipt.rs`. Hai record:
  ```
  CalmQueryReceipt  (CQR-<sha256>): snapshot_id, tool, returned_set_digest, produced_at
  CalmEditReceipt   (CER-<sha256>): snapshot_id, action_class, path, ranges,
                                    before_digest (SHA-256), after_digest (SHA-256),
                                    parse_ok, base_index_refreshed, overlay_states,
                                    gate_decision, gate_reason_code, postcondition
  ```
- **Action taxonomy (thay giả định "một write tool"):**
  ```
  arbitrary_write               → edit_lines
  structure_anchored_write      → edit_symbol
  semantics_preserving_transform→ format_files
  generated_file_write          → (tương lai)
  ```
  Cả `edit_lines`, `edit_symbol`, `format_files` đều phát CER (đóng inventory drift 2.2).
- **Đồng thời sửa mô tả `edit_lines`** (edit.rs:20 + `__toolsnaps__/edit_lines.snap`) bỏ
  "the only write-capable tool" → "the general line-range write path (see also edit_symbol,
  format_files)".
- **Test:** `every_write_path_emits_a_content_addressed_receipt`,
  `format_files_receipt_marks_semantics_preserving_transform`.
- **Done:** không write path nào ghi mà không có CER.

#### P0-4. Append-only authenticated ledger (nâng cấp `calm_audit`)
- **Mục tiêu:** biến audit stream thành chuỗi event content-addressed, chống chèn/sửa.
- **Thay đổi:** thêm bảng SQLite `audit_ledger(seq, prev_hash, event_hash, ts, actor, payload)` +
  `crates/calm-core/src/ledger.rs`. `event_hash = SHA-256(canonical(payload) ‖ prev_hash)`, giống
  `provenance.py:60-61`. `AUDIT_TARGET` tracing **giữ nguyên** (SIEM), ledger là kênh bền song song.
- **Test:** `ledger_detects_tampering_via_broken_hash_chain`,
  `ledger_replays_to_same_head_digest`.
- **Done:** xoá/sửa một event làm `verify_chain()` fail.

#### P0-5. Positional evidence acknowledgment (thay lexical reason, đặt tên đúng epistemic)
- **Mục tiêu:** đóng lỗ hổng "empty-caller ⇒ mọi reason non-rỗng đều qua" (edit.rs:995-996) mà
  **không** tuyên bố chứng minh understanding.
- **Thay đổi:** `edit_context` trả `query_receipt_id: CQR-…` + `returned_set_digest` +
  danh sách evidence có index. Gate chấp nhận `acknowledged_positions: [int]` (vị trí caller thực
  sự được tham chiếu) thay vì so token tự do. Giữ `cites_token` như fallback tương thích ngược.
- **Ngôn ngữ:** đổi `REASON_NOT_GROUNDED` doc/text → "reason not acknowledged" (lexically/positionally),
  không dùng chữ "grounded/understood" ngụ ý cognition.
- **Test:** `empty_caller_set_still_requires_positional_acknowledgment`,
  `acknowledgment_binds_to_returned_set_digest`.
- **Done:** không còn nhánh "any non-empty reason passes".

#### P0-6. Replayable change lifecycle
- **Mục tiêu:** phân biệt tường minh các mốc, thay vì "reindex xong = complete".
- **Thay đổi:** state machine (mirror `lifecycle.py`) với các state:
  ```
  disk_applied → base_index_refreshed → formal_overlay_refreshed
              → impact_reviewed → tests_verified → complete
  ```
  Mỗi transition ghi vào ledger (P0-4), bắt buộc `actor`+`reason`, có `sequence`, replay được.
  `diff_impact` đẩy `impact_reviewed`; base reindex đẩy `base_index_refreshed`; overlay refresh
  đẩy `formal_overlay_refreshed` (async, có thể đến sau response — đúng bản chất).
- **Test:** `lifecycle_state_is_derived_by_replay_not_asserted`,
  `formal_overlay_refreshed_may_arrive_after_disk_applied`.
- **Done:** completion không thể tuyên bố khi overlay/tests chưa tới mốc.

### CALM P1 — evidence identity ổn định & VHEATM read-only adapter

#### P1-1. Stable evidence IDs (không dùng SQLite rowid)
- **Vấn đề:** reindex có thể xoá/tạo lại edge, đổi ordering, đổi provider confidence, đổi call-site span.
- **Thay đổi:** edge identity =
  ```
  SHA-256( snapshot_id ‖ caller_symbol_identity ‖ callee_symbol_identity ‖
           callsite_path ‖ callsite_span ‖ provider ‖ confidence )
  ```
  hoặc receipt chỉ bind `returned_set_digest` + `acknowledged_positions` (ít phụ thuộc nội bộ hơn —
  **ưu tiên cách này** cho P0-5).
- **Test:** `edge_id_survives_reindex_when_content_unchanged`.

#### P1-2. Read-only VHEATM adapter (capability handshake)
- **Mục tiêu:** CALM làm code-evidence provider cho VHEATM, **chưa cho write**.
- **Bề mặt:** `snapshot`, `callers`, `callees`, `diff_impact`, `session_context` + một capability
  handshake khai báo: languages, coverage_state per language, overlay states, provider integrity.
- **Khớp VHEATM:** VHEATM tách MCP surface (xem §7) — CALM đứng ở tầng `provider` read-only.
- **Test:** `read_only_adapter_never_exposes_a_write_capability`.

### CALM P2 — authorization binding & privileged action

#### P2-1. Scoped, single-use, signed approval token cho remote mode
- **Mượn trực tiếp** `schemas/approval-token.schema.json`: `APR-<sha256>`, `exact_scope:
  workspace:…`, `request_digest`, `expires_at`, `nonce`, `single_use: true`, `signature:
  hmac-sha256`. Thay boolean `human_approval` bằng token verify được ở remote/HTTP transport
  (`crates/calm-server/src/http.rs`).
- **Test:** `approval_token_rejected_when_expired_or_replayed_or_scope_mismatch`.

#### P2-2. Bind VHEATM authorization vào CALM prepare/execute (dual authority)
- Triển khai §3.1 + §3.2: `prepare` verify snapshot + re-evaluate local gate; `execute` atomic;
  phát CER; VHEATM chỉ tuyên bố completion sau khi validate CER + postconditions.
- **Chỉ làm sau khi P0 evidence contract ổn định.** Không đưa write adapter lên trước.

### CALM P3 — release qualification & doc-drift gate (mượn mô hình VHEATM)

#### P3-1. Một job qualification chặn publish
- **Mô hình VHEATM RG registry** áp cho CALM: gom các ngưỡng đã có (fitness-check, clippy
  -D warnings, cargo audit, stack-graphs regression, SDK interop) + bằng chứng cần thiết thành
  **một** job `qualify-release`; `release.yml` mọi publish job `needs: qualify-release`. (Song song,
  đây cũng là VHEATM P0-2 cho chính VHEATM — xem §8.)
- **Test/CI:** publish không chạy được nếu qualification chưa pass.

#### P3-2. `status.generated.md` + CI no-drift check
- Sinh trạng thái từ nguồn khả thi hành (tool inventory, feature flags, language coverage, write-path
  taxonomy). CI kiểm generated file không drift so với repo. Chấm dứt việc mô tả trạng thái bằng tay
  ở README (bài học từ VHEATM 1.3/8.2).

---

## 6. Metadata đa chiều cho trust (chống gộp trục)

Thay vì `formal → validated`, receipt/evidence CALM giữ nhiều trục độc lập:

```yaml
resolution_confidence: formal        # từ EdgeConfidence (types.rs)
snapshot_state:        fresh|stale|refreshing
provider_integrity:    verified|unverified
coverage_state:        formal_partial|symbols_only|none
compilation_profile:   <features/build flags>
runtime_reachability:  unknown
receipt_validation:    validated|pending
```

Trust = tích hợp các trục này, **không** suy ra chỉ từ edge confidence. Một `formal` edge từ
provider stale không phải validated evidence.

---

## 7. Bề mặt MCP: tách theo trust boundary (khuyến nghị đối xứng cho cả hai)

Không expose mọi CLI/broker qua một MCP server. Tách:

```
vheatm-control-mcp    read-only:  validate / evaluate / route / inspect evidence
vheatm-provider-mcp   read-only / bounded: probe / linker / CALM code-evidence
vheatm-action-mcp     privileged: brokered execution / write / network
```

Privileged server cần capability declaration + approval-token verification (P2-1) + isolation
theo deployment. CALM đứng ở tầng `provider` (read-only, P1-2) trước, `action` sau (P2-2).

---

## 8. Phụ thuộc: VHEATM phải tự đóng ba lỗ trước khi authorization của nó đáng tin

CALM chỉ nên bind vào VHEATM authorization khi ba việc sau xong (đây là VHEATM-side, nhưng ảnh
hưởng trực tiếp niềm tin tích hợp):

1. **Unified report validation path** — MCP `vheatm_validate_report` phải gọi cùng lõi với CLI.
   Đề xuất API:
   ```python
   def validate_report_document(root, report, *, now=None) -> list[ReportIssue]: ...
   # validate_report_file() = parse file → validate_report_document(); MCP cũng gọi hàm này.
   ```
   Thêm **parity mutation test**: với mỗi report bị mutate,
   `issues_from_cli_core == issues_from_mcp_core` (so theo `(source, message)`/error code, không chỉ
   `valid: true/false`).
2. **Release qualification chặn publish** — chèn job `qualify-release` (immutable evidence bundle →
   verify signatures/digests → RG-00…RG-15 → content-addressed release qualification report); mọi
   publish `needs: qualify-release`.
3. **Doc/package/capability drift gate** — sinh `status.generated.md`; sửa `pyproject.toml` URL
   `vheatm/VHEATM` → `Eilodon/VHEATM`; đồng nhất repo metadata ↔ package URLs ↔ MCP registry ↔
   README ↔ origin.

---

## 9. Thứ tự ưu tiên & scorecard (kiến trúc, không phải benchmark chuẩn hoá)

**CALM adopt (theo giá trị/rủi ro):**

1. P0-1 dual-hash SHA-256 · P0-3 durable receipts · P0-4 ledger — nền provenance.
2. P0-5 positional acknowledgment · P0-6 lifecycle — nâng epistemic strength.
3. P0-2 snapshot contract đa chiều — điều kiện tiên quyết cho TOCTOU.
4. P1 stable IDs + read-only adapter.
5. P2 approval token + dual-authority binding (sau khi evidence contract ổn định).
6. P3 release qualification + doc-drift gate.

**Scorecard (Design vs Operational closure hiện tại):**

| Năng lực | Design | Operational |
|---|---|---|
| CALM live code intelligence | 9.3 | 8.8 |
| CALM guarded writes | 9.0 | 8.0 |
| CALM durable provenance | 6.0 | 4.5 |
| CALM productization (CI/release) | 9.2 | 8.8 |
| VHEATM deterministic planning | 9.5 | 8.5 |
| VHEATM provenance/replay | 9.4 | 8.0 |
| VHEATM report completion validation | 9.2 | 7.5 |
| VHEATM action enforcement | 8.0 | 6.0 |
| VHEATM release qualification | 9.4 | 5.5 |
| VHEATM agent/MCP UX | 7.0 | 5.5 |

CALM **durable provenance** thấp nhất (6.0/4.5) — đó chính là mặt trận P0 ở trên và là "món quà"
lớn nhất VHEATM tặng CALM.

---

## 10. Anti-goals (đừng làm)

- **Đừng** biến CALM thành một audit framework đầy đủ. Chỉ mượn provenance/receipt/lifecycle.
- **Đừng** thay mọi FNV nội bộ bằng SHA-256 — giữ FNV cho concurrency/cache (P0-1).
- **Đừng** tuyên bố evidence acknowledgment chứng minh agent "hiểu".
- **Đừng** cho một "allow" của bên này bypass invariant của bên kia (dual authority).
- **Đừng** thêm write/execution autonomous mới mà không có policy change + test + rollback + review
  (khớp `AGENTS.md`).
- **Đừng** tạo "distributed monolith" hai bộ não cùng tưởng mình là authority.

---

## 11. Kết luận

Bài phân tích gốc mạnh về kiến trúc và đúng ở mọi finding quan trọng nhất (đã xác minh trên code
thật, §1). Giá trị lớn nhất CALM nên học từ VHEATM **không** phải kỹ thuật code-intelligence — CALM
đã dẫn trước ở đó — mà là **kỷ luật provenance/epistemic**: content-addressed immutable records,
authenticated append-only ledger, typed action receipts bind quyết định ↔ digest ↔ postcondition,
replayable lifecycle, taint model có validation receipt, single-use signed approval token, và
release qualification chặn publish. Cộng thêm mô hình tích hợp **dual-authority fail-closed** +
**prepare–authorize–execute–verify** để hai dự án bổ sung nhau mà không đẻ ra hai authority mâu thuẫn.

Ba việc ưu tiên hàng đầu cho CALM: (1) durable receipts + SHA-256 evidence digest, (2) authenticated
ledger + replayable lifecycle, (3) positional evidence acknowledgment thay lexical reason — tất cả
đều nằm trong đúng "khoảng trống provenance" mà scorecard chỉ ra.
