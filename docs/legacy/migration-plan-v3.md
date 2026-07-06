---
title: "Kế hoạch Migration CI → Rust v3 — Index-Engine-First"
date: 2026-06-30
supersedes: "migration-plan-v2.md"
status: ACTIVE
---

# Kế hoạch Migration CI → Rust v3

> v3 thay thế v2. Lý do: v2 mô tả Phase 1B–5 là "chưa làm/stub", nhưng thực trạng code đã
> đi xa hơn — phần khó còn lại được định vị sai. v3 reframe theo đúng nút thắt thực tế.

---

## Phần A — Thực trạng (đã kiểm chứng qua git + code, 2026-06-30)

### A.1 — Đã hoàn thành trong Rust (đóng băng, không làm lại)

| Lớp | Module Rust | Trạng thái |
|-----|-------------|-----------|
| Analysis | `hotspot`, `coverage`, `codeowners`, `diff_impact`, `dead_code` | ✅ Port xong, parity pass (DEBT-002 resolved) |
| Graph | `coreness` (O(V+E)), `hub` (C-2), `path` (bidirectional BFS F1/F2/F3/F10) | ✅ Port xong + C-1 regression (DEBT-004 resolved) |
| Resolver | `conservative` (6 ngôn ngữ + alias), `formal` (Stack Graphs Python) | ✅ Có (ADR-0002) |
| Search (query) | `search.rs` FTS5 dual-column BM25, RRF k=20; `sanitize.rs` 10 patterns | ✅ Có |
| Tool surface | 16 handlers rmcp `#[tool]`, `ci-cli` clap, `fitness.rs`, `telemetry.rs` | ✅ Có |
| Bug fixes | H-1, H-2, H-3, H-4 | ✅ (DEBT-003 resolved) |

### A.2 — Nút thắt thực sự: **Layer 1 — Index Engine = 0% trong Rust**

Rust hiện là **lớp truy vấn** trên một DB **không thể build bằng Rust**. Bằng chứng:

- `ci index` là stub: in ra *"Index command not yet implemented."*
- **Không có symbol extraction**: tree-sitter trong Rust chỉ được `resolver` dùng để dò
  *call edges*. Không có code extract symbol/signature/docstring/line-range →
  không có `INSERT INTO symbols` nào ngoài test (`#[cfg(test)]`) và `fitness.rs`.
- **Không có file watcher**: `notify`/`watchexec` không phải dependency. Không có
  incremental indexing 2-level, không có edge invalidation.
- **Không có embedding generation**: schema `embedding_vecs` (vec0, 768-dim) tồn tại nhưng
  không model nào sinh vector; `search_semantic` trả rỗng khi `!embeddings_ready`.

### A.3 — Vai trò Python được hiểu lại

`codeindex/` **không phải** sản phẩm song song cần parity vĩnh viễn. `indexer/__init__.py`
**rỗng (0 dòng)**. Đây là tập con ~2.5k dòng đóng vai **parity oracle**, sinh JSON golden
output qua `generate_oracle.py` + `build_synthetic_db.py`. Phần deterministic đã port xong
và đã đóng băng vào golden JSON → Python không còn là runtime dependency của hệ thống.

---

## Phần B — Quyết định chiến lược (locked 2026-06-30)

1. **Bỏ paradigm "port dòng-qua-dòng với parity sống".** Nó chỉ áp dụng cho analysis/graph
   (đã xong). Index Engine **không có bản Python đầy đủ để port** → xây **greenfield Rust**
   từ `architecture-design.md` + `CONTRACTS.md`, test bằng **property-based + spec-based**.

2. **Embeddings: HOÃN.** Semantic search vốn disabled-by-default theo design. Ship Index
   Engine + FTS5/RRF trước. Embeddings là phase cuối, optional. Khi làm: đánh giá lại
   ort(ONNX bge-base, parity thật) vs Model2Vec ở thời điểm đó — không quyết bây giờ.

3. **Python: đóng băng golden JSON, gỡ runtime NGAY.** Sinh golden JSON fixtures một lần từ
   oracle hiện tại, commit, gỡ `.py` khỏi vòng lặp dev. Parity tests so với JSON tĩnh.
   Codebase trở thành pure-Rust ngay từ đầu Phase I.

---

## Phần C — Kế hoạch 3 Phase

### Phase 0 — Đóng băng oracle + dọn nền *(làm ngay, ~0.5 ngày)*

**Mục tiêu**: cắt Python khỏi runtime, codebase pure-Rust.

- Chạy `generate_oracle.py` lần cuối → commit toàn bộ JSON output vào
  `crates/ci-core/tests/fixtures/golden/`.
- Refactor `parity_test.rs`: đọc golden JSON tĩnh, **không** invoke Python.
- Di chuyển `codeindex/`, `build_synthetic_db.py`, `generate_oracle.py` vào
  `legacy/` (hoặc xóa, giữ trong git history) — đánh dấu rõ "frozen oracle, do not run".
- Cập nhật CI: gỡ mọi step gọi Python; xác nhận `stack-graphs-corpus` gate vẫn xanh.
- ADR-0003: "Retire Python to frozen golden fixtures."

**Exit**: `cargo test --workspace` xanh không cần Python interpreter. CI xanh.

---

### Phase I — Index Engine Core *(long pole, ~10-14 ngày)*

> ~70% công sức còn lại. Đây là phần khiến hệ thống thực sự chạy được end-to-end bằng Rust.

**I.1 — Symbol extraction (tree-sitter, 6 ngôn ngữ tier-0)**
- Module mới `ci-core/src/indexer/` (parser per-language reuse grammar đã là dep).
- Dùng **tree-sitter Cursor API trực tiếp** (nguyên tắc v2 §4.2 — không xây Visitor wrapper).
- Extract: `qualified_name`, `name`, `kind`, `language`, `path`, `line_start/end`,
  `signature`, `docstring`, `name_tokens` (qua `tokenize.rs` đã có), `is_entry_point`.
- Per-language node-type constants — mirror `lang_constants.rs` của resolver.

**I.2 — Edge building + nối graph đã có**
- Import edges + call edges: tái dùng `resolver::conservative`/`formal` (đã có).
- `INSERT INTO symbols/call_edges/import_edges` + `fts_exact`/`fts_tokens` trigger sync
  (schema + trigger đã có trong `db/schema.rs`).
- Sau khi edges ready: gọi `coreness` + `update_is_hub_flags` (đã có) — chỉ cần wire.

**I.3 — Phase ladder + wiring**
- State machine `scanning → parsing → building_edges → ready` (enum đã có ở `types.rs`).
- Coverage Reader (`coverage.rs` đã có) chạy cuối `scanning`; coreness cuối `building_edges`.
- Wire vào `ci index` (one-shot) và `ci serve` (background indexer + `edges_ready` gating).
- `EmbedStatus` để `disabled` (Phase III sẽ bật).

**Testing**: property-based (proptest) — "mọi file parse được → mọi symbol có line range
hợp lệ, không overlap sai"; spec-based cho entry-point detection, phase transitions; reuse
golden fixtures cho coreness/hub trên DB do Rust-indexer build (đóng vòng end-to-end).

**Exit**: `ci index` build index thật từ source tree; `ci serve` trả kết quả đúng cho 16
tools trên index Rust-built; coreness/is_hub khớp golden. CI xanh.

---

### Phase II — Incremental + File Watcher *(~5-7 ngày)*

- Thêm `notify` crate. File watcher với debounce 500ms (design §K-core F5).
- Incremental 2-level: node-hash check + edge invalidation lan ra `imported_by`.
- Re-index chỉ file đổi → re-resolve edges liên quan → recompute coreness (global recompute
  OK ≤50k edges; debounce ở 50k-500k — theo bảng scale trong design).
- `index_freshness.stale_callers` semantics cho `edit_context` (đã định nghĩa ở design).

**Testing**: sửa 1 file → đúng symbol đổi, đúng edge invalidate; coreness recompute (mở rộng
C-1 regression sang đường incremental); watcher debounce không thrash.

**Exit**: `ci serve` cập nhật index live khi file đổi, không cần restart. CI xanh.

---

### Phase III — Embeddings *(optional, ~5-7 ngày — chỉ khi cần)*

- Quyết định model tại thời điểm này (ort/ONNX bge-base vs Model2Vec — xem Quyết định B.2).
- Sinh vector → `embedding_vecs` (vec0 KNN, schema đã có).
- Bật `search_semantic` + `search_hybrid` (RRF đã có). State machine `EmbedStatus`.
- Sanity check: Spearman rank correlation ≥ 0.85 vs baseline nếu chọn Model2Vec.

**Exit**: `search(kind="semantic"/"hybrid")` trả kết quả; embeddings opt-in qua config.

---

### Phase IV — Distribution *(~3-5 ngày, đa số khung đã có ở v2/handoff)*

- `release.yml` cross-compile musl 3 platform; `Containerfile` multi-stage; `compose.yaml`
  hardened; `ci init` (khung đã mô tả ở session-handoff). Lưu ý cross-compile tree-sitter
  grammars cho musl — fallback build native per-platform nếu fail.

**Exit**: binary 3 platform downloadable; container build; release workflow xanh trên tag.

---

## Phần D — Timeline & Rủi ro

```
Phase 0   Đóng băng oracle + pure-Rust     ~0.5 ngày   ← NGAY
Phase I   Index Engine core                ~10-14 ngày  ← long pole
Phase II  Incremental + watcher            ~5-7 ngày
Phase III Embeddings (optional)            ~5-7 ngày   (có thể bỏ/hoãn)
Phase IV  Distribution                     ~3-5 ngày
                                           ─────────
                         Tổng (no embed):  ~19-27 ngày
```

| Phase | Rủi ro | Mức | Mitigation |
|-------|--------|-----|-----------|
| I | Symbol extraction lệch tree-sitter node-types giữa ngôn ngữ | TB | Per-language constants + spec test mỗi ngôn ngữ; bắt đầu Python rồi mở rộng |
| I | Không có Python oracle cho indexer → khó verify | TB | Property-based + đóng vòng qua golden coreness/hub trên Rust-built DB |
| II | Incremental edge invalidation sai → stale graph | Cao | Mở rộng C-1 regression sang incremental; invariant test |
| III | Cross-compile ONNX runtime nếu chọn ort | TB | Ưu tiên hoãn; nếu cần, cân nhắc Model2Vec để giữ static binary |
| IV | Cross-compile tree-sitter cho musl | TB | Test CI matrix; fallback native per-platform |

---

## Phần E — Checklist (cập nhật tiến độ 2026-06-30)

- [x] Phase 0: đóng băng golden JSON, in-memory DB port, `codeindex/` → `legacy/`, ADR-0003 *(commit 0c7a89e)*
- [x] Phase I: `ci-core/src/indexer/` symbol extraction (6 ngôn ngữ, Cursor API), edge build → coreness → `is_hub`, nối `ci index`/`ci serve` *(commit 21d0459)*
- [x] Phase II: `notify` watcher debounce + incremental hash-diff (`call_sites` + `rebuild_graph`) *(commit 11d5714)*
- [x] Phase II hardening: alias-aware edges (`resolver::conservative`), live watcher integration test, debounce starvation fix *(commit 9cb40f4)*
- [x] Phase IV: `release.yml` (musl x2 + macOS ARM, checksums, GHCR publish), `Containerfile`, `compose.yaml`, `ci init`; universal `scripts/mcp-launcher.sh` (Claude Code/Cursor/VS Code + Windsurf/JetBrains docs), `LICENSE` *(2026-07-02)*
- [ ] Phase III embeddings — **HOÃN** (theo quyết định 2026-06-30)
- [ ] Còn lại: `import_edges`, phase-ladder `edges_ready` gating runtime trong `ci serve`, mở rộng resolver alias→cross-file
