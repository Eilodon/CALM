# Spec: "1 lệnh / 1 câu chat, mọi môi trường" — cài CALM MCP ở bất kỳ client nào

**Ngày:** 2026-07-13
**Trạng thái:** Nghiên cứu + kế hoạch (chưa thực thi)
**Mục tiêu:** biến câu "cài CALM MCP cho tui đi" thành một thao tác agent tự làm được, đúng một lần, ở bất kỳ client/OS nào — ngang tầm Serena / Semgrep / Smithery.

---

## 0. Phát hiện cốt lõi (đọc cái này trước)

Hạ tầng phân phối của CALM **về kiến trúc đã gần như hoàn chỉnh và thiết kế tốt**.
Cái đang chặn mục tiêu **không phải cơ chế còn thiếu, mà là *cadence* và *go-live* —
cỗ máy đã dựng nhưng chưa được quay tay đủ:**

| Mảnh | Đã dựng? | Đang live đúng chưa? |
|---|---|---|
| npm `@eilodon/calm-mcp` (3 platform pkg) | ✅ | ⚠️ đang serve **v0.1.4**, trễ **146 commit** so với main |
| `release.yml` (tag `v*` → build 3 platform → GH release) | ✅ auto trên tag | ✅ nhưng lần cuối là v0.1.4 (2026-07-08) |
| `publish-mcp-registry.yml` (`server.json` + `mcp-publisher` + github-oidc) | ✅ | ❌ **chưa từng publish thành công** — registry search không thấy `io.github.Eilodon/calm-mcp` |
| `stage-release.sh` + `npm publish` | ✅ | ⚠️ **thủ công có chủ đích** (human sanity-check) → là điểm nghẽn cadence |
| edge release (auto trên push main) | ✅ | ⚠️ chỉ `x86_64-linux-musl`, chỉ dùng nội bộ cho launcher tier 1.5 |
| `calm setup` (ghi `.mcp.json`/`.cursor`/`.vscode`) | ✅ | ⚠️ chỉ 3 file project, dùng absolute path, không detect client, không có Codex/Windsurf/JetBrains |

**Kết luận:** không cần đại tu. Cần (a) đóng vòng cadence để npm/registry luôn tươi,
(b) bật MCP Registry, (c) một meta-installer client-aware. Còn lại là mở rộng.

---

## 1. Bài toán tách làm 3 trục độc lập

"1 câu chat, mọi môi trường" = giải đồng thời 3 thứ. Trộn lẫn 3 trục này là lý do
dễ tưởng "còn xa" trong khi thực ra mỗi trục ở một mức độ hoàn thiện khác nhau:

- **A. Discoverability** — agent "lạnh" (không có context CALM) phải *resolve* được
  "CALM" → đúng package + đúng lệnh. → MCP Registry + Smithery + tên package dễ nhớ.
- **B. Universality** — *một* lệnh phải chạy được trên nhiều client. → hoặc Smithery,
  hoặc `calm install` tự-detect client, hoặc các native add-command sẵn có.
- **C. Freshness** — thứ cài ra phải là code hiện tại, không phải bản cũ. → release
  cadence tự động (vì CALM là binary Rust, **không** làm được trick run-from-HEAD).

Trục C là điểm CALM khác biệt về bản chất với đối thủ — xem §2.4.

---

## 2. Đối thủ giải bài này thế nào (nghiên cứu 2026-07)

### 2.1 Smithery — "cài MCP bất kỳ vào client bất kỳ bằng 1 lệnh"

- `npx @smithery/cli install <server> --client <name>` — client-aware, tự ghi config
  vào đúng file của từng client, **không cần sửa JSON tay**. ~6.000 server, Node 18+
  chạy trên Win/mac/Linux. Đây chính là hiện thân của mục tiêu "trục A + B".
- Điều kiện: server phải được **list trên Smithery registry** trước.
- Bài học cho CALM: list lên Smithery là cách **mua trọn trục A+B của bên thứ ba gần
  như miễn phí** — nhưng đánh đổi bằng phụ thuộc hạ tầng ngoài (nên là P2, không phải nền móng).

### 2.2 Official MCP Registry (`registry.modelcontextprotocol.io`)

- "App store" chứa *metadata* (không chứa artifact — npm/GH mới chứa binary).
- Publish: `npm publish` trước → `mcp-publisher init` → `mcp-publisher login github` →
  `mcp-publisher publish`. Yêu cầu `package.json` có `"mcpName": "io.github.OWNER/name"`
  (marker xác thực sở hữu) + `server.json` khớp tên.
- VS Code / Cursor / Claude Code đang tích hợp search registry vào UI → **đây là lớp
  discoverability chuẩn của toàn ngành**. CALM đã có `server.json` + `mcpName` + workflow
  → chỉ còn thiếu cú bấm nút.

### 2.3 Serena — `uvx --from git+https://github.com/oraios/serena serena start-mcp-server`

- Chạy thẳng từ **HEAD của git** qua `uvx`, luôn tươi, không pin binary.
- `claude mcp add serena -- uvx --from git+… serena start-mcp-server --project $(pwd)`.
- Bài học: Serena giải trục C bằng cách **không bao giờ ship binary pinned** — Python nên
  `uvx` build/run tức thì. **CALM không copy được** (Rust compile chậm) → xem §2.4.

### 2.4 Vì sao CALM buộc phải giải Freshness bằng release cadence, không bằng run-from-source

Serena/Semgrep là Python → `uvx`/`pipx` dựng môi trường trong vài giây từ source HEAD.
CALM là **binary Rust biên dịch** (musl static, ~100MB, cargo build hàng phút). Không tồn
tại lệnh "chạy CALM từ commit mới nhất tức thì" cho end-user. Hệ quả thiết kế **bắt buộc**:

> Freshness của CALM = độ trễ giữa "merge vào main" và "npm/GH-release/registry cập nhật".
> Rút ngắn độ trễ đó = **tự động hoá release train**, không phải đổi cách chạy.

`edge` release (auto trên push main) đã chứng minh cơ chế auto-publish-binary khả thi —
chỉ cần mở rộng nó thành kênh chính thức đa nền tảng thay vì chỉ linux-x64 nội bộ.

### 2.5 Deeplink / install badge (bổ trợ, không phải "chat")

- Cursor: `cursor://anysphere.cursor-deeplink/mcp/install?name=$NAME&<base64_config>`
  (CALM **đã có** trong README).
- VS Code: `vscode:mcp/install?<urlencoded_json>` + nút badge markdown. CALM **chưa có**.
- LM Studio: nút "Add to LM Studio". 1-click từ README — không phải "1 câu chat" nhưng
  hạ ma sát tối đa cho người đọc README.

### 2.6 Native client CLI (nền tảng của "agent tự chạy")

- `claude mcp add … -- npx -y @eilodon/calm-mcp serve` ✅ (CALM đã có)
- `codex mcp add calm -- npx -y @eilodon/calm-mcp serve` ✅ (CALM đã có)
- `code --add-mcp "{\"name\":\"calm\",\"command\":\"npx\",\"args\":[\"-y\",\"@eilodon/calm-mcp\",\"serve\"]}"`
  ✅ **VS Code có sẵn — nhưng docs CALM chưa nhắc** (quick win).

---

## 3. Trạng thái CALM theo từng trục

| Trục | Mức độ | Thiếu gì |
|---|---|---|
| **A. Discoverability** | 🟡 40% | MCP Registry chưa live; chưa lên Smithery; agent cold phải "biết trước" tên package |
| **B. Universality** | 🟢 70% | Claude Code + Codex = 1 lệnh chuẩn ✅; Cursor/VS Code/Windsurf/JetBrains phải sửa file tay hoặc dùng deeplink; chưa có 1 meta-installer |
| **C. Freshness** | 🔴 25% | npm/registry đang serve v0.1.4; chưa auto npm-publish; edge chỉ 1 platform |

---

## 4. Kế hoạch (ưu tiên theo đòn bẩy)

### P0 — Đóng vòng Freshness (đòn bẩy cao nhất, unlock mọi commit đã làm)

1. **Cắt release mới ngay từ main** (`v0.2.0`). Chỉ việc này đã biến `npx @eilodon/calm-mcp`
   từ "v0.1.4 cũ 146 commit" thành "code hiện tại".
2. **Tự động hoá npm-publish trong CI.** Thêm job vào `release.yml` (chạy sau `build`, trên
   tag `v*`): chạy `npm/stage-release.sh $TAG` rồi `npm publish` cho 4 package, dùng secret
   `NPM_TOKEN`. Gỡ điểm nghẽn thủ công ở §0. (Giữ tuỳ chọn `--dry-run`/approval gate nếu vẫn
   muốn human sanity-check — biến "thủ công toàn bộ" thành "1 lần approve".)
3. **Cân nhắc `release-please`** (hoặc tag-on-merge có điều kiện) để tự bump version + tạo tag
   khi merge vào main → cadence tiến tới liên tục, không phụ thuộc trí nhớ con người.

*Touchpoints:* `.github/workflows/release.yml`, `npm/stage-release.sh`, secret `NPM_TOKEN`.

### P0/P1 — Bật MCP Registry (đóng trục Discoverability)

4. **Chạy `publish-mcp-registry.yml`** cho version mới (sau khi npm publish xong — registry
   validate rằng npm package tồn tại). Mọi thứ đã sẵn: `server.json`, `mcpName`, github-oidc.
   Sau bước này, agent cold trên VS Code/Cursor/Claude Code **resolve được "CALM"** từ registry.
5. **Chain nó vào release train** (P0.2): sau job npm-publish thành công → tự trigger registry
   publish. Xoá dòng "manual, not on-push" trong comment workflow khi npm đã auto.

*Touchpoints:* `.github/workflows/publish-mcp-registry.yml`, `server.json` (đã đúng).

### P1 — `calm install`: meta-installer client-aware (đóng trục Universality, self-owned)

6. **Mở rộng `calm setup` → `calm install`** (main.rs:655). Hiện tại chỉ ghi 3 file project với
   absolute path. Nâng cấp:
   - **Detect client đang gọi** qua env/parent-process, hoặc cờ `--client <claude|cursor|vscode|codex|windsurf|jetbrains>`.
   - **Phủ thêm** Codex (`~/.codex/config.toml`), Windsurf global (`~/.codeium/windsurf/mcp_config.json`),
     JetBrains (in hướng dẫn).
   - **Tuỳ chọn `--npx`**: ghi config dạng `npx -y @eilodon/calm-mcp serve` thay vì absolute path
     → portable + tự tươi theo npm, hợp với người không clone repo.
   - Đây là "Smithery-lite của riêng CALM" — không phụ thuộc bên thứ ba cho trục B.

*Touchpoints:* `crates/calm-cli/src/main.rs` (`Commands::Setup` → thêm `Install`, `write_mcp_config`).

### P2 — Mở rộng bề mặt (bổ trợ)

7. **List lên Smithery** → user có `npx @smithery/cli install @eilodon/calm-mcp --client X` cho
   ~15 client miễn phí. Piggyback discoverability + universality của bên thứ ba.
8. **VS Code install badge + `code --add-mcp` vào README/docs**; verify lại Cursor deeplink.

### P3 — Hoàn thiện

9. Mở rộng edge release ra đủ platform (hiện chỉ linux-x64) cho cold-start agent trên web.
10. `mcpName`/registry cho cả bản binary (không chỉ npm) nếu registry hỗ trợ nhiều `registryType`.

---

## 5. UX đích: "cài CALM MCP cho tui" thực sự xảy ra gì

Sau P0+P1 (đủ để tuyên bố mục tiêu đạt cho các client chính):

- **Claude Code** → agent chạy `claude mcp add calm -- npx -y @eilodon/calm-mcp serve`
  → cài **code hiện tại**, 1 lệnh, agent tự làm. ✅
- **Codex** → `codex mcp add calm -- npx -y @eilodon/calm-mcp serve`. ✅
- **Client bất kỳ** (sau P2 Smithery) → `npx @smithery/cli install @eilodon/calm-mcp --client <detected>`. ✅
- **Agent cold** (không biết trước) → tìm thấy `io.github.Eilodon/calm-mcp` trong MCP Registry
  → dựng đúng lệnh. ✅
- **Người đọc README** → bấm badge VS Code / Cursor deeplink. 1-click. ✅

Lệnh canonical duy nhất để dạy/nhớ:
> `npx -y @eilodon/calm-mcp serve` (là "hạt nhân" mà mọi kênh trên đều bọc quanh).

---

## 6. Rủi ro & quyết định cần chốt

- **Auto npm-publish bỏ human sanity-check?** — npm/README cố tình để thủ công. Đề xuất:
  giữ 1 approval gate (environment protection) thay vì thủ công 4 lệnh → vừa nhanh vừa an toàn.
- **`--npx` config vs absolute-path config trong `calm install`** — npx tươi hơn nhưng thêm phụ
  thuộc Node + cold-start tải package. Cho người-đã-cài-binary thì absolute path nhanh hơn. →
  để cả hai, mặc định theo cách họ vừa cài (install.sh → absolute; không clone → npx).
- **Phụ thuộc Smithery** — là bề mặt bên thứ ba, không nên là nền móng. Xếp P2 sau khi registry
  chính thống đã live.

---

## 7. Bước đi ngắn nhất nếu chỉ làm 1 việc hôm nay

**Cắt `v0.2.0` từ main + auto npm-publish + chạy registry workflow.** Ba việc này (P0.1, P0.2,
P0/P1.4) dùng hạ tầng đã dựng sẵn, và ngay lập tức: (a) mọi user mới nhận code hiện tại thay vì
v0.1.4, (b) agent cold discover được CALM. Đó là 80% giá trị mục tiêu với ~20% công sức, vì phần
còn lại (meta-installer, Smithery, Windows) là *mở rộng bề mặt*, không phải *sửa nền móng*.
