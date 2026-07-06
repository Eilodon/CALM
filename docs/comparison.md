# So sánh `calm` với các code-intelligence MCP server khác

Bối cảnh (tính đến giữa 2026): "code intelligence cho AI agent" đã trở thành một nhóm sản phẩm
riêng, không còn là niche — vài công cụ đã vượt hàng chục nghìn sao GitHub. Bảng dưới đối chiếu
`calm` với các đại diện chính trong nhóm này, dựa trên tài liệu công khai của từng dự án tại thời
điểm viết bài. Số liệu (sao, thị phần...) đổi nhanh — đọc bảng này để hiểu **hình dạng khác biệt**,
không phải bảng xếp hạng cố định.

| Công cụ | Ngôn ngữ | Call-graph / blast-radius | Sửa file trực tiếp | An toàn trước khi sửa | Memory bền qua session | Điều hướng/workflow tích hợp |
|---|---|---|---|---|---|---|
| **`calm`** | 6 Tier-0 (call-graph đầy đủ) + 8 Tier-0.5 (symbol nông) | Có — `callers`/`callees`/`edit_context`/`diff_impact` | Có — `edit_lines`/`edit_symbol` | Có — hash-verified conflict guard, hard-refuse hub/high-caller nếu thiếu `confirm:true`, `diff_impact` bắt buộc trước commit | Có — `remember`/`recall`, bền qua restart | Có — `suggested_next` mọi response, 8-stage workflow (`AGENTS.md`) |
| **Serena** | 40+ (qua LSP) | Hạn chế — chủ yếu symbol reference, không chấm điểm rủi ro | Có, mức symbol | **Không** — verified trực tiếp (xem B11): `replace_symbol_body` không có field `confirm`/`force` nào trong schema, sửa thật một hub symbol không cần xác nhận | **Có** — verified trực tiếp (xem B11): `write_memory`/`read_memory`, bền qua process restart. *(Trước đây bảng này ghi "Không" — sai, đã sửa sau khi test thật thấy Serena có 6 memory tool: write/read/list/delete/rename/edit_memory.)* | Không |
| **CodeGraph** | 23, graph đầy đủ | Có, đầy đủ | **Không** — chỉ query, read-only | N/A (không sửa file) | File watcher (không phải memory diễn giải) | Không |
| **grepai** | Đa ngôn ngữ qua tree-sitter | Có — `trace_callers`/`trace_callees`/`trace_graph`, cộng semantic search (Ollama, 100% local) | **Không** — chỉ query, read-only | N/A (không sửa file) | Không | Không |
| **GitNexus** | Chủ yếu TypeScript | Có, qua 16 tool | Không công bố rõ | Không công bố rõ | Skills + hooks (khác memory tool) | Có (skills/hooks riêng, gắn với Claude Code) |
| **Sourcegraph/Cody MCP** | Đa ngôn ngữ, đa repo | Có, cross-repo (Deep Search) | Không công bố rõ | Không | Không | Không |
| **Cursor (indexing built-in)** | Đa ngôn ngữ | Không — embedding-based, không phải graph | Có (qua editor riêng của Cursor, không phải MCP) | Không | Không | Không |
| **Aider (repo-map)** | Đa ngôn ngữ | Không — PageRank trên tag-map, không phải graph có edge type | Có (qua Aider, không phải MCP tool tương tác) | Không | Không | Có, nhưng đóng trong vòng lặp riêng của Aider |

Bảng trên dựa trên tài liệu công khai (định tính) — **trừ hàng Serena/grepai/CodeGraph ở 3 cột "Sửa
file"/"An toàn"/"Memory", đã verified bằng tool call thật, không chỉ đọc docs** (xem B11). Cho số đo
được bằng tool call thật — cùng self-repo corpus, `calm` vs CodeGraph vs Semble vs grepai vs Serena —
xem [`benchmarks/b11_extended_competitor_ab/`](../benchmarks/b11_extended_competitor_ab/):
CodeGraph bỏ sót cùng 1 caller cross-crate trên cả `find_callers` (1/2) lẫn `pre_edit_blast_radius`
(1/5); Serena thực sự viết đè một hub symbol khi không có xác nhận nào (`calm` từ chối, kèm giải
thích `is_hub=true`); token ratio thô không nên đọc như bảng xếp hạng vì mỗi tool trả lời một mức
độ chi tiết khác nhau (chi tiết trong README của B11).

## Vì sao bảng này quan trọng hơn nó trông có vẻ

Một khảo sát độc lập về nhóm công cụ này (2026) kết luận thẳng hai điểm:

> "No tools [in this category] implement pre-edit safety gates or impact warnings before structural changes."
>
> "Memory integration [is] notably absent across all tools — a gap that remains."

Hai trụ cột mà `calm` đầu tư nhiều nhất — **hard-gate rủi ro trước khi sửa** và **`remember`/`recall` bền
qua session** — nằm đúng vào hai khoảng trống đó, theo khảo sát trên. Riêng điểm thứ hai, test thật
(B11) cho thấy khảo sát này không đúng với Serena cụ thể — Serena có `write_memory`/`read_memory`
bền qua process restart, verified trực tiếp. Điểm thứ nhất (risk-gate) thì khảo sát đúng: Serena
`replace_symbol_body` không có field xác nhận nào, verified cùng lúc. Phần lớn công cụ cùng nhóm vẫn
dừng ở "giúp agent *tìm* code nhanh hơn"; `calm` đi thêm một bước ở cả hai trục, nhưng "cả nhóm không
ai có memory" không còn đúng sau khi test thật ít nhất 1 tool trong nhóm.

## Khi nào nên chọn `calm`

- Agent của bạn **sửa code trực tiếp**, không chỉ tra cứu/trả lời câu hỏi — và bạn muốn có lưới an
  toàn thật (hash-verified, risk-gated) chứ không chỉ "hy vọng agent cẩn thận".
- Codebase chính nằm trong 6 ngôn ngữ Tier-0 (Python/TypeScript/JavaScript/Java/Rust/Go) — nơi
  `calm` có call-graph đầy đủ, không phải symbol nông.
- Bạn muốn agent **tự nhớ** quyết định kiến trúc/gotcha qua nhiều session, không phải giải thích lại
  từ đầu mỗi lần.
- Bạn dùng nhiều MCP client khác nhau (Claude Code, Cursor, VS Code, Windsurf, JetBrains) và muốn
  cùng một lớp an toàn/điều hướng hoạt động giống nhau ở mọi nơi, không khoá vào 1 host.
- Bạn coi trọng chạy local, không gọi ra ngoài, không phụ thuộc embedding API trả phí.

## Khi nào không nên chọn `calm` (hoặc nên cân nhắc thêm)

- Codebase chủ yếu nằm ngoài 6 ngôn ngữ Tier-0 (ví dụ Kotlin/Swift/PHP là chính) — `calm` vẫn parse
  được (Tier-0.5) nhưng không có call-graph, nên giá trị cốt lõi (blast-radius) yếu đi nhiều; CodeGraph
  (23 ngôn ngữ, graph đầy đủ) có thể phù hợp hơn cho trường hợp này.
- Bạn chỉ cần tra cứu/tìm kiếm nhanh, không cần agent tự sửa file qua MCP — các tool read-only
  thuần (CodeGraph, CodeGraphContext, claude-context) nhẹ hơn, cộng đồng lớn hơn, ít bề mặt để lo.
- Bạn làm việc ở quy mô **nhiều repo / enterprise**, cần tìm kiếm và điều hướng xuyên repo —
  Sourcegraph/Cody được xây cho đúng bài toán đó, `calm` hiện chỉ scope 1 repo tại 1 thời điểm.
- Bạn đã dùng Aider và chỉ cần context tự động chọn sẵn cho mỗi lượt chat, không cần bộ tool
  tương tác (callers/callees/edit_context...) — repo-map có sẵn của Aider có thể đã đủ.
- `calm` là dự án nhỏ, ít người bảo trì — nếu bạn cần một công cụ đã được cộng đồng lớn kiểm chứng
  lâu dài, các lựa chọn có nhiều sao/nhiều người dùng hơn (Serena, CodeGraph) có thể là lựa chọn
  an toàn hơn về mặt đó, dù đánh đổi lại là không có risk-gate cứng trước khi sửa như `calm` (Serena
  có memory bền qua session tương tự — không phải điểm đánh đổi ở đây, xem mục dưới).

## Đối chiếu gần nhất: `calm` vs Serena

Serena là công cụ gần `calm` nhất về mặt hình dạng — cả hai đều cho agent *sửa* code qua MCP, không
chỉ đọc, và cả hai đều có memory bền qua session (`write_memory`/`read_memory` ở Serena,
`remember`/`recall` ở `calm` — verified cả hai qua process restart thật, xem B11). Khác biệt chính:
Serena mạnh về độ phủ ngôn ngữ (qua LSP, 40+ ngôn ngữ) và đã là "chuẩn de facto" được cộng đồng dùng
rộng; `calm` đánh đổi độ phủ ngôn ngữ lấy độ sâu — call-graph có gắn độ tin cậy
(`resolved`/`inferred`/`formal`/`textual`) và risk-gate cứng trước khi sửa hub/high-caller symbol
(`replace_symbol_body` của Serena không có field xác nhận nào — verified: nó viết đè một hub symbol
thật không cần hỏi lại), điểm mà Serena thực sự không có, không phải chỉ "không công bố".
