# Dùng "calm" MCP server với nhiều agent/IDE khác nhau

`calm` không phải MCP server chỉ dành riêng cho Claude Code — `scripts/mcp-launcher.sh`
là entrypoint dùng chung cho **mọi** client MCP nói stdio (Claude Code, Cursor,
VS Code, Windsurf, JetBrains, hoặc bất kỳ tool nào có thể spawn một command).
File này giải thích launcher hoạt động ra sao và cách trỏ từng client vào nó.

## Không muốn clone cả repo? — cài thẳng binary `calm`

Phần "Launcher resolve binary theo 3 tầng" bên dưới mô tả cách self-host
**trong chính checkout** của CALM (dùng tốt nếu bạn đang dev
`calm`, hoặc project của bạn chính là repo này). Nếu bạn chỉ muốn dùng `calm`
như một MCP server bình thường cho **project khác**, không cần checkout gì
cả, có 2 cách:

### 1. Install script (không cần Node)

```bash
curl -fsSL https://raw.githubusercontent.com/Eilodon/CALM/main/scripts/install.sh | sh
```

Tải đúng prebuilt binary cho platform hiện tại (Linux x86_64/aarch64, macOS
Apple Silicon — cùng matrix 3 platform mà `release.yml` build), verify
SHA256 với `SHA256SUMS` publish kèm release, cài vào `~/.local/bin/calm`
(đổi qua biến `CI_INSTALL_DIR`). Không có tầng build-from-source — không có
source checkout để build; platform chưa hỗ trợ thì tự `git clone` +
`cargo build --release --bin calm` theo README thay vì tự động fallback.

### 2. npm (`@eilodon/calm-mcp`)

```json
{
  "mcpServers": {
    "calm": {
      "command": "npx",
      "args": ["-y", "@eilodon/calm-mcp", "serve"]
    }
  }
}
```

Package JS mỏng, tự chọn đúng binary prebuilt cho platform qua
`optionalDependencies` (không postinstall tải mạng — binary nằm sẵn trong
tarball npm). Xem [`../npm/README.md`](../npm/README.md) để biết cách
publish/kiểm tra package này.

### Sau khi cài xong bằng 1 trong 2 cách trên: `calm setup`

Từ bên trong project bạn muốn `calm` phân tích:

```bash
calm setup
```

Tự viết/merge entry `"calm"` vào `.mcp.json`, `.cursor/mcp.json`,
`.vscode/mcp.json` trong project đó — không đụng tới các entry khác đã có
sẵn — trỏ thẳng vào binary vừa cài. Đã có entry `"calm"` trỏ chỗ khác (ví dụ
bạn từng dùng launcher script) thì `calm setup` mặc định để yên, dùng
`calm setup --force` nếu thật sự muốn ghi đè. Windsurf/JetBrains vẫn phải dán
tay (xem 2 phần riêng bên dưới) vì đó là global config, không phải
project-level.

## Launcher resolve binary theo 3 tầng

`scripts/mcp-launcher.sh` luôn thử theo đúng thứ tự sau, dùng ngay binary đầu
tiên tìm thấy:

1. **Fast path** — binary đã có sẵn: `$CI_MCP_BIN` (override thủ công) →
   `~/.cache/calm-mcp/<tag>/calm` (bản đã tải-và-verify từ lần trước) →
   `target/release/calm` → `target/debug/calm` (build local đã có).
2. **Verified download** — chỉ áp dụng cho Linux x86_64/aarch64, và **chỉ khi
   `HEAD` đang đứng đúng một git tag đã release** (không bao giờ đoán mò
   version). Tải asset đúng platform từ GitHub Release của tag đó, verify
   SHA256 với `SHA256SUMS` đã publish kèm, rồi sanity-check `calm --version`
   khớp với version mong đợi — xong hết mới cache lại và exec. Bất kỳ bước
   nào thất bại (tải lỗi, sai checksum, sai version) đều rơi xuống tầng 3,
   **không bao giờ** exec một binary chưa verify xong.
3. **Build from source** — `cargo build -p calm-cli`, luôn hoạt động miễn có
   Rust toolchain. Đây là đường duy nhất cho macOS/Windows, cho checkout
   đang dev dở (không nằm đúng tag), hoặc môi trường không có mạng.

Vì sao không mặc định lấy "latest release": nếu bạn đang dev trên `main`
giữa hai lần release, tải "latest" sẽ âm thầm cài một binary **cũ hơn**
source đang có trên máy — sai lệch này rất khó nhận ra. Launcher mặc định
chỉ tải khi checkout đang đúng một tag (tag khớp source thì mới an toàn để
tin tưởng); muốn ưu tiên khởi động nhanh và chấp nhận rủi ro lệch version đó
thì set `CI_MCP_LAUNCHER_ALLOW_LATEST=1`.

Nếu SHA256 sai (nghi ngờ download hỏng hoặc bị can thiệp), launcher **không
exec** binary đó — log lỗi rõ ràng ra stderr rồi tự động build từ source
thay vì dừng hẳn, để server vẫn luôn khởi động được.

## Client đã có sẵn config trong repo

Ba file sau đều trỏ vào `scripts/mcp-launcher.sh`, khác nhau ở tên field
top-level:

| Client | File (repo-level) | Field top-level |
|---|---|---|
| Claude Code | `.mcp.json` | `mcpServers` |
| Cursor | `.cursor/mcp.json` | `mcpServers` |
| VS Code | `.vscode/mcp.json` | `servers` (khác tên, cùng shape `command`/`args`) |

Clone repo về là dùng được ngay với cả ba — không cần cấu hình thêm gì.

## Windsurf (global config, không check-in được)

Windsurf chỉ đọc config từ `~/.codeium/windsurf/mcp_config.json` (theo user,
không có project-level) — không thể checkout kèm repo được, phải dán tay.
Dán đoạn sau vào, thay `/absolute/path/to/CALM` bằng đường dẫn
thật nơi bạn clone repo này (khác với 3 config trên, path ở đây **phải là
tuyệt đối** vì không có khái niệm "project root" cho một file config toàn
cục):

```json
{
  "mcpServers": {
    "calm": {
      "command": "bash",
      "args": ["/absolute/path/to/CALM/scripts/mcp-launcher.sh"]
    }
  }
}
```

## JetBrains AI Assistant

Cấu hình qua UI settings riêng của JetBrains (không phải file check-in vào
repo) — trỏ command/args giống hệt snippet Windsurf ở trên (path tuyệt đối
tới `scripts/mcp-launcher.sh`).

## Liên quan: race điều kiện lúc cold-start trên Claude Code on the web

`docs/cloud-environment-setup.md` giải thích một vấn đề khác, riêng cho
Claude Code trên web: MCP client dial server **song song** với SessionStart
hook, không đảm bảo thứ tự — nên `.claude/hooks/session-start-build-calm.sh`
vẫn tồn tại độc lập với launcher này. Fast path (tầng 1) của launcher chỉ
kiểm tra "binary đã tồn tại chưa", không kiểm tra binary có bị stale hay
không (ví dụ đang sửa dở source của chính `calm`) — đó vẫn là vai trò riêng
của SessionStart hook đó, không bị thay thế bởi launcher này.
