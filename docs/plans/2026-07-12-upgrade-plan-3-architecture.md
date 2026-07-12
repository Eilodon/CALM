# CALM Upgrade Plan 3/3 — Architecture: Incremental Graph, Ranking, Hub Tiering & Agent-UX

> Nguồn: `docs/audit/2026-07-12-vheatm-deep-audit.md` (Đợt 3 + mục Chiến lược). Series: [Plan 1](2026-07-12-upgrade-plan-1-correctness-safety.md) → [Plan 2](2026-07-12-upgrade-plan-2-performance-robustness.md) → **Plan 3 (file này)**.
>
> **Phạm vi:** F1 (incremental reindex — item giá trị nhất toàn dự án), F3 (personalization ranking), F10 (hub tiering), F15 (entry_points), và 4 hạng mục chiến lược cho agent-UX. Đây là plan duy nhất có thay đổi kiến trúc + schema. Ước lượng: 1-2 tuần, làm theo phase, **mỗi phase ship được độc lập**.
>
> **Điều kiện tiên quyết:** Plan 1 + Plan 2 merged. Đặc biệt: bảng baseline Plan 2 đã có số — Plan 3 phải chứng minh cải thiện bằng chính bảng đó mở rộng.
>
> **Rủi ro chung & phanh an toàn:** mọi thay đổi graph-related phải giữ **golden equivalence** (§3.1 Phase B) — incremental result == full-rebuild result. Sai lệch = revert, không "sửa cố". Config flag tắt được từng tính năng mới.

---

## §3.1 — F1: Incremental reindex — gỡ trần scale  ⏱ 4 phase, ship độc lập

**Bức tranh evidence (từ audit):**
- `tools/edit.rs:420-470` — reindex chạy ĐỒNG BỘ trong `edit_lock`, mỗi edit.
- `pipeline.rs:1464-1597` — `reindex_changed` walk + đọc + hash **toàn bộ repo** để tìm file đổi, dù edit path đã biết chính xác file nào đổi.
- `pipeline.rs:880` — `rebuild_graph` mở đầu bằng `DELETE FROM call_edges` (toàn bộ), re-resolve mọi call_site; đồng thời giết mọi `formal` upgrade từ SCIP/LSP overlay (chính comment tại `tools/edit.rs:444-456` xác nhận + workaround chạy lại overlay ~20s nền mỗi edit).
- `pipeline.rs:1585-1594` — `CrateMap`/`Psr4Map`/`NamespaceMap` build lại từ disk-walk mỗi lần.

### Phase A — Dirty-path reindex cho edit path  ⏱ ~1 ngày · ship riêng

**Mục tiêu:** `edit_lines`/`edit_symbol` không walk repo nữa — reindex đúng file vừa ghi.

**Thiết kế** — hàm mới trong `pipeline.rs`, tái dùng ruột `reindex_changed`:
```rust
/// Reindex đúng các rel_paths cho trước — KHÔNG walk repo. Dùng bởi edit
/// path (biết chính xác file vừa ghi) và watcher (biết path từ event).
/// File không còn tồn tại → remove_file_rows (deletion). Hash không đổi
/// → skip parse. Trả cùng ReindexSummary như reindex_changed.
pub fn reindex_paths(
    conn: &mut Connection,
    project_root: &Path,
    rel_paths: &[String],
) -> rusqlite::Result<ReindexSummary>
```
Các bước bên trong (mirror `reindex_changed_cancellable`, bỏ walk):
1. Load config + formal resolver 1 lần (như 1469-1477).
2. Per path: `exists()`? không → trong tx: `remove_file_rows`, `summary.deleted += 1`. Có → ext→lang / recognized_unparsed check (không nhận diện → skip); đọc, `hash_content`, so `file_index.hash` — trùng → skip; khác → `extract_file_data` → `remove_file_rows` + `persist_file` + `upsert_file_index` (đúng trình tự 1559-1574).
3. `!summary.is_noop()` → graph update: Phase A tạm gọi `rebuild_graph` như cũ (vẫn thắng lớn: bỏ được full-walk + full-hash); Phase B thay bằng incremental.
4. Call site: `tools/edit.rs:423` — `reindex_changed(&mut write_conn, &self.project_root)` → `reindex_paths(&mut write_conn, &self.project_root, &[path.to_string()])`.
5. Watcher (`crates/calm-server/src/watcher.rs`): **[audit-design flag — CONFIRMED, không còn ASSUMED]** đọc trực tiếp `run_watch_loop` (watcher.rs:71-217): vòng debounce chỉ track `coverage_touched: bool` + gọi `event_is_relevant(&res)` để quyết định có reindex hay không — KHÔNG bao giờ thu thập `res.paths` vào một list. Vẫn gọi `reindex_changed_cancellable` (full-walk) không đổi. **Quyết định cho Phase A:** để nguyên watcher như cũ (không phải thiếu sót âm thầm — giữ nguyên **có chủ đích**, ghi backlog “watcher path-tracking” cho phase sau nếu cần); phần thắng của Phase A **CHỈ** áp dụng cho edit-tool path (`edit_lines`/`edit_symbol`), không áp dụng cho watcher-driven reindex (branch switch, external editor) — đúng như nghi ngời.

**Tests:** `test_reindex_paths_only_touches_given_paths_not_whole_repo` — fixture 3 file; sửa 1 file qua `reindex_paths` → symbols file đó cập nhật, `file_index` 2 file kia giữ nguyên `last_indexed` (chứng minh không re-scan); xoá file → rows biến mất; path không tồn tại + chưa từng index → no-op không lỗi. **XANH.**
**Done when:** ĐÃ XONG — call site `tools/edit.rs:441` đổi sang `reindex_paths`; full workspace suite xanh (672 calm-core lib + 183 calm-server); dogfood live trên chính CALM xác nhận correctness (xem bảng đo cuối file).
**Rollback:** call site đổi 1 dòng — revert dễ.

### Phase B — Incremental graph update  ⏱ ~2-3 ngày · giá trị lớn nhất

> **HOÃN LẠI 2026-07-12 — quyết định có chủ đích, không phải bỏ sót.** Sau khi A/C/D xong (shipped+tested+dogfooded, commit `f567314`/`d341fbb`/`3bd9820`), đã hỏi user có muốn làm full Phase B ngay không — đây là hạng mục DUY NHẤT trong toàn Plan 3 thay đổi thuật toán resolve call-graph lõi (khác hẳn A/C/D — những cái chỉ thêm cache/reorder, luôn có full-rebuild làm fallback đúng). Sai sót ở đây = call edge sai **lặng lẽ** (không crash, không test nào bắt được trừ khi golden-equivalence infra đủ mạnh) — rủi ro cao nhất toàn plan theo cả audit gốc lẫn audit-design pass ở đầu file này. User chọn **bỏ qua hoàn toàn** cho phiên này, chuyển sang F3/F10/§3.5 — thuật toán incremental graph dưới đây **chưa code**, `rebuild_graph` (full rebuild) vẫn là con đường duy nhất thật (không có config flag `indexing.incremental_graph` nào đang bật một nhánh code không tồn tại). Dành cho 1 phiên riêng, đủ context budget, không phải mảnh cuối của 1 phiên đã rất dài.

**Mục tiêu:** edit 1 file chỉ đụng các edge liên quan tới file đó; edge của phần repo không đổi — kể cả `formal` upgrade từ SCIP/LSP — **sống nguyên**.

**Bước 0 (bắt buộc):** đọc trọn `rebuild_graph` (`pipeline.rs:557-~880`) + `refresh_caller_counts` + phần coreness/hub trước khi code — audit mới đọc phần đầu; xác nhận danh sách bước global hiện tại (dự kiến: resolve edges → insert → caller_count → coreness k-core → `update_is_hub_flags` → import_edges rebuild `pipeline.rs:1363`).

**Thiết kế** — hàm mới `incremental_graph_update(tx, changed_paths, old_names, hub_config, maps)`:
1. **Thu delta tên symbol:** `reindex_paths` (Phase A) capture `old_names` = `SELECT name FROM symbols WHERE path = ?` TRƯỚC `remove_file_rows`; sau persist lấy `new_names`; `names_delta = old ∪ new`.
2. **Xoá edge lỗi thời, có chủ đích (thay cho DELETE toàn bộ):**
   - `DELETE FROM call_edges WHERE from_path IN changed` (out-edges rebuild từ call_sites của chính file đó — đã có index `idx_call_edges_fpath`).
   - `DELETE FROM call_edges WHERE to_symbol NOT IN (SELECT qualified_name FROM symbols)` — quét dangling do symbol biến mất/rename (bounded: chỉ chạy khi `names_delta` non-empty; tối ưu thêm bằng `AND to_path IN changed` cho case thường).
3. **Re-resolve có phạm vi:** resolve lại `call_sites WHERE from_path IN changed` (toàn bộ site của file đổi) **CỘNG** `call_sites WHERE callee_name IN names_delta` (site ở file KHÁC trước đây unresolved/resolved-khác nay có thể đổi đích do tên mới xuất hiện/biến mất). Bounded theo độ trùng tên, không theo kích thước repo.
4. **Candidate maps:** resolution cần `by_name` toàn cục — Phase B chấp nhận 1 lần `SELECT name, qualified_name, path, class_context, signature, language FROM symbols` (đọc, không resolve; rẻ hơn resolve-all-sites nhiều lần). **Phase B+ (backlog):** daemon giữ `by_name` cache in-memory trong `CalmServer`, invalidate theo changed paths — ghi vào backlog, không làm ngay.
5. **Metrics:** `caller_count` — recompute chỉ cho `to_symbol` bị đụng (tập target của edges xoá + chèn); `coreness` (k-core) + `update_is_hub_flags` + import_edges của changed files — chạy global như cũ (in-memory trên 5K edges là trivial; đo trên repo lớn ở bước acceptance, nếu >100ms thì đưa vào backlog riêng, KHÔNG chặn Phase B).
6. **`ruled_out_by_scip` / formal edges:** edge của file KHÔNG đổi giữ nguyên row → giữ flag/tier — chính là cái thắng lớn so với hiện tại (hiện tại chết toàn bộ mỗi edit). Overlay nền sau edit (`tools/edit.rs:457-464`) GIỮ NGUYÊN ở Phase B (giờ nó chỉ còn phải vá edges của changed files).
7. **Phanh an toàn:**
   - Config `indexing.incremental_graph: bool` default `true`; `false` → `rebuild_graph` như cũ.
   - `changed_paths.len() > 50` (branch switch, mass refactor) → tự fallback full rebuild.
   - `calm index --full` (CLI) luôn là full rebuild — escape hatch vĩnh viễn.

**Tests — golden equivalence là vua:**
1. `incremental_equals_full_rebuild` — fixture đa file đa ngôn ngữ (tái dùng `tests/fixtures/multi_lang_workspace` nếu còn/khôi phục được): áp N=5 vòng mutation ngẫu nhiên nhỏ (sửa body, đổi tên fn, thêm file, xoá file) — sau mỗi vòng: chạy incremental trên DB A, full `rebuild_graph` trên DB B clone cùng cây → so **tập** `(from_symbol,to_symbol,call_site_line,edge_confidence,edge_kind)` + `caller_count` + `is_hub` per symbol: PHẢI bằng nhau tuyệt đối.
2. `unchanged_file_edges_survive_by_rowid` — edit file X; edge A→B (A,B ∉ X) giữ nguyên `rowid` (chứng minh không bị delete+reinsert).
3. `scip_flag_survives_edit_of_other_file` — set tay `ruled_out_by_scip=1` trên 1 edge; edit file khác → flag còn.
4. `rename_reroutes_cross_file_edge` — file A gọi `foo` ở file B; rename `foo`→`bar` trong B → edge cũ biến mất, call_site của A re-resolve (textual/unresolved hoặc sang đích mới nếu A cũng đổi).
5. **[audit-design flag]** `golden_equivalence_on_calm_repo_itself` — chạy thêm ít nhất 1 vòng của test #1 trên chính DB đã index của repo CALM (2932 symbol thật, nhiều helper trùng tên như `new`/`build`/`run`), không chỉ trên fixture nhỏ `multi_lang_workspace` — fixture nhỏ hiếm khi kích hoạt nhánh `MAX_CALLEE_CANDIDATES` fallback (candidate list lớn → edge bị drop), nhánh này phổ biến hơn ở repo thật. Bắt buộc trước khi đóng Phase B.

**Done when:** golden test 5-vòng xanh ổn định (chạy 3 lần liên tiếp); đo `edit_symbol` end-to-end trên CALM repo — mục tiêu **< 150ms** phần reindex+graph (điền bảng đo).
**Rollback:** flag config về `false` — code cũ còn nguyên.

### Phase C — Đưa embedding ra khỏi edit lock  ⏱ ~half day

**Evidence:** `tools/edit.rs:428-442` — `embed_pending` + `embed_pending_chunks` chạy sync trong lock; semantic-search freshness không đáng giữ response.
**Thiết kế [ĐÃ LÀM, XÁC NHẬN]:** sau `tx.commit()` (bên trong `reindex_paths`) + drop 2 guard → `std::thread::spawn` chạy embed (mở writer riêng qua `open_writer`, busy_timeout lo va chạm). `static EMBED_BG: Mutex<()>` (module-level, `tools/edit.rs`) serialize NGAY TỪ ĐẦU, không điều kiện. **Bước 0 [đã xác nhận]:** đọc `embedding.rs::embed_pending` (SELECT `WHERE id NOT IN (SELECT symbol_id FROM embedding_vecs)` — trạng thái-động, không snapshot) + `store_embedding` (`INSERT OR REPLACE` — upsert thật) → **`embed_pending*` idempotent thật sự**, không phải giả định chưa kiểm chứng. `EMBED_BG` vẫn giữ (belt-and-suspenders — tránh 2 thread cùng embed trùng lặp lãng phí compute, không phải vì cần cho correctness). Không cần re-xác nhận sau Phase B — tính idempotent này đến từ SQL query shape, không phụ thuộc tần suất gọi.
**Test:** không có sẵn hạ tầng "embedder test double với inject sleep" trong codebase (kiểm tra trực tiếp: không test nào trong `tools.rs` set `self.embedder()` thành `Some`) — xây dựng hạ tầng mock đó từ đầu vượt quá scope "~half day" của Phase C này. Thay vào đó: full regression suite xanh (672 calm-core + 183 calm-server, không ai set embedder nên nhánh `should_embed_bg` chưa từng thực thi trong test — ghi nhận rõ để không nhầm là đã test) + dogfood live: nhiều lần `edit_lines` thật qua chính code path này trong chính phiên này (embedder ready trên daemon thật) không lỗi. **Backlog:** xây `MockEmbedder`+inject-delay test double nếu Phase C's background thread gây sự cố thật sau này — không bắt buộc trước.
**Done when:** ĐÃ XONG — `embed_pending`/`embed_pending_chunks` không còn trong đường đồng bộ của `edit_lines_impl`; SCIP overlay background thread (pattern cũ) giữ nguyên không đổi. Full test suite xanh, không regression.

### Phase D — Cache resolution maps  ⏱ ~half day

**Evidence:** `pipeline.rs` — `CrateMap::build`/`Psr4Map::build`/`NamespaceMap::build` walk disk (hoặc spawn `cargo metadata`) mỗi reindex, ngay cả 3 lần gọi liên tiếp cho cùng `project_root`.
**Thiết kế [ĐÃ LÀM]:** cache theo `project_root` (KHÔNG phải single-slot — test suite dùng nhiều temp dir khác nhau trong cùng process, single-slot sẽ trả sai map cho project khác). **Bước 0 [đã xác nhận, sai giả thuyết ban đầu của plan]:** đọc trực tiếp `from_cargo_metadata`/`from_toml_scan` (CrateMap) + `from_composer_json` (Psr4Map) — input thật là `Cargo.toml`/`Cargo.lock`/`composer.json`, khớp giả thuyết. NHƯNG `NamespaceMap::build` **KHÔNG phải manifest-driven** — nó walk **toàn bộ** file `.cs` trong repo và đọc content từng file để tìm `namespace X.Y {}` — không có `*.csproj` nào được đọc cả, giả thuyết `*.csproj` của plan gốc SAI. Invalidate: `Cargo.toml`+`Cargo.lock`+`composer.json` mtime (bao phủ CrateMap/Psr4Map) HOẶC `RESOLUTION_MAPS_TTL` (60s) hết hạn — TTL là cách đúng duy nhất cho NamespaceMap (không có manifest để theo dõi), không phải thiếu sót. Cả 3 struct thêm `#[derive(Clone)]` để clone ra khỏi Mutex thay vì giữ lock suốt `rebuild_graph`. Áp dụng cho cả 3 call site (`run_indexing_pipeline_cancellable`, `reindex_changed_cancellable`, `reindex_paths`).
**Tests:** `test_cached_resolution_maps_hits_cache_then_invalidates_on_manifest_change` — xây một crate synthetic trong temp dir (không dùng `tests/fixtures/rust_workspace` dùng chung — tránh mutate state test khác đọc dưới cache global mới); gọi 2 lần liên tiếp → map giống nhau; đổi tên package trong `Cargo.toml` (sleep 1.1s tránh trung mtime-tick) → gọi lại → map phải cập nhật (không phục vụ map cũ). **XANH.**
**Done when:** ĐÃ XONG — cả 3 call site dùng `cached_resolution_maps`; full test suite xanh (673 calm-core + 183 calm-server).

---

## §3.2 — F3: Personalization boost — chuyển sang normalize + bounded  ⏱ ~half day

**Evidence:** `common.rs:349-362` (`r.score += 0.15 × boost` cộng thẳng); thang điểm thật: RRF top-1 ≈ 0.048-0.17 (`search.rs:22-31`), grep/file = 1.0 hằng (`search.rs:354,569`), bm25 symbol 1-30+ (`search.rs:177,206`), semantic 0-1. Boost đè bẹp RRF, vô dụng trên bm25 — mâu thuẫn contract trong chính doc comment `common.rs:307-309`.

**Thiết kế (chọn phương án ít xâm lấn — min-max normalize):** trong `apply_personalization_boost`:
1. Normalize score của result set về [0,1]: `s_norm = (s - min) / (max - min)` (max==min → tất cả 0.5).
2. `s_final = s_norm + weight × boost` (weight giữ config `personalization_weight` 0.15), re-sort theo `s_final`, ghi `s_final` vào `r.score`.
3. **Invariant mới (ghi vào doc comment + test):** hai kết quả có khoảng cách normalize > `weight` KHÔNG BAO GIỜ bị hoán vị bởi boost — đúng lời hứa "never overrides a strong match", giờ đúng theo toán trên MỌI kind.
4. Trade-off minh bạch: `score` trả về đổi thang (thành normalized) — field vốn opaque với agent, và `personalized: true` đã được report sẵn (`locate.rs:56+`). Ghi 1 dòng vào doc field score nếu có schema comment.
5. **Phương án thay thế đã cân nhắc, để backlog:** proximity như nguồn RRF thứ 4 (rank-fusion thuần, đẹp hơn về lý thuyết nhưng đổi scoring path của cả 7 kind — làm sau nếu min-max lộ khuyết điểm thực tế).

**Thiết kế [ĐÃ LÀM]:** logic thuần túy tách ra hàm free `normalize_then_boost(results, boosts, weight)` (không cần `&self`/DB) trong `common.rs`, `apply_personalization_boost` giờ chỉ là wrapper gọi nó sau khi query `compute_proximity_boosts` — tách để test trực tiếp không cần `CalmServer`/DB fixture. No-op guarantee mở rộng: không chỉ khi `boosts` rỗng mà cả khi không path nào trong `results` khớp `boosts` — giữ `results` NGUYÊN VẹN (không cả normalize).

**Tests [ĐÃ XONG, XANH]:**
1. `normalize_then_boost_never_flips_a_large_gap` — property test 50 vòng ngẫu nhiên (xorshift64 seed cố định, không thêm dependency `rand`), scores theo 4 thang thật (rrf 0.03-0.2/bm25 1-30/const 1.0/semantic 0-1), boosts ngẫu nhiên trong (0,1] → assert invariant với MỌI cặp.
2. `normalize_then_boost_regression_top1_survives_neighbor_boost` — đúng kịch bản audit (RRF [0.071...0.036], boost 1.0 ở rank-8) → rank-1 giữ nguyên. Fail trên code cũ (đã xác nhận trước khi refactor).
3. `normalize_then_boost_leaves_results_untouched_when_no_path_matches` — test mới cho no-op guarantee mở rộng ở trên.
4. Test cũ trong `personalization_tests` giữ xanh (test `compute_proximity_boosts`, không đụng).
5. **[Regression thật tìm thấy]** `tools.rs::locate_boosts_result_near_recently_explored_file` (integration test end-to-end qua `locate`) FAIL ngay sau khi đổi code — assert cũ hard-code đúng công thức cộng thẳng cũ (`baseline + weight*decay`). Đã sửa assert sang công thức mới (single-result set → min==max → normalize luôn = 0.5, boosted = 0.5 + weight*decay) — đúng hành vi mới, không phải che giấu lỗi.

**Done when:** ĐÃ XONG — 673 calm-core + 186 calm-server (+3 test mới, +1 test sửa) xanh. Dogfood: `search`/`locate` thật trên daemon sau khi explore vài file — xem bảng đo.

---

## §3.3 — F10: Hub tiering — gate mạnh đúng chỗ, hết friction tràn lan  ⏱ ~1 ngày

**Evidence:** `graph/hub.rs:66-68` (bridge-hub = `callers ≥ 2 && coreness ≥ p75`), default `min_callers_bridge: 2` (`config.rs:86-93`) → thực đo trên chính CALM: 185/2797 = 9.8% hub; mỗi hub-touch đòi đủ 3 lớp gate (`tools/edit.rs:264-380`) → agent học cách né edit tools của CALM = phản tác dụng an toàn.

**Thiết kế — 2 phần, 2 commit:**

**(a) Phân loại hub_kind + siết default:**
1. Schema: `ALTER TABLE symbols ADD COLUMN hub_kind TEXT` (nullable; theo pattern migration `table_info` sẵn có trong `schema.rs:331,416`). `update_is_hub_flags` ghi `'degree' | 'bridge' | 'both' | NULL`.
2. Default mới: `min_callers_bridge: 2 → 4`; giữ `coreness_pct 75` trước (đổi 1 biến 1 lần); **bước calibrate bắt buộc:** chạy lại indexing trên chính CALM, ghi hub_pct mới vào plan — mục tiêu 3-5%; chưa đạt → nâng `coreness_pct` 75→90, đo lại.
3. `fitness_report`/`repo_overview.health_summary` thêm breakdown `hub_degree_count`/`hub_bridge_count` (additive fields).

**(b) Gate theo tier** — trong `tools/edit.rs` (`compute_touch_risk` trả thêm hub_kind mạnh nhất; `symbols_overlapping_ranges` SELECT thêm cột):
- `degree`/`both` hoặc `risk == "high"` (>10 callers): giữ nguyên 3 lớp `EDIT_CONTEXT_REQUIRED` → `CONFIRM_REQUIRED` → `REASON_NOT_GROUNDED`.
- `bridge`-only (và risk ≤ medium) **VÀ mọi caller edge của symbol đó có `edge_confidence ∈ {resolved, formal}`** (không có `textual`/`ambiguous` nào — nghĩa là caller_count không bị resolver undercounting): chỉ `CONFIRM_REQUIRED` (bỏ ép edit_context-this-session + reason-grounding) — message lỗi nói rõ "bridge hub: confirm là đủ, nhưng edit_context vẫn được khuyến nghị". **[audit-design flag]** Nếu có bất kỳ caller edge nào ở confidence thấp (textual/ambiguous), giữ nguyên 3 lớp bất kể hub_kind — true blast radius có thể lớn hơn caller_count đếm được cho thấy (dynamic dispatch, reflection).
- AGENTS.md Stage 5/6 + doc `EditLinesParams::confirm` cập nhật khớp.

**Tests:** unit cho `update_is_hub_flags` phân loại đúng 3 kind; gate test: symbol bridge-only + confirm:true không cần reason → applied; symbol degree-hub thiếu edit_context → `EDIT_CONTEXT_REQUIRED` như cũ.
**Done when:** hub_pct trên CALM 3-5%; test xanh; dogfood 5 edit thật thấy friction giảm (ghi nhận xét vào plan).
**Rollback:** default config revert được; column nullable vô hại.

---

## §3.4 — F15: entry_points hết nhiễu  ⏱ ~half day

**Evidence:** output `repo_overview` phiên audit: 20 entry đầu gồm `Config`, `HubThresholdConfig::default`, `LspConfig::default`… (struct config!) — đè mất `main`/`serve` thật; sai mental model từ call đầu tiên của mọi phiên + ~600 token nhiễu.

**Thiết kế:**
1. **Bước 0 [đã xác nhận qua audit-design bằng đọc source thật — không cần re-điều tra, root cause CHÍNH XÁC hơn giả thuyết ban đầu của plan]:** KHÔNG phải `pipeline.rs:340-346` (nhánh substring-match `entry_point_patterns` đó xác nhận INERT trên CALM vì `Config::default().entry_points` là `Vec::new()` (config.rs:53) và repo không có `config.json` nào). Root cause THẬT nằm ở `detect_entry_point` (`parser.rs:844-925`), nhánh `"rust"`, điều kiện thứ 3: `decorators.iter().any(|d| rust_attr_is_dispatch_signal(d))` — đây là decorator CỦA CHÍNH node đang xét (không phải container-inherited), áp dụng cho MỌI kind (struct/enum/fn...) vì Rust arm không check `node.kind()` như Go arm. `rust_attr_is_dispatch_signal` (parser.rs:741-767) loại trừ `#[derive(...)]` (`NON_DISPATCH_ATTRS` có `"derive"`) nhưng KHÔNG loại trừ `#[serde(...)]` thuần — xác nhận trực tiếp: `IndexingPhase` (types.rs:5) có `#[serde(rename_all = "snake_case")]`, `FitnessThresholds` (fitness.rs:19) có `#[serde(default)]` — cả hai đều không phải derive nên `path=="serde"` khớp `!NON_DISPATCH_ATTRS.contains(&path)` → `is_entry_point=true` trực tiếp, LIVE mỗi lần reindex (không phải dữ liệu tồn đọng). Riêng các entry `X::default` (`HubThresholdConfig::default`, `ConservativeResolver::default`...) đến từ một nhánh KHÁC, đơn giản hơn: `TRAIT_DISPATCH_NAMES.contains(&name)` có sẵn `"default"` trong list — khớp bất kỳ hàm/method nào tên `default`, không liên quan attribute. CẢ HAI nhánh đều KHÔNG check kind. Không cần `calm index --full` để "xoá cờ tồn đọng" — cần reindex để ÁP DỤNG code fix mới (mọi reindex, không phải dọn stale data).
2. Fix chính (bắt buộc): thêm kind-gate ngay tại điểm gán `is_entry_point` trong `walk_symbols` (`parser.rs:556-563`, sau `let is_entry_point = detect_entry_point(...)`) — `is_entry_point` chỉ giữ `true` khi `kind` là function/method; struct/enum/const/static/trait/impl không bao giờ là entry point dù `detect_entry_point`/attribute nói gì. Đây là backstop đúng cho CẢ hai nhánh (attribute-own + TRAIT_DISPATCH_NAMES) mà không phải sửa logic tinh vi của `detect_entry_point` (vẫn hữu ích cho FUNCTION/METHOD, vd `#[tokio::main]`).
3. Fix phụ (khuyến nghị, không bắt buộc cho "Done when"): thêm `"serde"` vào `NON_DISPATCH_ATTRS` (`parser.rs:743-761`) để sửa từ gốc cho cả trường hợp member-level tương lai (vd 1 method mang `#[serde(skip)]` vì lý do nào đó), không chỉ dựa vào kind-gate ở bước 2. Đừng quên giữ `"main"`/`::main` vẫn dispatch signal (không xóa nhầm nhánh đó).
4. Giữ nguyên pipeline.rs:340-346's kind-gate như đã viết ở bản audit trước (defense-in-depth cho nhánh substring-pattern, hiện inert nhưng sẽ sống lại nếu user cấu hình `entry_points` không rỗng).
5. Siết pattern default (`Config::default().entry_points`) về: `main`, `__main__`, `serve`, `run`, `handler`, `cli` — hiện đang RỖNG (không phải "quá rộng" như audit gốc đoán) nên đây là THIẾT LẬP mới, không phải siết; match theo **tên symbol**, không theo substring path.
6. `repo_overview` sort entry_points: function tên `main` trước, rồi theo caller_count desc; cap 10 (đang 20).

**Tests:** fixture có `struct Config` (với `#[serde(default)]`) + `fn main` + `fn helper` → entry_points chỉ chứa `main`; thêm case riêng cho `enum X { .. }` với `#[serde(rename_all="snake_case")]` — không được flag. Chạy trên chính CALM sau reindex: list phải gồm `calm-cli main`, `serve_stdio*` — không còn `IndexingPhase`/`FitnessThresholds`/`X::default`.
**Done when:** reindex (calm tự động hoặc `calm index --full`) đã áp dụng code fix mới (không phải để "xóa cờ tồn đọng" — xem Bước 0) VÀ repo_overview trên CALM ra entry points đúng nghĩa.

---

## §3.5 — Hạng mục chiến lược cho agent-UX (mỗi cái 1 commit nhỏ, độc lập)

### (a) `repo_overview` compact mode  ⏱ ~2h
Param mới `compact: bool` default `false`: bỏ `workflow_guide` (client có AGENTS.md rồi), bỏ `entry_points`, `core_symbols` cap 8, `module_map` cap 10. Đo token: hiện ~2.4K → mục tiêu ≤ 1K. AGENTS.md Stage 1 ghi chú "phiên tiếp theo trong cùng repo: repo_overview(compact=true) đủ".

### (b) `suggested_next.gate` — phân biệt gate thật vs gợi ý mềm  ⏱ ~2h
Field additive `gate: Option<bool>` trên `SuggestedNext` (`common.rs:494-500`): `true` CHỈ cho 2 hint hook-enforce (edit_context→diff_impact tại `guardrails.rs:271-274`, edit_*→diff_impact tại `tools/edit.rs:541-544`); mọi hint khác không set. AGENTS.md "Follow suggested_next" cập nhật: `gate:true` = bắt buộc, còn lại = advisory. Giảm cognition load phân loại hint cho agent.

### (c) `scan_text.decode_scan_exhausted` — đã nằm trong Plan 2 §2.5 (chỉ nhắc để không làm trùng).

### (d) HMAC integrity cho `project_memory` (chống cấy note ngoài luồng)  ⏱ ~1 ngày · ưu tiên thấp nhất
Key 32B random tại `.calm/memory.key` (0600, tạo lazy); cột mới `content_mac` (nullable); `remember` ghi `HMAC-SHA256(topic ‖ content)`; `recall` verify → field `integrity: "ok" | "unverified"(note cũ, pre-feature) | "mismatch"`; `related_notes` drop `mismatch`. Cần dep `hmac`+`sha2` (kiểm tra `hash_content` đang dùng gì — nếu blake3 sẵn, dùng keyed-blake3 khỏi thêm dep). Làm CUỐI CÙNG — chỉ khi các mục trên xong.

### (e) Backlog ghi nhận (KHÔNG làm trong plan này)
- Proximity-as-RRF-source (§3.2 phương án 2) · by_name in-memory cache trong daemon (§3.1 B+) · read-conn pool (Plan 2 §2.3 — chỉ khi số đo đòi) · ONNX embedder backend sau feature flag (giữ potion default; interface `Embedder::embed_batch` đã đủ trừu tượng) · `max_tokens_hint` per-call response budget · catch_unwind middleware (Plan 2 §2.1c).

---

## Trình tự thực thi & tiêu chí nghiệm thu toàn Plan 3

**Thứ tự khuyến nghị:** §3.4 (nhỏ, độc lập, thắng nhanh) → §3.1 Phase A → C → D → B (B sau cùng trong F1 vì cần golden test hạ tầng) → §3.2 → §3.3 → §3.5 a,b → d.

**Nghiệm thu cuối (điền số thật):**
| Metric | Trước | Mục tiêu | Sau |
|---|---|---|---|
| `edit_lines` end-to-end reindex (CALM repo, .md file) | ~5.5s trước Phase A (full-walk 199 file: 5460/5554/5766/5500ms, daemon.log) | < 200ms | **Đạt gần mục tiêu, cải thiện ~15×** — sau `cached_formal_resolver()`: lần đầu/process (cold `OnceLock` init) ~5.3s (không tránh được, 1 lần/life-of-daemon), các lần sau đo được 0ms → 369ms qua nhiều lần gọi thật (daemon.log `tool_execution_completed`, không ước lượng) — số 369ms đáng tin hơn (0ms 1 vài lần có thể trùng no-op ngẫu nhiên). Vẫn chưa chắc chắn < 200ms mọi lần — cần đo thêm nhiều mẫu trước khi đánh dấu "đạt" tuyệt đối. Win thật sự của Phase A nằm ở đây (FormalResolver caching), không phải ở bỏ full-walk (xem note ở cột trước). 
| Phần reindex+graph trong edit | _ms | < 150ms | |
| Formal edges sống sót sau 1 edit (đếm `edge_confidence='formal'` trước/sau) | chết 100% | sống 100% trừ changed files | |
| hub_pct trên CALM | 9.8% | 3-5% | |
| `repo_overview(compact=true)` tokens (ước bằng chars/4) | ~2.4K | ≤ 1K | |
| Golden equivalence 5-vòng | n/a | 3 lần chạy liên tiếp xanh | |

**Sau khi xong toàn series:** cập nhật `docs/audit/2026-07-12-vheatm-deep-audit.md` — đánh dấu từng finding RESOLVED/DEFERRED kèm commit hash; `remember(topic="audit-2026-07-12-outcome", ...)` tóm tắt để phiên sau `recall` được; hẹn re-audit theo Attestation (sau Plan 3 hoặc 6 tháng).

---

## Risk Assessment (audit-design)
<!-- audit-design: DO NOT DUPLICATE — update this section, do not append a second one -->
<!-- last-run: 2026-07-12 | trigger: NORMAL -->

**Tier:** 2 (Production — MCP server nhiều agent dựa vào để gate edit an toàn; không PII/payments/multi-tenant nên không phải Tier 3) | **Date:** 2026-07-12

**Phương pháp:** mọi evidence anchor cốt lõi (F1/F3/F10/F15) đã được đọc lại trực tiếp từ source thật qua `mcp__calm__source`/`symbol_info`/`search` (không suy ra từ doc/comment) trước khi chạy VHEATM FAST pre-mortem. Kết quả xác minh: `search.rs:354,569` (score=1.0 file/grep), `compute_touch_risk`/gate wiring (`tools/edit.rs:266,546,607`), và `HubThresholdConfig::default` (config.rs:87-94: top_pct=5.0, min_callers=5, min_callers_bridge=2, coreness_pct=75.0) khớp chính xác. Line number của `rebuild_graph`/`reindex_changed_cancellable` đã trôi nhẹ so với plan gốc (nay 557-899 và 1464-1598, không đổi logic) — cập nhật lại khi code Phase A/B, đừng copy số dòng cũ. **Phát hiện quan trọng nằm ngoài phạm vi plan gốc:** `Config::default().entry_points` là `Vec::new()` (config.rs:53) và repo này không có `config.json`/`.calm/config.json` nào — nghĩa là nhiễu `entry_points` quan sát được trong `repo_overview` hiện tại **không đến từ pattern match đang chạy** (nhánh đó không thể fire với pattern rỗng) mà gần như chắc chắn là **`is_entry_point=1` tồn đọng từ một config đã bị xoá trước đây**, chưa được xoá cờ vì các symbol đó (struct/enum) chưa đổi hash nên chưa được reindex lại. Bug logic thật (thiếu filter theo `kind` tại `pipeline.rs:340-346`) vẫn xác nhận đúng và vẫn cần sửa — nhưng §3.4's "Done when" (repo_overview sạch trên chính CALM) sẽ KHÔNG đạt chỉ bằng code fix; cần `calm index --full` sau khi merge để xoá cờ tồn đọng trên toàn repo. Đã bổ sung bước này vào §3.4 execution note bên dưới.

### Failure Modes

1. **F10's bridge-tier gate loosening (§3.3b) tương tác xấu với confidence tier của resolver: một symbol coreness cao nhưng caller_count bị đếm thiếu (qua cạnh `textual`/`ambiguous`, nguồn undercounting đã biết — dynamic dispatch, reflection) mất bảo vệ `edit_context`-required dù blast radius THẬT của nó có thể lớn** → đây là plan LỚN LÊN chứ không siết safety gate ở đúng nơi cần — **HIGH** — mitigation trong plan: PARTIAL (mục tiêu hub_pct 3-5% + `min_callers_bridge` 2→4 giảm số lượng bị hạ tier, nhưng không có cơ chế nào loại trừ riêng case "coreness cao + caller_count thấp do resolver confidence yếu" khỏi việc hạ tier — khuyến nghị: gate theo tier chỉ áp dụng khi TẤT CẢ caller edges của symbol đó có confidence ∈ {resolved, formal} (không có textual/ambiguous nào); nếu có edge textual/ambiguous, giữ nguyên 3 lớp bất kể hub_kind).
2. **Phase C (embed ra khỏi lock) + Phase B (incremental graph, chạy SAU Phase C theo thứ tự khuyến nghị) thay đổi tần suất race giữa background embed thread và reindex ghi đè cùng file** → `edit_lock` chỉ giữ trong lúc reindex đồng bộ; sau khi Phase C spawn thread nền rồi trả response, một `edit_lines` THỨ HAI (cùng session dồn dập, hoặc session khác) có thể chạy trọn Phase A/B reindex — xoá + persist lại chunks của CÙNG file — trong khi thread nền của lần edit ĐẦU vẫn đang embed dựa trên rowid/state cũ → double-embed hoặc embed vào row đã bị xoá. Plan tự nhận đây là điều cần verify ("Bước 0": `embed_pending*` có idempotent không) nhưng chỉ verify 1 lần trước khi code Phase C — cần re-chạy lại test đó (hoặc ít nhất review lại) SAU KHI Phase B xong, vì Phase B tăng tần suất ghi đè cùng file so với full-rebuild cadence ban đầu — **HIGH** — mitigation trong plan: PARTIAL (Bước 0 có nhưng chỉ 1 lần, không lặp lại sau Phase B; đề xuất thêm `static EMBED_BG: Mutex<()>` ngay từ đầu thay vì chỉ "nếu không idempotent" — rẻ, loại bỏ hẳn nhóm race này thay vì dựa vào giả định).
3. **F10's calibration loop (`coreness_pct` 75→90 nếu chưa đạt 3-5%) không có stopping condition hay automated regression guard** — chỉ có "Done when" quan sát thủ công 1 lần, không giống golden equivalence test của Phase B (chạy tự động, lặp lại được trong CI) → một thay đổi code tương lai (thêm/bớt nhiều helper nhỏ) có thể làm hub_pct trôi lại ngoài 3-5% mà không ai phát hiện — **MEDIUM** — mitigation trong plan: NO (khuyến nghị thêm assertion nhẹ trong `fitness_report`: cảnh báo nếu hub_pct nằm ngoài khoảng cấu hình được, không cần block).

### Layer Signals

- **L1 Logic:** Phase B's `seen_pairs` dedup hiện construct MỚI trong 1 pass toàn cục (`rebuild_graph`) — bản incremental phải tự dựng lại đúng phạm vi dedup CHO PHẦN DELTA mà không double-count edge đã tồn tại từ trước; nhánh chưa có test rõ: symbol đổi tên trong file A trùng tên với symbol có sẵn (không đổi) trong file B cùng lúc.
- **L2 Concurrency:** xác nhận thật — `edit_lock` (in-process) chỉ bọc phần đồng bộ; Phase C's `std::thread::spawn` chạy SAU khi lock đã drop (đúng theo thiết kế Phase C) — xem Failure Mode 2.
- **L3 Data:** F10's `ALTER TABLE symbols ADD COLUMN hub_kind TEXT` nullable — additive, an toàn theo pattern `table_info` đã dùng ở schema.rs (cần re-verify line 331/416 hiện tại trước khi copy, đã trôi so với audit gốc do Plan 1/2 sửa các file lân cận). `unchanged_file_edges_survive_by_rowid` test giả định SQLite rowid ổn định qua DELETE/INSERT trong cùng transaction cho các row KHÔNG bị đụng — đúng với rowid table thường (không WITHOUT ROWID) nhưng nên xác nhận `call_edges` schema không có `INTEGER PRIMARY KEY` alias gây reassign, và test phải kiểm tra rowid của các edge KHÔNG bị xoá/insert lại, không phải toàn bảng.
- **L4 Integration:** overlay SCIP/LSP nền (~20s rust-analyzer batch) chạy độc lập, ghi `formal_source='scip'` sau `rebuild_graph`/incremental update — Phase B giữ nguyên hành vi này nhưng không có test cho trường hợp overlay đang chạy CÙNG LÚC một incremental update thứ hai xảy ra (2 edit liên tiếp nhanh trên file khác nhau, overlay của edit 1 vẫn chưa xong khi edit 2 bắt đầu).
- **L5 Security:** Failure Mode 1 (F10 gate loosening) là finding an toàn chính — xem trên. HMAC §3.5(d) dùng lazy key + nullable column, thiết kế hợp lý, không có gap mới đáng kể.
- **L6 Observability:** không có field telemetry BỀN VỮNG nào phân biệt "chạy incremental" vs "fallback full rebuild" ngoài log debug tạm thời của Phase A — nếu logic fallback (>50 file) có off-by-one hoặc watcher không truyền đúng path list, hệ thống âm thầm quay về full-rebuild mãi mãi mà không ai biết trừ khi đọc log tay. Đề xuất: thêm 1 field `reindex_mode: "incremental"|"full"` vào response debug hoặc `indexing_status`.
- **L7 Cross-cutting:** ngưỡng "changed_paths.len() > 50" — plan không nói rõ áp dụng ở CẢ edit-tool path lẫn watcher path, hay chỉ 1 trong 2 (edit-tool thực tế luôn = 1 file/call, ngưỡng 50 chỉ có ý nghĩa thật ở watcher/branch-switch) — cần xác nhận cả hai call site đều check trước khi coi Phase B "an toàn".

### Assumptions to Verify

- **ASSUMED:** `embed_pending`/`embed_pending_chunks` chọn row theo trạng thái pending trong DB (không phải snapshot list) — plan tự flag "Bước 0" nhưng chưa verify tại thời điểm audit này; PHẢI xác nhận trước khi code Phase C, và re-xác nhận (không chỉ giả định còn đúng) sau khi Phase B đổi tần suất ghi.
- **ASSUMED:** watcher hiện KHÔNG giữ path list theo event batch ("nếu watcher hiện gom event không giữ path — để nguyên watcher ở Phase A") — nếu đúng, phần thắng lớn nhất của Phase A (bỏ full-walk) CHỈ áp dụng cho edit-tool path, KHÔNG áp dụng cho watcher-driven reindex (branch switch, external editor) — cần xác nhận watcher.rs thật trước khi tuyên bố Phase A "done", vì đây là nguồn dirty-reindex lớn thứ hai sau edit path.
- **ASSUMED:** SQLite rowid ổn định qua DELETE+INSERT cho `call_edges` (không có VACUUM tự động chạy giữa 2 lần đo trong test) — hợp lý vì WAL mode không tự VACUUM, nhưng chưa verify schema.rs không có `WITHOUT ROWID`/explicit `INTEGER PRIMARY KEY` reassignment.
- **ASSUMED (từ chính audit gốc, chưa re-verify ở đây):** F10's coreness_pct 75→90 sẽ đủ để đạt 3-5% hub_pct trên CALM — không có bằng chứng số liệu, chỉ là ước lượng; nếu 90 vẫn không đủ, plan không có bước tiếp theo.

### Abductive Hypotheses

- **Abductive 1 (tương tác giữa các component đúng riêng lẻ):** Phase D's cache CrateMap/Psr4Map/NamespaceMap dùng mtime-invalidation + TTL fallback 60s. Edit trực tiếp vào MỘT manifest (`Cargo.toml`) đi qua ĐÚNG dirty-path reindex của Phase A (chỉ file đó) — nhưng Phase A không chủ động invalidate cache của Phase D, chỉ dựa vào lazy mtime-check ở lần ĐỌC tiếp theo. Nếu filesystem mtime granularity không đủ mịn (giây, không phải ms, tuỳ OS/FS) HOẶC request thứ hai tới đủ nhanh trong cùng giây, edit_symbol NGAY SAU đó trên file khác có thể resolve call edges bằng CrateMap CŨ trong tối đa 60s — sai lệch âm thầm (route sai crate), không crash, khó phát hiện. Không có test nào trong Phase A/B/D riêng lẻ bắt được lỗi liên-phase này.
- **Abductive 2 (chỉ thấy ở scale/adversarial input):** Golden equivalence test (Phase B) chạy trên `multi_lang_workspace` — fixture nhỏ, thủ công. Nhánh `MAX_CALLEE_CANDIDATES` fallback (`t.len() <= MAX_CALLEE_CANDIDATES → resolved; else → Vec::new()` — edge bị DROP hoàn toàn khi candidate list quá lớn) hiếm khi bị kích hoạt trên fixture nhỏ nhưng phổ biến hơn nhiều trên chính repo CALM (2932 symbol, nhiều helper cùng tên như `new`/`build`/`run`). Vì Phase B step 4 làm lại đúng 1 lần `SELECT` toàn cục cho `by_name` (giống full rebuild), kết quả PHẢI khớp về mặt lý thuyết — nhưng test 5-vòng trên fixture nhỏ sẽ không bao giờ exercise nhánh fan-out lớn này, nên một lỗi chỉ xuất hiện ở scale thật (vd thứ tự xử lý candidate khác nhau giữa incremental và full khi > MAX_CALLEE_CANDIDATES) có thể lọt qua CI. Khuyến nghị: chạy thêm golden-equivalence 1 vòng trên chính DB đã index của repo CALM (không chỉ fixture) trước khi đánh dấu Phase B "Done".

### Gate Result
<!-- PASS | PASS WITH FLAGS | HOLD -->
PASS WITH FLAGS — evidence anchor cốt lõi khớp code hiện tại (trừ line-number drift đã ghi chú, không đổi logic). 2 HIGH finding (Failure Mode 1: F10 gate loosening vs resolver confidence; Failure Mode 2: embed/reindex race qua Phase C+B) **có mitigation cụ thể được thêm vào plan** — không phải lý thuyết suông: (1) F10's gate-by-tier chỉ nới lỏng khi TẤT CẢ caller edges confidence ∈ {resolved, formal}; (2) `EMBED_BG: Mutex<()>` serialize ngay từ Phase C thay vì điều kiện. Plan **sẵn sàng thi công** theo thứ tự gốc (§3.4 → §3.1 A → C → D → B → §3.2 → §3.3 → §3.5), với 3 bổ sung bắt buộc trước khi đóng Phase B: (a) chạy `calm index --full` sau khi merge §3.4 để xoá `is_entry_point` tồn đọng, (b) verify watcher.rs có giữ path list theo batch hay không trước khi tuyên bố Phase A done toàn diện, (c) chạy thêm 1 vòng golden-equivalence trên DB thật của CALM (không chỉ fixture nhỏ) trước khi đóng Phase B.
