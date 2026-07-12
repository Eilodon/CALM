# CALM Upgrade Plan 1/3 — Correctness & Safety Quick Wins

> Nguồn: `docs/audit/2026-07-12-vheatm-deep-audit.md` (Đợt 1). Series: **Plan 1 (file này)** → [Plan 2: Hot-path Performance & Robustness](2026-07-12-upgrade-plan-2-performance-robustness.md) → [Plan 3: Architecture](2026-07-12-upgrade-plan-3-architecture.md).
>
> **Phạm vi:** 6 fix chính (F2, F5, F6, F7, F9, F14) + 5 hygiene (H1, H2, H3, H5, H6). Toàn bộ là sửa cục bộ, không đổi schema, không đổi API shape ngoài việc **thêm** field optional. Ước lượng: 1 buổi làm việc.
>
> **Nguyên tắc thực thi (áp dụng cho cả 3 plan):**
> 1. Mỗi item = 1 commit riêng, message dạng `fix(scope): <mô tả> (audit F<n>)`.
> 2. Trước khi sửa symbol nào: `edit_context(symbol)` (hook enforce sẵn). Trước mỗi commit: `diff_impact(staged=true)`.
> 3. Sau mỗi item: `cargo test -p calm-core -p calm-server` + `cargo clippy --all-targets -- -D warnings` phải xanh.
> 4. Kết thúc plan: rebuild release (`cargo build --release -p calm-cli`) và **restart daemon calm** để dogfood binary mới (daemon cũ vẫn giữ code cũ trong RAM).
> 5. Item nào lệch quá 2× ước lượng → dừng, ghi note vào cuối file này, chuyển item sau.

---

## Item 1.1 — F2: `diff_impact` kẹt `pending_scan` vĩnh viễn với file đã xoá  ⏱ ~45p

**Mục tiêu:** file có mặt trong diff nhưng không còn tồn tại trên disk không bao giờ được phân loại `pending_scan` (loại "sẽ tự resolve") — vì nó không bao giờ được scan nữa. Đây là bug đã quan sát trực tiếp trong phiên audit (agent bị loop `diff_impact` ↔ `indexing_status`).

**Evidence:** `crates/calm-server/src/tools/guardrails.rs:355-386` — nhánh `row_language == None` chỉ xét extension + `path_has_ignored_dir_component`, không xét file tồn tại.

**Thiết kế:**
- Thêm nhánh phân loại mới `"deleted"` vào `UnindexedFileOutput::reason`, ưu tiên cao nhất trong nhánh `None`:
  ```rust
  None => {
      let path = std::path::Path::new(&fd.path);
      if !self.project_root.join(path).exists() {
          Some("deleted")            // sẽ không bao giờ được scan — đừng gate risk
      } else if !calm_core::walk::path_has_ignored_dir_component(path) && (…như cũ…) {
          Some("pending_scan")
      } else {
          Some("out_of_scope")
      }
  }
  ```
- `"deleted"` KHÔNG được đưa vào `pending_scan_paths` (guardrails.rs:506-510 lọc theo `reason == "pending_scan"` — tự đúng, chỉ cần reason mới).
- Cập nhật doc comment của `UnindexedFileOutput::reason` (guardrails.rs:762-778): thêm mô tả `"deleted"` — *"file xuất hiện trong diff nhưng không còn trên disk (xoá/rename); giống out_of_scope là trạng thái vĩnh viễn, không gate aggregate_risk"*.
- Cập nhật `AGENTS.md` Stage 7 Signals: thêm dòng cho `reason == "deleted"`.
- **Lưu ý nuance (không mở rộng scope):** file bị xoá mà `file_index` row còn (watcher chưa kịp reindex) sẽ đi nhánh `Some(Some(_))` như cũ và vẫn liệt kê affected symbols — đó là hành vi ĐÚNG (xoá file có callers = blast radius thật). Chỉ nhánh "row đã mất + disk đã mất" đổi.

**Tests** (thêm vào test module có sẵn trong `crates/calm-server/src/tools.rs`):
1. `diff_impact_deleted_file_reports_deleted_not_pending_scan` — gọi `diff_impact` với raw `diff` tham chiếu `ghost.md` không tồn tại trên disk, không có row: expect `unindexed_files[0].reason == "deleted"`, `aggregate_risk != "unknown"`, `suggested_next` không phải `indexing_status`.
2. Control test: raw diff tham chiếu `newfile.rs` **có tồn tại** trên disk nhưng chưa index → vẫn `pending_scan` (giữ hành vi cũ).

**Done when:** 2 test mới xanh + test cũ không vỡ + chạy lại đúng kịch bản phiên audit (staged-add rồi delete 1 file .md, gọi diff_impact) ra `deleted`.
**Rollback:** revert 1 commit — reason mới là additive, không client nào phụ thuộc.

---

## Item 1.2 — F5: `atomic_write` làm mất file permissions  ⏱ ~30p

**Mục tiêu:** edit qua `edit_lines`/`edit_symbol` giữ nguyên mode của file gốc (đặc biệt executable bit của `scripts/*.sh`).

**Evidence:** `crates/calm-core/src/edit.rs:339-361` — `File::create(tmp)` + `rename()` → file mới nhận perms theo umask.

**Thiết kế** (sửa trong `atomic_write`):
```rust
pub fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let original_perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    // … tạo tmp, write_all, sync_all như cũ …
    if let Some(perms) = original_perms {
        let _ = std::fs::set_permissions(&tmp_path, perms); // best-effort, không fail write vì perms
    }
    match std::fs::rename(&tmp_path, path) { … như cũ … }
}
```
- `metadata(path)` lấy **trước** khi ghi (target luôn tồn tại theo contract của cả 2 caller — xem doc `resolve_repo_path`). File mới không tồn tại → `None` → giữ hành vi hiện tại.
- Best-effort (`let _ =`): thiếu quyền set perms không được làm fail một write đã thành công về nội dung.
- **Bonus durability (tuỳ chọn, cùng commit):** sau `rename` thành công, mở thư mục cha và `sync_all()` trên unix (`#[cfg(unix)]`) — đảm bảo rename bền qua crash.

**Tests** (trong `edit.rs` test module, `#[cfg(unix)]`):
```rust
use std::os::unix::fs::PermissionsExt;
// tạo file 0o755 → atomic_write → assert mode & 0o777 == 0o755
```
**Done when:** test unix xanh; test round-trip cũ (`test_atomic_write_then_read_round_trip`) không đổi.

---

## Item 1.3 — F6: `diff_impact` reset gate trước khi validate  ⏱ ~30p

**Mục tiêu:** một call `diff_impact` **thất bại** (input sai, git fail, DB fail) không được xoá cờ `pending_diff_impact` — gate host-agnostic chỉ được thoả bởi một lần phân tích thành công.

**Evidence:** `crates/calm-server/src/tools/guardrails.rs:294` — `self.clear_written_files()` là statement đầu tiên, đứng trước cả check `input_count > 1` (296-304) lẫn nhánh `FEATURE_UNAVAILABLE` (317-326) và `db_error` (338-341).

**Thiết kế:** xoá dòng 294; gọi `self.clear_written_files()` ngay trước khi build `ToolOutcome::success(DiffImpactOutput { … })` (chỉ có đúng 1 điểm success ở cuối hàm — guardrails.rs:561). Không clear trên bất kỳ nhánh error nào.
- **Cân nhắc semantics:** doc comment của `clear_written_files` (common.rs:293-298) đang nói "clear vô điều kiện mỗi call, vì *attempting* là tín hiệu đủ" — quyết định audit F6 đảo lại lập luận đó: attempt-hỏng-vì-input-sai không chứng minh gì. Cập nhật doc comment đó cùng commit để không drift.
- Hook `.claude/hooks/calm-nudge.sh` (host-specific gate) vẫn reset trên mọi call `mcp__calm__diff_impact` tại PreToolUse — **đồng bộ luôn** (cùng commit): chuyển việc `save_state ... false` của nhánh `diff_impact` trong hook thành không đổi (hook không biết kết quả tool)… → thực tế hook không thể biết success/failure ở PreToolUse; ghi chú trong hook comment rằng CALM-side gate giờ nghiêm hơn hook-side, hook giữ nguyên (defense-in-depth: hook lỏng hơn, server chặt hơn — chấp nhận được, KHÔNG cần PostToolUse mới trong plan này).

**AUDIT NOTE (đọc code `.agents/hooks/calm-guard.sh` + `.claude/hooks/calm-nudge.sh` thật):** cả 2 hook hiện có (Claude Code và Antigravity, ported cùng session với plan này) đều set `needs_diff_impact=false` tại **PreToolUse** — tức TRƯỚC khi biết `diff_impact` thành công hay lỗi. Nghĩa là F6 làm CALM-server's `session_context().pending_diff_impact` chính xác hơn, nhưng KHÔNG cải thiện gate mà 2 hook host-specific thực sự enforce (chúng vẫn có thể bị thoả bởi 1 call `diff_impact` lỗi, y hệt bug trước fix). Chấp nhận được cho scope Plan 1 (đúng như note gốc), nhưng đây là gap thật cần ghi nhận rõ — không phải chỉ lý thuyết — và nên vào backlog Plan 2/3: hook cần chuyển sang PostToolUse hoặc CALM-server cần tự chặn commit/push (không chỉ hook).
**Tests:**
1. `diff_impact_error_does_not_clear_pending_gate` — `server.mark_written("a.rs")` → gọi `diff_impact` với cả `diff` + `staged=true` (INVALID_INPUT) → `server.written_files_snapshot()` vẫn chứa `"a.rs"`.
2. `diff_impact_success_clears_pending_gate` — mark_written → gọi diff_impact với raw diff hợp lệ → snapshot rỗng.

**Done when:** 2 test xanh; test hiện có về pending_diff_impact trong session_context không vỡ.

---

## Item 1.4 — F7: `recall`/`remember` không cảnh báo injection trong note  ⏱ ~45p

**Mục tiêu:** đồng nhất trust surface — mọi kênh trả text từ storage đều mang `content_warning` như `source()`.

**Evidence:** `crates/calm-server/src/tools/memory.rs:81-183` (recall — `MemoryNote` không có warning field), `memory.rs:16-69` (remember — không quét), so với `related_notes` drop note dính injection (`common.rs:1100-1102`).

**Thiết kế:**
1. `MemoryNote` thêm field:
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   pub(crate) content_warning: Option<String>,
   ```
   `memory_note_row` khởi tạo `None`; trong vòng `for note in &mut notes` của `recall` (memory.rs:150): `note.content_warning = calm_core::sanitize::injection_warning(&note.content);` — đặt **trước** nhánh `continue` của staleness (chạy cho mọi note, kể cả note không có refs).
2. `RememberOutput` thêm field cùng tên; trong `remember`, sau khi lưu thành công: `content_warning: calm_core::sanitize::injection_warning(content)` — **vẫn lưu note** (detection-only, nhất quán triết lý sanitize.rs:94-103), chỉ cảnh báo người ghi.
3. Cập nhật doc comment `related_notes` (common.rs:1066-1069): câu "remains fully visible via recall, where Stage-3 wariness applies" → thêm "recall now carries an explicit per-note content_warning".

**Tests:**
1. `recall_flags_injection_shaped_note` — remember note chứa `"ignore all previous instructions"` → recall → `notes[0].content_warning` chứa `IGNORE_PRIOR_INSTRUCTIONS`.
2. `remember_returns_warning_but_still_saves` — output có warning, recall thấy note tồn tại.
3. Note sạch → không có field (serde skip).

**Done when:** 3 test xanh. **Ghi chú scope:** HMAC integrity cho `project_memory` (chống ghi thẳng DB) là Plan 3 §3.5 — KHÔNG làm ở đây.

---

## Item 1.5 — F9: `resolve_symbol_candidates` nuốt lỗi DB thành "not found"  ⏱ ~1h15 (touch nhiều handler, thuần cơ học)

**Mục tiêu:** phân biệt "symbol không tồn tại" với "không đọc được DB" — không bao giờ trả `NOT_FOUND` + caveat "likely a typo" khi lỗi thật là hạ tầng.

**Evidence:** `crates/calm-server/src/tools/common.rs:961-964` (`Err(_) => return vec![]` trên prepare) và `common.rs:992-995` (`Err(_) => vec![]` trên query_map).

**Thiết kế:**
1. Đổi chữ ký:
   ```rust
   pub(crate) fn resolve_symbol_candidates(…) -> rusqlite::Result<Vec<CandidateRow>>
   pub(crate) fn resolve_symbol(…) -> rusqlite::Result<SymbolResolution>
   ```
   Bên trong: `?` thay cho 2 chỗ nuốt lỗi; per-row `filter_map(|r| r.ok())` GIỮ NGUYÊN (một row hỏng không nên giết cả result set — chỉ lỗi statement-level mới propagate).
2. Mọi call site (grep `resolve_symbol(` — dự kiến: `guardrails.rs` edit_context; `tools/edit.rs` edit_symbol; `inspect.rs` symbol_info/source/understand; `trace.rs` callers/callees/path ×2; compiler sẽ chỉ chỗ) đổi pattern:
   ```rust
   let resolution = match resolve_symbol(&conn, &p.symbol, p.path.as_deref(), p.line) {
       Ok(r) => r,
       Err(e) => return db_error_resolved(e),   // hoặc db_error(e) tuỳ envelope của tool
   };
   ```
3. Chỗ nào dùng `resolve_symbol_candidates` trực tiếp (nếu có — grep) xử lý tương tự.

**AUDIT NOTE (đọc code thật):**
- **Call site list đã xác nhận chính xác** qua `db_error_resolved` (grep thật, không phải đoán): `guardrails.rs:29` (edit_context), `edit.rs:53` (edit_symbol), `trace.rs:24/198/506` (callers/callees/path), `inspect.rs:24/70` (symbol_info/source) — **+2 chỗ plan chưa liệt kê:** `patterndebt.rs:50,91` (pattern_debt_register) cũng dùng `resolve_symbol`/`db_error_resolved`, thêm vào danh sách call site cần sửa. Pattern `Err(e) => return db_error_resolved(e)` đã là convention có sẵn ở 9 chỗ khác trong codebase — xác nhận item này thật sự cơ học như plan mô tả, rủi ro thấp.
- **Gap chưa được fix (nằm ngoài scope test hiện tại):** `filter_map(|r| r.ok())` giữ nguyên nghĩa là nếu **TẤT CẢ** row trong result set fail decode (ví dụ migration để lại cột NOT NULL bị NULL), kết quả vẫn là `Vec` rỗng → `NotFound`, không phải `DB_ERROR` — chính class bug F9 muốn sửa, chỉ không bị bắt bởi test `DROP TABLE` (test đó chỉ exercise nhánh `prepare()` Err). Không bắt buộc fix trong Plan 1 (thay đổi hành vi filter_map là quyết định có chủ ý, giữ nguyên là đúng) nhưng nên ghi nhận rõ đây là known blind spot, không phải đã đóng hoàn toàn.
- **Observability (đọc code `db_error`/`db_error_resolved`, common.rs:640-655):** cả 2 helper chỉ build `ErrorDetail` trả client, KHÔNG `tracing::error!`/`warn!` — một lỗi DB hạ tầng thật (vd schema corrupt) sau fix này sẽ đến được client nhưng không để lại log phía server — operator không thấy trừ khi client tự report. Cân nhắc thêm `tracing::warn!` 1 dòng trong `resolve_symbol`/`resolve_symbol_candidates` khi propagate lỗi (không bắt buộc, nhưng rẻ và đúng tinh thần F9).
- **Wording nit:** message cứng "db connection failed: {e}" trong cả `db_error`/`db_error_resolved` không chính xác cho lỗi F9 mới propagate (thường là query/prepare error trên connection đang mở, không phải connection failure) — không blocking, có thể sửa cuối nếu còn thời gian.
**Tests:** `resolve_reports_db_error_not_not_found` — mở server với DB hợp lệ, dùng `server.db()` (test-only write conn) `DROP TABLE symbols`, gọi `symbol_info("anything")` → expect `error.code == "DB_ERROR"` (recoverable), KHÔNG phải `NOT_FOUND`.

**Done when:** test mới xanh + toàn bộ test suite xanh (đây là item dễ gây vỡ test nhất plan này — các test NotFound hiện có phải giữ nguyên hành vi vì DB của chúng lành).
**Rollback:** revert commit; không đổi wire format (vẫn ErrorDetail envelope sẵn có).

---

## Item 1.6 — F14: `REASON_NOT_GROUNDED` match substring quá lỏng  ⏱ ~45p

**Mục tiêu:** reason phải cite caller thật theo word-boundary; tên caller ngắn (`new`, `get`, `run`) không thể pass nhờ tình cờ chứa chuỗi con, và không false-deny reason chính đáng.

**Evidence:** `crates/calm-server/src/tools/edit.rs:341-349` — `reason.contains(short)` với `short = qn.rsplit("::").next()`.

**Thiết kế:** thay closure check bằng helper (đặt cạnh `best_live_range` trong cùng file):
```rust
/// true khi `reason` chứa `needle` như một token trọn vẹn — ký tự liền
/// trước/sau không thuộc [A-Za-z0-9_] (hoặc là đầu/cuối chuỗi).
fn cites_token(reason: &str, needle: &str) -> bool { /* find + boundary check, lặp mọi occurrence */ }

const MIN_BARE_NAME_LEN: usize = 4;

let cites_real_signal = if known_caller_qns.is_empty() {
    !reason.is_empty()
} else {
    known_caller_qns.iter().any(|qn| {
        let short = qn.rsplit("::").next().unwrap_or(qn);
        let last_two = { /* ghép 2 segment cuối: "Type::name" nếu có */ };
        (short.len() >= MIN_BARE_NAME_LEN && cites_token(reason, short))
            || cites_token(reason, &last_two)
            || cites_token(reason, qn)
    })
};
```
- Message lỗi `REASON_NOT_GROUNDED` (edit.rs:366-378): với caller tên ngắn, `examples` phải show dạng `Type::name` (last-two) thay vì bare name, để agent biết cần cite dạng nào.

**Tests:**
1. Caller `CalmServer::new`: reason `"checked CalmServer::new — return shape unchanged"` → pass; reason `"renewed the flow"` → **deny** (fix hành vi cũ: `contains("new")` từng pass).
2. Caller `refresh_caller_counts`: reason cite đúng tên → pass (bare name ≥4 vẫn hoạt động).
3. Boundary: reason `"xrefresh_caller_countsy"` → deny.

**Done when:** 3 test xanh + test gate hiện có (`EDIT_CONTEXT_REQUIRED`/`CONFIRM_REQUIRED` flow) không vỡ.

---

## Item 1.7 — Hygiene gộp (H1, H2, H3, H5, H6)  ⏱ ~45p, 1 commit `chore: audit hygiene batch`

| # | Việc | File | Chi tiết |
|---|---|---|---|
| H1 | Sửa doc `RecallParams::query` | `memory.rs:240-243` | "SQL LIKE, case-insensitive" → "FTS5 phrase match (bm25-ranked); input luôn được escape thành literal phrase" |
| H2 | AGENTS.md hứa risk `"critical"` cho edit_context | `AGENTS.md` Stage 5 Signals | Sửa thành `"high"` (code chỉ sinh low/med/high — `common.rs:1545-1553`); đối chiếu thêm mọi mention "critical" khác trong Stage 5 (Stage 7 aggregate_risk CÓ critical thật — giữ) |
| H3 | 3 doc refs chết trong README (fitness gate đang FAIL vì đây) — **AUDIT CORRECTION (đã đọc code `check_config_drift`/`extract_path_refs`, `crates/calm-core/src/analysis/doc_refs.rs:9-17`): chỉ 1/3 là dead link thật, 2/3 là false positive của detector** | README.md + `crates/calm-core/src/analysis/doc_refs.rs` | **(a) Dead link thật:** `docs/legacy/architecture-design.md` (README.md:307, markdown link) → không tồn tại, file thật là `docs/architecture.md` (đã xác nhận `ls`) → sửa link. **(b) 2 false positive:** README.md:78 `` `~/.codeium/windsurf/mcp_config.json` `` và :80 `` `~/.gemini/config/mcp_config.json` `` là *user home-dir path* hợp lệ (hướng dẫn user sửa config trên máy họ), không phải repo-relative path — KHÔNG phải lỗi. `path_ref_regex()` (doc_refs.rs:9-17: `` r"\.?[A-Za-z0-9_][A-Za-z0-9_/.-]*\.(?:...)" ``) không có `~` trong char-class nên match bị cắt cụt thành `.codeium/windsurf/mcp_config.json` (mất `~/`) → không bao giờ resolve được, false positive vĩnh viễn. **Fix đúng root-cause:** thêm 1 dòng skip trong `extract_path_refs` (doc_refs.rs, ngay cạnh check `preceding.contains("://")` đã có sẵn — cùng logic, cùng chỗ): nếu ký tự ngay trước match là `~` thì bỏ qua (đối xử như URL — path tuyệt đối ngoài repo, không phải doc-drift). **KHÔNG sửa README.md dòng 78/80** — chúng đã đúng, sửa sẽ làm hỏng hướng dẫn Windsurf/Antigravity thật. **Done-check:** `fitness_report().passed == true` VÀ README.md dòng 78/80 không đổi (diff chỉ có dòng 307 + doc_refs.rs). |
| H5 | `PRAGMA synchronous=NORMAL` cho writer | `crates/calm-core/src/db/conn.rs::open_writer` | Sau `busy_timeout`: `conn.execute_batch("PRAGMA synchronous=NORMAL;")?;` — an toàn dưới WAL cho index-DB rebuild-được; KHÔNG set cho read conn (query_only, không cần). Test: PRAGMA query trả 1 (NORMAL) |
| H6 | Ghi invariant lock-order | `common.rs::touch_active_session` (247-262) | Comment: "lock order toàn codebase: session_log TRƯỚC active_sessions — mọi hàm chạm cả hai phải giữ thứ tự này (hiện: touch_active_session). Đổi thứ tự = deadlock tiềm năng" |

---

## Checklist kết thúc Plan 1

- [x] 7 commit (6 item + 1 hygiene batch) đều qua `diff_impact` gate, push lên branch làm việc — `2eef364` `657edc5` `181ca55` `e2ac36b` `2695331` `ba8ac29` `738f4ad`
- [x] `cargo test --workspace` xanh (178 lib tests + 3 watcher integration; `daemon_integration.rs` có 3 test flaky KHÔNG liên quan đến Plan 1 — xác nhận fail giống hệt trên HEAD sạch qua `git stash`, socket contention với các daemon process cũ còn sót trong môi trường này); `cargo clippy --workspace --all-targets -- -D warnings` sạch
- [x] `fitness_report()` passed — xác nhận `"passed":true`, `config_drift_count: 0` trên daemon mới rebuild
- [x] Rebuild release + restart daemon (build_info `738f4ad4d24c` khớp HEAD, không `-dirty`); dogfood trực tiếp trên daemon mới: **F2** — `diff_impact` với diff xoá file không tồn tại → `reason:"deleted"`, `aggregate_risk:"low"` (không `unknown`); **F14** — `edit_symbol(resolve_symbol, reason="outsourced this properly, resourceful fix")` → `REASON_NOT_GROUNDED` (reason chứa "source" như substring giữa "outsourced"/"resourceful" — bug cũ sẽ PASS sai, fix mới DENY đúng)
  - **Recipe xác nhận thật trong session này (không phải suy đoán — đã tự gặp daemon stale thật và tự heal thành công bằng recipe này):** đừng chỉ `pkill calm` — MCP client cần một binary MỚI HƠN mọi source file mới re-exec vào connect mode (`mcp-launcher.sh`'s `is_binary_fresh`, mtime-based, xem `scripts/mcp-launcher.sh:165-181`). Cách chắc chắn nhất để force self-heal daemon đang chạy: `./target/release/calm connect --project-root <repo_root> </dev/null` — daemon tự so `daemon.meta`'s `build_info` (git SHA(+`-dirty`) tại compile time) với binary vửa exec, tự SIGTERM bản cũ + respawn nếu khác (log dòng `"is a stale build — signaling it to shut down and respawning"` nếu hoạt động đúng). Side effect: mọi session khác đang attach cùng daemon sẽ thấy `Connection closed` thoáng qua rồi tự reconnect ở tool-call kế tiếp — xác nhận THẬT ở chính session này khi gọi `fitness_report()` ngay sau self-heal.
  - **Verify sau restart:** `cat .calm/daemon.meta` → `pid` mới (3126895) + `build_info` khớp HEAD; `find crates Cargo.toml Cargo.lock -newer target/release/calm` → rỗng. Cả hai đã xác nhận trước khi dogfood.
- [ ] Cập nhật `docs/pattern-debt-registry.yaml` nếu pattern `unwrap-in-handler` được đăng ký ở Plan 2 (xem Plan 2 §2.1) — **N/A cho đến khi Plan 2 chạy**, không phải bỏ sót

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-12 | trigger: NORMAL -->

**Tier:** 2 (Production — live-dogfooded MCP server nhiều agent dựa vào để gate edit an toàn; không PII/payments/multi-tenant nên không phải Tier 3) | **Date:** 2026-07-12

**Phương pháp:** mọi evidence anchor trong Plan 1 (6 fix + 5 hygiene) đã được đọc lại trực tiếp từ source thật qua `mcp__calm__source`/`file_overview`/`search` (không suy ra từ doc/comment) trước khi chạy VHEATM FAST pre-mortem. Kết quả: **F2/F5/F6/F7/F9/F14/H1/H2/H5/H6 khớp chính xác với code hiện tại**, từ line number đến logic được trích dẫn đều đúng, không lệch. **H3 có sai lệch thật** — đã sửa trực tiếp trong bảng hygiene ở trên (xem AUDIT CORRECTION).

### Failure Modes

1. **F9's per-row `filter_map(|r| r.ok())` vẫn có thể trả `NotFound` sai khi TOÀN BỘ row fail decode (không chỉ khi prepare/query_map fail statement-level)** → class bug F9 muốn đóng vẫn còn 1 khe hở, và test đề xuất duy nhất (`DROP TABLE`) không exercise nhánh này — **MEDIUM** — mitigation trong plan: NO (đã ghi note ở Item 1.5 nhưng không yêu cầu fix/test thêm; chấp nhận được cho Plan 1, nên ghi backlog).
2. **F6 sửa đúng state nội bộ server (`session_log`/`session_context`) nhưng cả 2 hook host-specific hiện có (Claude Code + Antigravity, kể cả hook mới commit cùng session này) đều reset gate ở PreToolUse — trước khi biết thành/bại — nên gate thực tế người dùng bị chặn vẫn có thể bị thoả bởi 1 call lỗi** → F6 không đóng được lỗ hổng thực tế này, chỉ đóng lỗ hổng ở tầng API nội bộ — **MEDIUM** — mitigation trong plan: PARTIAL (đã tự nhận trong note gốc nhưng hạ thấp mức độ nghiêm trọng hơn thực tế; đã bổ sung AUDIT NOTE rõ hơn ở Item 1.3).
3. **Checklist yêu cầu "rebuild release + restart daemon" nhưng không nêu cơ chế thật** (mtime-based freshness check trong `mcp-launcher.sh` + build-identity self-heal chỉ trong `calm connect`, không tự động) → người thực thi có thể `pkill` rồi tin rằng daemon mới đã chạy code mới trong khi thực tế chưa, dẫn tới dogfood test (F2/F14 ở cuối checklist) chạy trên binary CŨ, kết luận sai ("fix không hoạt động" trong khi thật ra chưa test đúng code) — xác nhận THẬT trong chính session này (daemon thật đã stale 5 commit, phải tự chạy recipe để heal) — **HIGH** (không phải lý thuyết — đã xảy ra) — mitigation trong plan: YES (đã bổ sung recipe cụ thể + verify step vào checklist ở trên).

### Layer Signals

- **L1 Logic:** nhánh "tất cả row fail decode" của F9 (Failure Mode 1) là untested branch thật.
- **L2 Concurrency:** H6 chỉ thêm comment ghi lại lock-order đã có (`session_log` → `active_sessions`, xác nhận đúng qua source `touch_active_session`) — document-only, không có enforcement mới (assert/lint). Đúng scope cho 1 hygiene item, nhưng không ngăn được vi phạm tương lai bằng code.
- **L3 Data:** no signal — không schema change, mọi field mới đều optional/additive (đã xác nhận qua `MemoryNote`/`RememberOutput`/`UnindexedFileOutput`).
- **L4 Integration:** no signal — không external API call mới.
- **L5 Security:** F7 tái sử dụng `calm_core::sanitize::injection_warning` đã có sẵn (không phải logic mới chưa test) — rủi ro thấp.
- **L6 Observability:** F9 — `db_error`/`db_error_resolved` (common.rs:640-655, đọc source xác nhận) không hề `tracing::error!`/`warn!`, chỉ trả client. Lỗi DB hạ tầng thật sau fix sẽ không để lại log phía server. Ghi ASSUMED bên dưới.
- **L7 Cross-cutting:** no signal — không rate limit/regulated data; idempotency của F6/F9 không đổi so với trước.

### Assumptions to Verify

- **ASSUMED:** đội thực thi Plan 1 sẽ dùng đúng recipe `calm connect ... </dev/null` ở checklist thay vì `pkill` đơn giản — không có gì ENFORCE điều này ngoài việc đọc checklist; nếu skip, Failure Mode 3 xảy ra im lặng (không crash, chỉ test sai).
- **ASSUMED:** H6's lock-order comment sẽ được tôn trọng bởi code tương lai — không có assert runtime hay lint ngăn vi phạm.
- **ASSUMED (từ plan gốc, chưa xác minh lại ở audit này):** F9's `?` sau khi đổi chữ ký không làm vỡ borrow-checker/lifetime nào ở 11 call site (9 + 2 patterndebt.rs mới phát hiện) — hợp lý vì pattern `Err(e) => return db_error_resolved(e)` đã tồn tại sẵn ở chính những hàm đó cho nhánh khác, nhưng compiler là trọng tài cuối cùng, không phải suy luận này.

### Abductive Hypotheses

- **Abductive 1 (tương tác giữa các component đúng riêng lẻ):** F6 (server-side gate chặt hơn) + F9 (DB error giờ propagate thay vì swallow) cộng lại có thể khiến một DB tạm thời busy/lock (không phải lỗi thật, chỉ contention bình thường dưới WAL) giờ biểu hiện thành `DB_ERROR` rõ ràng hơn trước (trước đây có thể âm thầm thành empty-result) đúng tại thời điểm `diff_impact` đang cần thành công để clear gate (F6) — nếu `busy_timeout` (đã xác nhận có sẵn ở `open_writer`) không đủ dài dưới tải concurrent cao (nhiều daemon/session cùng project-root như quan sát được thật trong session này — từng thấy đến 2 daemon cùng listen 1 socket path trước self-heal), user có thể thấy gate `pending_diff_impact` "kẫt" nhiều hơn trước do lỗi thoáng qua thay vì swallow âm thầm. Không blocking Plan 1, nhưng đáng theo dõi sau khi ship.
- **Abductive 2 (chỉ thấy ở scale/adversarial input):** F14's `cites_token` word-boundary fix siết chặt đúng như thiết kế, nhưng nếu một symbol có tên ngắn (<4 ký tự, dưới `MIN_BARE_NAME_LEN`) VÀ không có `Type::` prefix nghĩa (function trần ở module level, không phải method), `last_two` sẽ không ghép được gì hơn ngoài chính tên ngắn đó — agent hợp lệ có thể bị DENY (REASON_NOT_GROUNDED) dù cite đúng tên, vì không có cách nào vượt ngưỡng độ dài mà không dùng full qualified_name. Plan đã có test cho trường hợp này (`refresh_caller_counts` — đủ dài) nhưng chưa có test cho một bare-name-ngắn KHÔNG-phải-method (không có `Type::` để fallback) — nên thêm 1 test case cho trường hợp này nếu có function trần <4 ký tự trong codebase (vd `new` ở module-level không thuộc impl block).

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
PASS WITH FLAGS — evidence anchors đã verify 100% khớp code (trừ H3 đã sửa ngay trong file này). Không có HIGH finding nào thiếu mitigation trong bản cập nhật này (Failure Mode 3 đã mitigate bằng recipe cụ thể trong checklist). Plan 1 **sẵn sàng thi công** theo thứ tự gốc (1.1 → 1.7), với 2 bổ sung khuyến nghị không bắt buộc: (1) thêm `tracing::warn!` vào nhánh lỗi mới của F9 (L6), (2) thêm 1 test bare-name-ngắn-non-method cho F14 (Abductive 2) nếu tìm thấy case thực tế trong codebase.
