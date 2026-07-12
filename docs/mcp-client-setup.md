# Dùng "calm" MCP server với nhiều agent/IDE khác nhau

`calm` không phải MCP server chỉ dành riêng cho Claude Code — `scripts/mcp-launcher.sh`
là entrypoint dùng chung cho **mọi** client MCP nói stdio (Claude Code, Cursor,
VS Code, Windsurf, JetBrains, Codex CLI, Antigravity, hoặc bất kỳ tool nào có
thể spawn một command). File này giải thích launcher hoạt động ra sao và cách
trỏ từng client vào nó.

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

### 3. Lệnh CLI add-server 1 dòng (client tự có sẵn, không cần sửa file tay)

Không cần biết trước path file config — 2 client dưới đây tự ghi config
đúng chỗ chỉ với 1 lệnh, đủ ngắn để chính agent (Claude/Codex, khi có quyền
chạy shell) tự thực thi thay người dùng nếu được yêu cầu kiểu "cài CALM của
Eilodon cho tui":

```bash
# Claude Code
claude mcp add --transport stdio calm -- npx -y @eilodon/calm-mcp serve

# Codex CLI
codex mcp add calm -- npx -y @eilodon/calm-mcp serve
```

Cursor/VS Code/Windsurf/Antigravity chưa có lệnh CLI tương đương — nhưng vì
agent của các tool này (ở chế độ agent/agentic mode) đều có quyền ghi file,
agent vẫn tự sửa được đúng file config (`.cursor/mcp.json`,
`.vscode/mcp.json`, `~/.codeium/windsurf/mcp_config.json`,
`~/.gemini/config/mcp_config.json`) khi được yêu cầu — chỉ là không có 1
lệnh built-in để gõ thẳng.

### Sau khi cài xong bằng cách 1 hoặc 2 ở trên (không áp dụng cho cách 3 — `codex`/`claude mcp add` đã tự ghi config, không cần `calm setup`): `calm setup`

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

## Chế độ daemon dùng chung (mặc định từ 2026-07-11)

Dù binary nào ở trên được chọn, launcher mặc định gọi nó qua `calm connect`
thay vì `calm serve` khi cả hai điều kiện đúng: đang chạy trên Unix (macOS/
Linux) và không có arg nào khác được truyền vào launcher. `calm connect` kết
nối (hoặc spawn nếu chưa có) 1 daemon dùng chung cho cả project — nhiều
client/session cùng mở 1 project sẽ chia sẻ chung 1 indexer/watcher/embedder
thay vì mỗi session tự chạy riêng (xem `docs/adr/0005-daemon-forwarder-
shared-process.md`). Nhận biết: `.calm/daemon.sock`/`daemon.meta`/
`daemon.log` xuất hiện trong thư mục project.

Bất kỳ arg tùy chỉnh nào (ví dụ client config tự thêm `--preset`) đều làm
launcher quay về `calm serve` như trước đây, không thay đổi. Muốn tắt hẳn
chế độ daemon (ví dụ môi trường không muốn chia sẻ process giữa các
session) thì set `CI_MCP_LAUNCHER_NO_DAEMON=1`.

## Client đã có sẵn config trong repo

Ba file sau đều trỏ vào `scripts/mcp-launcher.sh`, khác nhau ở tên field
top-level:

| Client | File (repo-level) | Field top-level |
|---|---|---|
| Claude Code | `.mcp.json` | `mcpServers` |
| Cursor | `.cursor/mcp.json` | `mcpServers` |
| VS Code | `.vscode/mcp.json` | `servers` (khác tên, cùng shape `command`/`args`) |

Clone repo về là dùng được ngay với cả ba — không cần cấu hình thêm gì.

## Windsurf / Devin Desktop (global config, không check-in được)

Windsurf đổi thương hiệu thành **Devin Desktop** (Cognition, 6/2026) — vẫn
cùng nền tảng Cascade cũ, đường dẫn config bên dưới không đổi.

Windsurf/Devin chỉ đọc config từ `~/.codeium/windsurf/mcp_config.json` (theo
user, không có project-level) — không thể checkout kèm repo được, phải dán
tay. Cách đơn giản nhất, **không cần clone CALM**, dùng npx như phần Quick
start ở README:

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

Nếu bạn đang dev trên chính repo CALM (không phải project khác), trỏ thẳng
vào `scripts/mcp-launcher.sh` thay vì npx — thay `/absolute/path/to/CALM`
bằng đường dẫn thật nơi bạn clone repo này (khác với 3 config check-in ở
trên, path ở đây **phải là tuyệt đối** vì không có khái niệm "project root"
cho một file config toàn cục):

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

Devin Desktop cũng có "MCP Marketplace" riêng ngay trong panel Cascade
(icon MCPs ở góc trên, hoặc Settings → Cascade → MCP Servers), hỗ trợ cài
1-click qua deeplink dạng
`windsurf://windsurf-mcp-registry?serverName=<tên-server>` — **CALM chưa
được liệt kê ở đó tại thời điểm viết tài liệu này**, nên deeplink kiểu đó
chưa dùng được cho CALM; dùng 1 trong 2 cách dán tay ở trên trong lúc chờ
nộp vào marketplace đó.

## JetBrains AI Assistant

Cấu hình qua UI settings riêng của JetBrains (không phải file check-in vào
repo) — trỏ command/args giống hệt snippet Windsurf ở trên (path tuyệt đối
tới `scripts/mcp-launcher.sh`).

## Codex CLI (OpenAI)

**Cách nhanh nhất — 1 lệnh, không cần sửa file tay:**

```bash
codex mcp add calm -- npx -y @eilodon/calm-mcp serve
```

Lệnh này tự ghi vào config global (`~/.codex/config.toml`). Xem lại bằng
`codex mcp list` hoặc `/mcp` trong Codex TUI.

**CORRECTION (2026-07-12):** bản trước của mục này nói Codex "giống
Windsurf/JetBrains — không có project-level, chỉ có config toàn cục" — sai,
đã kiểm chứng lại với tài liệu OpenAI hiện tại. Codex **có hỗ trợ
project-scoped config** qua `.codex/config.toml` ngay trong repo, chỉ cần
project đó được đánh dấu "trusted" (cơ chế trust cụ thể chưa được tài liệu
OpenAI mô tả chi tiết). Một số key nhạy cảm (`model_provider`,
`model_providers`, `openai_base_url`, `notify`) bị khoá, không override được
ở project-level — nhưng `mcp_servers.*` không nằm trong danh sách bị khoá,
nên vẫn khai báo được CALM ở đây thay vì chỉ global:

```toml
# .codex/config.toml (check in cùng repo, cần project được Codex "trust")
[mcp_servers.calm]
command = "npx"
args = ["-y", "@eilodon/calm-mcp", "serve"]
```

Hoặc nếu đang dev trên chính repo CALM, trỏ vào `scripts/mcp-launcher.sh`
(path tuyệt đối, cùng lý do như Windsurf) thay vì npx:

```toml
[mcp_servers.calm]
command = "bash"
args = ["/absolute/path/to/CALM/scripts/mcp-launcher.sh"]
```

Xem chi tiết: [developers.openai.com/codex/mcp](https://developers.openai.com/codex/mcp).

**Codex Cloud (bản hosted/async, khác ChatGPT web):** chưa xác nhận được có
setup-script/environment-config tương đương cho việc pre-build binary hay
không — tài liệu công khai của OpenAI không đủ chi tiết ở phần này (khác
với ChatGPT web, xác nhận là *không* đọc config Codex local, dùng cơ chế
plugin riêng). Nếu cần hỗ trợ Codex Cloud thật sự, phải thử nghiệm trực
tiếp thay vì suy đoán từ tài liệu.

## Antigravity (Google)

Cũng config toàn cục, dùng chung giữa Antigravity IDE và Antigravity CLI, tại
`~/.gemini/config/mcp_config.json` — cùng shape JSON `mcpServers` như Claude
Code/Cursor, chỉ khác chỗ đặt file (global, không phải project-level):

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

Sửa xong lưu file, Antigravity tự reload — không cần restart. Trong IDE cũng
sửa được qua "..." ở agent panel → "Manage MCP Servers" → "View raw config".
Path tới `mcp-launcher.sh` vẫn phải tuyệt đối, cùng lý do như Windsurf.

## Liên quan: race điều kiện lúc cold-start trên Claude Code on the web

`docs/cloud-environment-setup.md` giải thích một vấn đề khác, riêng cho
Claude Code trên web: MCP client dial server **song song** với SessionStart
hook, không đảm bảo thứ tự — nên `.claude/hooks/session-start-build-calm.sh`
vẫn tồn tại độc lập với launcher này. Fast path (tầng 1) của launcher chỉ
kiểm tra "binary đã tồn tại chưa", không kiểm tra binary có bị stale hay
không (ví dụ đang sửa dở source của chính `calm`) — đó vẫn là vai trò riêng
của SessionStart hook đó, không bị thay thế bởi launcher này.
