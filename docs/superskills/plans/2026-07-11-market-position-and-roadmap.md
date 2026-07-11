# CALM — Market Position & Roadmap (2026-07-11)

Nghiên cứu chiến lược: xu hướng coding agent 2026, harness/loop engineering, và khoảng trống trong chính roadmap của CALM — để xác định nên đầu tư tiếp vào đâu để dẫn đầu category "code intelligence for AI agents".

Phương pháp: 3 luồng research song song (2 external qua WebSearch, 1 internal audit qua CALM's own docs), tổng hợp bởi phiên làm việc chính. Độ tin cậy của từng claim được giữ nguyên như agent gốc báo cáo — không làm phẳng "well-supported" thành "single-source" hay ngược lại.

---

## 1. Bức tranh thị trường 2026 (external, đã verify qua WebSearch)

**Đã xác nhận đa nguồn:**
- Thị trường phân cực IDE-first (Cursor, $2B ARR, Feb 2026) vs. agent-first (Cognition/Devin, $26B valuation, Devin ARR $37M→$492M YoY, nuốt luôn Windsurf sau khi thương vụ OpenAI đổ vỡ).
- **Claude Code dẫn đầu về satisfaction, không phải usage-share**: JetBrains khảo sát 4/2026 — Claude Code 46% "most-loved" vs Cursor 19%, Copilot 9% — dù Copilot vẫn dẫn về raw adoption (29%).
- **MCP đã thành hạ tầng, không còn là canh bạc**: donated cho Linux Foundation's Agentic AI Foundation (12/2025), SDK downloads ~97M/tháng (3/2026, từ ~100K cuối 2024). Registry 9,652–17,468 server tuỳ cách đếm.
- **A2A (Agent-to-Agent protocol)** — Linux Foundation + Google, v1.0 tháng 4/2026, 150+ tổ chức. Định vị bổ sung cho MCP (MCP = agent↔tool, A2A = agent↔agent), không cạnh tranh.
- **MCP có vấn đề bảo mật thật**: Wiz Research — MCP server hiện diện trong 80% cloud environment quan sát được (đầu 2026), lỗ hổng cốt lõi là auth/authz chưa được đặc tả rõ trong spec. Vụ Postmark MCP (9/2025, một bản update bị compromise âm thầm BCC email) là ví dụ cảnh báo hay được trích dẫn nhất.

**Single-source, cần thận trọng khi trích dẫn ra ngoài** (từ một blog research, chưa verify độc lập qua GitHub API):
- CodeGraph (~47.4k sao), GitNexus (~42k), Serena (~25.2k) là 3 "breakout leader" trong sub-category "code intelligence for agents". grepai claim giảm 97% input token.
- **Phát hiện đáng chú ý nhất: CALM không xuất hiện trong khảo sát 14 tool này** — đây là vấn đề visibility/distribution, không phải vấn đề kỹ thuật (xem mục 5).

**Định hướng ngành (nguồn: Anthropic's own "2026 Agentic Coding Trends Report", đọc trực tiếp — tự nhận là dự đoán của Anthropic, không phải consensus trung lập):**
- Chuyển từ single-agent sang **multi-agent orchestrator/specialist**; agent chạy dài hơi (giờ-đến-ngày, ví dụ Claude Code hoàn thành 1 feature 12.5M dòng ở Rakuten trong 7 giờ, 99.9% accuracy).
- **Con người chỉ "fully delegate" được 0–20% task dù dùng AI trong ~60% công việc** — vì delegation hiệu quả cần "active supervision, validation, judgment" cho việc quan trọng. Đây chính là lý do tồn tại của pre-edit safety gate.
- Guardrail vẫn còn khoảng trống thật trong ngành: 9/30 agent trong 1 khảo sát AI Agent Index **không có guardrail nào được document**. OpenAI Codex giờ *bắt buộc* JSON "Plan" + persona "Reviewer" trước khi sửa file/network — tức là đối thủ lớn nhất đang tự đi tới đúng mô hình CALM đã có sẵn từ đầu (hard gate trước khi edit).

---

## 2. Harness & Loop Engineering — nguyên lý hiện tại, đối chiếu với CALM

**Kết luận quan trọng nhất cho định vị chiến lược:** [Harness-Bench](https://arxiv.org/html/2605.27922v1) đo được **swing 10–20 điểm phần trăm trên SWE-Bench-style score chỉ từ thay đổi harness, giữ nguyên model** — chứng minh bằng số rằng lớp "harness/tooling" quanh model không phải là tính năng phụ, mà là alpha thật. Đây là bằng chứng khách quan mạnh nhất để CALM dùng khi định vị: CALM không phải "tiện ích thêm", CALM tác động trực tiếp đến khả năng agent hoàn thành task đúng — nhưng **CALM hiện chưa tự đo được điều này** (xem Tier 2 bên dưới).

Đối chiếu nguyên lý đã được cộng đồng đồng thuận với thiết kế CALM hiện có:

| Nguyên lý harness/loop engineering (research 2026) | CALM đã có |
|---|---|
| Loop cần termination condition + no-progress detection rõ ràng | `session_context.possibly_stuck` (10+ tool call không tiến triển) |
| Externalized state thay vì giữ sống trong context | `remember`/`recall` sống sót qua restart, tách biệt session_context |
| Tool trả về **high-signal-only** output (Anthropic's "Writing effective tools") | `source()` đọc đúng 1 symbol thay vì cả file; noise-penalty ranking |
| Sub-agent isolation để cô lập context | Chưa có tương đương — CALM không tự orchestrate sub-agent |
| Hard gate ở tool-call boundary, không phải ở reasoning layer (Checkmarx) | `edit_context`/`diff_impact` hook-enforced — đúng chính xác mô hình này |
| Compaction khi gần giới hạn context | Không áp dụng trực tiếp — CALM ngăn context phình từ đầu (targeted read) hơn là nén sau |

CALM đang **tình cờ đã đúng hướng** với phần lớn best-practice hiện tại của ngành, mà không cần thiết kế lại — điều cần làm là **đo lường và công bố** điều này một cách có bằng chứng (đúng tinh thần "proof not promises" đã có sẵn), không phải xây thêm cơ chế mới.

---

## 3. Khoảng trống nội bộ — những gì CALM đã nghĩ tới nhưng chưa làm

Từ audit trực tiếp `docs/pattern-debt-registry.yaml`, `docs/superskills/plans/`, `docs/adr/`, `benchmarks/README.md`:

**Documentation/process debt (rẻ, nên sửa ngay):**
- `docs/adr/0004-lsp-optional-confidence-upgrade.md:3` — status vẫn ghi "Proposed (draft — chờ review, chưa implement)" dù đã shipped từ lâu, được xác nhận ngay trong "Update 2026-07-10" section của chính file đó.
- `docs/adr/0002-formal-resolver-stack-graphs.md:15` — vẫn ghi "TypeScript/JavaScript/Java: Future" dù cả 3 đã ship.
- `docs/superskills/plans/2026-07-10-25-language-expansion.md` — dừng ở "Phase B done", nhưng thực tế Phase A, toàn bộ Phase C (9 ngôn ngữ), và Phase D (D.0–D.4) đều đã xong tính đến HEAD hiện tại. Đây là *lần thứ 4* agent phát hiện kiểu lệch pha "implementation đi trước, doc không theo kịp" trong phiên hôm nay (README, provider.rs, lang_constants.rs, giờ là ADR + plan doc) — đủ để coi là một **pattern có hệ thống**, không phải sự cố đơn lẻ.

**Kỹ thuật gần xong, đáng hoàn thiện (leverage cao, effort thấp-trung bình):**
- ADR-0005 daemon/forwarder: code tự nhận "no idle-timeout yet, no version-handshake enforcement yet" (`crates/calm-server/src/daemon.rs:11-12`) — đúng 2 risk-mitigation mà chính ADR yêu cầu trước khi coi là production-ready.
- Go SCIP còn giới hạn single-module (`go.work` multi-module bị hoãn có chủ đích).
- `DEBT-006` (duy nhất còn mở trong pattern-debt registry): ý tưởng dùng `ty check` làm tier `TypeChecked` bị từ chối sau POC (chỉ báo lỗi, không xác nhận resolution dương) — nhưng để lại 2 hướng chưa quyết: (a) `has_type_error` như health signal riêng, (b) live-LSP để lấy positive-resolution data thật, "chi phí khác hẳn, cần đánh giá riêng."

**Benchmark còn thiếu — đây là khoảng trống chiến lược nhất:**
`benchmarks/README.md` liệt kê 5 track vẫn ở trạng thái **Planned, chưa xây**: B1 (AST accuracy vs regex), B5 (tốc độ incremental indexing), B7 (task-correctness regression qua refactor thật), B8 (model-tier leveling — model rẻ + calm vs model đắt không có calm), B9 (scaling curve theo repo size).

**B7 và B8 chính là loại bằng chứng mà nghiên cứu harness-engineering ở mục 2 nói là quan trọng nhất** (Harness-Bench đo tác động harness lên task success, không phải lên token cost). CALM hiện có B2 (precision/recall call-graph), B11 (so găng thật với 4 MCP server đối thủ) — đều là proof mạnh, nhưng **chưa có con số nào chứng minh CALM cải thiện tỷ lệ hoàn thành task thật**, đúng loại bằng chứng thị trường đang coi là gold-standard.

**Khoảng trắng thật sự — chưa từng được cân nhắc ở đâu trong docs (không phải "đã hoãn", mà là chưa từng nghĩ tới):**
Multi-repo indexing, IDE-native (non-MCP) integration, agent-to-agent coordination, test generation, PR/code-review automation, CI-native feature — **0 mention** trong toàn bộ `docs/`. Đây là đất trống thật, không phải nợ kỹ thuật.

---

## 4. Khuyến nghị ưu tiên — 4 tier

### Tier 0 — Vệ sinh tài liệu (làm ngay, rủi ro ~0)
Sửa status ADR-0004, ADR-0002 khớp thực tế; refresh `2026-07-10-25-language-expansion.md` để phản ánh Phase A/C/D đã xong. Cân nhắc thêm 1 dòng vào quy trình release/commit (adr-commit skill đã có sẵn) để bắt buộc đối chiếu status ADR mỗi khi 1 plan/phase đóng — vì đây đã lặp lại đủ nhiều lần trong 1 ngày để coi là lỗi quy trình, không phải lỗi người.

### Tier 1 — Hoàn thiện cái đã 80% xong (leverage cao, effort thấp-trung bình)
1. Đóng 2 gap còn lại của ADR-0005 v1: idle-timeout thật, version-handshake enforcement thật.
2. Sau đó chuyển default entry point (`scripts/mcp-launcher.sh`) từ `calm serve` sang `calm connect` — biến "an toàn khi nhiều agent chạy song song trên 1 repo" từ tính năng ẩn (chỉ dogfood nội bộ) thành **tuyên bố định vị công khai**, đúng lúc thị trường đang chuyển sang multi-agent/agent-fleet (Antigravity 2.0, A2A). Đây là cầu nối rẻ nhất giữa cái CALM đã xây và xu hướng lớn nhất của ngành.
3. Go SCIP multi-module support — gap đã biết, phạm vi rõ.

### Tier 2 — Đầu tư benchmark (leverage chiến lược cao nhất theo đúng nghiên cứu harness-engineering)
Xây **B7** (task-correctness regression qua refactor thật) và **B8** (model-tier leveling). Đây là khoản đầu tư có ROI định vị cao nhất tìm được trong toàn bộ nghiên cứu: nếu B8 cho ra con số kiểu "model rẻ + CALM ≈ model đắt không có CALM" trên một tập task cụ thể, đó là claim vừa cụ thể, vừa đúng thứ thị trường đang định giá (cost-consciousness + harness-quality-as-alpha), vừa chưa có đối thủ nào trong khảo sát 14 tool công bố con số tương đương.

### Tier 3 — Đất trống thật, đặt cược chiến lược (effort cao hơn, nhưng dùng hạ tầng đã có sẵn, không cần kiến trúc mới)
- **PR/blast-radius review**: đóng gói `diff_impact` + `fitness_report` + `hotspots` thành 1 MCP Prompt "review_pr" chấm điểm rủi ro cho cả PR, không chỉ 1 symbol. Không đối thủ nào trong khảo sát làm tốt việc này, và CALM đã có sẵn mọi nguyên liệu.
- **Test-generation hint**: dùng coreness × dead-code/coverage để gợi ý "hàm nào cần test nhất" — cũng chỉ là tổ hợp lại dữ liệu đã có, không cần thu thập gì mới.
- **Nhận diện agent đồng thời như một khái niệm hạng nhất** (không cần implement full A2A): mở rộng `session_context` để agent A biết "agent B đang sửa file X" — đi trước xu hướng fleet mà không phải cam kết cả một chuẩn giao thức mới.

### Tier 4 — Visibility (rẻ nhất về kỹ thuật, có thể là đòn bẩy lớn nhất)
Toàn bộ công sức kỹ thuật ở trên vô nghĩa về mặt thị trường nếu CALM tiếp tục vắng mặt trong chính bài khảo sát liệt kê 14 đối thủ cùng category. Hành động cụ thể, rẻ: nộp CALM vào các directory/khảo sát MCP-server tương tự; viết 1 bài kỹ thuật (không phải marketing) dựa trên chính research hôm nay về harness/loop engineering + benchmark B11 — CALM có đủ bằng chứng thật để đóng góp nội dung có giá trị vào một mảng hiện đang bị content-farm SEO chiếm phần lớn (agent nghiên cứu external ghi nhận rõ điều này). Đồng thời làm nổi bật rõ hơn khía cạnh bảo mật (local-only, redaction, prompt-injection flagging) — đúng lúc Wiz Research vừa công bố MCP là bề mặt tấn công đang tăng, đây là câu chuyện "CALM đã làm đúng từ đầu" có thể kể ngay mà không cần code thêm.

---

## Ghi chú về độ tin cậy

Phần lớn số liệu external ở mục 1 được từ WebSearch với 2 agent riêng biệt; bộ phân loại an toàn (claude-sonnet-5) không khả dụng để review 2 kết quả đó khi trả về — đã đọc và thấy nội dung hợp lý, có trích nguồn, tự gắn cờ rõ phần nào single-source/cần verify thêm (đặc biệt: số sao GitHub của CodeGraph/GitNexus/Serena chưa verify qua API, nên xử lý như ước lượng, không phải số chính thức khi trích dẫn ra ngoài). Phần internal audit (mục 3) trích trực tiếp từ file:line trong repo, đã đối chiếu qua CALM's own `search`/`file_overview` — độ tin cậy cao hơn.
