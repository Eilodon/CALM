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

Providers không nằm trong bảng tiếp tục fail-closed như hiện tại (không mở rộng rủi ro ra ngoài phạm vi đã verify) — kể cả provider generic/unknown tương lai: đây là **evidence contract**, không phải fallback toàn cục, nên mặc định vẫn strict fail-closed trừ khi provider được liệt kê tường minh.

**Lớp 2 — Defense-in-depth: verify span bằng `CallSite` thật, KHÔNG parse SCIP moniker** (đây là phần khiến fix "chính xác nhất" chứ không phải chỉ "chạy được"; sửa so với bản nháp đầu — moniker SCIP là opaque theo spec, suy ra local name từ nó không đủ an toàn):
Sau khi có `byte_range` (dù từ encoding khai báo hay fallback), sinh tối đa 3 ứng viên span (declared encoding nếu có + các encoding fallback hợp lý theo Lớp 1). Chỉ nâng cấp edge khi ĐÚNG MỘT ứng viên khớp duy nhất với một `CallSite` đã có trong pipeline hiện tại, theo toàn bộ tiêu chí: source hash của file tại thời điểm ingest, byte-span trùng khớp, `edge_kind == Call`, identity version của symbol khớp, và `callee_name` bằng đúng source text tại span đó (đọc trực tiếp từ file, không suy ra từ moniker). Nếu ≥2 ứng viên (từ các encoding khác nhau) cùng trỏ tới các `CallSite` hợp lệ nhưng KHÁC nhau → từ chối vì mơ hồ (`ambiguous`), không đoán đại một cái. Không khớp ứng viên nào → fail-closed như cũ (log lý do). Đây chính là cơ chế biến "đoán encoding" thành "đoán rồi tự kiểm chứng bằng dữ liệu thật" mà không cần tin vào một chuỗi tên suy diễn.

**Lớp 3 — Version hoá evidence policy trong cache/state (bắt buộc, không tuỳ chọn):**
`effective_encoding()` + quy tắc verify ở Lớp 2 gộp lại thành một `EVIDENCE_POLICY_VERSION` cố định, lưu cạnh SCIP semantic cache/state hiện có (`crates/calm-core/src/scip/mod.rs`). Nếu version trong cache khác version code đang chạy → coi cache đó stale, buộc re-evaluate, không cache-hit mù. Thiếu bước này, một repo không đổi file nào giữa 2 lần chạy sẽ tiếp tục trả `last_inserted=0` cho Python/JS mãi mãi sau khi fix được merge, vì kết quả cũ (từ trước khi có Lớp 1/2) vẫn còn trong cache — bug thật đã sửa trong code nhưng vẫn biểu hiện y hệt bug cũ trên máy đã từng chạy qua bản lỗi. Việc version hoá này thuộc về SCIP orchestration ở `scip/mod.rs`, không thể chỉ sửa trong `parse.rs`.

**Lớp 4 — Provenance/funnel metrics (cụ thể hoá đề xuất observability gốc):**
Thêm bộ counter theo từng bước của phễu, không chỉ 1 con số tổng: `declared` (occurrence có encoding tự khai báo), `fallback_attempted` (rơi vào bảng Lớp 1), `verified` (qua được Lớp 2), `ambiguous` (bị Lớp 2 từ chối vì ≥2 candidate hợp lệ khác nhau), `rejected` (không candidate nào khớp), `matched`/`upgraded` (thực sự nâng cấp edge). Surface qua `scip_overlay`/`indexing_status` cạnh `last_match_rate`/`last_inserted` hiện có. Nhờ phễu chi tiết này, `upgraded=0` không còn tự động bị đọc thành "unhealthy" — có thể là `declared=0` (repo không có occurrence nào của ngôn ngữ đó) chứ không phải encoding gap; đây cũng là dữ liệu đầu vào cho §4 bên dưới.

### Test

- Unit test **không cần network/npx** cho `effective_encoding()` — lock bảng mapping, chạy trong CI thường (không phải nightly-only), phát hiện regression ngay nếu ai đó sửa nhầm bảng.
- Giữ nguyên 2 live-ignored fixture test hiện có làm end-to-end proof thật (đã confirm chạy được khi có node/npx) — nightly vẫn là nơi chạy chúng.
- Test cho Lớp 2: fabricate một SCIP occurrence có encoding "đúng nhãn" nhưng offset trỏ sai chỗ (giả lập trường hợp đoán sai) → phải bị `rejected`, không match nhầm.
- Test ambiguity: fabricate 2 encoding-candidate cùng trỏ tới 2 `CallSite` hợp lệ nhưng khác nhau → phải bị đếm vào `ambiguous`, không tự ý chọn một cái.
- Test cache-version: chạy overlay 1 lần với policy cũ (giả lập bằng version thấp hơn trong state), bump `EVIDENCE_POLICY_VERSION`, chạy lại không đổi file nguồn nào → phải re-evaluate (không cache-hit), `upgraded` phản ánh kết quả policy mới.
- Test generic/unknown provider: provider không có trong bảng Lớp 1 vẫn phải fail-closed (không vô tình được hưởng fallback của provider khác).

**Done when:** `cargo test scip::tests::python_overlay_upgrades_a_real_edge_on_the_multi_lang_fixture -- --ignored` và bản JS tương đương pass thật (không sửa assertion), 3 nightly liên tiếp xanh, `indexing_status` trên chính repo CALM cho thấy `python`/`javascript` có `last_inserted > 0` hoặc `last_match_rate > 0` khi có call cross-file thật, và funnel metrics (Lớp 4) phân biệt rõ nguyên nhân khi `upgraded == 0`.

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

## Revision 2026-08-01 (v2) — nâng §2/§3/§5 lên kiến trúc thống nhất

> Sau khi §1 đã đóng ở mức root-cause, tái nghiên cứu cho thấy §2/§3 (watcher) và §5 (CalmServer) nếu làm tuần tự theo đúng như draft v1 sẽ vá từng triệu chứng riêng lẻ (thêm field, thêm ngưỡng, tách file) thay vì đóng root cause kiến trúc chung. Phần dưới đây THAY THẾ cách tiếp cận thực thi của §2/§3/§5 (giữ nguyên bằng chứng/root cause đã nêu ở trên), không đổi §1/§4.

### Core refresh protocol thay cho watcher tự quyết (thay thế cách làm ở §2/§3)

Thay vì watcher tự gọi `reindex_paths` hay tự quyết ngưỡng `dirty.len() > 50`, đưa quyết định "reindex kiểu gì" vào core dưới dạng `ChangeSet`/`RefreshRequest` mà bất kỳ caller nào (watcher, edit-tool path, CLI) đều tạo ra và core tự phân loại:

- source path xác định được → reindex đúng path (`reindex_paths`, không đổi).
- metadata/context input đổi (`go.mod`, `pyproject.toml`, `tsconfig`/`extends`, package lock, v.v.) → rebuild graph từ DB, không hash lại toàn repo.
- coverage-only thay đổi → chỉ reload coverage, không đụng graph.
- `need_rescan()`, notify error, hoặc rename không phân loại an toàn được → full reconciliation, có `reason` field ghi rõ vì sao (không phải một fallback im lặng).

Song song, dựng một **input catalog** dùng chung giữa resolver (đã biết input nào ảnh hưởng edge nào) và provider fingerprint (dùng để invalidate cache) — đây là cách duy nhất tránh lặp lại lỗi đã có: metadata invalidation hiện tại ở `crates/calm-core/src/indexer/pipeline.rs:2236` không bao phủ hết input thực tế ảnh hưởng graph (ví dụ `tsconfig` `extends` chain), và watcher ở `crates/calm-server/src/watcher.rs:100` vẫn full-scan gần như mọi event vì không có danh mục nào để tra "input này có nghĩa gì". `ChangeSet` là nơi cả hai phía (watcher-path thu thập `dirty: HashSet<PathBuf>` như §2 mô tả, và metadata-path) hội tụ về CÙNG một cơ chế phân loại, thay vì mỗi nơi tự đoán ngưỡng riêng.

### WatchSupervisor — watcher là accelerator, không phải nguồn chân lý duy nhất

Bọc `run_watch_loop` trong một `WatchSupervisor` chịu trách nhiệm: retry/backoff khi init/`watch()` lỗi (đúng tinh thần bounded self-heal đã nêu ở §3), health state (`armed`/`last_event_at`/`last_reindex_at`/`last_error` như §3 đã thiết kế), factory seam để test lỗi init/channel/notify mà không cần OS thật, và periodic full reconciliation có giới hạn thời gian (đề phòng watcher "tưởng sống" nhưng thực ra bỏ sót event lặng lẽ — khác với chết hẳn mà §3 đã xử lý). Watcher chết KHÔNG được để `indexing_phase` âm thầm đứng ở `Ready` — đúng nguyên tắc §3 đã nêu, tách rõ 5 trục trạng thái độc lập thay vì gộp vào 1 huy hiệu: index freshness, watcher liveness, last successful refresh, graph mode, và SCIP efficacy (§4) kèm lý do suy giảm. Không dùng "N lần zero uplift" chung chung làm tín hiệu sức khoẻ watcher — cache hit không được tính vào đó, và zero mutation hoàn toàn khoẻ mạnh nếu mọi edge liên quan đã formal từ trước.

### Đích đến cho §5 — tách theo hành vi, không chỉ theo schema

`__toolsnaps__` (lưới an toàn §5 bước 1) chỉ chứng minh schema của tool, không chứng minh session isolation, guardrail behavior, edit serialization, hay HTTP/daemon behavior không đổi qua refactor — 4 thứ rủi ro cao nhất khi tách `CalmServer`. Trước khi tách file theo 4 cụm đã liệt kê, cần thêm một **behavioral characterization suite**: transcript/tool-invocation chuẩn hoá, 2 session chạy song song, preset/toolset switching, guardrail pre/post-edit, locking, và status transitions — verify các hành vi này KHÔNG đổi ở mỗi bước Strangler-Fig, không chỉ toolsnaps bit-for-bit. Đích kiến trúc cụ thể hoá thêm từ 4 cụm đã liệt kê ở §5:

```text
CalmServer transport facade
├─ ServerRuntime        (DB, indexing, refresh, watcher health)
├─ ConnectionState      (session-local review/write/toolset state)
├─ SessionRegistry
├─ EditCoordinator
└─ ToolRouter / policy
```

`SessionLog` giữ theo connection như hiện tại — không biến review freshness hay pending diff-impact reminder thành state global khi tách `GuardrailState` ra khỏi `CalmServer`; đây chính là bẫy dễ mắc nhất khi strangler-fig một guardrail gate. Sau khi behavioral suite xanh, mới dùng `[[boundaries]]` trong `thresholds.toml` (dòng ~27, cơ chế đã tồn tại) để khoá dependency ngược giữa các struct mới — đúng bước 5 đã nêu ở §5, không đổi.

## Thứ tự thi công khuyến nghị (v2 — thay thế thứ tự v1)

1. **Baseline contract, migration plan, deterministic fixtures, benchmark** — trước khi sửa dòng code nào: fixture đa ngôn ngữ có ký tự multi-byte, snapshot `indexing_status`/toolsnaps hiện tại làm baseline so sánh.
2. **§1 (SCIP evidence policy) + `EVIDENCE_POLICY_VERSION` + provenance/funnel status** — độc lập, giá trị cao nhất, rủi ro thấp nhất (chỉ ảnh hưởng provider hiện đang 0% hiệu quả).
3. **Core `ChangeSet`/`RefreshRequest` + input catalog + golden-equivalence với full rebuild** — nền tảng dùng chung cho cả watcher lẫn metadata-path, làm trước khi đụng watcher.
4. **`WatchSupervisor`, recovery, và reconciliation scheduler** (gộp §2 + §3) — dùng `ChangeSet` vừa dựng ở bước 3, có health signal trước khi bật dirty-path optimization.
5. **§4 (freshness/efficacy tách trục)** — dùng dữ liệu funnel thật từ bước 2 để calibrate ngưỡng N lần chạy thay vì đoán số.
6. **Behavioral characterization harness cho `CalmServer`, rồi Strangler-Fig extraction từng lát** (§5) — quy mô lớn nhất, rủi ro cao nhất, làm cuối cùng khi 4 mục trên đã ổn định.
7. **Pinned CI contract lane** cho toàn bộ trên; upstream-provider canary (scip-python/scip-typescript qua `npx`) tách thành lane cảnh báo riêng, không để version `npx` mutable quyết định merge gate.

### Gate bắt buộc (không tự nhận "done" bằng lời)

- Unicode/CRLF/alias/duplicate-call-name và ambiguity-rejection cho SCIP evidence policy (§1 Lớp 2).
- Provider generic/unknown vẫn strict fail-closed kể cả sau khi thêm policy.
- Semantic-policy migration (bump `EVIDENCE_POLICY_VERSION`) chỉ rerun đúng một lần trên state cũ, sau đó cache hit hợp lệ trở lại.
- Metadata-only refresh (Cargo/composer/Go/Python/TS) cho kết quả tương đương full rebuild qua golden-equivalence.
- Watcher failure rồi reconciliation vẫn bắt được drift đã bỏ lỡ trong lúc chết.
- Session isolation và guardrail behavior không đổi sau MỖI lát Strangler-Fig của §5, không chỉ sau khi xong hết.
- 3 nightly liên tiếp xanh với provider đã pin; upstream canary chỉ cảnh báo drift, không chặn merge.

### Hướng đã loại bỏ tường minh (để không lặp lại)

Fallback encoding toàn cục (không theo provider); parse SCIP moniker để suy ra tên gọi; dùng ngưỡng `dirty.len() > 50` làm discovery/refresh policy thay vì phân loại theo `ChangeSet`; dùng `last_graph_mode` làm tín hiệu watcher health; coi toolsnap là lưới an toàn DUY NHẤT cho refactor `CalmServer`.

Mỗi mục vẫn đi qua đúng quy trình dự án đã dùng cho các plan trước: audit-design trước khi code, ADR sau khi xong, `calm fitness_report`/toolsnaps làm bằng chứng "Done" — không tự nhận đã xong bằng lời.

## Execution record — §2 + §3 (2026-08-01)

**Đã thực thi và verify trên workspace hiện tại.**

- **§2 — ChangeSet / refresh executor:** `ChangeSet` phân loại theo ngữ nghĩa input (source, context, coverage, unsafe) và chuyển `notify::Event::need_rescan()`, rename không an toàn, watcher error thành reconciliation có lý do tường minh. Không còn ngưỡng số lượng dirty path. `InputCatalog` là nguồn chung cho watcher, refresh và SCIP eligibility/cache context.
- **Correctness closure cho reconciliation/restart:** thêm persisted `index_input_state` contract. Source bytes không đổi vẫn được xử lý đúng: contract sạch dùng hash-delta; context/manifest drift rebuild graph từ index; config/policy drift hoặc contract thiếu/cũ chạy full atomic baseline. Bootstrap chỉ persist input quan sát *trước* index; sau khi backend watch đã arm, `WatcherStart` reconcile trước readiness sẽ bắt cả config/context đổi trong lúc index lẫn source write ở observation gap.
- **§3 — WatchSupervisor:** thay vòng watch trực tiếp bằng lifecycle có arm/re-arm bounded backoff, panic/channel-disconnect recovery, degraded mode, periodic reconciliation và status tách `lifecycle` khỏi `freshness`. `ready` chỉ phát sau startup reconciliation (writer-quiescence boundary), trong khi backend đã arm từ trước để buffer event. Event rescan của `notify` là fallback an toàn, không phải full scan vô điều kiện trên mọi event.
- **SCIP/observability:** overlay sau graph rebuild được coalesce; `indexing_status` đã có watcher health và toolsnap công khai tương ứng.

Evidence mới nhất:

- `cargo fmt --check` và `cargo clippy --workspace --all-targets -- -D warnings` xanh.
- `cargo test -p calm-server watch_supervisor --lib --no-fail-fast -q`: 5 passed; `watcher_integration`: 3 passed.
- `cargo test --workspace --no-fail-fast -q`: toàn bộ xanh (`calm-core` 898 passed, 12 ignored; `calm-server` 286 passed).
- Regression proof gồm config drift với source hash không đổi, persisted contract phân biệt context/config drift, bootstrap observation-gap, và concurrent edit/watcher lock race; toolsnap schema check xanh.

## Task Risk Summary (task-risk-score)

<!-- last-run: 2026-08-01 | context: INFRASTRUCTURE -->

| Task | S×B/D | QBR | Risk | Boundary | Action |
|---|---:|---:|---|---|---|
| Core input catalog + `ChangeSet` classifier | 3×3/2 | 4.5 | MEDIUM | SINGLE | Unit/property tests and golden-equivalence |
| Core refresh executor + metadata-only graph rebuild | 3×3/2 | 4.5 | MEDIUM | SINGLE | Golden equivalence against full rebuild |
| Watch lifecycle, retry and health state | 3×3/1 | 9 | HIGH | SINGLE | Decomposed; injected failures + integration verification |
| Periodic reconciliation and stale-drift recovery | 3×3/1 | 9 | HIGH | SINGLE | Decomposed; virtual clock/cancellation + missed-event recovery test |
| Indexing-status watcher surface | 2×3/2 | 3 | MEDIUM | SINGLE | Schema mirror + behavior test |

High-risk work is intentionally split so lifecycle/retry and reconciliation can be independently tested before integration.
