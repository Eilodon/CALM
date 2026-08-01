# VHEATM Tier-2 Findings — Root-Cause Remediation Plan

> Nguồn: VHEATM Tier-2 audit (2026-08-01), độc lập verify lại từng claim (reproduce local + nightly CI + spec cross-check) trước khi lên plan này. Không có finding nào bị bác bỏ — tất cả đều CONFIRMED. Phạm vi: 5 finding còn mở (Cargo.lock stale đã fix trực tiếp, commit `7e9d972`).
>
> **Nguyên tắc xuyên suốt:** mọi fix ở đây nhắm vào root cause đã xác định bằng bằng chứng trực tiếp (debug reproduction, spec chính thức, code hiện tại) — không phải patch che triệu chứng (tăng timeout, catch-all try/catch, disable test). Nếu một finding có "quick fix" hời hợt, plan sẽ nêu rõ vì sao nó không đủ và chọn hướng sâu hơn.

---

## §1 — SCIP Python/JS: root cause đã xác định chính xác, không còn là bí ẩn

**Mức độ: Required. Đây là finding quan trọng nhất — closes toàn bộ "Python/JS chưa verified".**

### Bằng chứng (tự tái hiện, không suy diễn)

Thêm `eprintln!` tạm thời vào `parse.rs::parse_index` + `ingest.rs::ingest_occurrences_with_proof_context`, chạy lại 2 live test:

```
DEBUG occ symbol=scip-python ... pos_enc=UnspecifiedPositionEncoding byte_range=None
DEBUG occ symbol=scip-typescript ... pos_enc=UnspecifiedPositionEncoding byte_range=None
```

100% occurrence của **cả hai** provider đều `UnspecifiedPositionEncoding`. Đối chứng rust-analyzer (test pass):

```
DEBUG occ symbol=rust-analyzer ... pos_enc=UTF8CodeUnitOffsetFromLineStart byte_range=Some((55, 59))
```

Truy vào code: `crates/calm-core/src/scip/parse.rs:174`

```rust
PositionEncoding::UnspecifiedPositionEncoding => None,
```

— fail-closed cứng, đúng như docstring `occurrence_byte_range` nói ("fail closed rather than becoming a line-key match"). Khi `byte_range=None`, `ingest.rs:179-185`'s `let (Some(..), Some(..), Some(..)) = (...) else { continue }` loại bỏ occurrence đó khỏi `ref_targets` — với Python/JS, **ref_targets rỗng tuyệt đối** (0 dòng debug `ref_targets key=` được in ra), nên `stats.upgraded` luôn = 0. Đây là 100% nguyên nhân, không phải flake, không phải npm/CI.

### Đây không phải guess — SCIP protocol tự ghi rõ quy ước

`scip-0.9.0/src/generated/scip.rs:580-586` (doc comment trên field `Document.position_encoding`, dịch từ `scip.proto` gốc):

```
- For an indexer implemented in JVM/.NET language or JavaScript/TypeScript,
  use UTF16CodeUnitOffsetFromLineStart.
- For an indexer implemented in Python,
  use UTF32CodeUnitOffsetFromLineStart.
- For an indexer implemented in Go, Rust or C++,
  use UTF8ByteOffsetFromLineStart.
```

scip-python 0.6.6 và scip-typescript 0.4.0 (cả hai đang được npx cache) **không set field này** — vi phạm hợp đồng SCIP tự khai báo, nhưng chính SCIP spec cho ta default đúng, có căn cứ, theo ngôn ngữ cài đặt của indexer — không phải một con số đoán mò.

### Vì sao KHÔNG chọn fix nông

- "Assume UTF-8 khi Unspecified" (global) → tình cờ đúng trên fixture ASCII hiện tại nhưng SAI về nguyên tắc cho Python (spec nói UTF-32) và JS (spec nói UTF-16) — trên source có ký tự multi-byte (tiếng Việt trong comment, emoji, ký tự Unicode bất kỳ), UTF-8 byte offset ≠ UTF-32/UTF-16 code-unit offset → byte span sai lệch nhưng **vẫn có thể nằm trong bound và hợp lệ char-boundary** → match nhầm sang một call site khác, ID edge sai một cách **im lặng**, còn nguy hiểm hơn cả không match (đúng nỗi lo mà `occurrence_byte_range`'s docstring gốc đã cảnh báo).
- "Bỏ luôn exact-byte-span match, quay lại line-only" → xoá bỏ toàn bộ lý do D4 tồn tại (SCIP-primary CallSite byte-span provenance, vừa ship commit `7672f52`) — mất chính xác trên toàn bộ 8 provider để vá 2 provider.
- "Un-ignore test và merge dù fail" hoặc "giảm threshold `upgraded > 0` xuống `>= 0`" → che triệu chứng, không sửa gì.

### Fix đề xuất — 3 lớp, theo đúng tinh thần "fail closed nhưng không fail mù"

**Lớp 1 — Per-provider fallback encoding (fix chính, có căn cứ spec):**
Truyền provider identity xuống `parse_index`/`occurrence_byte_range` (mỗi call site trong `mod.rs` — `run_python_overlay_and_log`, `run_js_overlay_and_log`, v.v. — đã biết chính xác đang gọi provider nào). Khi `doc.position_encoding == Unspecified`, áp fallback theo bảng spec ở trên thay vì `None` ngay:

```rust
fn effective_encoding(declared: PositionEncoding, provider: ScipProvider) -> PositionEncoding {
    if declared != PositionEncoding::UnspecifiedPositionEncoding {
        return declared; // provider tự khai báo → luôn ưu tiên, không override
    }
    match provider {
        ScipProvider::Python => PositionEncoding::UTF32CodeUnitOffsetFromLineStart,
        ScipProvider::TypeScript | ScipProvider::Java | ScipProvider::CSharp
            => PositionEncoding::UTF16CodeUnitOffsetFromLineStart,
        ScipProvider::Rust | ScipProvider::Go | ScipProvider::Clang
            => PositionEncoding::UTF8CodeUnitOffsetFromLineStart,
        _ => PositionEncoding::UnspecifiedPositionEncoding, // provider mới/không rõ → vẫn fail-closed
    }
}
```

Providers không nằm trong bảng tiếp tục fail-closed như hiện tại (không mở rộng rủi ro ra ngoài phạm vi đã verify).

**Lớp 2 — Defense-in-depth: verify byte span bằng nội dung thật, không chỉ tin encoding đoán được** (đây là phần khiến fix "chính xác nhất" chứ không phải chỉ "chạy được"):
Sau khi có `byte_range` (dù từ encoding khai báo hay fallback), cross-check `source[start..end]` khớp với local-name suy ra từ SCIP symbol moniker (ví dụ `` `pkg.helper`/helper(). `` → local name `helper`) trước khi chấp nhận. Không khớp → fail-closed như cũ (log lý do), không âm thầm chấp nhận offset sai. Cái này biến "đoán encoding" thành "đoán rồi tự kiểm chứng bằng dữ liệu thật" — loại bỏ đúng rủi ro mà lớp 1 một mình không đủ tự tin loại bỏ trên source ngoài ASCII.

**Lớp 3 — Observability, đúng đề xuất gốc của audit nhưng cụ thể hoá:**
Thêm counter `IngestStats::skipped_unspecified_encoding: usize` (tăng khi rơi vào nhánh fail-closed dù đã thử fallback) + `skipped_content_mismatch: usize` (tăng khi lớp 2 từ chối). Surface qua `scip_overlay`/`indexing_status` cạnh `last_match_rate`/`last_inserted` hiện có — để lần sau một provider tương lai (hoặc bản nâng cấp scip-python/scip-typescript) tái diễn lỗi này, dashboard tự phân biệt được "0 vì encoding gap" và "0 vì thật sự không có overlap", thay vì che trong một con số 0 mù mờ như hiện tại (đây cũng chính là gốc rễ của finding §4 bên dưới).

### Test

- Unit test **không cần network/npx** cho `effective_encoding()` — lock bảng mapping, chạy trong CI thường (không phải nightly-only), phát hiện regression ngay nếu ai đó sửa nhầm bảng.
- Giữ nguyên 2 live-ignored fixture test hiện có làm end-to-end proof thật (đã confirm chạy được khi có node/npx) — nightly vẫn là nơi chạy chúng.
- Thêm test cho lớp 2: fabricate một SCIP occurrence có encoding "đúng nhãn" nhưng offset trỏ sai chỗ (giả lập trường hợp đoán sai) → phải bị từ chối, không match nhầm.

**Done when:** `cargo test scip::tests::python_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture -- --ignored` và bản JS tương đương pass thật (không sửa assertion), 3 nightly liên tiếp xanh, `indexing_status` trên chính repo CALM cho thấy `python`/`javascript` có `last_inserted > 0` hoặc `last_match_rate > 0` khi có call cross-file thật.

---

## §2 — Watcher full-scan mỗi debounce

**Mức độ: Required, nhưng cần đính chính khung: đây là nợ kỹ thuật ĐÃ ĐƯỢC TRACK có chủ đích, không phải phát hiện mới.**

`docs/plans/2026-07-12-upgrade-plan-3-architecture.md` dòng 42 (audit-design đã CONFIRMED lúc đó, đọc trực tiếp `run_watch_loop`):

> "để nguyên watcher như cũ (không phải thiếu sót âm thầm — giữ nguyên **có chủ đích**, ghi backlog "watcher path-tracking" cho phase sau nếu cần)"

Lý do hoãn: `reindex_paths` (Phase A) khi đó chỉ ưu tiên thắng lớn nhất — edit-tool path (chắc chắn 1 file/call). Watcher-driven reindex (external editor, branch switch) là nguồn dirty-reindex lớn thứ nhì, để lại cho phase sau. Giờ là lúc làm phase đó.

### Vì sao KHÔNG chọn fix nông

"Cứ gọi `reindex_paths` với path lấy từ event đầu tiên nhận được" là sai — một cửa sổ debounce (`DEBOUNCE`, xem `watcher.rs:150-175`) có thể gom nhiều event từ nhiều file khác nhau; chỉ lấy 1 path bỏ sót phần còn lại, tệ hơn cả full-walk hiện tại (silent data loss thay vì chậm).

### Fix đề xuất — thu thập path thật qua debounce, có ngưỡng an toàn fallback

1. Đổi `coverage_touched: bool` hiện tại thành thu thập luôn `dirty: HashSet<PathBuf>` trong cả vòng nhận event đầu (`watcher.rs:138-148`) lẫn vòng debounce nội (`watcher.rs:154-175`) — mọi path trong `res.paths` mà `is_relevant_path` chấp nhận được thêm vào set (dedup tự nhiên qua `HashSet`).
2. **Fallback an toàn, không phải tối ưu hoá mù quáng:** nếu `event.need_rescan()` (API có sẵn của crate `notify`, hiện đã được `event_is_relevant` tại `watcher.rs:57` coi là "relevant" nhưng chưa được dùng để phân biệt full vs incremental) xuất hiện ở bất kỳ event nào trong cửa sổ, **hoặc** `dirty.len()` vượt ngưỡng (dùng lại đúng ngưỡng `changed_paths.len() > 50` đã có tiền lệ trong Phase A/edit path, theo đúng note L7 cross-cutting của plan gốc) → bỏ qua path list, gọi `reindex_changed_cancellable` (full-walk) như hiện tại. `need_rescan()` chính là tín hiệu "OS-level watch buffer overflow, path list không còn đáng tin" — đây là lý do CHÍNH XÁC cần full-walk, không phải một ước lượng.
3. Nếu không rơi vào 2 case trên: chuyển `dirty` set thành `Vec<String>` (relative path, chuẩn hoá separator cho Windows — release matrix có `x86_64-pc-windows-msvc`, phải test trên path có `\`), gọi `calm_core::indexer::pipeline::reindex_paths(&mut conn, &project_root, &rel_paths)` thay `reindex_changed_cancellable`.
4. Rename events: `notify` phát 2 event riêng (from/to) trên hầu hết backend — cả hai path đều phải vào `dirty` (từ = coi như xoá, đến = coi như thêm); `reindex_paths` đã tự xử lý "path không còn trên đĩa = deletion" (comment tại `pipeline.rs:2622`) nên không cần logic riêng, chỉ cần đảm bảo cả hai path lọt vào set.

### Rủi ro cần kiểm soát trước khi golden-equivalence hoá

Đúng như plan gốc đã tự nêu rủi ro (dòng 232, L6 Observability): không có field bền vững phân biệt "chạy incremental" vs "fallback full" cho watcher path. Fix này **phải** đi kèm field `reindex_mode` (đã tồn tại cho edit-tool path qua `last_graph_mode` — dùng lại, không tạo field mới) được watcher path cũng ghi vào, để một off-by-one trong ngưỡng 50 hay `need_rescan()` bị bỏ sót không âm thầm quay về full-rebuild vĩnh viễn mà không ai biết.

### Test

- Integration test watcher thật (đã có sẵn hạ tầng dùng `ready` channel để chờ watch armed — `watcher.rs:98`): touch 2 file cụ thể trong 1 cửa sổ debounce, assert `reindex_paths` được gọi (không phải full-walk) và **chỉ** 2 file đó bị re-parse (dùng `file_index.hash` hoặc counter re-parse để verify, không suy đoán).
- Test riêng cho `need_rescan()` fallback: giả lập overflow event → assert full-walk vẫn chạy.
- Test ngưỡng 50: N > 50 file thay đổi trong 1 cửa sổ → assert fallback full-walk.
- Golden-equivalence: kết quả graph sau incremental-watcher-path phải == kết quả full-rebuild trên cùng state (dùng lại hạ tầng golden test đã có ở `crates/calm-core/tests/golden_graph_equivalence.rs`).

**Done when:** watcher path và edit-tool path cùng đi qua `reindex_paths` trong trường hợp thường; `reindex_mode` phân biệt được 2 nhánh; golden-equivalence xanh.

---

## §3 — Watcher chết không hiện trạng thái lỗi

**Mức độ: Required — đây MỚI thực sự là gap chưa ai track (khác §2), nghiêm trọng hơn về mặt "silent failure" vì không có backlog note nào ghi nhận nó.**

### Bằng chứng

`watcher.rs:104-116`: cả `recommended_watcher()` lỗi lẫn `watcher.watch()` lỗi đều chỉ `tracing::error!` rồi `return` — thread kết thúc, không ai biết. Grep toàn repo `armed`/`liveness`/`last_event`/`watcher_status` trong output `indexing_status` → **0 kết quả**. `IndexingStatusOutput` (`recover.rs:164-200`) có `scip_overlay`, `lsp_providers`, `embeddings_status`... không một field nào phản ánh watcher còn sống.

Hệ quả thực tế nguy hiểm hơn cả reindex chậm (§2): agent tin `indexing_phase=ready` là index luôn tươi, trong khi file trên đĩa đổi liên tục mà không ai reindex — độ lệch tăng dần, không crash, không log nào agent nhìn thấy qua MCP tool.

### Vì sao KHÔNG chọn fix nông

"Thêm field `watcher_armed: bool` set khi start, không bao giờ update" → false sense of security, tệ hơn không có field (báo sai "true" mãi mãi sau khi thread đã chết).

### Fix đề xuất — health state thật + tự phục hồi có giới hạn, không chỉ báo cáo

1. Thêm `WatcherHealth` state (`Arc<RwLock<WatcherHealth>>`, cùng pattern với `phase`/`coverage` hiện có trong `CalmServer`): `armed: bool`, `last_event_at: Option<i64>` (epoch), `last_reindex_at: Option<i64>`, `last_error: Option<String>`, `consecutive_failures: u32`.
2. `run_watch_loop` cập nhật state ở đúng các điểm hiện có: `armed=true` sau `watcher.watch()` Ok (đối xứng với `ready.send(())` đã có ở dòng 117-119, tận dụng luôn điểm này); `last_event_at` mỗi event relevant chấp nhận; `last_reindex_at` sau mỗi `ReindexOutcome::Completed`; `last_error` + `armed=false` khi init/watch fail HOẶC khi loop thoát bất thường.
3. **Bounded self-heal trước khi báo failed hẳn** — đây là phần "hiệu quả nhất" chứ không chỉ "báo cáo trung thực": lỗi `watcher.watch()` phổ biến nhất trong thực tế là cạn `fs.inotify.max_user_watches` (Linux, repo lớn) — lỗi này thường transient nếu process khác giải phóng watch, hoặc cần người dùng biết để tăng sysctl. Thay vì chết hẳn ngay lần đầu: retry có backoff (giống pattern `daemon.meta` self-heal đã dùng ở ADR-0005) tối đa N lần; nếu vẫn fail, chuyển `armed=false` VĨNH VIỄN và field `last_error` trả về message actionable kiểu `install_hint` đã có sẵn cho SCIP providers (ví dụ: "inotify watch limit reached — increase fs.inotify.max_user_watches (see docs/...)"), không chỉ raw OS error string.
4. Surface qua `IndexingStatusOutput.watcher: Option<WatcherHealthOutput>` (mirror đúng shape `scip_overlay`/`lsp_providers` đã có). Quyết định thiết kế cần chốt rõ ràng (không lập lờ): **không** đổi `indexing_phase` sang `Failed` khi watcher chết — index hiện có vẫn đúng-tại-thời-điểm-cuối-cùng, khác hẳn ý nghĩa `Failed` hiện tại (index chưa bao giờ build xong). Thay vào đó, watcher-dead là một trục riêng ("live-update dead, data có thể stale") — đúng tinh thần tách biệt freshness/efficacy ở §4 dưới.

### Test

- Simulate `watcher.watch()` fail (dùng path không tồn tại hoặc mock) → assert `WatcherHealth.armed=false`, `last_error` có nội dung, retry đúng N lần trước khi dừng.
- Simulate panic giữa loop (qua injection point có sẵn cho test, tương tự cách `ready` channel đã được thiết kế riêng cho test) → assert health chuyển sang dead, không phải thread biến mất im lặng.

**Done when:** `indexing_status` trả về watcher health thật; test kill watcher giữa chừng rồi gọi `indexing_status` phải thấy phản ánh đúng, không phải trạng thái "ready" cũ đứng yên.

---

## §4 — Dashboard xanh dù zero-uplift

**Mức độ: Recommended, nhưng nên làm cùng đợt với §1 vì §1's Lớp 3 (observability) đã tạo phần lớn hạ tầng cần thiết.**

### Bằng chứng sống (không phải giả định)

`indexing_status` ngay lúc audit chạy trên chính repo CALM:
```
python:     available=true, up_to_date=true, last_match_rate=0, last_inserted=0
javascript: available=true, up_to_date=true, last_match_rate=0, last_inserted=0
```
Đúng là "freshness" (available/up_to_date) xanh trong khi "efficacy" (match_rate/inserted) = 0 — hai khái niệm khác nhau đang bị gộp vào một huy hiệu "xanh" duy nhất.

### Fix đề xuất — tách 2 trục, ngưỡng dựa trên chuỗi run chứ không phải 1 lần

1. `PerLanguageOverlayStatus` thêm field `health: "healthy" | "zero_uplift_suspect" | "unavailable"` (không phá vỡ `available`/`up_to_date` hiện có — additive).
2. `zero_uplift_suspect` chỉ bật khi: `indexed_file_count > 0` **và** `last_match_rate == 0` **và** `last_inserted == 0` **trong N lần chạy liên tiếp** (dùng lịch sử đã lưu, không phải 1 snapshot — tránh false-positive trên repo thật sự không có cross-file call). N cụ thể cần đo trên vài repo thật trước khi hardcode — không đoán số.
3. Field mới **không** default vào `fitness_report`'s pass/fail (một repo hợp lệ có thể chính đáng có 0 Python cross-call) — chỉ opt-in qua `thresholds.toml` cho ai muốn gate cứng, đúng tinh thần các threshold khác trong file này.
4. Đây chính là cơ chế lẽ ra đã tự động bắt được finding §1 mà không cần audit thủ công — "Done when" tốt nhất cho mục này là: bật cờ này trên chính CALM repo trước khi merge §1's fix, xác nhận nó tự chuyển `zero_uplift_suspect: true`, rồi confirm nó tự tắt sau khi §1 merge xong.

---

## §5 — God-component `tools/common.rs` / `CalmServer`

**Mức độ: Recommended, quy mô lớn nhất — không phải việc 1 PR, cần dogfood chính cơ chế `[[boundaries]]` của CALM để khoá kết quả.**

### Bằng chứng cụ thể hơn buổi audit ban đầu

`hotspots()`: `tools/common.rs` #1 toàn repo (score 0.52, risk "high", 42 commit, 15/47 symbol là hub). Quan trọng: **đã có một lần refactor trước** (`81f7315 refactor(tools): split common.rs into toolset/outcome/detail`) tách được `toolset.rs`(413 dòng)/`outcome.rs`(574 dòng)/`detail.rs`(1004 dòng) — nhưng đó là tách theo **shape của response** (DTO/formatting), không đụng tới root cause: struct `CalmServer` cùng ~40 inherent method của nó (872 dòng còn lại) vẫn là một khối duy nhất. Đọc toàn bộ symbol list của `common.rs` hiện tại, có thể nhóm rõ theo 6 trách nhiệm KHÔNG liên quan nhau về mặt thay đổi (mỗi nhóm đổi vì lý do khác nhau):

| Cụm | Method đại diện | Lý do tách |
|---|---|---|
| DB/connection | `db`, `make_read_conn`, `memory_write_conn` | đổi khi schema/connection pooling đổi |
| Index/embed runtime state | `phase_handle`, `embedder_handle`, `embed_status_handle`, `retry_embeddings_if_failed`, `edges_ready`, `current_phase` | đổi khi indexing pipeline đổi |
| Session/telemetry | `session_registry_handle`, `track_symbol`, `track_file`, `touch_active_session`, `for_connection`, `new`/`new_with_preset` | đổi khi concurrency/multi-session model đổi |
| Edit-review/guardrail state | `record_edit_context_review`, `edit_context_review`, `mark_written`, `elicit_declined_*`, `pending_diff_impact_reminder_text` | đổi khi hub-edit gate policy đổi (đây chính là logic mà `edit_lines`/`edit_symbol` phụ thuộc — an toàn nhất repo, rủi ro refactor cao nhất) |
| Onboarding/orientation | `is_orientation_adjacent`, `orientation_*` (4 method) | đổi khi first-session UX đổi |
| Search/personalization | `apply_personalization_boost`, `co_changes_cached`, `ownership_entropy_for`, `related_notes` | đổi khi ranking/search đổi |

6 lý-do-thay-đổi khác nhau trong 1 struct là chính xác định nghĩa vi phạm Single Responsibility — không phải cảm tính "file dài".

### Vì sao KHÔNG chọn fix nông

"Chia common.rs thành common_a.rs/common_b.rs theo số dòng" → hotspot_score không đổi thật sự (coupling vẫn nguyên, chỉ dời chỗ), và rủi ro cao nhất — cụm "Edit-review/guardrail state" — vẫn dính chặt vào các cụm khác không cần thiết, làm review injection an toàn khó hơn không dễ hơn.

### Fix đề xuất — Strangler-Fig, có lưới an toàn TRƯỚC khi đổi cấu trúc

1. **Lưới an toàn trước tiên (không thương lượng, vì đây là hotspot rủi ro cao nhất repo — is_hub trên `new`/`db`/`timed_tool`/`filter_sn`/`make_read_conn`, tức gần như mọi tool call đều đi qua):** confirm `__toolsnaps__` hiện có (đã thấy `indexing_status.snap`, `repo_overview.snap`) phủ đủ mọi tool; bổ sung snapshot cho tool nào thiếu trước khi đổi 1 dòng structure nào.
2. Tách 4 struct mới theo đúng 4 cụm rủi ro cao/độc lập nhất trước (không làm cả 6 cùng lúc): `SessionRegistry` (cụm 3), `IndexRuntimeHandle` (cụm 2), `GuardrailState` (cụm 4 — ưu tiên cao nhất vì đây là cơ chế an toàn edit path, tách sạch nó ra giúp review riêng logic gate mà không lẫn với DB/session code), `OnboardingPolicy` (cụm 5). Cụm 1 (DB) và 6 (search/personalization) giữ lại trong `CalmServer` facade tạm thời — độ rủi ro thấp hơn, tách sau.
3. Mỗi struct mới **wrap** field hiện có (không đổi kiểu dữ liệu bên trong ở bước 1) và **delegate** — `CalmServer`'s method cũ gọi thẳng sang struct mới, giữ nguyên chữ ký public/`pub(crate)` để không phải sửa call site ở `tools.rs`/`edit.rs`/`recover.rs` cùng lúc. Verify bằng `__toolsnaps__` bit-for-bit trước/sau mỗi bước — sai lệch = revert ngay, đúng nguyên tắc "golden equivalence" mà chính Plan 3 gốc đã áp dụng cho graph.
4. Sau khi delegate ổn định (toolsnaps xanh nhiều ngày/nhiều PR), mới di chuyển call site trực tiếp sang struct mới, xoá lớp delegate.
5. **Khoá kết quả bằng chính cơ chế CALM đã có:** thêm `[[boundaries]]` vào `thresholds.toml` (cơ chế đã tồn tại, dùng cho `watcher → tools` trước đây) cấm `GuardrailState`/`SessionRegistry` import ngược vào nhau hoặc vào `IndexRuntimeHandle` ngoài interface công khai — biến quyết định kiến trúc thành gate CI tự động (`boundary_violations` fitness metric), không phải quy ước bằng lời rồi trôi dần trở lại god-object như đã xảy ra sau `81f7315`.

**Done when:** `hotspots()` không còn xếp `tools/common.rs`/struct kế thừa vào top-3 repo; `boundary_violations` = 0 với boundary mới khai báo; toolsnaps toàn bộ không đổi qua suốt quá trình.

---

## Thứ tự thi công khuyến nghị

1. **§1 (SCIP encoding fix)** — độc lập, giá trị cao nhất, rủi ro thấp nhất (chỉ ảnh hưởng 2 provider hiện đang 0% hiệu quả, không thể làm tệ hơn).
2. **§4 (freshness/efficacy tách trục)** — làm ngay sau §1 vì dùng chung hạ tầng observability, và §1 tạo dữ liệu thật để calibrate ngưỡng N lần chạy.
3. **§3 (watcher liveness)** — độc lập, rủi ro thấp, giá trị vận hành cao.
4. **§2 (watcher dirty-path)** — cần golden-equivalence test kỹ hơn, làm sau khi §3 đã có health signal để phát hiện nếu §2 giới thiệu regression (một reindex "âm thầm biến mất" sẽ lộ ra qua watcher health's `last_reindex_at` không nhích).
5. **§5 (God-component decomposition)** — quy mô lớn nhất, rủi ro cao nhất, nên làm cuối cùng khi 4 mục trên đã ổn định (bớt code churn đúng lúc đang refactor structure).

Mỗi mục nên đi qua đúng quy trình dự án đã dùng cho các plan trước: audit-design trước khi code, ADR sau khi xong, `calm fitness_report`/toolsnaps làm bằng chứng "Done" — không tự nhận đã xong bằng lời.
