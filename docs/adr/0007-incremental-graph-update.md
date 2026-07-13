# ADR-0007: Cập nhật call-graph tăng dần theo file thay đổi (incremental), giữ nguyên full rebuild làm fallback đúng

- **Status**: Accepted & Implemented — shipped 2026-07-13 (Phase B), default `on`. Commits: `d6481e1` (plan) → `0806531` (`incremental_graph_update` + flag + wiring) → `fe873bf` (8 targeted tests) → `7df3800` (golden trên copy DB CALM thật + A-1 chunk test) → `313b623` (`graph_mode` qua `indexing_status`) → `efbd9d2` (flip default `true`).
- **Date**: 2026-07-13
- **Decision makers**: TBD (draft do Claude chuẩn bị theo yêu cầu, cần chủ dự án duyệt)
- **Related**: ADR-0002 (Formal Resolver), ADR-0004 (LSP/SCIP confidence overlay), `docs/plans/2026-07-13-phase-b-incremental-graph-update.md` (plan thi công chi tiết), `docs/audit/2026-07-12-vheatm-deep-audit.md` F1, `docs/plans/2026-07-12-upgrade-plan-3-architecture.md` §3.1 (F1)

## Context

Trước Phase B, **mỗi** edit qua công cụ CALM (và mỗi lần watcher reindex) chạy `rebuild_graph`: `DELETE FROM call_edges` **toàn bộ** rồi re-resolve **mọi** call site trong repo — kể cả khi chỉ 1 file đổi. Hai hệ quả:

1. **Chi phí O(repo) mỗi edit.** Phase A đã cắt phần re-hash/re-walk toàn repo (dirty-path `reindex_paths`) và cache `FormalResolver`, nhưng bước graph vẫn re-resolve toàn bộ.
2. **Formal edges chết mỗi edit.** Overlay SCIP/LSP nâng `edge_confidence='formal'` cho hàng nghìn cạnh (đo được 6738 trên chính CALM). `rebuild_graph` xoá sạch chúng mỗi edit; overlay nền phải dựng lại toàn bộ mỗi lần — churn lớn, và trong khoảng giữa hai lần overlay thì gate an toàn (`edit_context`/hub) đọc confidence sai thấp.

Đây là finding F1 của audit 2026-07-12 — hạng mục giá trị lớn nhất nhưng cũng correctness-critical nhất: một call edge sai **lặng lẽ** (không crash) làm sai gate an toàn mà nhiều agent dựa vào. Vì vậy Phase B bị hoãn có chủ đích cho tới khi có hạ tầng golden-equivalence đủ mạnh.

## Decision

Thêm `incremental_graph_update` (`indexer/pipeline.rs`) chạy thay `rebuild_graph` cho các pass non-noop khi bật cờ `indexing.incremental_graph` (default `true` từ T6):

- **Granularity theo `from_path`, không theo site lẻ (D1).** `delta_paths = changed ∪ deleted ∪ {from_path của call_sites có callee_name ∈ names_delta}`, với `names_delta = old_names ∪ new_names` của mỗi file đổi (union, không symmetric-diff — bắt cả đổi signature/class/namespace của tên không đổi, D2). Chỉ `DELETE FROM call_edges WHERE from_path IN delta_paths` rồi re-resolve đúng các site đó. Mỗi edge thuộc đúng 1 `from_path` nên tập cạnh được phân hoạch sạch — dedup trong phạm vi delta là đủ.
- **Một resolver dùng chung (D4).** `build_resolution_context` + `resolve_sites_to_edges` được tách khỏi `rebuild_graph` và dùng chung cho cả hai đường. Incremental và full khác nhau DUY NHẤT ở (a) tập site nạp vào, (b) phạm vi DELETE — không tồn tại bản resolver thứ hai để có thể divergence.
- **Metric pass giữ global (D3).** `refresh_caller_counts`/`resolve_import_targets`/`compute_coreness`/`update_is_hub_flags`/`update_boundary_ambiguous_flags` chạy y hệt full rebuild (chúng là hàm thuần của trạng thái DB) — equivalence-by-construction một khi tập cạnh khớp. Chi phí ~ mili-giây ở scale CALM.
- **Dangling sweep vô điều kiện (D5).** `DELETE FROM call_edges WHERE to_symbol NOT IN symbols` mỗi pass — dọn các cạnh do SCIP `insert_missing_edges` chèn (không có call_sites backing) khi symbol đích biến mất.
- **Fallback + escape hatch.** `delta_paths.len() > 50` → tự quay về full `rebuild_graph` (`FullFallback`). `calm index` (one-shot/fresh) và `config.json: {"indexing":{"incremental_graph":false}}` luôn full — công tắc rollback vĩnh viễn.
- **Quan sát được (D-L6).** `graph_mode` (`incremental`/`full`/`full_fallback:<reason>`) surfaced qua `indexing_status`; edit path log `edit_reindex_completed{reindex_ms,graph_mode}`.

Cờ chỉ được bật default sau khi golden-equivalence (`incremental == full` trên DB **không có** overlay — điều kiện ngữ nghĩa D7) xanh trên cả fixture (18 so sánh, permanent/CI) lẫn một copy của repo CALM thật.

## Consequences

**Tích cực:**
- Reindex+graph đo thật: **88ms** (delta tối thiểu) / **188ms** (edit fan-out tới file lớn nhất) vs **~337ms** full/fallback — incremental nhanh 1.8–4×. Floor bị chặn bởi 5 metric pass global (cố ý giữ global theo D3).
- Formal edges NGOÀI delta sống 100% qua edit; overlay nền chỉ phải vá phần delta (~451 cạnh) thay vì cả 6738 — giảm mạnh churn SCIP và thu hẹp cửa sổ gate đọc confidence sai.
- Trên DB đã overlay, incremental cho kết quả **tốt hơn** full rebuild (giữ enrichment full rebuild phá) — nên golden equivalence chỉ được khẳng định trên DB thuần (D7), survival của enrichment kiểm bằng test riêng.

**Tiêu cực / đánh đổi có ghi nhận:**
- Edit một file định nghĩa tên "nóng" (vd `common.rs` định nghĩa `CalmServer::new`, ~740 call site) đẩy `delta_paths` vượt 50 → `full_fallback` đúng thiết kế, không nhanh hơn full. Quan sát được qua `graph_mode`; ngưỡng 50 có thể hạ nếu cần.
- Rủi ro chính là **delta under-selection** (một input của resolution đổi mà không lọt `names_delta` → cạnh cũ sai giữ lại lặng lẽ). Kiểm soát bằng: bảng chứng minh exhaustive (plan §3.1) liệt kê từng input resolution và đường bắt nó, resolver dùng chung D4 (loại divergence logic), 8 test T5 nhắm các đường khó (rename-collision, sig-only-change, dangling sweep, fallback), và golden trên DB thật. Input resolver MỚI trong tương lai phải cập nhật bảng §3.1 (comment trỏ ngược tại `resolve_sites_to_edges`).
- Tương tác với Phase D map-cache (TTL): incremental không tự-chữa mis-resolution do map cũ như full rebuild làm ở edit kế tiếp bất kỳ. Đóng đường phổ biến nhất bằng T4b (invalidate cache khi changed set chứa manifest); đường còn lại bounded bởi TTL như hiện trạng.
- `MAX_CALLEE_CANDIDATES`(20) fan-out **không** phải rủi ro differential (D4 làm hai đường cho output y hệt) — điều chỉnh nhận thức so với plan gốc; giá trị của vòng golden trên DB thật là **scale**, không phải "kích hoạt nhánh fan-out".
