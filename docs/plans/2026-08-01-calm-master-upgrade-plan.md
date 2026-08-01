---
title: "CALM Master Upgrade Plan — hợp nhất Deep Technical Audit + VHEATM adoption"
date: 2026-08-01
status: master plan (nguồn hợp nhất, supersedes phần kế hoạch trong 2026-08-01-calm-adopt-from-vheatm-plan.md)
inputs:
  - docs/plans/2026-08-01-calm-adopt-from-vheatm-plan.md   # VHEATM → CALM, đã xác minh file:line
  - "Audit kỹ thuật chuyên sâu dự án CALM (deep research report 4)"  # CALM-A01..A20 + P0/P1/P2
audited_head: CALM @ 7477721 (branch main/0.4.0) · VHEATM @ 9303d87
verification: mọi finding "Confirmed" của audit đã được đối chiếu lại trên code thật (file:line); các mục "Runtime hypothesis" giữ nhãn cần kiểm chứng runtime
scope: kế hoạch nâng cấp/tối ưu CALM tối đa, không tạo artifact visual
---

# CALM Master Upgrade Plan

> **Luận đề hợp nhất.**
> Bản **audit sâu** nói CALM cần gì để trở thành *trusted write authority*. Bản **VHEATM
> adoption** cho thấy VHEATM *đã* có primitive governance/provenance đã kiểm nghiệm cho một
> phần lớn nhu cầu đó. Kế hoạch này ghép hai nguồn thành **một chương trình workstream duy
> nhất**: mỗi lỗ hổng audit → pattern VHEATM mượn được (nếu có) → thay đổi CALM cụ thể
> (file, schema, test, metric). Kết quả: không phát minh lại cái VHEATM đã làm đúng, không bỏ
> sót những mảng audit chỉ ra mà VHEATM không chạm tới (sandbox provider, verification pipeline,
> search scale, filesystem durability, multi-repo).

Đọc kèm: `2026-08-01-calm-adopt-from-vheatm-plan.md` giữ toàn bộ bảng xác minh VHEATM↔CALM và
mô hình tích hợp dual-authority; tài liệu này **là kế hoạch tổng** và thay thế phần "§5 Kế hoạch"
của file đó.

---

## 0. Cách hai nguồn khớp nhau (bản đồ hội tụ)

Hai tài liệu độc lập, viết từ hai góc, **hội tụ vào cùng các P0**. Đây là tín hiệu mạnh nhất
rằng thứ tự ưu tiên đúng:

| Nhu cầu (audit) | Finding audit | VHEATM analog (đã có, đã kiểm) | Mục kế hoạch VHEATM |
|---|---|---|---|
| Edit là **state machine bền vững** chứ không phải một function call | CALM-A01, A06 | `lifecycle.py` `AuditLifecycle` (replayable, `ALLOWED_TRANSITIONS`, sequence, actor+reason); journal event hash chain | P0-4 ledger, P0-6 lifecycle |
| **Review token bind state thật** + approval độc lập | CALM-A02, A03, A20 | `approval-token.schema.json` (single-use, signed HMAC, `exact_scope`, `nonce`, `expires_at`, `request_digest`); `tool-receipt.schema.json` | P0-5 acknowledgment, P2-1 token, P2-2 dual-authority |
| **Crypto hash** cho trust boundary | CALM-A08 | `provenance.py` SHA-256 content-addressing (`_content_id`) | P0-1 dual-hash |
| **Snapshot/coverage state** đa chiều thay 1 boolean | CALM-A09, A11 | bundle content-address per-file; deterministic byte-budget | P0-2 snapshot contract |
| **Provenance ledger/lattice** (evidence, không phải enum) | CALM-A10 | `provenance.py` immutable content-addressed source/claim/receipt + taint model; append-only | 2.A/2.C/§6 |
| **Provider sandbox** + supply-chain | CALM-A04, A11, A15 | `sandbox.py` bwrap reference-monitor (action_digest, fail-closed); `providers.py` fail-closed transport; `supply-chain-attestation.schema.json` | (mới, §WS-4) |
| **Verification** ngoài syntax + taint→cleared | CALM-A07, A17 | `validation-receipt.schema.json`, `taint_state` tainted→validated, `judge.py` | 2.F taint model |
| **Workflow MCP** + tách trust boundary | CALM-A16 | 3-server split control/provider/action; capability declaration | §7, P1-2 read-only adapter |
| **Release qualification** chặn publish + chống doc-drift | CALM-A18, A19 | RG-00..RG-15 registry (`evaluation.py`); generated status | P3-1, P3-2 |

Những mảng audit thêm mà **VHEATM không có analog** (thuần CALM-native, vẫn thuộc kế hoạch):
durable filesystem semantics (A06 dir-fsync/symlink/hardlink), HTTP transport hardening (A05),
per-path lock + lease (A12, A13, A20), ANN search scale (A14), dependency-aware incremental
engine, multi-repo identity.

---

## 1. Trạng thái CALM đã đối chiếu (điểm mạnh thật vs lỗ hổng thật)

Để tránh hạ thấp CALM (một sai lầm dễ mắc khi chỉ đọc audit), phải nói rõ **CALM đã mạnh ở đâu**,
xác nhận trên code:

- **Resolution-proof provenance đã tốt.** `external_proofs` (`crates/calm-core/src/db/schema.rs:107-125`)
  bind `call_site_id`, `source_file_hash`, exact `callee_start/end_byte`, `provider_fingerprint`,
  `context_fingerprint`, `graph_generation`, `call_site_identity_version`, `status`, với
  `graph_generation_state` (`schema.rs:147-151`) và `ON DELETE CASCADE`. Đây là mô hình proof
  gắn call-site + hash + generation **mạnh hơn** phần lớn MCP code-search tool.
- **Optimistic concurrency + cross-process lock đã có.** `atomic_write` (`edit.rs:477`), exact
  range hash guard, multi-hunk all-or-nothing, CRLF preserve; cross-process `edit.lock`
  (`db/edit_lock.rs`, blocking `lock_exclusive`) đóng lost-update race giữa các process CALM.
- **CI/release trưởng thành.** fmt, clippy `-D warnings` trên nhiều feature matrix, `cargo audit`,
  stack-graphs regression, all-language build, cross-SDK TS MCP interop, self-fitness gate,
  SHA256SUMS + build-provenance attestation + cosign + npm packages.

**Khoảng trống cốt lõi (điểm mấu chốt của cả hai nguồn):**

> CALM có **resolution-proof provenance** mạnh, nhưng **action/edit provenance** yếu. Proof rằng
> "edge X do SCIP xác nhận tại generation G" thì tốt; còn proof rằng "edit E được cho phép bởi
> quyết định D, ghi từ digest A→B, đã qua verification V, đạt trạng thái complete" thì **chưa
> tồn tại như một record bất biến, replay được, chống giả mạo**. Đây đúng là mảng VHEATM giỏi
> nhất, và là lý do scorecard "durable provenance 6.0/4.5".

Kết luận phân loại triển khai (giữ nguyên từ audit, đã đồng thuận): CALM = *"early, technically
serious, pilot-ready; chưa production-trusted cho autonomous writes"*.

---

## 2. Sổ đăng ký rủi ro hợp nhất (đã đối chiếu code)

Trạng thái xác minh: ✅ đã xác nhận trên code trong lần rà này · 🕓 runtime hypothesis (cần fault
injection/benchmark) · 🧩 design limitation.

| ID | Phát hiện | Mức | XM | Bằng chứng / ghi chú | Workstream |
|---|---|---:|:--:|---|---|
| A01 | Edit không atomic xuyên file+graph+SCIP+embeddings | Cao | ✅ | reindex fail → success + `index_stale` (`edit.rs:602,634`); SCIP/embed refresh async | WS-1 |
| A06 | Không có durable transaction journal / crash recovery | Cao | ✅ | `atomic_write` (`edit.rs:477-515`): temp `.{name}.ci-edit-{PID}.tmp`, `sync_all` temp, **không dir-fsync**, perms best-effort | WS-1, WS-3 |
| A02 | `edit_context` review hết hạn theo tool-call count | Cao | ✅ | `FRESHNESS_WINDOW_CALLS=200` (`edit.rs:929`), không bind file/graph/caller hash | WS-2 |
| A03 | Human approval phụ thuộc client; `confirm:true` là self-attestation | Cao | ✅ | `elicit_hub_confirm` opt-in (`edit.rs:1342-1349`); confirm do chính agent gửi | WS-2, WS-10 |
| A20 | Active-session state chỉ advisory, không lease | TB | ✅ | `reviewing_symbol` "Deliberately advisory only" (`common.rs:430`) | WS-2, WS-11 |
| A04 | Provider chạy toolchain/build ngoài process | Cao | 🧩 | SCIP/LSP provider table; `npx`/build có thể chạy project code | WS-4 |
| A11 | Provider cache-key giản lược theo ecosystem | Cao–TB | ✅ | same source hash / different semantic env | WS-4, WS-7 |
| A15 | Model/tool/binary provenance chưa thành policy thống nhất | Cao–TB | 🧩 | embeddings network fallback mặc định bật (`config.rs:657`) | WS-4, WS-8 |
| A05 | HTTP thiếu TLS/mTLS/scope/rate-limit/peer-IP audit | Cao* | ✅ | bearer `p == token` (`http.rs:92`, không constant-time); policy ở CLI không phải server module | WS-10 |
| A07 | Syntax validation = đếm ERROR/MISSING node | TB–Cao | ✅ | `validate_syntax_diff` (`edit.rs:459`); `None`=no grammar → **allow write** (`edit.rs:411-413`) | WS-6 |
| A08 | File hash FNV-1a cho trust boundary | TB | ✅ | `hash_content` FNV-1a 64 (`pipeline.rs:114-130`) | WS-3 |
| A09 | Candidate fan-out cap + shallow parser fallback | TB | ✅ | cap 20; shallow symbol khi grammar thiếu | WS-6, WS-7 |
| A10 | "Formal" chỉ phản ánh nguồn binding, không bảo đảm runtime | TB–Cao | ✅ | `EdgeConfidence` enum tuyến tính (`types.rs:32-58`) trộn nhiều trục | WS-5 |
| A12 | Repo-wide edit lock serialize cả file độc lập + giữ qua reindex | TB | ✅ | `edit.lock` blocking `lock_exclusive` bao chuỗi write→reindex | WS-11 |
| A13 | Một số lock error bị coi như contention | TB | ✅ | classification cần tách `WouldBlock` khỏi permanent error | WS-11 |
| A14 | Brute-force vector scan O(N·d) | TB | ✅ | exact cosine scan BLOB, không ANN | WS-8 |
| A16 | MCP surface lớn (~30 tool), workflow phân mảnh | TB | ✅ | `suggested_next` advisory; gate chỉ nhận đúng `edit_context` | WS-9 |
| A17 | Prompt-injection scanner regex/decode, detection-only | TB | ✅ | `scan_text` (`security.rs`, `SCAN_TEXT_MAX_CHARS=500_000`), "clean không chứng minh safe" | WS-6, WS-12 |
| A18 | Benchmark chủ yếu self-run trên chính CALM repo | TB–Cao | 🕓 | nguy cơ overfit, chưa chứng minh generalization | WS-13 |
| A19 | CI từng treo ~6h trước khi thêm timeout | TB | ✅ | commit `7477721` thêm job timeout + sửa boundary violation | WS-13 |

`*` A05 chỉ Cao khi bind ngoài loopback.

---

## 3. Workstreams (WS) — kế hoạch chi tiết

Mỗi WS: **Vấn đề → Bằng chứng → Thiết kế đích → Pattern VHEATM mượn → Thay đổi CALM cụ thể
(file/schema) → Test → Metric → Done**. Ràng buộc chung (khớp `AGENTS.md` cả hai repo): không
thêm autonomous write/execution mới không có policy+test+rollback+review; provenance là
observability/audit, không nới gate; giữ FNV cho concurrency, thêm crypto hash chỉ cho trust
boundary.

### WS-1 · Durable edit transaction + maintenance outbox (P0)
**Vấn đề (A01, A06).** Edit không phải transaction: disk có thể mới trong khi graph/SCIP/vector
cũ; crash giữa rename→reindex→proof/embed refresh để lại state khó phân biệt; async refresh không
có durable retry.

**Thiết kế đích.** Coi mỗi edit là state machine bền vững + outbox job bền vững cho maintenance.

```sql
CREATE TABLE edit_transactions (
  tx_id TEXT PRIMARY KEY, project_id TEXT NOT NULL, path TEXT NOT NULL,
  base_digest TEXT NOT NULL, proposed_digest TEXT NOT NULL, review_token_id TEXT,
  state TEXT NOT NULL CHECK (state IN (
    'PREPARED','FILE_COMMITTED','INDEX_COMMITTED','PROOFS_PENDING',
    'EMBEDDINGS_PENDING','VERIFY_PENDING','DONE','FAILED','ROLLED_BACK')),
  temp_path TEXT, graph_generation_before INTEGER, graph_generation_after INTEGER,
  created_at REAL NOT NULL, updated_at REAL NOT NULL, error_code TEXT, error_detail TEXT);

CREATE TABLE maintenance_jobs (
  job_id TEXT PRIMARY KEY, tx_id TEXT, job_kind TEXT NOT NULL,
  dedupe_key TEXT NOT NULL UNIQUE, state TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
  available_at REAL NOT NULL, lease_owner TEXT, lease_expires_at REAL, last_error TEXT);
```

**Pattern VHEATM.** Trạng thái transaction = **replayable lifecycle** đúng như `lifecycle.py`
`AuditLifecycle` (state dẫn xuất bằng replay event, mỗi transition có sequence + actor + reason,
`from_document` tái dựng). Mượn nguyên tắc: **state phải replay được từ event log, không được
assert trực tiếp**.

**Thay đổi CALM.**
- Mới `crates/calm-core/src/txn.rs` (state machine) + bảng trên trong `db/schema.rs` (migration
  versioned như `evidence_state`/`graph_generation` đã làm — `schema.rs:375,460`).
- Algorithm commit: canonicalize path → read+digest (WS-3) → `PREPARED` → exclusive-create temp →
  write+fsync temp → base-digest recheck → atomic rename → **dir fsync** → `FILE_COMMITTED` →
  reindex trong DB txn + bump generation → `INDEX_COMMITTED` → ghi durable SCIP/embed/verify jobs
  → chỉ trả `consistency_state=complete` khi lớp bắt buộc xong; async thì trả `tx_id` + pending.
- MCP: `edit_transaction_status(tx_id)`, `retry_maintenance(tx_id, job_kind)`,
  `repair_consistency(path|tx_id)`; startup recovery scan các tx dở.

**Test.** Kill process ở **mọi** transition; corrupt temp; disk-full; SQLite busy; permission
fail; watcher race; provider timeout. Fault-injection suite trên Linux/macOS/Windows.
**Metric.** stuck-tx rate; p95 convergence; startup-recovery count; duplicate-edit retry;
stale-index duration; **zero** "disk changed nhưng tx không biết".
**Done.** Crash suite không để lại divergence không được journal phát hiện.
**Rollout.** shadow (ghi journal, không đổi behavior) → dual-read (status tool đọc journal) →
enforce (mọi write qua transaction).

### WS-2 · State-bound review token + approval độc lập (P0)
**Vấn đề (A02, A03, A20).** Review freshness theo tool-call count (`edit.rs:929`), grounding bằng
substring (`edit.rs:990-1005`), approval là `confirm:true` do chính agent gửi.

**Thiết kế đích.** `prepare_edit` phát **opaque token ký/MAC** bind state thật; `commit_edit` tiêu
thụ một lần; high/critical cần approver-class khác agent hoặc fail-closed.

```text
ReviewTokenPayload {
  token_id, target_path, target_digest, target_symbol_ids[],
  source_range_digest, graph_generation, caller_set_digest,
  evidence_policy_digest, provider_health_digest, proposed_change_digest,
  risk_level, required_approver_class, expires_at, nonce }
```
`reason` không còn substring — nhận **structured evidence**:
```json
{ "reviewed_callers":[{"symbol_id":"repo://…#getUserByToken","impact":"signature-compatible",
   "verification":"unit-test:auth_token"}], "assumptions":[], "verification_plan":["cargo test -p auth"] }
```

**Pattern VHEATM (mượn gần như trực tiếp).** `schemas/approval-token.schema.json`:
`APR-<sha256>`, `single_use:true`, `signature{hmac-sha256,key_id,value}`, `exact_scope:workspace:`,
`request_digest`, `expires_at`, `nonce`, `approved_by`. `tool-receipt.schema.json` bind
`request_digest`+`action_digest`+`decision`+`approval_token_id`. → CALM `ReviewToken` = biến thể
`approval-token` bind thêm `graph_generation`+`caller_set_digest`; `commit_edit` verify+consume
như sandbox verify tool-receipt (`sandbox.py:85-90`).

**Thay đổi CALM.**
- `EditContextReview` (session, tool-call time) → durable/session `PreparedReview` trong DB, bind
  digest; invalidation khi watcher refresh đổi target/caller/generation/provider-health.
- Freshness = **state equality**, không phải call-count. Giữ `cites_token` (`edit.rs:1001-1003`)
  làm fallback tương thích, nhưng đường chính là `acknowledged_positions`/structured evidence
  (đặt tên đúng epistemic: *positionally acknowledged*, **không** "grounded/understood").
- Approval-principal policy tier: Low self-commit · Medium structured token · High independent
  human/policy-bot · Critical human + verification suite · provider degraded/graph incomplete →
  **auto +1 tier**.

**Test.** token rejected khi expired/replayed/scope-mismatch/generation-changed;
`empty_caller_set_still_requires_positional_acknowledgment`.
**Metric.** stale-token rejection rate; human veto rate; false-block rate; % high-risk có
independent approver; post-edit defect escape.
**Done.** Không còn nhánh "any non-empty reason passes" (`edit.rs:995-996`); review không sống sót
qua thay đổi state thật.

### WS-3 · Crypto hashes + filesystem hardening (P0)
**Vấn đề (A08, A06).** FNV-1a cho trust boundary (`pipeline.rs:114-130`); temp path PID-based
không random+O_EXCL; không dir-fsync; perms best-effort; chưa có symlink/hardlink/ACL policy.

**Thiết kế đích.** Dual-hash + FS containment cứng.

**Pattern VHEATM.** `provenance.py` SHA-256 content-address cho identity. → CALM giữ FNV
(`fast_hash`) cho cache/stale-write, thêm `evidence_digest` (BLAKE3 hoặc SHA-256) cho digest ở
trust boundary (review token, tx journal, receipt). **Không** thay cột `file_index.hash`.

**Thay đổi CALM.**
- `pipeline.rs`: thêm `evidence_digest()` (crate `blake3`/`sha2`), streaming; cache theo inode/mtime
  chỉ như optimization.
- `atomic_write` (`edit.rs:477`): random-nonce temp + exclusive create (O_EXCL); **fsync parent
  dir** sau rename (nơi hỗ trợ); metadata-preservation failure surfaced trong high-assurance mode.
- Path policy 3 mode: `reject_symlinks` · `follow_internal_symlinks` · `allow_external_symlinks_with_approval`.
  Linux dùng `openat2` `RESOLVE_BENEATH`/`NO_SYMLINKS`; platform khác canonical parent handle +
  explicit symlink policy. Hardlink count>1 / ACL / special file → phát hiện + surfaced.

**Test.** symlink swap race, `..`, Unicode NFC/NFD, case-insensitive collision, junction/reparse,
hardlink, read-only, exec-bit, ACL, network FS, PID-reuse temp collision.
**Metric.** denied unsafe paths; metadata mismatch; path-normalization failures; post-write digest
mismatch. **Done.** digest collision domain-separated; không silent metadata loss ở high-assurance.

### WS-4 · Provider sandbox + supply-chain provenance (P0)
**Vấn đề (A04, A11, A15).** SCIP/LSP chạy `npx`/package-manager/compiler-plugin — project-controlled
code (post-install hook, proxy attack, resource abuse, exfiltration). Cache key giản lược →
*same source hash, different semantic environment*. Model/tool/binary provenance chưa thống nhất.

**Thiết kế đích.** Provider manifest pinned + sandbox no-network + Merkle cache key + signed artifact.

```yaml
provider_id: scip-typescript
version: 0.x.y
artifact_digest: sha256:...
runtime_image: ghcr.io/...@sha256:...
languages: [javascript, typescript]
network: deny
cpu_limit: 4
memory_limit_mb: 4096
timeout_seconds: 600
read_only_source: true
output_paths: [index.scip]
toolchain_fingerprint_schema: 2
```
Cache key = Merkle digest của {provider image digest, toolchain version, relevant env, full
transitive build-config closure, lockfiles, compiler flags, generated-source manifest, commit/tree
hash, CALM ingest schema version}.

**Pattern VHEATM (mượn trực tiếp — đây là điểm VHEATM vừa vượt lên).** `sandbox.py` là bwrap
**reference monitor** đã fail-closed: bind `policy_decision`+`tool_receipt`+`action_digest`, chặn
khi *"reference monitor has no policy broker"*, yêu cầu POSIX host. `providers.py` fail-closed
transport: `_validate_network_request` ép HTTPS + `scope:workspace:`. `supply-chain-attestation.schema.json`
+ `vulnerability-scan` cho digest/CVE. → CALM mượn cả **mô hình reference-monitor bind action
digest** lẫn **policy fail-closed khi thiếu broker/authorization**; đồng thời tránh lỗi VHEATM
đang mắc (host isolation chưa production-qualified) bằng cách coi host-isolation là điều kiện GA.

**Thay đổi CALM.** Provider execution tách khỏi CALM process (executor kiểu Sourcegraph: lifecycle,
retry, logs, sandbox, resource policy). Ưu tiên thứ tự: (1) ingest prebuilt signed SCIP từ CI →
(2) pinned provider trong sandbox no-network → (3) local auto-detect chỉ trong trusted-dev mode →
(4) cấm unpinned network-fetched package ở high-assurance. Output `.scip` validate size + protobuf
structure + path containment + source digest + provider signature trước ingest.
Rollout: Rust+TS trước → Go, Java, C#, Python, Ruby, PHP, Clang; mỗi provider có **canary** diff
edge-set old/new trước promotion.
**Metric.** provider success/match rate; % formal coverage; stale-proof count; network attempts
blocked; edge diff giữa versions; false formal-edge rate trên golden corpus.
**Done.** không untracked binary execution ở high-assurance; formal coverage không suy ra từ
"binary tồn tại" (bài học PR #23).

### WS-5 · Provenance lattice / evidence ledger (P1)
**Vấn đề (A10).** `EdgeConfidence` tuyến tính (`types.rs:32-58`) trộn nhiều trục: evidence source,
precision, completeness, freshness, dispatch semantics, provider health. Một `call_edges` row mang
trạng thái **tổng hợp** — không append-only, không biểu diễn contradiction.

**Thiết kế đích.** Tách **fact emission** khỏi **fact synthesis** (triết lý Kythe). Evidence
append-only; edge là materialized view từ policy rõ ràng.

```sql
CREATE TABLE resolution_runs ( run_id TEXT PRIMARY KEY, provider TEXT, provider_version TEXT,
  project_digest TEXT, context_digest TEXT, started_at REAL, completed_at REAL, status TEXT,
  coverage_json TEXT );
CREATE TABLE edge_evidence ( evidence_id TEXT PRIMARY KEY, call_site_id INTEGER, candidate_symbol_id TEXT,
  run_id TEXT, evidence_kind TEXT, polarity TEXT CHECK(polarity IN('supports','rules_out')),
  precision_class TEXT, completeness_class TEXT, freshness_state TEXT, dispatch_semantics TEXT,
  payload_digest TEXT, observed_at REAL );
```
Materialized: `binding_status ∈ {contradicted, compiler_verified_unique, compiler_verified_set,
syntactically_resolved, heuristic_candidate, unresolved}`; `coverage_status ∈
{complete_for_compilation_unit, partial, unknown, degraded}`.

**Pattern VHEATM.** `provenance.py` **content-addressed immutable** source/claim/receipt +
`taint_state` + tách epistemic-status khỏi confidence + append-only (đổi nội dung = ID mới). →
CALM mượn: (1) evidence là record bất biến content-addressed, (2) **không downgrade/overwrite**,
chỉ tạo materialized view mới, (3) trust = tích hợp đa trục (đúng §6 kế hoạch VHEATM):
`resolution_confidence × snapshot_state × provider_integrity × coverage_state × compilation_profile
× runtime_reachability × receipt_validation`.

**Thay đổi CALM.** Đổi tên khái niệm `formal` → `compiler_verified_binding`/`semantic_binding`
trong output (giữ enum cột để tương thích; thêm trục mới). MCP/UI trả cả `binding_status`,
`coverage_status`, `freshness`, `provider`, `provider_version`, `why_selected`,
`why_others_rejected`. `Unresolved` (`types.rs:44-57`, hiện *reserved*) trở thành producible qua
`edge_evidence.polarity='rules_out'`.
**Golden corpus.** overload, generics, virtual/interface, macros, decorators, monkey-patching, DI,
generated code, TS path alias, C++ template, mixed-language FFI.
**Metric.** precision/recall theo language×edge-kind; expected calibration error; contradiction
rate; % edges không có completeness evidence; blast-radius decision change khi policy đổi.
**Done.** mọi served edge có evidence source + freshness + completeness + contradiction state.

### WS-6 · Verification pipeline nhiều tầng (P1)
**Vấn đề (A07, A17, A09).** `validate_syntax_diff` (`edit.rs:459`) chỉ đếm ERROR/MISSING; `None`
grammar → allow write; bỏ sót type/borrow/macro/semantic regression. Prompt-injection scanner
detection-only.

**Thiết kế đích.** Tree-sitter chỉ là **Tier 0**.

| Tier | Check | Budget |
|---|---|---:|
| Fast | parse diff, formatter dry-run, touched-file lint | <1–3s |
| Semantic | compiler/typecheck scoped theo package | <10–30s |
| Security | Semgrep targeted rules, secret scan | <10–30s |
| Deep | CodeQL/path queries, full+integration tests | async/CI |
| Runtime | selected tests từ coverage/call-impact mapping | theo policy |

MCP: `verify_change(tx_id, profile)`, `verification_status(run_id)`, `findings_for_change(tx_id)`.
High-risk tx ở `VERIFY_PENDING` (WS-1), **không** `DONE`/PR-ready attestation cho tới khi
verification đạt policy.

**Pattern VHEATM.** `validation-receipt.schema.json` + `taint_state` tainted→validated: một claim
"verified" trên nguồn tainted **bắt buộc** validation receipt (`report_validator.py:291-317`). →
CALM: kết quả verification là **validation receipt** gắn vào edit transaction; touched region
"cleared" chỉ khi có receipt tương ứng. Ingest **SARIF** (Semgrep/CodeQL) thay vì tự viết taint
engine.
**Metric.** compile-failure caught pre-commit; security finding density; FP suppression rate;
median feedback time; test-selection recall; defects CI bắt nhưng local bỏ sót.
**Done.** high-risk không đạt `DONE` khi thiếu verification receipt; file không grammar không còn
mặc nhiên "safe".

### WS-7 · Dependency-aware incremental + generation snapshots (P1)
**Vấn đề (A09, A11).** Threshold cứng (50-path fallback, cap 20) không phản ánh cost thật; reader
có thể thấy graph lai.

**Thiết kế đích.** Invalidation graph (config/build input là node riêng) + work estimator +
MVCC/RCU generation.
```text
estimated_cost = source_bytes*lang_parse_factor + changed_symbols*resolver_factor
               + reverse_dependents*graph_factor + changed_chunks*embedding_factor
```
Reader pin `graph_generation`; writer tạo generation mới, **atomically publish pointer** sau khi
mọi mandatory graph table commit.

**Pattern VHEATM.** deterministic byte-budget + content-address bundle → snapshot đa chiều
(P0-2): thay `indexing_phase` đơn trị (`common.rs:503`) bằng per-provider/overlay state
(`base_state`, `scip_overlay`, `lsp_overlay`, `embeddings`, `coverage` per-language, `limitations`).
CALM đã có `graph_generation_state` (`schema.rs:147`) làm nền RCU.
**Metric.** p50/p95/p99 refresh latency; invalidated-to-actually-changed ratio;
full-reconciliation rate; reader generation age; queue depth; CPU/changed-KB; memory peak.
**Done.** reader không bao giờ thấy graph lai; full-fallback rate giảm.

### WS-8 · ANN hybrid search + model registry (P1)
**Vấn đề (A14, A15).** Exact cosine O(N·d) không scale; ONNX encode per-text loop; model provenance
là prefix `onnx:<dir>`.

**Thiết kế đích.** Giữ exact cho repo nhỏ, tự chuyển HNSW/disk-ANN sau threshold; hybrid
lexical(BM25/FTS/trigram) + symbol-exact + ANN-semantic + graph-proximity + optional cross-encoder
rerank; exact fallback + recall calibration + `ef_search` cấu hình.
```sql
CREATE TABLE embedding_models ( model_id TEXT PRIMARY KEY, artifact_digest TEXT, tokenizer_digest TEXT,
  dimensions INTEGER, max_tokens INTEGER, pooling TEXT, normalization TEXT, license TEXT, origin TEXT,
  created_at REAL );
```
Vector row có `model_id`+`content_digest`+generation; đổi model không xóa vector cũ tới khi index
mới atomically promoted.

**Pattern VHEATM.** `supply-chain-attestation.schema.json` digest-pinning → model là
data-supply-chain component: SHA-256/BLAKE3 digest, license, origin, signed manifest, **policy
cấm network ở air-gapped mode** (sửa mặc định `allow_network_fallback:true` — `config.rs:657` — ít
nhất phải cảnh báo/opt-in ở high-assurance).
**Eval.** CodeSearchNet-style external corpus + internal expert queries + adversarial metamorphic
(rename, dead-code, comment injection, identifier obfuscation — bài học ContraBERT).
**Metric.** Recall@5/10/50, MRR, nDCG@10, p95 latency, index size, build time, rename robustness,
result stability. **Done.** scale tới ≥1M LOC với recall công bố.

### WS-9 · Workflow-oriented MCP + tách trust boundary (P1)
**Vấn đề (A16).** ~30 tool, workflow phân mảnh; `suggested_next` advisory; gate chỉ nhận
`edit_context`; session memory reset khi restart.

**Thiết kế đích.** Public default = ít tool cấp workflow; primitive → `expert` toolset.
```text
orient_repository · search_code · understand_symbol · analyze_change
prepare_edit · commit_edit · verify_change · transaction_status
```
Mọi response chuẩn hoá: `{data, generation, freshness, coverage, consistency_state, next_actions,
warnings, trace_id}` + pagination + cancellation + byte/token budget + schema versioning + typed
error codes. Mỗi finding có feedback `useful/not_useful/incorrect/already_known` (bài học Tricorder).

**Pattern VHEATM.** tách MCP theo trust boundary (§7 kế hoạch VHEATM): `control` (read-only) ·
`provider` (bounded) · `action` (privileged, cần capability declaration + approval-token). CALM
đứng tầng **provider read-only** trước (adapter: `snapshot`, `callers`, `callees`, `diff_impact`,
`session_context` + capability handshake khai báo languages/coverage/overlay/provider-integrity),
tầng **action** sau khi evidence contract ổn định.
**Metric.** median MCP calls/task; response tokens/task; tool-selection error; repeated-call ratio;
abandoned edit workflow; time-to-first-useful-result; human override rate.
**Done.** cross-SDK tests cover mọi preset + schema evolution; read-only adapter không lộ write
capability.

### WS-10 · HTTP transport hardening (P0 nếu remote)
**Vấn đề (A05).** bearer `p == token` không constant-time (`http.rs:92`); policy read-only quyết ở
CLI (`resolve_http_launch`) không phải server module; không peer-IP audit; không TLS/mTLS/scope/rate-limit.

**Thiết kế đích.** Defense-in-depth enforce ở **server handler**, không chỉ CLI launcher. Loopback
mặc định; TLS profile built-in hoặc reverse-proxy template; constant-time token compare; peer
address vào audit ledger (WS-1/WS-5); rate limit + scope-based authorization.

**Pattern VHEATM.** approval-token verification ở transport (P2-1): request privileged qua HTTP
mang scoped single-use signed token; server verify trước khi chạy. `providers.py` fail-closed
transport là hình mẫu "validate trước khi cho qua".
**Done.** read-only enforced ở handler; token compare constant-time; audit có peer address.

### WS-11 · Concurrency: per-path lock, lease, error classification (P1)
**Vấn đề (A12, A13, A20).** Repo-wide `edit.lock` serialize cả file độc lập + giữ qua reindex;
`try_lock` error có thể bị quy contention; `reviewing_symbol` advisory không lease.

**Thiết kế đích.** Per-path lock ordering ổn định cho multi-file edit; **commit lock** ngắn chỉ
quanh final hash recheck + atomic rename; index update qua serialized durable queue (WS-1 outbox);
snapshot generation cho reader (WS-7). Error classification giữ `WouldBlock` riêng, permanent error
(FD/permission/I-O) **surfaced** (`edit_lock.rs` doc đã đúng hướng — mở rộng cho instance_lock).
Nâng `reviewing_symbol` từ advisory → optional **lease/reservation** (soft, opt-in) để hai agent
không cùng chuẩn bị cùng symbol.
**Metric.** lock wait p99; multi-file edit throughput; misclassified-error count; concurrent-prepare
collisions. **Done.** file độc lập không serialize lẫn nhau; permanent error không bị che.

### WS-12 · Untrusted content: taint, không phải gate (P1)
**Vấn đề (A17).** Scanner regex/decode detection-only; content từ source code có thể biến thành
instruction gọi write tool.

**Thiết kế đích.** Scanner là **signal**, không phải gate. Origin-taint metadata xuyên MCP response;
output redaction; secret scanner chuyên dụng; **capability firewall** ngăn content-origin=source
trở thành instruction có quyền write.

**Pattern VHEATM.** `taint_state` tainted→validated qua receipt + tách epistemic status. CALM đã có
`wrap_untrusted`/`<untrusted-external-content>` (ADR-0006, `security.rs:44,68`) — mở rộng thành
**taint propagation** gắn origin vào mọi response và chặn ở ranh giới capability, không chỉ wrap
văn bản. **Done.** content origin=untrusted không thể trực tiếp trigger write path.

### WS-13 · Release qualification + doc-drift gate + benchmark rigor (P1)
**Vấn đề (A18, A19).** Benchmark self-run trên chính CALM repo (overfit); CI từng treo ~6h.

**Thiết kế đích.** (a) **Một job qualification chặn publish**: gom fitness-check, clippy-D,
cargo-audit, stack-graphs regression, SDK interop, external-corpus benchmark thành `qualify-release`;
`release.yml` mọi publish job `needs: qualify-release`. (b) `status.generated.md` sinh từ nguồn
khả thi hành (tool inventory, feature flags, language coverage, write-path taxonomy); CI kiểm
no-drift. (c) External benchmark corpus (CodeSearchNet-style) tách khỏi self-repo; per-job timeout
+ watchdog + flaky quarantine (đã bắt đầu ở `7477721`).

**Pattern VHEATM.** RG-00..RG-15 registry (`evaluation.py:53-70`) = evidence-gated publish barrier;
generated status chống doc-drift (bài học chính VHEATM đang drift: README "pilot" vs registry
"complete"). CALM áp cùng khuôn: qualification report content-addressed, publish `needs` nó.
**Done.** publish không chạy nếu qualification chưa pass; docs sinh tự động không drift.

### WS-14 · Multi-repo immutable snapshots + global symbol IDs (P2, defer)
**Vấn đề (schema).** `call_edges.from_symbol/to_symbol` là text qualified-name, không phải FK tới
stable semantic node (`schema.rs`); single project root. Cản rename/overload/cross-language/multi-repo.

**Thiết kế đích.** `repository_id · commit_id · compilation_unit_id · semantic_node_id ·
artifact_generation · dependency_repository_id · package_coordinate`. Index immutable per commit;
serving chọn nearest compatible snapshot; cross-repo dùng URI kiểu **Kythe VName**/SCIP symbol,
không path-local. Local write-safety chỉ cho working checkout; central multi-repo **read-only**.

**Pattern VHEATM.** stable content-addressed IDs (P1-1): edge identity =
`SHA-256(snapshot_id ‖ caller_id ‖ callee_id ‖ callsite_path ‖ callsite_span ‖ provider ‖ confidence)`.
**Migration.** thêm `repository_id`/`commit_id` nullable → backfill local repo → dual-write stable
IDs → chuyển query sang IDs (giữ text làm display) → immutable snapshot store → shard lexical/graph/
vector độc lập → pilot 5–20 repo. **Anti-goal:** không ép multi-repo lên local user; giữ
single-binary SQLite cho local mode.

---

## 4. Kiến trúc tích hợp CALM ↔ VHEATM (giữ từ kế hoạch VHEATM, là "đỉnh" của mọi WS)

Khi CALM đủ trưởng thành (sau WS-1..WS-6), bind với VHEATM authority theo **dual-authority
fail-closed** + **prepare–authorize–execute–verify**:

```
VHEATM deny → deny · VHEATM unknown → block · CALM deny → deny · CALM stale → block+replan
chỉ khi CẢ HAI allow → execute
```
```
1 CALM capture snapshot S           5 CALM PREPARE: verify S còn hiện hành, re-eval local gate
2 CALM query/context receipt Q(S)   6 CALM EXECUTE atomic
3 VHEATM validate Q → plan P        7 CALM action receipt R (A-id, before/after SHA-256, parse,
4 VHEATM one-time authorization A       reindex, overlay states, postcondition)
  bound S+P+action_digest           8 VHEATM validate R → lifecycle/completion
Snapshot đổi giữa (4)-(5) ⇒ SNAPSHOT_MISMATCH ⇒ authorization consumed ⇒ replan (không dùng lại A cũ).
```
WS-1 (transaction), WS-2 (review token), WS-5 (evidence ledger) chính là các mảnh CALM cần để phát
`R` mà VHEATM validate được. **Điều kiện phụ thuộc:** VHEATM phải tự đóng 3 lỗ (MCP/CLI report
parity, release gates on publish path, doc/package drift) trước khi authorization của nó đáng bind
— chi tiết ở §8 file kế hoạch VHEATM.

---

## 5. Phasing, roadmap, milestone gates

**P0 — điều kiện "write-capable infrastructure" (song song được):**
WS-1 transaction journal · WS-2 review token/approval · WS-3 crypto+FS hardening · WS-4 provider
sandbox · WS-10 HTTP hardening (nếu remote).

**P1 — correctness & scale:**
WS-5 provenance lattice · WS-6 verification pipeline · WS-7 incremental+snapshots · WS-8 ANN+registry
· WS-9 workflow MCP · WS-11 concurrency · WS-12 taint · WS-13 release qualification.

**P2 — platform:** WS-14 multi-repo.

**Roadmap (từ 2026-08, giả định 3–5 kỹ sư; 1 maintainer thì kéo dài đáng kể):**

| Giai đoạn | Hạng mục | Cửa (milestone gate) |
|---|---|---|
| 08–10/2026 | Threat model + golden corpus; WS-1 shadow; WS-2; WS-3 | **Write-Safety Beta:** crash suite 0 divergence không-journal; mọi high-risk edit dùng state-bound token |
| 10/2026–01/2027 | WS-4 (Rust+TS), durable outbox, WS-5 ledger v2, provider canary | **Provider-Security Beta:** Rust+TS no-network sandbox, artifacts pinned, 0 untracked binary exec |
| 12/2026–02/2027 | WS-7 invalidation+snapshots, WS-8 ANN beta, external benchmark | **Scale Beta:** p95 incremental đạt SLO trên ≥1M LOC; ANN Recall@10 đạt target |
| 01–04/2027 | WS-6 (Semgrep/compiler/SARIF → CodeQL async), WS-9 MCP, WS-10, cross-client approval | **Verification Beta:** findings bind vào edit transaction; deep checks có durable lifecycle |
| 03–07/2027 | WS-14 identity + read-only pilot; chaos+security review | **Production RC:** cross-platform chaos, external security review, migration test từ 0.4.x, runbook |
| song song | WS-13 | **Provenance v2:** mọi served edge trả source+freshness+completeness+contradiction |

---

## 6. SLO/KPI & governance

**SLO/KPI** (theo lĩnh vực): Correctness (precision/recall theo language×edge-kind, stale-proof
rate, contradiction rate) · Freshness (p95 fs-event→published generation, max generation lag) ·
Write-safety (lost-update incidents, stale-review rejection, crash-recovery success, unfinished-tx
age) · Security (sandbox violations, blocked network, unsafe-path attempts, auth failures) · Search
(Recall@10, MRR, nDCG@10, rename robustness, p95 query) · Operations (full-reconciliation rate,
queue depth, provider failure, CI flake, MTTR) · UX (calls/task, tokens/task, veto rate, finding
usefulness, abandonment) · Quality (defects pre-PR/CI/post-merge, FP rate, escaped regression).

**Governance release:** SemVer nghiêm cho MCP schema + DB migration; reproducible build manifest
(binary/grammar/model/provider image); security policy + coordinated disclosure; migration dry-run
+ backup trước schema change; canary trên corpus đa ngôn ngữ; compatibility window ≥2 minor cho MCP
client cũ; public benchmark methodology + raw results + corpus version; **mọi claim "formal/safe/
atomic/compiler-accurate" phải có định nghĩa operational + test tương ứng** (khớp bài học A10).

**Rủi ro khi triển khai (tóm tắt, đầy đủ ở audit §"Rủi ro"):** transaction journal → shadow mode +
batched WAL + recovery tests trước enforce; crypto digest → streaming BLAKE3, cache như optimization;
review token → risk-tier + fast refresh + override có audit; provider sandbox → per-language images
+ warm cache + CI-SCIP preferred; evidence ledger → append-only retention + compaction + content
dedup; ANN → exact fallback + recall calibration + dual-run canary; MCP consolidation → versioned
schema + compatibility adapter + deprecation window; multi-repo → tách feature, central read-only,
local vẫn single-binary SQLite.

---

## 7. Anti-goals (đừng làm)

- **Đừng** biến CALM thành audit framework đầy đủ; chỉ mượn provenance/receipt/lifecycle/sandbox
  pattern từ VHEATM.
- **Đừng** thay mọi FNV nội bộ bằng crypto hash — giữ FNV cho concurrency/cache (WS-3).
- **Đừng** tuyên bố evidence acknowledgment chứng minh agent "hiểu" (WS-2).
- **Đừng** gọi tree-sitter diff là "verification" hay tier yếu là "compiler proof" (WS-5, WS-6).
- **Đừng** cho một "allow" bên nào bypass invariant bên kia; không tạo distributed monolith hai
  authority (§4).
- **Đừng** ép multi-repo lên local user; giữ local-first simplicity (WS-14).
- **Đừng** thêm write/execution autonomous mới mà không có policy + test + rollback + review.
- **Đừng** để CI benchmark chỉ self-run làm bằng chứng generalization (WS-13).

---

## 8. Traceability (mọi finding → workstream)

- **Audit CALM-A01..A20:** A01→WS-1 · A02→WS-2 · A03→WS-2/WS-10 · A04→WS-4 · A05→WS-10 ·
  A06→WS-1/WS-3 · A07→WS-6 · A08→WS-3 · A09→WS-6/WS-7 · A10→WS-5 · A11→WS-4/WS-7 · A12→WS-11 ·
  A13→WS-11 · A14→WS-8 · A15→WS-4/WS-8 · A16→WS-9 · A17→WS-6/WS-12 · A18→WS-13 · A19→WS-13 ·
  A20→WS-2/WS-11.
- **VHEATM adoption (file kèm):** P0-1→WS-3 · P0-2→WS-7 · P0-3(receipts)→WS-1 · P0-4(ledger)→WS-1/WS-5
  · P0-5(acknowledgment)→WS-2 · P0-6(lifecycle)→WS-1 · P1-1(stable IDs)→WS-14 · P1-2(read-only
  adapter)→WS-9 · P2-1(approval token)→WS-2/WS-10 · P2-2(dual authority)→§4 · P3-1(release
  qualification)→WS-13 · P3-2(doc-drift)→WS-13.
- **VHEATM primitives mượn:** `lifecycle.py`→WS-1 · `provenance.py`→WS-5 · `approval-token.schema.json`
  →WS-2 · `tool-receipt.schema.json`→WS-1/WS-2 · `sandbox.py`+`providers.py`→WS-4 ·
  `validation-receipt.schema.json`+`taint_state`→WS-6/WS-12 · `supply-chain-attestation.schema.json`
  →WS-8 · RG registry→WS-13 · MCP trust-split→WS-9.

---

## 9. Kết luận

Hai nguồn độc lập hội tụ vào cùng kết luận: **CALM là hạ tầng nghiêm túc, pilot-ready, nhưng chưa
đủ để trao quyền write tự trị cho repo quan trọng.** Con đường ngắn nhất tới "trusted write
authority" không phải thêm ngôn ngữ hay tăng recall — mà là **đóng vòng provenance của hành động**:
biến edit thành transaction bền vững (WS-1), review thành token bind-state có approver độc lập
(WS-2), hash trust-boundary thành crypto + FS hardening (WS-3), provider thành sandbox pinned
(WS-4). Bốn việc này là P0 và **ba trong bốn có pattern VHEATM mượn gần như trực tiếp** — đó là giá
trị lớn nhất của việc ghép hai dự án. Sau P0, provenance lattice (WS-5) và verification pipeline
(WS-6) nâng correctness; incremental/ANN/MCP nâng scale & UX; multi-repo là chân trời P2. Toàn bộ
được đóng khung bởi một invariant duy nhất, đúng tinh thần cả hai repo:

> Không claim "safe/atomic/formal/complete" nào được phép tồn tại nếu không có một record bất biến,
> replay được, kiểm chứng được đứng sau nó — và không một "allow" nào được bypass invariant của lớp
> thực thi.
