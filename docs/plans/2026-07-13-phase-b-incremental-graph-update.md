# Phase B — Incremental Graph Update: Plan thi công chi tiết

> Nguồn: `docs/plans/2026-07-12-upgrade-plan-3-architecture.md` §3.1 Phase B (bị hoãn có chủ đích 2026-07-12, khởi động lại theo go-ahead tường minh của user 2026-07-13). Đây là plan THI CÔNG — thay thế bản phác thảo trong Plan 3 ở mọi chỗ hai bản khác nhau, vì bản này viết SAU khi đọc trọn code thật (không tin plan text/comment/line-number cũ).
>
> **Mục tiêu:** edit 1 file chỉ đụng các `call_edges` liên quan tới file đó; edges của phần repo không đổi — kể cả enrichment `formal`/`ruled_out_by_scip` từ SCIP/LSP overlay — **sống nguyên**. Đo được: phần reindex+graph của `edit_symbol` trên chính repo CALM < 150ms; formal edges sống 100% trừ changed files.

---

## §1 — Bản đồ code thật (đọc trực tiếp 2026-07-13, mọi line number tươi)

Đây là danh sách evidence anchor ĐÃ VERIFY — khác biệt so với Plan 3 gốc được đánh dấu **[≠plan]**:

| Thành phần | Vị trí thật | Fact đã verify |
|---|---|---|
| `rebuild_graph` | `pipeline.rs:578-921` | Nhận `(tx, hub_config, crate_map, psr4, namespace_map)`. Các bước: build `by_name`/`by_name_class`/`sig_by_qn`/`path_lang` từ **toàn bộ** `symbols` → load **toàn bộ** `call_sites` → `caller_usings` từ `import_edges` (C#-only) → resolve candidates song song (rayon) → dedup tuần tự `seen_pairs: HashSet<(enc_qn, to_qn, Option<line>)>` → `DELETE FROM call_edges` (dòng 901) → `insert_call_edges_batch` → `UPDATE formal_source='stack_graphs' WHERE edge_confidence='formal' AND formal_source IS NULL` → `refresh_caller_counts` → `resolve_import_targets` **[≠plan: plan không biết bước này]** → `compute_coreness` → `update_is_hub_flags` → `update_boundary_ambiguous_flags` **[≠plan: mới thêm ở d1518d8, sau khi plan viết]** |
| Resolution per site | trong `rebuild_graph` | Chuỗi narrowing: tier-2 `by_name_class[(callee, target_class)]` hoặc tier-1 `by_name[callee]` → same-language filter (`path_lang` của caller) → return-shape filter khi `looks_option_or_result_chained` (đọc `sig_by_qn` của **candidates**) → `module_hint` (file_stem match) → same-file → same-dir (go/java/c/cpp) → same-namespace (C#, `caller_usings`+`namespace_map`, narrowing-to-1 upgrade lên `resolved`) → global fan-out nếu `len ≤ MAX_CALLEE_CANDIDATES`(20), else **drop**. `targets.len()>1` → downgrade `ambiguous`. |
| `reindex_paths` (Phase A) | `pipeline.rs:1749-1833` | Per path: hash-compare → `remove_file_rows` + `persist_file` + `upsert_file_index`; nếu `!summary.is_noop()` → gọi `rebuild_graph` full. Điểm hook capture names-delta nằm quanh `remove_file_rows`/`persist_file`. |
| `reindex_changed_cancellable` (watcher) | `pipeline.rs:1598-1726` | **CÓ SẴN** danh sách path đổi (`changed: Vec<Candidate>` với `rel`) VÀ path bị xóa (vòng `existing.keys() ∉ seen_paths`) — **[≠plan/≠audit-design cũ: L7 hỏi "watcher có biết path list không" — CÓ, ngay tại hàm này, không cần sửa watcher.rs]**. Incremental áp dụng được cho cả hai đường. |
| `remove_file_rows` | `pipeline.rs:154-161` | Xóa `symbols`/`call_sites`/`import_edges`/`file_index`/`code_chunks` theo path. **KHÔNG** đụng `call_edges` — edges chỉ bị dọn trong `rebuild_graph`. |
| `persist_file` | `pipeline.rs:543-571` | Ghi symbols + import_edges + call_sites (10 cột, gồm `receiver`) + code_chunks. |
| `refresh_caller_counts` | `pipeline.rs:939-949` | 1 câu UPDATE global: `COUNT(DISTINCT from_symbol)` loại `ruled_out_by_scip=1` và `edge_confidence='ambiguous'`. |
| `resolve_import_targets` | `pipeline.rs:954-994` | Global trên toàn bộ `import_edges`, resolve song song, UPDATE `to_path`. |
| `compute_coreness` | `graph/coreness.rs:13-98` | K-core in-memory trên **toàn bộ** edges, **không lọc confidence** — reset rồi ghi lại toàn bộ `coreness`. Kết quả là fixpoint duy nhất → order-independent. |
| `update_is_hub_flags` | `graph/hub.rs:11-86` | Percentile rank **global** trên caller_count + p75 coreness; ghi `is_hub` + `hub_kind` (degree/bridge/both). Bản chất global — không incremental hóa được, cũng không cần. |
| `update_boundary_ambiguous_flags` | `graph/boundary.rs:14-46` | Global sweep `symbols` theo (path, line_start) window. |
| SCIP overlay ingest | `scip/ingest.rs:75-191` | Làm **3 việc**: (1) UPDATE `edge_confidence='formal', formal_source='scip'` theo edge id; (2) `mark_ruled_out_siblings` set `ruled_out_by_scip=1`; (3) **`insert_missing_edges`** — chèn edges KHÔNG có `call_sites` backing (tree-sitter miss). Điểm (3) là lý do dangling-sweep **bắt buộc**. |
| Schema | `db/schema.rs` | `call_edges`: `id INTEGER PRIMARY KEY AUTOINCREMENT` (rowid alias, không reuse) + `ruled_out_by_scip INTEGER NOT NULL DEFAULT 0` + `formal_source TEXT` (nullable). Index sẵn: `idx_call_edges_from/to/fpath`, `idx_call_sites_from`, `idx_call_sites_callee`. **Phase B không cần migration schema nào.** |
| Edit path | `tools/edit.rs:670-707` | `edit_lines_impl` gọi `reindex_paths(&[path])` trong edit lock; nếu non-noop: `should_embed_bg` + spawn `scip_overlay::run_all_coalesced` nền. `EMBED_BG: Mutex<()>` (edit.rs:14) đã serialize embed từ Phase C. |
| `ReindexSummary` | `pipeline.rs:141-149` | Chỉ có `changed`/`deleted: usize` — cần mở rộng cho names-delta + observability. |
| Config | `config.rs:6-15` | Struct-level `#[serde(default)]`; chưa có section `indexing` — flag mới sẽ là field mới + được `diff_from_default`/`repo_overview.config_override` tự surface (đã ship d50353a). |

## §2 — Các quyết định thiết kế (rút từ code thật, khác bản phác thảo Plan 3)

**D1 — Granularity theo `from_path`, KHÔNG theo từng call site.** Plan 3 step 3 re-resolve theo site lẻ; điều đó tạo lỗ hổng dedup/xóa-sót: một site ở file KHÔNG đổi được re-resolve trong khi edges cũ của chính nó chưa bị xóa (ví dụ: file X thêm `foo` trùng tên `foo` sẵn có ở B → site ở Y gọi `foo` giờ ra 2 targets `ambiguous`, nhưng edge cũ Y→B::foo `resolved` vẫn nằm trong DB) → duplicate + confidence mâu thuẫn. Thay bằng:
```
delta_paths = changed_paths ∪ deleted_paths
            ∪ { DISTINCT from_path FROM call_sites WHERE callee_name IN names_delta }
```
Xóa **mọi** edge `WHERE from_path IN delta_paths`, re-resolve **mọi** site `WHERE from_path IN delta_paths`. Vì mỗi edge thuộc đúng 1 `from_path` và `enc_qn` chứa path (không thể trùng cross-file), tập edges được **phân hoạch sạch** theo from_path → `seen_pairs` chạy trong phạm vi delta là đủ, không thể double-count với edges giữ lại. Giải trọn finding L1 của audit cũ.

**D2 — `names_delta` = old ∪ new (UNION, không phải symmetric-diff).** `old_names` = mọi `name` trong `symbols` của changed file TRƯỚC `remove_file_rows`; `new_names` = mọi name SAU persist. Union bắt cả: rename (old có, new mất + ngược lại), **signature-only change** (name không đổi nhưng `sig_by_qn` đổi → return-shape filter ở site khác file có thể đổi kết quả), `class_context` đổi (by_name_class), namespace declaration đổi quanh symbol không đổi. Chứng minh đủ (bảng §3.1).

**D3 — Mọi metric pass giữ NGUYÊN global.** `refresh_caller_counts`, `resolve_import_targets`, `compute_coreness`, `update_is_hub_flags`, `update_boundary_ambiguous_flags` chạy y hệt full rebuild — chúng là hàm thuần của trạng thái DB, nên **equivalence-by-construction**: nếu tập edges khớp thì mọi metric khớp. Ở scale CALM (3K symbols/≈6K edges) tổng chi phí là mili-giây. **[≠plan: Plan 3 step 5 muốn scoped caller_count — bỏ, phức tạp không cần thiết và là nguồn rủi ro divergence duy nhất có thể tự loại.]**

**D4 — MỘT resolver dùng chung (quan trọng nhất).** Tách phần thân `rebuild_graph` thành helper: `build_resolution_context(tx, namespace_map) -> ResolutionCtx` (by_name/by_name_class/sig_by_qn/path_lang/caller_usings) + `resolve_sites_to_edges(ctx, sites) -> Vec<CallEdge>` (closure narrowing + seen_pairs + effective_confidence). `rebuild_graph` và `incremental_graph_update` **cùng gọi đúng code đó** — khác nhau DUY NHẤT ở (a) tập sites nạp vào, (b) phạm vi DELETE. Golden equivalence khi đó quy về đúng 1 mệnh đề phải chứng minh: *tập delta_paths chọn đúng*. Logic resolve không thể divergence vì không tồn tại bản thứ hai.

**D5 — Dangling sweep bắt buộc, chạy mọi lần.** `DELETE FROM call_edges WHERE to_symbol NOT IN (SELECT qualified_name FROM symbols)` — vì SCIP `insert_missing_edges` tạo edges không có call_sites backing: khi symbol đích bị xóa, không có site nào trong delta trỏ tới nó để dọn qua đường re-resolve. Chi phí: 1 scan 6K rows vs hash set 3K — trivial, chạy vô điều kiện.

**D6 — Áp dụng cho CẢ HAI call site.** `reindex_paths` (edit path — luôn 1 file) và `reindex_changed_cancellable` (watcher/branch-switch — đã có sẵn changed+deleted list, §1). Ngưỡng fallback `delta_paths.len() > 50` → full `rebuild_graph`, check ở **cả hai** (đóng L7 của audit cũ). Edit path thực tế không bao giờ chạm ngưỡng; watcher branch-switch là nơi ngưỡng sống.

**D7 — Ngữ nghĩa golden equivalence phải nêu tường minh.** Equivalence tuyệt đối `incremental == full-rebuild` **chỉ đúng trên DB chưa có overlay enrichment** — vì incremental *cố ý giữ* những gì full rebuild *phá* (`formal_source='scip'`, `ruled_out_by_scip=1`, scip-inserted edges, confidence đã upgrade). Trên DB đã overlay, incremental cho kết quả **tốt hơn** full rebuild, không bằng. Do đó: (a) golden test so sánh trên DB được index thuần (không chạy overlay); (b) survival của scip enrichment test riêng (test #3); (c) so sánh bằng **semantic key** `(from_symbol, to_symbol, call_site_line, edge_confidence, edge_kind, from_path, to_path)` + per-symbol `(caller_count, coreness, is_hub, hub_kind, boundary_ambiguous)` — KHÔNG so `id`.

**D8 — Flag ship tắt, bật sau khi golden trên DB thật xanh.** `Config.indexing.incremental_graph: bool`. T4 ship với default `false` (code mới nằm im); commit T6 flip default `true` sau khi golden-on-real-CALM-DB + dogfood pass. `calm index --full` (CLI) luôn full — escape hatch vĩnh viễn. **[≠plan: Plan 3 nói default true ngay — đổi để không bao giờ tồn tại build nào bật thuật toán chưa qua real-DB golden.]**

## §3 — Thuật toán `incremental_graph_update`

```rust
/// Chỉ gọi khi summary non-noop. delta_seed = changed ∪ deleted rel_paths.
/// names_delta = ∪ per changed/deleted file (old_names ∪ new_names).
pub fn incremental_graph_update(
    tx: &Transaction,
    delta_seed: &[String],
    names_delta: &HashSet<String>,
    hub_config: &HubThresholdConfig,
    crate_map: &CrateMap, psr4: &Psr4Map, namespace_map: &NamespaceMap,
) -> rusqlite::Result<()>
```
1. **Mở rộng delta:** `delta_paths = delta_seed ∪ {from_path của call_sites có callee_name ∈ names_delta}` — query `idx_call_sites_callee`, chunk IN theo lô ≤500 (tránh trần SQLITE_MAX_VARIABLE_NUMBER; xem A-1).
2. **Fallback check:** `delta_paths.len() > 50` → return qua `rebuild_graph` (caller quyết định, xem §4 T4).
3. **DELETE có phạm vi:** `DELETE FROM call_edges WHERE from_path IN delta_paths` (`idx_call_edges_fpath`; chunk như trên). Ghi chú: scip-inserted edges có `from_path ∈ delta` cũng bị xóa — đúng ngữ nghĩa (nội dung file đó đã đổi, overlay nền sẽ vá lại, giờ chỉ còn phải vá changed files).
4. **Dangling sweep (D5):** chạy vô điều kiện.
5. **Build context (D4):** `build_resolution_context(tx, namespace_map)` — vẫn 1 lần SELECT toàn bộ `symbols` (chấp nhận, như plan gốc step 4; backlog Phase B+ cache in-daemon).
6. **Re-resolve:** load `call_sites WHERE from_path IN delta_paths` (giữ thứ tự ổn định `ORDER BY id` để "first site wins" như full rebuild — full rebuild load không ORDER BY nhưng insertion-order của bảng ≈ id; **phải dùng cùng ORDER BY ở cả hai đường sau refactor D4** để dedup attribution y hệt) → `resolve_sites_to_edges` → insert → `UPDATE formal_source='stack_graphs' WHERE edge_confidence='formal' AND formal_source IS NULL` (global, kết quả y hệt scoped vì rows cũ đã có formal_source).
7. **Metric passes (D3):** đúng 5 hàm, đúng thứ tự như `rebuild_graph:915-919`.

### §3.1 — Bảng chứng minh delta đủ (mọi input của resolution → đường nào bắt)

| Input của resolve | Đổi khi nào | Được bắt bởi |
|---|---|---|
| `by_name`/`by_name_class` entry | symbol thêm/xóa/rename/đổi class ở changed file | tên đó ∈ `names_delta` (union) → mọi site gọi tên đó vào delta |
| `sig_by_qn` của candidate | signature đổi, name giữ | name ∈ `names_delta` (union chứa cả tên không đổi của changed file) |
| `path_lang` của caller | chỉ đổi cho changed file | site của changed file ∈ delta_seed |
| `caller_usings` (C#) | `using` đổi trong changed file | chỉ ảnh hưởng sites của chính file đó ∈ delta_seed |
| `namespace_map` membership của candidate | namespace declaration đổi trong changed file | mọi symbol của changed file có name ∈ names_delta |
| `crate_map`/`psr4`/`namespace_map` (Phase D cache) | manifest đổi | ngoài phạm vi per-edit — xem Risk/Abductive-1 (kế thừa từ Phase D, có mitigation T4b) |
| Edges không có call_sites backing (scip-inserted) | target symbol biến mất | dangling sweep (D5) |

## §4 — Task thi công (thứ tự bắt buộc, mỗi task 1 commit, độc lập revert được)

- **T1 — Hạ tầng golden-equivalence TRƯỚC TIÊN** (điều kiện user đặt khi hoãn). `crates/calm-core/tests/golden_graph_equivalence.rs`: (a) `graph_fingerprint(conn) -> (BTreeSet<EdgeKey>, BTreeMap<Qn, MetricKey>)` theo semantic key D7; (b) mutation driver RNG seed cố định, 5 vòng × {sửa body, rename fn, thêm file, xóa file, rename-thành-tên-trùng-cross-file}; (c) **sanity chạy trên code HIỆN TẠI**: sau mỗi mutation, full-rebuild-tiếp-diễn (DB A) vs index-lại-từ-đầu (DB B) phải khớp — chứng minh harness đúng + `compute_coreness`/hub determinism, TRƯỚC khi incremental tồn tại. Fixture: copy `multi_lang_workspace` vào tempdir (không mutate fixture gốc).
- **T2 — Refactor D4, zero behavior change.** Tách `build_resolution_context` + `resolve_sites_to_edges` khỏi `rebuild_graph`; thêm `ORDER BY id` khi load sites ở CẢ full path (chốt thứ tự dedup). Bằng chứng không đổi hành vi: T1 sanity + full suite xanh, fingerprint DB CALM trước/sau refactor khớp.
- **T3 — Plumbing names-delta.** `ReindexSummary` thêm `changed_paths: Vec<String>`, `names_delta: HashSet<String>`, `graph_mode: GraphMode` (`Full`/`Incremental`/`FullFallback(reason)`); capture old/new names trong `reindex_paths` + `reindex_changed_cancellable` (old: SELECT trước remove; new: từ `ExtractedFile.symbols` in-memory). Vẫn gọi full rebuild — plumbing thuần.
- **T4 — `incremental_graph_update` + flag + wire.** Config `indexing.incremental_graph` default **false**; cả 2 call site: flag on && delta ≤50 → incremental, else full; set `graph_mode` tương ứng. **T4b:** nếu changed path là manifest (`Cargo.toml`/`Cargo.lock`/`composer.json`) → chủ động invalidate `cached_resolution_maps` entry của project_root (đóng Abductive-1 của audit Plan 3 luôn thể — 3 dòng).
- **T5 — Tests đặc thù** (ngoài golden): `unchanged_file_edges_survive_by_id` (so cột `id` của edges ngoài delta trước/sau); `scip_flag_survives_edit_of_other_file` (set tay `ruled_out_by_scip=1`+`formal_source='scip'`); `rename_reroutes_cross_file_edge`; `rename_collides_with_existing_name` (L1 case: Y→B::foo resolved → X thêm foo → Y phải thành 2 edges ambiguous, edge resolved cũ PHẢI biến mất); `scip_inserted_edge_cleaned_when_target_deleted`; `sig_only_change_reresolves_chained_caller` (D2 union case); `delta_over_50_falls_back_full`; `flag_off_is_bitwise_full_rebuild`.
- **T6 — Golden trên DB thật + flip default.** Chạy T1 driver ≥1 vòng trên copy DB đã index của chính repo CALM (kích hoạt nhánh `MAX_CALLEE_CANDIDATES` — điều kiện (c) của gate cũ); dogfood live qua daemon; đo phần reindex+graph < 150ms (log `tool_execution_completed`); flip default `true`; expose `graph_mode` trong `indexing_status` (đóng L6).
- **T7 — Chốt sổ.** Điền 2 dòng N/A trong bảng nghiệm thu Plan 3; đánh dấu F1 RESOLVED hoàn toàn trong audit doc; ADR nếu adr-commit yêu cầu; re-verify `embed_pending*` idempotency (nghĩa vụ carried-forward từ Risk Plan 3 FM2 — đọc lại `embedding.rs` sau khi B đổi tần suất ghi).

**Done when:** golden 5-vòng xanh 3 lần liên tiếp (fixture) + ≥1 vòng xanh trên DB CALM thật; formal edges đếm trước/sau 1 edit: sống 100% trừ changed files; < 150ms; `graph_mode` quan sát được. **Rollback:** flag về `false` (T4) hoặc revert từng commit — full rebuild còn nguyên vẹn không đụng.

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-13 | trigger: NORMAL -->

**Tier:** 2 (Production — MCP server nhiều agent dựa vào; graph sai = gate an toàn sai lặng lẽ; không PII/payments) | **Date:** 2026-07-13

**Phương pháp:** mọi evidence anchor trong §1 đọc trực tiếp từ source qua `mcp__calm__source`/`search(kind="grep")` trong phiên này (2026-07-13), KHÔNG kế thừa line number hay giả định từ Plan 3/audit cũ. Các khác biệt so với plan gốc đánh dấu [≠plan] tại chỗ.

### Failure Modes
1. **Delta under-selection — một input của resolution đổi mà không lọt `names_delta`/`delta_paths` → edge cũ sai giữ lại LẶNG LẼ** (không crash, chỉ graph dối) → thiết kế cho phép vì delta là whitelist suy luận tay — **HIGH** — mitigation trong plan: **YES** — (a) bảng chứng minh exhaustive §3.1 liệt kê từng input; (b) D4 một-resolver loại divergence logic; (c) golden driver có case rename-collision + sig-only-change nhắm thẳng các đường khó; (d) T6 chạy trên DB thật kích hoạt fan-out branch. Residual: input MỚI thêm vào resolver tương lai mà quên cập nhật §3.1 — chốt bằng comment trỏ ngược tại `resolve_sites_to_edges` yêu cầu cập nhật bảng khi thêm narrowing mới.
2. **Overlay nền race với incremental update thứ hai** — `run_all_coalesced` (writer conn riêng) snapshot edge-ids rồi UPDATE theo id; một edit khác commit incremental xen giữa → id đã bị xóa: UPDATE by-id thành no-op lành tính; nhưng `insert_missing_edges` có thể chèn edge cho file vừa đổi dựa trên SCIP index cũ → edge stale tồn tại đến lần overlay sau — **MED** — mitigation trong plan: **PARTIAL** (không tệ hơn hiện trạng: hôm nay overlay race với full-DELETE còn thô bạo hơn; dangling sweep dọn được target-mất; chấp nhận + ghi nhận, KHÔNG chặn Phase B; nếu thành vấn đề thật → backlog "overlay generation counter").
3. **Fallback plumbing sai → âm thầm full-rebuild mãi mãi (mất toàn bộ giá trị) hoặc tệ hơn: summary non-noop nhưng delta_seed rỗng → skip graph update sai** — **MED** — mitigation trong plan: **YES** — `graph_mode` trong `ReindexSummary` + `indexing_status` (T6) làm mode quan sát được; T4 thêm `debug_assert!(!delta_seed.is_empty())` khi summary non-noop; test `delta_over_50_falls_back_full` + `flag_off_is_bitwise_full_rebuild` khóa cả hai nhánh.

### Layer Signals
- **L1 Logic:** case rename-thành-tên-trùng (site ở file không đổi phải chuyển resolved→ambiguous VÀ edge cũ phải biến mất) — có test T5 chuyên trách; dedup scope giải bằng phân hoạch from_path (D1), không còn seen_pairs cross-boundary.
- **L2 Concurrency:** `EMBED_BG` đã serialize embed (Phase C, edit.rs:14, đọc lại hôm nay); nghĩa vụ re-verify `embed_pending*` idempotency sau Phase B ghi thành task T7, không bỏ rơi.
- **L3 Data:** KHÔNG có schema migration nào — `call_edges.id` là `INTEGER PRIMARY KEY AUTOINCREMENT` (verify hôm nay), id không reuse → test survival so theo `id` hợp lệ; comparator golden dùng semantic key, không dùng id.
- **L4 Integration:** Failure Mode 2 (overlay race) — chấp nhận có ghi nhận. `run_all_coalesced` giữ nguyên hành vi spawn sau edit (edit.rs:700-707).
- **L5 Security:** không có surface mới — thuật toán chỉ đổi CÁCH tính cùng một dữ liệu; hub/gate flags vẫn từ đúng 5 pass global cũ. No signal mới.
- **L6 Observability:** `graph_mode` ("incremental"/"full"/"full_fallback:reason") trong `ReindexSummary` → `indexing_status` (T6). Trước đó (T4-T5) flag default false nên không có giai đoạn mù.
- **L7 Cross-cutting:** ngưỡng >50 check ở CẢ `reindex_paths` lẫn `reindex_changed_cancellable` (D6) — điểm treo của audit cũ đã đóng bằng đọc code thật (watcher path CÓ path list).

### Assumptions to Verify
- **ASSUMED A-1:** chunk IN-list ≤500 tránh trần biến SQLite trong mọi build rusqlite đang dùng — verify `SQLITE_MAX_VARIABLE_NUMBER` thực tế (mặc định 32766 từ SQLite 3.32, nhưng chốt bằng test names_delta 10K tên trong T5).
- **ASSUMED A-2:** `multi_lang_workspace` fixture có đủ cặp symbol trùng tên cross-file để mutation driver kích hoạt các nhánh narrowing — kiểm tại T1, nếu thiếu thì bổ fixture (không nới driver).
- **ASSUMED A-3:** thứ tự load sites hiện tại của full rebuild ≈ insertion order = `ORDER BY id` — T2 chốt cứng bằng ORDER BY tường minh ở cả hai đường TRƯỚC khi so sánh; nếu fingerprint trước/sau T2 lệch trên DB CALM thì chính T2 đã tìm ra một nondeterminism có sẵn (tốt — sửa luôn ở T2).
- **ASSUMED A-4:** `compute_coreness` deterministic dù iterate HashSet (k-core fixpoint duy nhất về GIÁ TRỊ) — T1 sanity identity-run xác nhận thực nghiệm.

### Abductive Hypotheses
- **Abductive 1 (tương tác giữa components đúng riêng lẻ):** Phase D map-cache stale (TTL 60s) + Phase B narrow re-resolve = mis-resolution do map cũ giờ **tồn tại lâu hơn**: full rebuild tự-chữa ở edit kế tiếp bất kỳ (resolve lại tất cả), incremental thì không bao giờ quay lại site cũ đến khi input nó đổi. Mitigation THẬT trong plan: T4b invalidate cache khi manifest nằm trong changed set — đường stale phổ biến nhất (sửa Cargo.toml qua edit tool) bị đóng; đường còn lại (sửa manifest ngoài CALM tool + watcher miss) bounded 60s TTL như hiện trạng.
- **Abductive 2 (chỉ lộ ở scale/adversarial):** repo có N site cùng gọi 1 tên phổ biến (`new`: hàng trăm site) — 1 edit vào file define `new` kéo `delta_paths` phình gần toàn repo → incremental chạy như full nhưng CỘNG thêm chi phí delta-expansion query → chậm hơn full thuần. Ngưỡng 50 file chặn được phần đuôi, nhưng adversarial case nằm NGAY DƯỚI ngưỡng (49 file × resolve) vẫn nặng hơn kỳ vọng "1 file = vài site". Không phải correctness risk (kết quả vẫn đúng), là perf-cliff: đo trên CALM thật ở T6 với edit vào file chứa helper trùng tên nhiều nhất (`common.rs`), ghi số vào bảng nghiệm thu; nếu tệ → hạ ngưỡng fallback theo `delta_paths.len()`, không cần đổi thuật toán.

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
PASS WITH FLAGS — 1 HIGH (delta under-selection) có mitigation nhiều lớp cụ thể trong plan (proof-table §3.1 + single-resolver D4 + targeted tests + real-DB round T6); 2 MED có mitigation hoặc chấp-nhận-có-ghi-nhận. Điều kiện cứng trước khi flip default `true` (T6): golden ≥1 vòng trên DB CALM thật xanh + 8 test T5 xanh + số đo <150ms có thật. Thứ tự task T1→T7 là bắt buộc — đặc biệt T1 (hạ tầng golden) và T2 (refactor một-resolver) phải đi trước mọi dòng code thuật toán mới, đúng điều kiện user đặt khi hoãn Phase B.

---

## Execution outcome (2026-07-13) — DONE

T1→T7 shipped in order. Commits: `d6481e1` (plan) → `0806531` (T4) → `fe873bf` (T5) → `7df3800` (T6.1 real-DB golden + A-1 chunk test) → `313b623` (T6.5 graph_mode observability) → `efbd9d2` (T6.4 default flip). T2/T3 landed within the T4 series.

**Gate conditions met:** golden equivalence (incremental == full) green on the fixture driver (18 comparisons, permanent/CI) AND a copy of the real CALM repo (`#[ignore]`, ~206s); A-1 chunk boundary (`names_delta`>500) covered; full calm-core (698+12) & calm-server (202+3) suites green after the flip. Live measurement via daemon `edit_reindex_completed{reindex_ms,graph_mode}`: **88ms** minimal-delta / **188ms** fan-out to largest file / **337ms** fallback — <150ms met for typical edits, incremental 1.8–4× faster than full. `graph_mode` (`incremental` / `full_fallback:delta_paths.len()=76 > 50`) visible in `indexing_status`. Formal edges outside the delta survive (T5 `scip_flag_survives_edit_of_other_file` + live).

**Corrections found during execution:**
- Abductive-2 / condition (c) refined: no real function/method name in CALM hits the `MAX_CALLEE_CANDIDATES`(20) fan-out threshold naturally (max ~18), and D4's single resolver makes that branch produce IDENTICAL output in both paths anyway — so it is NOT a differential-correctness risk. The real-DB round's genuine value is scale (large `by_name` map, real cross-language resolution), not “exercising fan-out”. Seed files for the real-DB test therefore use repo-UNIQUE names — a collision with a hot name like `new` (~740 sites) would blow the delta past the 50-file fallback and mask the incremental path.
- Finding B (perf-cliff, live-confirmed): editing a file that defines a hot name (`common.rs` defines `CalmServer::new`) fans `names_delta` out to 76 files >50 → correct `full_fallback`, not a slow incremental. This is the fallback working as designed, observable via `graph_mode`.
- A-1 was un-tested at T5 despite the plan hinting it; added `names_delta_over_chunk_size_matches_full` at T6.1.

**T7 carried-forward obligation — `embed_pending*` idempotency re-verified (Plan 3 FM2):** `embed_pending`/`embed_pending_chunks` (`embedding.rs:320/397`) select only rows `WHERE id NOT IN (SELECT symbol_id FROM embedding_vecs)` and store via `INSERT OR REPLACE`, so re-running is idempotent by construction; `EMBED_BG` mutex (edit.rs:14) still serializes background embeds. Phase B changes the GRAPH-update path only — it does not change embed frequency (still one spawn per non-noop reindex) or the embed contract — so it introduces no new embed race. A background embed racing a subsequent reindex can at worst leave an orphaned `embedding_vecs` row (invisible to `knn`, which JOINs `symbols`), identical to pre-Phase-B behavior. No code change needed.
