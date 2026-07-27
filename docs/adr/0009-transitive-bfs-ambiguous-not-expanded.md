# ADR-0009: BFS xuyên-cấp (`transitive_bfs`) báo cáo cạnh `ambiguous` nhưng không mở rộng qua nó; lọc `ruled_out_by_scip` trong cả `transitive_bfs` lẫn `compute_coreness`

- **Status**: Accepted & Implemented — shipped 2026-07-27 (Tier A resolver audit). Commit: `8634111` (`fix(graph): stop SCIP-disproven and ambiguous edges from corrupting traversal`).
- **Date**: 2026-07-27
- **Decision makers**: TBD (draft do Claude chuẩn bị theo yêu cầu, cần chủ dự án duyệt)
- **Related**: ADR-0008 (cùng branch), `crates/calm-server/src/tools/common.rs::transitive_bfs`, `crates/calm-core/src/graph/coreness.rs::compute_coreness`

## Context

`transitive_bfs` (dùng chung bởi `callers`/`callees` khi `transitive: true`) và `compute_coreness` đều đọc thẳng `call_edges`. Cả hai **không lọc `ruled_out_by_scip`** — dù truy vấn direct-callers/callees cùng file, cách đó ~100 dòng, luôn lọc. `transitive_bfs` còn **mở rộng traversal xuyên qua cạnh `ambiguous`** (fan-out lúc index: 1 call site sinh 1 cạnh cho mỗi ứng viên cùng tên) — mỗi hop BFS nhân frontier lên theo đúng độ rộng fan-out đó.

Đo được TRƯỚC fix, ở depth 3: kotlinpoet trả về 1053 symbol (**47% toàn bộ repo**), lwt 560, fmt 188 — một truy vấn transitive trên 1 symbol hub trả về gần như cả codebase, gắn nhãn như thể đó là chuỗi phụ thuộc transitive thật.

Riêng `compute_coreness` dựng degeneracy graph trên MỌI cạnh kể cả cạnh đã bị chứng minh sai. Coreness feed vào `is_hub`, và `is_hub` gate yêu cầu `confirm:true` khi edit — nên 1 cạnh sai có thể lặng lẽ đẩy 1 symbol thường vào ngưỡng hub (hoặc che giấu 1 hub thật). Test xác nhận: 2 cạnh bị SCIP loại đóng kín 1 tam giác ảo, đẩy coreness từ 1 lên 2.

## Decision

1. `transitive_bfs` lọc `ruled_out_by_scip = 0` ở cả 2 nhánh SQL (Callers/Callees) — khớp với truy vấn direct đã làm.
2. `transitive_bfs` vẫn ghi nhận MỌI cạnh `ambiguous` gặp phải vào kết quả trả về (người gọi vẫn cần thấy fan-out thật tại call site) — nhưng **không** đưa target của cạnh đó vào frontier kế tiếp: `expandable = edge_confidence != Ambiguous`. Lý do: confidence được coi là **không transitive** — bất cứ gì nằm sau 1 cạnh ambiguous thừa hưởng đúng độ bất định của cạnh đó, không phải một giá trị confidence mới; mở rộng xuyên qua nó sẽ lặng lẽ "tẩy trắng" độ bất định thành vẻ chắc chắn ở các hop sâu hơn.
3. `compute_coreness` cũng loại `ruled_out_by_scip` khỏi đồ thị degeneracy nó tính.

**Đây là thay đổi hành vi công khai, người dùng công cụ `callers`/`callees` (MCP) với `transitive: true` trên 1 symbol hub sẽ thấy tập kết quả nhỏ hơn hẳn** — có thể nhỏ hơn RẤT nhiều (case kotlinpoet giảm từ ~47% cả repo xuống đúng phạm vi fan-out thật, có giới hạn). Một agent trước đây coi kích thước `callers(transitive:true)` là thước đo "symbol này liên kết rộng cỡ nào" cần hiệu chỉnh lại kỳ vọng.

## Consequences

**Tích cực:** traversal transitive trên symbol hub giờ dừng ở 1 frontier có giới hạn, có ý nghĩa thật, thay vì suy biến thành "trả về gần hết repo". `is_hub`/coreness không còn bị thổi phồng bởi cạnh SCIP đã chứng minh sai.

**Tiêu cực / nợ ghi nhận:** còn 11 điểm tiêu thụ `call_edges` khác trong code chưa lọc `ruled_out_by_scip` — đã đăng ký thành PATTERN-DEBT `call-edges-missing-ruled-out-filter` (xem dưới), mỗi điểm cần 1 quyết định ngữ nghĩa riêng (thống kê tổng thật vs chỉ-cạnh-đáng-tin), nên cố ý chưa sửa hàng loạt trong commit này.

## Alternatives Considered

Gắn confidence suy giảm dần (decayed) cho cạnh phía sau 1 `ambiguous` và vẫn mở rộng xuyên qua nó — bác bỏ: `EdgeConfidence` hiện không có khái niệm "confidence suy giảm theo transitive depth", và vấn đề đo được (47% fan-out) đã được giải quyết triệt để chỉ bằng cách không mở rộng, trong khi vẫn giữ báo cáo cho cạnh ambiguous kề trực tiếp.

## Evidence

Commit `8634111` [verified 2026-07-27]: `cargo test`: calm-core 809 passed, calm-server 263 passed. Test hồi quy `reports_but_does_not_expand_ambiguous_edges` (`tools/common.rs`) khoá đúng hành vi này lại; test coreness khoá fix cho `compute_coreness`.

## Owner

TBD — theo đúng quy ước ADR hiện có của repo.

## Known Debts (PATTERN-DEBT)

`call-edges-missing-ruled-out-filter` — 11 site còn lại chưa lọc `ruled_out_by_scip`: `db/queries.rs:15,49`; `fitness.rs:282`; `tools/common.rs:1925`; `tools/edit.rs:1764`; `tools/inspect.rs:424,833,877`; `tools/recover.rs:33,407`; `tools/trace.rs:464`. **Đã tự xác minh lại độc lập** (grep toàn bộ `FROM call_edges` + đối chiếu `mcp__calm__pattern_debt_status()`) — danh sách khớp 100%, không thiếu không thừa.

Lưu ý về cơ chế tracking: registry `pattern_debt_status()` hiện chỉ có entry cho anchor `compute_coreness` (đã `resolved` — đúng, vì bug CỦA compute_coreness đã sửa), và danh sách 11 site trên chỉ tồn tại dưới dạng text trong ghi chú của entry đó, KHÔNG phải một entry `OPEN` riêng — vì `pattern_debt_register` bám theo similarity semantic của 1 symbol neo, không phù hợp để đại diện cho 11 call site rời rạc, khác hình dạng code. Không ép vào cơ chế đó (sẽ cho status sai); danh sách này nên được coi là nguồn sự thật cho tới khi mỗi site được sửa và có test riêng (xem C1 trong audit kế hoạch, ưu tiên `edit.rs:1764` trước — khả năng là bug thật về undercount confidence, không chỉ nợ kỹ thuật cosmetic).

## Next Cycle Trigger

Khi bất kỳ site nào trong 11 site trên bị chạm tới bởi 1 thay đổi không liên quan, sửa filter của site đó luôn trong cùng thay đổi thay vì để dành cho 1 đợt riêng. HOẶC khi có PR thứ 2 thêm 1 điểm tiêu thụ `call_edges` mới mà không lọc `ruled_out_by_scip` — dấu hiệu cần 1 query-builder helper bắt buộc luôn thêm điều kiện này thay vì để opt-in-by-convention.

## Cycle Retrospective

- Giả định sai lúc đầu: "truy vấn direct đã lọc `ruled_out_by_scip`, nên khả năng cao mọi nơi khác cũng vậy" — sai, 2 điểm tiêu thụ khác của CHÍNH BẢNG ĐÓ đã lặng lẽ trôi khỏi quy ước.
- Bất ngờ: quy mô fan-out (47% cả repo sau 1 truy vấn) — bug vô hình trên test nhỏ/synthetic, chỉ lộ ra khi đo trên corpus OSS thật có tên method phổ biến (kotlinpoet's fluent builder API).
- Nếu làm lại: 1 helper query-builder luôn tự thêm `AND ruled_out_by_scip = 0` cho mọi lần đọc `call_edges` — sẽ chặn hẳn lớp bug này thay vì để convention rơi rụng dần theo thời gian; ghi nhận cho lần refactor sau (vài trong 11 site trên là ứng viên tốt cho helper này).
- Nợ chủ đích: 11 site chưa lọc, mỗi cái cần quyết định riêng.
- Tín hiệu cần theo dõi: `edit.rs:1764` (`all_caller_edges_confident`) — mẫu số của tỷ lệ confidence bao gồm cả cạnh đã bị loại mà không trừ ra, nhiều khả năng làm ĐÁNH GIÁ THẤP confidence thật (không chỉ là nợ cosmetic) — cần ưu tiên sửa trước trong batch C1.
