# ADR-0005: Daemon dùng chung + forwarder mỏng cho index/watch/embed

- **Status**: Proposed — **Deferred (kiến trúc cân nhắc cho tương lai, chưa lên lịch
  implement)**. Giải pháp hiện tại (fix `load_embedder_readonly` hôm nay) đã đủ dùng ở
  quy mô hiện tại; ADR này ghi lại bậc thang tiếp theo *nếu/khi* chi phí N-process-per-project
  trở nên đáng kể (model lớn hơn, nhiều session hơn). Không nên implement cho tới lúc đó — xem
  "Khi nào nên kích hoạt lại" ở cuối.
- **Date**: 2026-07-07 (draft), 2026-07-07 (revised sau review kỹ thuật)
- **Decision makers**: TBD (draft do Claude chuẩn bị theo yêu cầu, cần chủ dự án duyệt)
- **Related**: fix `load_embedder_readonly`/`owns_indexer_lock` (`crates/calm-server/src/lib.rs`,
  2026-07-07, cùng phiên làm việc dẫn tới ADR này), `calm_core::db::instance_lock`
  (`crates/calm-core/src/db/instance_lock.rs`), `scripts/mcp-launcher.sh`

> **Đọc trước khi dùng ADR này**: đây **không** phải việc cần làm ngay. Giải pháp đang chạy trên
> production (mỗi process tự load một `Embedder` read-only ~63MB) đúng và đủ. ADR này chỉ đáng
> lôi ra khi một trong các điều kiện ở mục cuối được thỏa. Nội dung dưới đây đã được rà lại đối
> chiếu source thật (rmcp 0.1.5, `CalmServer`, `instance_lock`, `serve_stdio_with_preset`,
> `mcp-launcher.sh`) và đã lấp các khoảng trống mà bản draft đầu bỏ sót.

## Context

**Bug vừa fix hôm nay, và giới hạn của fix đó.** Mỗi MCP client (một cửa sổ Claude Code, một
session `--resume`, v.v.) tự spawn một tiến trình `calm serve --project-root X` riêng qua
`scripts/mcp-launcher.sh`. Xác nhận trực tiếp trên chính repo này lúc điều tra: **3 tiến trình
`calm serve` cùng chạy song song** cho cùng một `project_root`, thuộc 3 session khác nhau (1
Claude Desktop, 2 Cursor). `calm_core::db::instance_lock::try_acquire` (advisory file lock trên
`.calm/indexer.lock`) đảm bảo chỉ MỘT trong số đó thực sự index/watch/embed; các process còn lại
("lock loser") trước đây bỏ qua toàn bộ bootstrap embeddings, khiến `embed_status` kẹt vĩnh viễn
ở `Disabled` bất kể config/feature flag. Fix đã ship: mọi process tự load một bản `Embedder`
riêng (an toàn — model vendored, zero-network, ~63MB) để tự phục vụ query-time embedding, dù
không phải chủ sở hữu lock.

Fix đó đúng và đủ dùng ở quy mô hiện tại, nhưng **không giải quyết tận gốc việc N process cùng
tồn tại cho cùng một project**: N× tiến trình OS, N× kết nối SQLite (dù read-only), N× lần
check/tranh advisory lock mỗi lần khởi động, N× bản copy model trong RAM. Đây đúng là *đúng dạng*
vấn đề đã có tiền lệ và giải pháp chuẩn trong hệ sinh thái LSP/dev-tool.

**Tiền lệ bên ngoài** (xem thêm câu trả lời trước trong hội thoại này):

- **gopls `-remote=auto`**: một daemon dùng chung xử lý mọi session; mỗi editor chỉ spawn một
  "sidecar" mỏng forward LSP qua Unix socket. Số liệu thật: 40 session riêng lẻ ~20GB RAM → qua
  daemon ~500MB. ([go.dev/gopls/daemon](https://go.dev/gopls/daemon))
- **`anthropics/claude-code#19517`**: đúng bài toán này, xảy ra ngay trong hệ sinh thái Claude
  Code — nhiều session Claude Code cùng workspace Go, mỗi session tự spawn `gopls` riêng, đề
  xuất chính là bật daemon mode. Không phải vấn đề riêng của CALM.
  ([github.com/anthropics/claude-code/issues/19517](https://github.com/anthropics/claude-code/issues/19517))
- **`ra-multiplex`/kế nhiệm `lspmux`**: rust-analyzer không tự hỗ trợ multi-client, cộng đồng viết
  multiplexer riêng ngoài core để chia sẻ một instance qua TCP socket.
  ([github.com/pr2502/ra-multiplex](https://github.com/pr2502/ra-multiplex))
- **Sourcegraph Zoekt**: tách hẳn indexer (ghi shard file bất biến) khỏi reader (mmap trực tiếp,
  không lock/IPC). CALM's SQLite single-writer + reader tự mở connection **đã đúng pattern này ở
  tầng dữ liệu trên đĩa** — khoảng trống chỉ nằm ở tầng *tài nguyên runtime trong RAM* (model đã
  load), thứ Zoekt không gặp phải vì mọi thứ cần cho một query đã nằm sẵn trong shard mmap, không
  cần "chạy" gì thêm.
  ([deepwiki.com/sourcegraph/zoekt](https://deepwiki.com/sourcegraph/zoekt/1.1-architecture))

**Khả thi kỹ thuật đã xác minh trực tiếp trong source `rmcp` 0.1.5** (crate MCP server CALM đang
dùng):

- `transport/io.rs`: có blanket impl `IntoTransport<Role, io::Error, _> for S where S: AsyncRead
  + AsyncWrite + Send + 'static` (nội bộ chỉ gọi `tokio::io::split(self)`). Tức
  `tokio::net::UnixStream` cắm thẳng vào được, không cần viết lại gì ở tầng JSON-RPC/MCP.
- `service/server.rs`: `serve_server_with_ct<S, T, E, A>(service, transport, ct) where S:
  Service<RoleServer>, T: IntoTransport<...>` — **service nhận theo giá trị, không có ràng buộc
  `Clone` hay singleton nào**. Gọi lại nhiều lần trong một accept-loop, mỗi lần một `service` nhẹ
  riêng, là cách dùng đúng chuẩn của crate.

Đây là **đổi transport, không phải viết lại giao thức**.

## Decision

**Chuyển từ "N process đầy đủ tranh nhau một lock" sang "1 daemon dùng chung + N forwarder mỏng
per-session", đúng mẫu gopls/ra-multiplex, áp dụng cho toàn bộ index/watch/embed — không chỉ
embedding.** (Chỉ triển khai khi các điều kiện ở mục cuối được thỏa.)

### Cấu trúc cơ bản

1. **Daemon nằm ở `.calm/daemon.sock`** (Unix domain socket) — tận dụng đúng scoping per-project
   `.calm/` đã dùng cho `indexer.lock` hôm nay, không cần registry toàn cục theo project_root.

2. **Tách rõ hai vai trò của binary `calm`:**
   - `calm serve --project-root X` giữ nguyên hành vi standalone hôm nay (tự index/watch/embed,
     tự phục vụ MCP qua stdio) — dùng cho CI, script một lần, hoặc bất kỳ ai không cần chia sẻ.
     Daemon ở bước 4 là chính lệnh này với thêm cờ `--listen unix:.calm/daemon.sock`.
   - Thêm chế độ mới — đề xuất `calm connect --project-root X` — là **forwarder thuần túy**: không
     parse JSON-RPC/MCP gì cả, chỉ relay byte hai chiều (stdin↔socket, socket↔stdout). Giữ
     forwarder tối giản để không phụ thuộc version giao thức MCP tương lai.
   - `scripts/mcp-launcher.sh` đổi target `exec` cuối cùng từ `calm serve` sang `calm connect`.

### Cơ chế giành quyền làm daemon (viết lại so với draft đầu)

3. **Arbiter spawn = `bind` socket, KHÔNG chồng nghĩa lên `instance_lock`.** Bản draft đầu tái
   dùng `instance_lock` làm "quyền spawn daemon" — sai lầm vì nó trộn hai nghĩa khác nhau
   (`instance_lock` hôm nay chỉ có một nghĩa: *"tôi là process chạy indexer/writer"*, xem docstring
   `try_acquire`) và có thể deadlock nếu daemon lẫn forwarder cùng tranh một file lock sai thứ tự.
   Thay vào đó:
   - Forwarder thử `connect(.calm/daemon.sock)`. Thành công → relay ngay, xong.
   - Thất bại (`ENOENT`/`ECONNREFUSED`) → forwarder spawn `calm serve --listen unix:...` **detached
     hoàn toàn** rồi retry connect. **`UnixListener::bind` chính là mutex spawn tự nhiên**: nếu
     nhiều forwarder cùng spawn daemon (đúng kịch bản gây bug gốc), chỉ một daemon `bind` thành
     công, các daemon còn lại nhận `EADDRINUSE` và tự thoát sạch ngay lập tức (rẻ, chỉ xảy ra một
     lần lúc cold-start). Không cần lock riêng cho việc này.
   - (Tùy chọn tối ưu, không bắt buộc cho v1) Nếu muốn tránh cả việc spawn N daemon rồi N-1 tự
     thoát, có thể thêm một lock **riêng** `.calm/spawn.lock` để chỉ một forwarder spawn — nhưng
     tuyệt đối **không** dùng lại `indexer.lock` cho mục đích này.

4. **`instance_lock` giữ nguyên nghĩa cũ, do daemon (và standalone serve) acquire.** Trong mô
   hình daemon, chính **daemon** gọi `try_acquire` để khẳng định quyền writer/indexer — đúng như
   `calm serve` làm hôm nay, không đổi ngữ nghĩa. Điều này vẫn cần thiết: mode standalone `calm
   serve` (CI/script, §2) vẫn tồn tại và có thể chạy song song, nên lock vẫn là thứ ngăn hai
   writer cùng ghi một DB. Bỏ `owns_indexer_lock`/`load_embedder_readonly` (xem §12) *không* có
   nghĩa là bỏ `instance_lock`.

5. **Detach đúng cách là chi tiết quan trọng nhất, dễ làm sai nhất.** Daemon phải độc lập vòng
   đời hoàn toàn với forwarder đã spawn ra nó. Nếu không detach đúng, session đầu tiên đóng lại
   (stdin đóng, hoặc shell gửi SIGTERM cả process group) sẽ kéo daemon chết theo, sập luôn mọi
   session khác đang dùng chung — **tái tạo lại đúng bug gốc dưới lớp áo mới**. gopls giải quyết
   bằng cách daemon luôn là tiến trình tách biệt, chưa từng phụ thuộc stdio của bất kỳ client nào.
   Cơ chế cụ thể trên Rust/Unix: `std::os::unix::process::CommandExt::process_group(0)` (đặt daemon
   vào process group riêng) + `Stdio::null()` cho cả stdin/stdout/stderr. Đây là rủi ro #1, cần
   test riêng (xem Risks).

### Vòng đời server per-connection

6. **Daemon accept-loop**: mỗi kết nối Unix socket mới → `tokio::spawn` một task chạy
   `serve_server_with_ct(server_for_conn, unix_stream, conn_ct)` riêng. Hai chi tiết bắt buộc,
   khác với `serve_stdio_with_preset` hôm nay:
   - **Phải `spawn` mỗi `RunningService`**, KHÔNG `.await service.waiting()` inline trong loop —
     nếu block, daemon chỉ phục vụ được đúng 1 client. (Code hôm nay `.await`-ing `waiting()` là
     đúng cho stdio 1-client, sai cho accept-loop.)
   - **Cancellation phân tầng**: một `global_ct` cho toàn daemon (idle-timeout/SIGTERM); mỗi
     connection dùng `conn_ct = global_ct.child_token()` để một forwarder disconnect chỉ hủy
     session của nó, không giết daemon.

7. **Tách field của `CalmServer` thành 2 nhóm** (hiện gộp chung tất cả trong `tools.rs`). Điểm
   thuận lợi: **mọi field chia sẻ đã sẵn là `Arc<...>`**, nên "tách" thực chất chỉ là clone Arc
   (bump refcount) — không cần refactor lớn. Thêm một constructor `CalmServer::for_connection(&shared)`
   thay vì gọi `new_with_preset` per-connection (`new_with_preset` mở + `init_db` SQLite mỗi lần,
   thừa cho mỗi accept).
   - **Chia sẻ một lần cho cả daemon** (clone Arc mỗi accept): `project_root`, `db_path`, `phase`,
     `last_index_error`, `embedder`, `embed_status`, `last_embed_error`, `coverage`, `edit_lock`
     (đặc biệt quan trọng: `edit_lock` PHẢI dùng chung toàn cục để serialize
     `edit_lines`/`edit_symbol` đúng nghĩa across mọi session — chúng ghi cùng một DB writer).
   - **Riêng theo từng kết nối** (tạo mới mỗi khi accept): `session_log` (lịch sử
     explore/frontier của một agent không được rò sang agent khác dùng chung daemon).
   - **`preset` là daemon-global, KHÔNG per-connection** (sửa so với draft đầu): forwarder là
     relay byte thuần (§2), không có kênh nào truyền preset per-client vào daemon, nên preset bị
     cố định ở lệnh spawn daemon (first-writer-wins, xem §11). Xếp nó vào nhóm shared và
     document rõ, đừng để §5 draft cũ ngụ ý nó đổi được per-connection.

8. **Bỏ hẳn `owns_indexer_lock`/`load_embedder_readonly`** sau khi daemon lên production — không
   còn khái niệm "lock loser" nữa vì chỉ có đúng một daemon làm việc theo thiết kế. Fix hôm nay là
   **bậc thang trung gian đúng đắn**, không phải công sức bỏ đi — primitive `load_embedder_model`
   vẫn chính là thứ daemon dùng để tự load model một lần khi khởi động. (Lưu ý: đây là bỏ *khái
   niệm lock-loser*, không phải bỏ `instance_lock` — xem §4.)

### Các mục bổ sung sau review (draft đầu bỏ sót)

9. **Version handshake — chống daemon cũ phục vụ code cũ (quan trọng).** `mcp-launcher.sh` đầu tư
   rất nhiều để không bao giờ exec binary cũ (`is_binary_fresh` mtime-check, `is_lfs_pointer`, cả
   một incident đã ghi lại). Một daemon dài hạn **tái tạo đúng rủi ro đó ở tầng khác**: forwarder
   mới (binary fresh) connect vào daemon đang chạy commit cũ → cả session chạy code cũ, và toàn bộ
   machinery freshness của launcher thành vô nghĩa cho phần thực sự làm việc. Bắt buộc có handshake:
   ghi version/git-SHA của daemon vào `.calm/daemon.meta` cạnh socket; forwarder so sánh với
   version hiện tại trước khi connect; lệch → coi như stale-socket (§14), giành quyền spawn daemon
   mới. (gopls làm đúng kiểu này — version trong protocol daemon.)

10. **Log của daemon phải có đích rõ ràng.** Hôm nay stderr của `calm serve` (mọi
    `tracing::info!/error!`) được MCP host hiển thị; launcher đảm bảo "stdout chỉ JSON-RPC, log ra
    stderr". Sau khi daemon detach với cả 3 fd `null` (§5), log đó **biến mất** — mù đúng lúc cần
    debug nhất (idle-timeout evict nhầm, indexer panic). Daemon phải ghi tracing vào
    `.calm/daemon.log` (có rotate).

11. **Config/env đóng băng theo forwarder đầu tiên — first-writer-wins, cần document.** Daemon load
    config + env một lần lúc spawn, từ môi trường của forwarder *đầu tiên*. Client thứ 2 với
    `RUST_LOG`/offline-policy env/config khác sẽ **im lặng nhận config của người spawn**. Đây là
    tính chất cố hữu của mọi mô hình daemon (gopls cũng vậy) — chấp nhận được, nhưng phải nói
    thẳng trong `docs/mcp-client-setup.md`, không để người dùng ngạc nhiên. `preset` (§7) là một
    trường hợp cụ thể của tính chất này.

12. **Quyền socket — điều kiện đủ để "an toàn hơn TCP" thực sự đúng.** Socket này cấp *toàn bộ*
    MCP surface, gồm `edit_lines`/`edit_symbol` **ghi thẳng vào repo**. Trên máy dùng chung, nếu
    `.calm/` theo umask mặc định (0755), user khác có thể connect → đọc code + **sửa file** của
    bạn. Bắt buộc: tạo `.calm/` với `0700` (hoặc socket `0600`) và assert quyền trước khi bind.
    Đây mới là cơ sở để lý lẽ "Unix socket an toàn hơn TCP theo mặc định" (xem Alternatives) đứng
    vững.

### Vận hành

13. **Idle-timeout tự tắt**, giống `gopls -remote.listen.timeout`: bắt đầu đếm ngược khi thực sự
    rảnh — **không chỉ dựa vào số kết nối = 0, mà còn phải không có index/embed job nào đang chạy**
    (xem Risks). Hết hạn → unlink socket + `.calm/daemon.meta`, thoát sạch. Ngưỡng cụ thể cần đo
    thực tế, không chốt số ở đây.

14. **Stale-socket recovery**: file socket tồn tại nhưng connect thất bại (daemon cũ crash không
    kịp dọn) → xóa socket cũ, quay lại bước spawn (§3) để trở thành daemon mới — cùng cách phần lớn
    Unix daemon khác xử lý stale socket. Cùng đường xử lý với version mismatch (§9).

15. **Forwarder chết thẳng khi daemon rớt kết nối giữa chừng** (crash, cancel) — đóng stdio, thoát
    khác 0, không tự âm thầm respawn/retry. Nhất quán với triết lý "trung thực thay vì đoán" đã có
    sẵn trong CALM (`embed_status`/`indexing_phase` không bao giờ giả vờ ready) — để MCP host tự
    quyết định phản ứng, không phải CALM tự ý che giấu một lỗi thật.

16. **Phạm vi platform v1: chỉ Unix domain socket** (Linux/macOS) — khớp đúng ma trận prebuilt
    binary hiện có (`x86_64`/`aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, không có
    Windows). Trên platform không có Unix socket, `calm connect` phải **fallback về hành vi hôm
    nay** (`exec calm serve` trực tiếp, standalone, không chia sẻ) thay vì lỗi cứng.

## Consequences

- Xóa hẳn phần chi phí nhân bản còn lại sau fix hôm nay: N× bản copy model (~63MB/session), N×
  connection SQLite mở lúc khởi động, N× lần check/tranh `instance_lock` mỗi session mới. (File
  watcher thì đã ổn từ trước — nhánh "lock loser" hôm nay return sớm trước khi chạm
  `watcher::run_watch_loop`, nên không có N× watcher chạy song song.)
- Sửa đúng nguyên nhân khiến việc điều tra hôm nay mất công: N tiến trình nặng tương đương nhau
  âm thầm tranh một lock, vô hình với người dùng, phải `ps -ef`/`lsof` thủ công mới phát hiện.
  Sau ADR này: một session = một forwarder mỏng, thoát ngay khi session đóng — mô hình mental
  đúng với những gì người dùng thấy trên `ps`.
- Bề mặt kỹ thuật mới cần build + test riêng — **lớn hơn draft đầu ước lượng**: IPC transport,
  daemonization đúng (rủi ro cao nhất), accept-loop + cancellation phân tầng, tách field
  per-connection/shared, **version handshake, daemon logging, socket permissions**. Không phải
  patch nhỏ — cần implementation plan + test riêng, triển khai dần sau cờ `--daemon` opt-in,
  dogfood, rồi mới cân nhắc đổi mặc định.
- `scripts/mcp-launcher.sh`, `docs/mcp-client-setup.md`, `docs/cloud-environment-setup.md` cần
  review lại toàn bộ (mô hình "launcher `exec` thay chỗ chính nó", và ghi chú first-writer-wins
  config ở §11).

## Risks

- **Daemonize sai → tái tạo lại đúng bug gốc dưới lớp áo mới** (xem Decision §5): daemon vẫn phụ
  thuộc process group của forwarder đầu tiên → session đầu đóng kéo sập mọi session khác. Rủi ro
  nghiêm trọng nhất, cần test trực tiếp: spawn daemon qua forwarder A, `kill -TERM -<pgid_A>` cả
  process group của A, assert daemon vẫn phục vụ được kết nối B bình thường.
- **Version skew** (Decision §9): quên handshake → daemon cũ âm thầm phục vụ code cũ, vô hiệu hóa
  toàn bộ freshness machinery của launcher. Test: spawn daemon, thay binary bằng version khác,
  forwarder mới phải phát hiện mismatch và respawn.
- **Idle-timeout tính sai** có thể evict daemon giữa lúc đang embed/index dở nếu một loạt session
  cùng disconnect ngắn hạn (ví dụ: IDE restart) — cần chỉ đếm ngược khi thực sự rảnh (không có
  index/embed job nào đang chạy), không chỉ dựa vào số kết nối = 0.
- **Race nhiều forwarder cùng giành làm daemon** khi mở nhiều session cùng lúc (đúng kịch bản gây
  ra bug gốc) — với mô hình §3 (bind-là-arbiter), cần test rằng đúng một daemon `bind` thành công
  và các daemon thừa thoát sạch không để lại socket rác.
- **Socket permissions** (Decision §12): test rằng socket/`.calm/` không world-accessible, đặc
  biệt trên môi trường multi-user/CI dùng chung.
- **Windows/no-Unix-socket fallback** cần test rõ ràng để đảm bảo suy biến về đúng hành vi hôm
  nay, không phải một lỗi mới.

## Alternatives Considered

- **Giữ nguyên fix hôm nay (mỗi process tự load embedder riêng), không làm gì thêm**: rẻ, đã
  ship, đủ dùng ở quy mô hiện tại vì model nhỏ (~63MB). **Đây là lựa chọn được khuyến nghị cho
  hiện tại** — ADR này chỉ vượt qua nó khi các điều kiện kích hoạt bên dưới được thỏa. Không giải
  quyết tận gốc N-process-per-project, nhưng chi phí nhân bản còn nhỏ nên chưa đáng đánh đổi lấy
  bề mặt kỹ thuật lớn của daemon.
- **TCP loopback socket thay vì Unix domain socket**: portable hơn (có sẵn trên Windows). Bị bác
  cho v1 vì lộ thêm bề mặt tấn công cục bộ (process khác trên máy có thể connect vào port nếu
  không có auth token riêng) — Unix socket dựa vào file permission của `.calm/` (đã gitignored,
  và phải enforce 0700 theo §12) an toàn hơn theo mặc định mà không cần thêm cơ chế auth.
- **Không dùng daemon, chỉ thêm shared mmap cache kiểu Zoekt cho riêng lớp vector đã embed**:
  không giải quyết được gốc vấn đề — mỗi process vẫn phải tự "chạy" một bản model runtime để
  encode query text, khác Zoekt (mọi thứ cần cho một query đã nằm sẵn trong shard mmap). Chỉ áp
  dụng được cho lớp lưu trữ vector đã có sẵn qua SQLite, không thay được cho lớp model-loading.

## Khi nào nên kích hoạt lại ADR này

Giữ ở trạng thái Deferred cho tới khi **ít nhất một** điều kiện sau đúng — lúc đó cân nhắc viết
implementation plan:

1. **Model embedding lớn lên đáng kể** (ví dụ đổi sang model ≫63MB, hoặc thêm tầng embedding thứ
   hai): chi phí N× RAM không còn nhỏ.
2. **Số session đồng thời trên cùng project tăng mạnh** (nhiều editor/agent thường trực), khiến
   N× process trở thành áp lực RAM/CPU thấy được.
3. **Có bằng chứng đo được** (không phải phỏng đoán) rằng N-process-per-project gây chậm/tốn tài
   nguyên trong thực tế sử dụng.

Cho tới lúc đó: fix hiện tại là câu trả lời đúng, và mọi công sức nên dành cho việc khác.
