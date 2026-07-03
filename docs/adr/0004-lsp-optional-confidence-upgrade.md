# ADR-0004: LSP như tầng nâng cấp confidence tùy chọn (không thay resolver mặc định)

- **Status**: Proposed (draft — chờ review, chưa implement)
- **Date**: 2026-07-03
- **Decision makers**: TBD (draft do Claude chuẩn bị theo yêu cầu, cần chủ dự án duyệt)
- **Related**: ADR-0001 (Stack Graphs Scope), ADR-0002 (Formal Resolver), `docs/comparison.md`

## Context

Mục tiêu mới: `ci` hướng tới top-tier về chất lượng/độ chính xác call-graph, hỗ trợ ~15 ngôn
ngữ phổ biến, mà không hy sinh tốc độ, độ robust và khả năng adopt rộng đang có.

Hiện trạng thực tế (đọc code, không phải theo aspiration trong ADR-0001/0002):

- Chỉ **Python** có formal resolution (`resolver/formal.rs::load_python`). TypeScript,
  JavaScript, Java, Rust, Go — 5/6 ngôn ngữ Tier-0 — hiện chạy hoàn toàn trên
  `ConservativeResolver`: name-match qua `file_symbols`/`import_map`/`alias_map`
  (`resolver/conservative.rs:61-78`), cộng thêm tier `Inferred` là receiver-type match cục bộ
  trong file (`indexer/pipeline.rs:288-304`) — không có type-checker thật, không xử lý
  generic/trait/interface đa hình.
- ADR-0002 đã cân nhắc và từ chối LSP một lần: *"Requires running external language servers;
  high latency, hard to embed."* Lý do đó vẫn đúng và được củng cố thêm bằng dữ liệu thực tế
  2026: rust-analyzer có case cold-start 30s index + 75s hang trước khi trả lời LSP request
  (rust-lang/rust-analyzer#18969); Roslyn (C#) chưa implement
  `textDocument/prepareCallHierarchy` — tức LSP-equivalent của `callers`/`callees` không phổ
  cập ngay cả ở ngôn ngữ phổ biến.
- Toàn bộ latency budget hiện tại được thiết kế quanh mili-giây/giây: `RESOLVE_TIMEOUT = 3s`
  mỗi file cho Tier-3 Stack Graphs (`formal.rs:48`, ra đời sau khi CPython's `_pydecimal.py`
  treo hàng phút), `transitive_timeout_ms` mặc định 3000ms cho `callers`/`callees`/`path`.
- `EdgeConfidence` (`types.rs:25-30`) là **contract công khai**, không phải chi tiết nội bộ:
  documented ở `docs/superskills/specs/CONTRACTS.md:174-182`, cột DB
  `call_edges.edge_confidence NOT NULL DEFAULT 'textual'` (`CONTRACTS.md:611`), và
  `caller_count_by_confidence` map đúng 4 key `formal/resolved/inferred/textual`
  (`CONTRACTS.md:379`). Thêm biến thể mới vào enum này chạm nhiều điểm tiêu thụ.
- Giá trị lõi hiện tại của `ci` mà không LSP nào tự động cho không: robust trên code đang sửa
  dở/chưa build được (tree-sitter parse cú pháp thuần, không cần build), risk-gate/hub/coreness
  trên call-graph, install zero-dependency (`scripts/mcp-launcher.sh` tự tải prebuilt binary).
  Serena — công cụ LSP-backed gần nhất — xác nhận đúng đánh đổi: dùng `multilspy` bọc real
  language server, nhưng theo `docs/comparison.md` lại "không chấm điểm rủi ro" — chưa ai kết
  hợp LSP-grade accuracy với risk-gate depth kiểu `ci`.

## Decision

**Không thay resolver mặc định bằng LSP cho bất kỳ ngôn ngữ nào, kể cả trong 6 Tier-0.**
Tree-sitter + ConservativeResolver/Stack-Graphs vẫn là nguồn sự thật **luôn chạy, luôn nhanh,
luôn robust trên code hỏng** — đây là bất biến không được phá vỡ bởi quyết định này.

LSP được thêm như một **tầng nâng cấp confidence tùy chọn, best-effort, non-blocking**, theo
đúng pattern Tier-3 Stack Graphs đã chứng minh hoạt động (timeout cứng, fallback êm về tier có
sẵn):

1. **Opt-in, không bundle vào launcher mặc định.** `scripts/mcp-launcher.sh` không cài hay yêu
   cầu bất kỳ language server nào. Bật qua `.codeindex/config.json`, ví dụ:
   ```json
   "lsp": { "go": { "enabled": true, "binary": "gopls" } }
   ```
   Không có mục nào trong config → hành vi giống hệt hôm nay.

2. **Detect-once, fail-silent.** Lúc server khởi động (không phải mỗi lần reindex), kiểm tra
   binary có trong PATH không. Thiếu, spawn lỗi, hoặc request đầu tiên lỗi → log một lần, tắt
   LSP cho cả session, không retry-loop, không chặn `ci serve`/`ci index`.

3. **Additive-only, không bao giờ downgrade hay xoá edge đã có.** LSP chỉ được phép *nâng*
   confidence của một edge đã tồn tại (từ `Resolved`/`Inferred`/`Textual` lên rank cao nhất),
   không bao giờ tạo edge mới ngoài những gì tree-sitter/ConservativeResolver đã thấy, và không
   bao giờ hạ hoặc xoá edge nếu LSP trả về khác/timeout — tránh một response flaky từ server
   ngoài làm hỏng graph đang hoạt động.

4. **Chạy sau khi `indexing_phase=ready`, không nằm trên đường găng.** Base pipeline (tree-sitter
   + Conservative/StackGraph) không chờ LSP. LSP enrichment chạy nền sau đó, tận dụng `tokio`
   (đã là dependency sẵn có — `Cargo.toml:48`, hiện dùng cho transport MCP) để quản lý subprocess
   + JSON-RPC async không cần thêm runtime mới. Thêm một tín hiệu riêng (đề xuất:
   `lsp_enrichment_phase` trong `indexing_status`) để agent biết edge nào đã được LSP xác nhận mà
   không phải đợi nó mới dùng được graph.

5. **Tái dùng rank `Formal` hiện có, không thêm biến thể `EdgeConfidence` mới.** Vì
   `EdgeConfidence` là contract công khai chạm CONTRACTS.md/mcp_types.ts/DB schema/nhiều tool
   consumer, thêm biến thể thứ 5 là thay đổi lan rộng không cần thiết cho lợi ích chưa được đo.
   Sửa duy nhất: broaden doc comment ở `CONTRACTS.md:178` từ "Stack Graphs complete path" thành
   mô tả backend-agnostic ("provable reference→definition path — Stack Graphs hoặc LSP"). Nếu
   sau pilot thấy cần phân biệt nguồn gốc (debug khi LSP sai khác Stack Graphs), thêm field
   `source` riêng ở tầng debug/telemetry, không đụng vào rank contract. **Đây là điểm mở — nếu
   người review thấy cần tier riêng ngay từ đầu, đảo quyết định này trước khi implement.**

6. **Mỗi ngôn ngữ là một quyết định độc lập, dựa trên chất lượng server thực tế** — không coi
   "bật LSP" là một cú chuyển kiến trúc toàn cục cho 15 ngôn ngữ cùng lúc. Phân nhóm sơ bộ theo
   độ trưởng thành/điều kiện ngoại vi của server (tham khảo khi mở rộng sau pilot):
   - *Ít điều kiện ngoại vi, ưu tiên trước*: Go (gopls), Python (pyright/pylsp — dù đã có Stack
     Graphs formal, có thể so sánh chéo), TypeScript/JavaScript (typescript-language-server).
   - *Phụ thuộc build system thành công — rủi ro cao với code đang sửa dở*: Java (jdtls cần
     Maven/Gradle resolve), C/C++ (clangd cần `compile_commands.json`), C# (omnisharp/Roslyn —
     và thiếu hẳn `callHierarchy`).
   - *Server chưa đủ trưởng thành hoặc giới hạn nền tảng/license*: Kotlin, Swift (sourcekit-lsp —
     yếu trên Windows), PHP (intelephense free tier giới hạn số file), Ruby.

## Pilot Plan — Go + gopls

**Vì sao Go trước**: Go hiện không có formal tier nào (chỉ Conservative) — nhiều nhất để được
lợi; gopls là static binary, không cần compile database phức tạp như clangd, không cần JVM như
jdtls — ít điều kiện ngoại vi nhất trong nhóm còn thiếu formal tier.

**Các bước**:

1. Thêm `lsp` module trong `ci-core` (feature-gated, tương tự pattern `lang-*` feature flags đã
   có cho Tier-0.5): spawn `gopls serve`, JSON-RPC stdio client tối thiểu (`initialize`,
   `textDocument/didOpen`, `textDocument/prepareCallHierarchy`,
   `callHierarchy/incomingCalls`/`outgoingCalls`).
2. Áp timeout hai lớp, nhất quán với pattern `formal.rs`: budget per-request (đề xuất 3s, khớp
   `RESOLVE_TIMEOUT` hiện tại) và budget tổng cho cả enrichment pass (đề xuất 60s — hết hạn thì
   dừng, giữ nguyên phần đã upgrade, phần chưa kịp giữ nguyên confidence cũ).
3. Với mỗi Go call edge hiện có (rank < 3): gọi call hierarchy tại call site; nếu target khớp
   callee đã resolve → nâng lên `Formal`; khác hoặc lỗi/timeout → giữ nguyên, không log noise trừ
   khi debug mode.
4. Đo trên corpus Go thật (đề xuất: chính `ci` — không có file `.go` nên cần thêm 1-2 fixture repo
   Go cỡ vừa vào `tests/fixtures`, hoặc benchmark tạm trên một repo Go ngoài đã có sẵn để test),
   dùng lại harness `benchmarks/b3_search_quality` để so precision/recall trước/sau.

**Go/no-go criteria trước khi mở rộng sang ngôn ngữ khác**:
- Tỷ lệ edge được nâng confidence phải đủ lớn để biện minh chi phí (đề xuất ngưỡng thảo luận với
  chủ dự án, không tự chốt số ở đây).
- gopls phải có sẵn hoặc cài được dễ dàng trên 3 platform CI chính hỗ trợ hiện tại; nếu tỷ lệ máy
  không có gopls cao, giá trị thực tế thấp dù accuracy tốt trên giấy.
- Overhead thời gian (wall-clock tới khi `lsp_enrichment_phase=done`) không được kéo dài trải
  nghiệm agent một cách cảm nhận được — nếu enrichment thường xuyên chạm trần 60s, cần đánh giá
  lại budget hoặc bỏ pilot.
- Không có regression nào ở đường base (Conservative/StackGraph) — pilot chỉ được cộng thêm, nếu
  có bất kỳ hồi quy nào ở graph hiện có, dừng ngay.

**Nếu pilot thất bại**: giữ nguyên `ConservativeResolver` cho Go vĩnh viễn (như ADR-0001 đã coi
đây "không phải fallback hạng hai"), đóng ADR này với Status = Rejected kèm lý do đo được, không
thử lại cho ngôn ngữ khác.

## Consequences

- Không ai bị buộc cài thêm gì để dùng `ci` — baseline install experience giữ nguyên 100%.
- Việc hoàn thành Stack Graphs formal cho TS/JS/Java (đã note "Future" từ ADR-0002, chưa làm) nên
  đi **trước hoặc song song**, không phụ thuộc ADR này — rẻ hơn LSP nhiều bậc, tận dụng đúng
  investment đã có, đóng phần lớn gap accuracy cho 3/6 Tier-0 mà không đổi risk profile.
- Thêm một trục vận hành mới (subprocess LSP theo ngôn ngữ) cần giám sát riêng: leak process nếu
  server crash giữa chừng, version drift giữa gopls trên máy user và version đã test.
- `stack-graphs` upstream đã archived (ADR-0002 Consequences) — rủi ro dài hạn cho formal tier
  hiện tại độc lập với quyết định này, nhưng đáng nhắc lại: nếu buộc phải fork `stack-graphs` một
  ngày nào đó, đó là lý do bổ sung để LSP hấp dẫn hơn cho các ngôn ngữ *chưa* có formal — không
  phải lý do để thay Python đang chạy tốt.

## Risks

- Chất lượng LSP server lệch rất nhiều theo ngôn ngữ (xem phân nhóm ở Decision §6) — "top-tier
  đồng đều trên 15 ngôn ngữ qua LSP" là kỳ vọng sai ở giai đoạn hiện tại; roadmap phải chấp nhận
  tiến độ không đều.
- `callHierarchy` không phải mọi server implement đủ (Roslyn/C# là ví dụ đã xác nhận) — với ngôn
  ngữ thiếu, pilot pattern này không áp dụng được, cần đánh giá lại riêng (có thể dùng
  `textDocument/references` thay thế, độ chính xác thấp hơn call hierarchy).
- Nếu review quyết định cần tier `EdgeConfidence` riêng cho LSP (khác quyết định §5 ở trên), toàn
  bộ ước lượng "additive, minimal-diff" trong ADR này cần tính lại chi phí.

## Alternatives Considered

- **Thay hẳn resolver mặc định bằng LSP cho 6 Tier-0**: bị bác — phá vỡ latency budget và độ
  robust trên code chưa build được, là 2 lợi thế cạnh tranh cốt lõi, để đổi lấy accuracy tăng
  không đồng đều (xem bảng trade-off trong thảo luận dẫn tới ADR này).
  Ngày phải chấp nhận: chưa.
- **Không làm gì, giữ nguyên ConservativeResolver cho mọi ngôn ngữ ngoài Python**: an toàn nhất,
  nhưng không tiến gần mục tiêu top-tier accuracy đã nêu — bị bác vì quá bảo thủ so với mục tiêu
  mới, không phải vì rủi ro kỹ thuật.
- **Bundle sẵn LSP server vào launcher (giống prebuilt binary hiện tại)**: bị bác cho pilot này —
  license/kích thước/nền tảng khác nhau quá nhiều giữa 15 server để bundle đồng loạt; có thể xét
  lại riêng từng server sau khi pilot chứng minh giá trị.
