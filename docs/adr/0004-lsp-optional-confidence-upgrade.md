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

   **Cập nhật 2026-07-04**: với ngôn ngữ có SCIP indexer trưởng thành (Rust qua
   `rust-analyzer scip` — đã implement, xem "Update 2026-07-04" bên dưới), đánh giá **batch SCIP
   trước live-LSP** trong thứ tự pilot — đơn giản hơn ở mọi trục vận hành cho cùng một kết quả.
   Live-LSP (pilot Go/gopls bên dưới) vẫn là đường đi cho ngôn ngữ **không** có SCIP indexer
   trưởng thành.

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

## Update 2026-07-04: Batch SCIP — cùng 6 nguyên tắc, transport khác, đã implement cho Rust

**Đây không phải một quyết định mới.** `rust-analyzer scip` (chạy batch, sinh file `.scip` rồi
thôi, không phải một language-server process sống) là một **transport khác** cho đúng cùng một
mẫu "additive confidence upgrade" mà ADR này thiết kế cho live-LSP ở Decision, không phải một
hướng đi khác. Đã implement đầy đủ cho Rust theo
`docs/superskills/plans/2026-07-03-rust-support.md` (Phase A: nâng cấp resolver cú pháp thuần;
Phase B: SCIP overlay), đối chiếu trực tiếp với 6 nguyên tắc ở Decision:

1. **Opt-in, không bundle — cập nhật thành auto-detect, xem "Update 2026-07-04 (2)" bên dưới.**
   `rust.scip.enabled` trong `config.json` ban đầu là `bool` mặc định `false`
   (`crates/ci-core/src/config.rs::ScipConfig`) — cùng hình dạng với `lsp.go.enabled` ở Decision §1.
2. **Detect-once, fail-silent** — `ci_core::scip::runner::resolve_binary` thử override/PATH/
   `rustup which`/VS Code extension một lần; không thấy hoặc spawn lỗi → trả `None`/`Err`, graph
   cơ sở nguyên vẹn, không retry-loop.
3. **Additive-only, không bao giờ downgrade/xóa edge** — `ci_core::scip::ingest::ingest_occurrences`
   chỉ `UPDATE call_edges SET edge_confidence = 'formal'` cho edge đã tồn tại được SCIP xác
   nhận cùng call-site → cùng def-site; không bao giờ INSERT/DELETE/hạ rank — 2 test
   `upgrades_matching_edge_to_formal`/`never_downgrades_or_inserts` khóa hành vi này.
4. **Chạy sau `phase=ready`, không nằm trên đường găng** — `ci_core::scip::run_overlay` được gọi
   trong `ci-server/src/lib.rs` ngay sau khối index nền, trước `watcher::run_watch_loop`.
5. **Tái dùng rank `Formal`, không thêm biến thể `EdgeConfidence` mới** — `types.rs::EdgeConfidence`
   vẫn đúng 4 biến thể (`Formal`/`Resolved`/`Inferred`/`Textual`) sau toàn bộ Phase A+B.
6. **Mỗi ngôn ngữ là một quyết định độc lập** — module ingest được viết **SCIP-generic** (nhận
   `ScipOccurrence` thuần, không có gì Rust-specific trong logic đối chiếu), nên cùng code
   đường này nhận được output của `scip-typescript`/`scip-java`/`scip-clang` sau này mà không
   phải viết lại (xem `docs/rust-support-research.md` §"Bonus kiến trúc").

**Vì sao batch-first cho ngôn ngữ có SCIP indexer trưởng thành** (so với live-LSP ở Pilot Plan
ở trên):
- Không có subprocess sống cần giám sát/leak-check suốt phiên làm việc — chỉ một lần chạy
  batch có timeout cứng (`SCIP_TIMEOUT` = 120s) rồi thoát, không phải một phiên JSON-RPC sống
  suốt thời gian index.
- Không cần xây JSON-RPC client (`initialize`/`didOpen`/`callHierarchy` như Pilot Plan đề xuất
  cho gopls) — chỉ spawn một lần rồi parse một file protobuf.
- Cache đơn giản hơn: khóa theo (phiên bản binary, hash `Cargo.lock`) đủ để bỏ qua lần chạy
  lặp, không cần theo dõi version-drift của một server đang sống giữa chừng phiên.
- SCIP được thiết kế gốc cho batch/CI indexing (Sourcegraph), không phải cho tương tác editor
  như LSP — đúng use-case hơn cho một lần enrichment pass sau khi index xong.

**Đo thực tế** (`benchmarks/b2_call_graph_quality/`, lần đầu, self-repo, Phase A trước khi bật
overlay): precision tổng thể 0.795, recall 0.193; precision theo `edge_confidence`:
`inferred`=0.967, `resolved`=0.935, `textual`=0.514 — xác nhận trực tiếp giả định ở Decision §5
(agent nên tin `inferred`/`resolved` hơn `textual`); đây cũng chính là khoảng cách mà SCIP overlay
nhắm tới thu hẹp (edge nâng lên `formal` sau khi bật `rust.scip.enabled`).

**Kết luận cho Decision §6**: với ngôn ngữ có SCIP indexer trưởng thành, đánh giá batch SCIP
**trước** live-LSP pilot — cùng kết quả (nâng confidence có bằng chứng), ít trục vận hành hơn.
Live-LSP (Pilot Plan Go/gopls ở trên) vẫn là con đường đúng cho ngôn ngữ không có SCIP indexer
trưởng thành (ví dụ: Go hiện chưa có SCIP indexer chính thức trưởng thành tương đương).

## Update 2026-07-04 (2): batch SCIP chuyển từ strict opt-in sang auto-detect

Sau khi đo thực tế trên self-repo (bật overlay: 1.474 edge nâng lên `formal`, ~75% tổng số edge
Rust có call-site — xem `benchmarks/b2_call_graph_quality/README.md`), quyết định nới nguyên tắc
§1 (Opt-in, không bundle) **chỉ cho batch SCIP**, không áp dụng ngược lại cho live-LSP (Pilot Plan
Go/gopls ở trên vẫn giữ nguyên strict opt-in — lý do khác nhau về bản chất, xem bên dưới).

`rust.scip.enabled` đổi từ `bool` sang ba trạng thái (`Option<bool>`,
`crates/ci-core/src/config.rs::ScipConfig`):

- **Không set / `null` (mặc định)**: auto-detect — tự chạy overlay khi `rust-analyzer` có sẵn trên
  `PATH`/rustup/VS Code, im lặng bỏ qua (không log) khi không thấy — đây là trường hợp phổ biến
  nhất (checkout chưa từng cấu hình gì) nên không đáng 1 dòng log mỗi phiên.
- **`true`**: ép bật — cùng cơ chế dò tìm, nhưng log 1 lần ở mức `info` nếu không thấy binary, vì
  user đã chủ động yêu cầu nên đáng được biết vì sao thành no-op.
- **`false`**: ép tắt — không dò tìm binary luôn.

Tương thích ngược 100% với config cũ: `{"enabled": true}` / `{"enabled": false}` vẫn parse đúng
thành `Some(true)`/`Some(false)`; chỉ có trường hợp *không nhắc tới key này* là đổi hành vi (trước:
tắt hẳn; nay: auto-detect).

**Vì sao chỉ nới cho batch SCIP, không nới nguyên tắc opt-in nói chung**: 3 lý do phân biệt batch
SCIP với live-LSP (Decision §6 đã tự phân nhóm 2 thứ này khác nhau):

1. Không có subprocess sống — dò tìm binary chỉ là 1 lần `which`-style probe (`runner::resolve_binary`),
   không phải spawn một server rồi giữ sống suốt phiên như live-LSP. Rủi ro vận hành khi "tự bật" is
   thấp hơn nhiều bậc.
2. Timeout cứng có sẵn (`SCIP_TIMEOUT` = 120s, cache theo lockfile hash) — tự bật không có nghĩa là
   tự chấp nhận rủi ro treo vô thời hạn.
3. Additive-only theo §3 vẫn nguyên vẹn — tự bật chỉ có thể *thêm* bằng chứng (`formal`/
   `ruled_out_by_scip`), không bao giờ đổi hành vi graph cơ sở nếu overlay thất bại/không chạy.

Live-LSP (Go/gopls) không có cả 3 đặc điểm này (subprocess sống, giám sát version-drift, chưa đo
go/no-go) nên **vẫn giữ nguyên strict opt-in** như Decision §1 quy định — nới lỏng này không phải
tiền lệ cho live-LSP.

## Consequences

- Không ai bị buộc cài thêm gì để dùng `ci` — baseline install experience giữ nguyên 100%.
- Việc hoàn thành Stack Graphs formal cho TS/JS/Java (đã note "Future" từ ADR-0002, chưa làm) nên
  đi **trước hoặc song song**, không phụ thuộc ADR này — rẻ hơn LSP nhiều bậc, tận dụng đúng
  investment đã có, đóng phần lớn gap accuracy cho 3/6 Tier-0 mà không đổi risk profile.
  **Cập nhật 2026-07-03**: TypeScript đã xong (xem ADR-0002 Update) — rẻ và ít rủi ro đúng như dự
  đoán, builtins upstream dùng thẳng được, không cần workaround. JavaScript hoá ra khó hơn
  Python/TypeScript đáng kể — builtins rỗng như Python nhưng cơ chế wiring khác hẳn (builtins
  gắn trực tiếp vào node `@prog` per-file, không qua file `<builtins>` fallback) — cần đọc kỹ
  `.tsg` rules từ đầu trước khi viết fix, không phải port nguyên `PYTHON_BUILTINS_STUB`.

  **Cập nhật 2026-07-06**: Java đã xong (`resolver/formal.rs::load_java`,
  `build_java_builtins_graph`). Sửa lại nhận định "không có bất kỳ khái niệm builtins nào
  cả" ở trên — **sai**, hoặc ít nhất chưa đọc đủ sâu: `stack-graphs.tsg` của Java có rule
  `(program (package_declaration)? @package) @prog { if none @package { edge ROOT_NODE ->
  @prog.defs } ... }` — một file KHÔNG có `package` declaration thì toàn bộ top-level
  declarations của nó nối thẳng vào `ROOT_NODE`, và mọi file khác (có package hay không)
  luôn có `lexical_scope -> ROOT_NODE` nên plain-identifier reference nào cũng chạm được tới
  đó. Kết quả: `build_java_builtins_graph` port gần như nguyên xi `build_python_builtins_graph`
  (source stub Java không package, không cần `<builtins>`/FILE_PATH-anchored pop_symbol trick
  như Python) — verify bằng `test_resolve_file_resolves_java_builtins` (System/Object resolve
  qua formal tier). Effort thực tế: S/M đúng như dự đoán ban đầu của ADR này, không phải L
  như ghi chú 2026-07-04 ở trên từng lo ngại — ghi chú đó áp dụng đúng cho JavaScript, sai
  khi gộp chung với Java. Go vẫn chưa có formal tier — xem Pilot Plan ở trên.
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

## Update 2026-07-10: live-LSP overlay đã implement — cho Rust, không phải Go, và vì sao đó là lệch có chủ đích

**Trạng thái**: live-LSP resolve-time overlay đã implement (`crates/calm-core/src/lsp/`,
tool `lsp_refresh`, feature `lsp-overlay` opt-in, KHÔNG nằm trong default), pilot trên
**Rust/rust-analyzer** — lệch so với Pilot Plan Go/gopls ở trên. Ghi nhận lý do để người đọc
sau biết đây là quyết định có cân nhắc, không phải bỏ sót ADR:

1. **Tiền đề "Go chưa có SCIP indexer trưởng thành" (Update 2026-07-04) đã stale**: `scip-go`
   provider ship vào CALM ngày 2026-07-08 (P2.1, commit 6603e49), cùng đợt với 5 provider khác.
   Cả 8 ngôn ngữ chính giờ đều có batch SCIP — nhóm "cần live-LSP vì thiếu SCIP" chỉ còn
   ruby/kotlin/swift/shell/r/sql, đúng nhóm Decision §6 xếp loại "server chưa đủ trưởng thành".
   Lợi thế accuracy của live-LSP-cho-Go so với scip-go không còn rõ ràng như lúc viết Pilot Plan.
2. **Giá trị biên đo thật (self-repo, 2026-07-10)**: sau một pass SCIP tươi, Rust còn 772/~6300
   edge dưới `formal` (~12%) đủ điều kiện cho LSP thử. Vì batch SCIP và live-LSP dùng **cùng một
   engine** (rust-analyzer), phần dư này chủ yếu là case engine khó (macro, dynamic dispatch) —
   yield kỳ vọng khiêm tốn. E2E thực tế trên fixture xác nhận cả hai mặt: `attempted=2,
   upgraded=1` — và edge được nâng chính là **trait-method dispatch** (`call_dynamic →
   Runner::run` qua `dyn Runner`), lớp edge mà cả resolver cú pháp lẫn name-match không bao giờ
   chứng minh được. Vai trò thật của overlay này: **lớp bổ sung sau SCIP**, không thay thế.
3. **Vì sao vẫn ship cho Rust trước**: (a) protocol plumbing (Content-Length framing, encoding
   negotiation, id-routing, warm-up/retry) là phần tái dùng được cho MỌI ngôn ngữ sau này —
   `LspClient` không có gì Rust-specific; (b) rust-analyzer là server duy nhất đã có sẵn cơ chế
   binary-discovery trong repo (`scip/runner.rs::resolve_binary`); (c) đo được ngay trên chính
   repo này. Mở rộng sang ngôn ngữ khác = thêm binary-discovery + config block, không phải viết
   lại client.

**Dữ liệu probe sống rust-analyzer 1.96 (2026-07-10) — các hành vi wire mà spec không nói rõ,
implement nào cũng phải xử lý**:
- Chấp nhận `positionEncodings: ["utf-8"]` khi negotiate → cột = byte offset, khỏi cần UTF-16 math.
- `textDocument/definition` trả `null` (không phải error!) trong lúc index ban đầu chưa xong —
  ~5.4s trên fixture 2-crate tí hon; giữa chừng có error `-32801 content modified`. Client
  one-shot không retry sẽ upgrade 0 edge mà không có dấu hiệu gì. Bắt buộc warm-up retry.
- Server gửi server→client REQUEST (`workspace/diagnostic/refresh`) với id 0,1,... — **va chạm số
  với id space của client**. Response routing bắt buộc phải loại message có `method`, và phải
  stub-reply (null result đủ) để server không treo chờ.

**Nguyên tắc §2 (detect-once, fail-silent), §3 (additive-only), §5 (tái dùng rank `Formal` +
`formal_source='lsp'` để phân nguồn)**: giữ nguyên, thực thi đúng. §1 (strict opt-in): giữ
nguyên cho live-LSP như Update 2026-07-04 (2) quy định — feature `lsp-overlay` không nằm trong
default, policy mặc định `on_demand` (không bao giờ tự chạy on-save cho tới khi đo latency thật).

**Bug hạ tầng tìm ra trong cùng đợt audit (điều kiện tiên quyết cho mọi overlay)**: edit qua
CALM tool (`edit_lines`/`edit_symbol`) → reindex nội tuyến wipe toàn bộ `call_edges` (gồm cả
formal) nhưng không chạy lại overlay; watcher sau đó thấy hash đã cập nhật → no-op → tầng formal
chết im lặng cho tới thay đổi kế tiếp đi đúng đường watcher (quan sát sống: DB 0 formal trong khi
sidecar ghi 2863 upgrade 30 phút trước). Đã fix: edit-tool giờ fan-out overlay nền sau reindex
non-noop, có coalescing guard chống stack rust-analyzer khi edit liên tiếp. Không fix bug này thì
mọi upgrade của cả SCIP lẫn LSP đều chỉ sống tới lần edit kế tiếp.
