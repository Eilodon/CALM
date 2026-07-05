# So sánh `calm` với các code-intelligence MCP server khác

Bối cảnh (tính đến giữa 2026): "code intelligence cho AI agent" đã trở thành một nhóm sản phẩm
riêng, không còn là niche — vài công cụ đã vượt hàng chục nghìn sao GitHub. Bảng dưới đối chiếu
`ci` với các đại diện chính trong nhóm này, dựa trên tài liệu công khai của từng dự án tại thời
điểm viết bài. Số liệu (sao, thị phần...) đổi nhanh — đọc bảng này để hiểu **hình dạng khác biệt**,
không phải bảng xếp hạng cố định.

| Công cụ | Ngôn ngữ | Call-graph / blast-radius | Sửa file trực tiếp | An toàn trước khi sửa | Memory bền qua session | Điều hướng/workflow tích hợp |
|---|---|---|---|---|---|---|
| **`ci`** | 6 Tier-0 (call-graph đầy đủ) + 8 Tier-0.5 (symbol nông) | Có — `callers`/`callees`/`edit_context`/`diff_impact` | Có — `edit_lines`/`edit_symbol` | Có — hash-verified conflict guard, hard-refuse hub/high-caller nếu thiếu `confirm:true`, `diff_impact` bắt buộc trước commit | Có — `remember`/`recall`, bền qua restart | Có — `suggested_next` mọi response, 8-stage workflow (`AGENTS.md`) |
| **Serena** | 40+ (qua LSP) | Hạn chế — chủ yếu symbol reference, không chấm điểm rủi ro | Có, mức symbol | Không thấy công bố risk-gate trước khi sửa | Không | Không |
| **CodeGraph** | 23, graph đầy đủ | Có, đầy đủ | **Không** — chỉ query, read-only | N/A (không sửa file) | File watcher (không phải memory diễn giải) | Không |
| **GitNexus** | Chủ yếu TypeScript | Có, qua 16 tool | Không công bố rõ | Không công bố rõ | Skills + hooks (khác memory tool) | Có (skills/hooks riêng, gắn với Claude Code) |
| **Sourcegraph/Cody MCP** | Đa ngôn ngữ, đa repo | Có, cross-repo (Deep Search) | Không công bố rõ | Không | Không | Không |
| **Cursor (indexing built-in)** | Đa ngôn ngữ | Không — embedding-based, không phải graph | Có (qua editor riêng của Cursor, không phải MCP) | Không | Không | Không |
| **Aider (repo-map)** | Đa ngôn ngữ | Không — PageRank trên tag-map, không phải graph có edge type | Có (qua Aider, không phải MCP tool tương tác) | Không | Không | Có, nhưng đóng trong vòng lặp riêng của Aider |

Bảng trên dựa trên tài liệu công khai (định tính). Cho số đo được bằng tool call thật — cùng
self-repo corpus, `ci` vs CodeGraph vs Semble — xem
[`benchmarks/b10_real_competitor_ab/`](../benchmarks/b10_real_competitor_ab/): CodeGraph bỏ sót
1/2 caller cross-crate mà `ci` bắt được đúng trên task `find_callers`; token ratio thô không nên
đọc như bảng xếp hạng vì mỗi tool trả lời một mức độ chi tiết khác nhau (chi tiết trong README của
B10).

## Vì sao bảng này quan trọng hơn nó trông có vẻ

Một khảo sát độc lập về nhóm công cụ này (2026) kết luận thẳng hai điểm:

> "No tools [in this category] implement pre-edit safety gates or impact warnings before structural changes."
>
> "Memory integration [is] notably absent across all tools — a gap that remains."

Hai trụ cột mà `ci` đầu tư nhiều nhất — **hard-gate rủi ro trước khi sửa** và **`remember`/`recall` bền
qua session** — nằm đúng vào hai khoảng trống đó. Phần lớn công cụ cùng nhóm dừng ở "giúp agent
*tìm* code nhanh hơn"; `ci` đi thêm một bước: giúp agent *biết khi nào nên dừng lại hỏi* trước khi
sửa, và *không phải tự nhớ* trạng thái điều hướng qua nhiều lượt.

## Khi nào nên chọn `ci`

- Agent của bạn **sửa code trực tiếp**, không chỉ tra cứu/trả lời câu hỏi — và bạn muốn có lưới an
  toàn thật (hash-verified, risk-gated) chứ không chỉ "hy vọng agent cẩn thận".
- Codebase chính nằm trong 6 ngôn ngữ Tier-0 (Python/TypeScript/JavaScript/Java/Rust/Go) — nơi
  `ci` có call-graph đầy đủ, không phải symbol nông.
- Bạn muốn agent **tự nhớ** quyết định kiến trúc/gotcha qua nhiều session, không phải giải thích lại
  từ đầu mỗi lần.
- Bạn dùng nhiều MCP client khác nhau (Claude Code, Cursor, VS Code, Windsurf, JetBrains) và muốn
  cùng một lớp an toàn/điều hướng hoạt động giống nhau ở mọi nơi, không khoá vào 1 host.
- Bạn coi trọng chạy local, không gọi ra ngoài, không phụ thuộc embedding API trả phí.

## Khi nào không nên chọn `ci` (hoặc nên cân nhắc thêm)

- Codebase chủ yếu nằm ngoài 6 ngôn ngữ Tier-0 (ví dụ Kotlin/Swift/PHP là chính) — `ci` vẫn parse
  được (Tier-0.5) nhưng không có call-graph, nên giá trị cốt lõi (blast-radius) yếu đi nhiều; CodeGraph
  (23 ngôn ngữ, graph đầy đủ) có thể phù hợp hơn cho trường hợp này.
- Bạn chỉ cần tra cứu/tìm kiếm nhanh, không cần agent tự sửa file qua MCP — các tool read-only
  thuần (CodeGraph, CodeGraphContext, claude-context) nhẹ hơn, cộng đồng lớn hơn, ít bề mặt để lo.
- Bạn làm việc ở quy mô **nhiều repo / enterprise**, cần tìm kiếm và điều hướng xuyên repo —
  Sourcegraph/Cody được xây cho đúng bài toán đó, `ci` hiện chỉ scope 1 repo tại 1 thời điểm.
- Bạn đã dùng Aider và chỉ cần context tự động chọn sẵn cho mỗi lượt chat, không cần bộ tool
  tương tác (callers/callees/edit_context...) — repo-map có sẵn của Aider có thể đã đủ.
- `ci` là dự án nhỏ, ít người bảo trì — nếu bạn cần một công cụ đã được cộng đồng lớn kiểm chứng
  lâu dài, các lựa chọn có nhiều sao/nhiều người dùng hơn (Serena, CodeGraph) có thể là lựa chọn
  an toàn hơn về mặt đó, dù đánh đổi lại là không có risk-gate/memory như `ci`.

## Đối chiếu gần nhất: `ci` vs Serena

Serena là công cụ gần `ci` nhất về mặt hình dạng — cả hai đều cho agent *sửa* code qua MCP, không
chỉ đọc. Khác biệt chính: Serena mạnh về độ phủ ngôn ngữ (qua LSP, 40+ ngôn ngữ) và đã là "chuẩn
de facto" được cộng đồng dùng rộng; `ci` đánh đổi độ phủ ngôn ngữ lấy độ sâu — call-graph có gắn
độ tin cậy (`resolved`/`inferred`/`formal`/`textual`), risk-gate cứng trước khi sửa hub/high-caller
symbol, và memory bền qua session, những thứ Serena hiện không tập trung vào.
