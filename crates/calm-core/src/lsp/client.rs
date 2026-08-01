//! Minimal LSP stdio client: Content-Length JSON-RPC framing hand-rolled
//! (LSP's wire framing is trivial and stable — not worth a dependency), but
//! every *message* is a real `lsp_types` struct rather than a hand-rolled
//! `serde_json::json!` literal. First `tokio::process` usage in this crate
//! (see `Cargo.toml`'s `lsp-overlay` feature comment); the runtime that
//! drives this lives on a dedicated OS thread — see `overlay.rs`.
//!
//! Wire behaviors below were validated against a real rust-analyzer 1.96
//! session (2026-07-10 probe, `lsp_probe.py`), not just the spec:
//! - rust-analyzer accepts `positionEncodings: ["utf-8", ...]` and answers
//!   `positionEncoding: "utf-8"` — column offsets are then plain UTF-8 byte
//!   offsets, no UTF-16 code-unit math needed (but the utf-16 fallback is
//!   kept for servers that don't negotiate).
//! - rust-analyzer sends server→client REQUESTS (`workspace/diagnostic/
//!   refresh`) with ids 0,1,... — colliding numerically with a client id
//!   counter that starts at 1. Response routing therefore must never treat
//!   a message bearing a `method` field as a response, and must stub-reply
//!   to server requests (a `null` result reply was verified sufficient) or
//!   the server may stall waiting on us.
//! - During initial indexing, `textDocument/definition` returns `null` or
//!   error `-32801` (content modified) before eventually resolving (~5.4s
//!   even on the tiny `rust_workspace` fixture) — callers must warm up /
//!   retry, never trust an early `null` (see `overlay.rs`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, GeneralClientCapabilities, GotoDefinitionParams,
    GotoDefinitionResponse, InitializeParams, InitializeResult, InitializedParams,
    PartialResultParams, Position, PositionEncodingKind, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// How the server counts `Position.character` units — negotiated during
/// `initialize`. LSP's un-negotiated default is UTF-16 code units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

/// Reviewed launch and initialization contract for one LSP provider. Keeping
/// this explicit prevents a provider's argv or language identity from becoming
/// an implicit client default that cannot be audited with its proof.
#[derive(Clone, Copy)]
pub struct LspClientProfile {
    pub server_args: &'static [&'static str],
    pub language_id: fn(&Path) -> String,
    pub initialization_options_json: Option<&'static str>,
    pub include_workspace_folder: bool,
}

fn extension_language_id(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| match ext {
            "rs" => "rust",
            "py" => "python",
            "go" => "go",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => "cpp",
            other => other,
        })
        .unwrap_or("plaintext")
        .to_string()
}

const DEFAULT_PROFILE: LspClientProfile = LspClientProfile {
    server_args: &[],
    language_id: extension_language_id,
    initialization_options_json: None,
    include_workspace_folder: true,
};

/// One `textDocument/definition` outcome, separating "server said nothing is
/// there" from "server said ask again" so the overlay can retry the latter.
#[derive(Debug)]
pub enum DefinitionOutcome {
    /// `(uri, 0-indexed line)` of the first location in the response.
    Resolved(Uri, u32),
    /// `null`/empty — no definition found (authoritative only once the
    /// server has finished its initial indexing; see module docs).
    NotFound,
    /// Error `-32801` (content modified) — the server is mid-index/mid-change
    /// and wants the request re-sent.
    Retryable,
}

/// JSON-RPC error code rust-analyzer returns while its view of the world is
/// still shifting (observed live during initial indexing).
const CONTENT_MODIFIED: i64 = -32801;

/// A spawned, initialized LSP server session over stdio. One instance per
/// overlay pass — not pooled/reused across runs (each run is a rare,
/// explicit refresh, not a hot path; see `LspConfig`'s doc comment).
pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    request_timeout: Duration,
    /// Negotiated during `initialize` — see `PositionEncoding`.
    pub encoding: PositionEncoding,
    language_id: fn(&Path) -> String,
}

impl LspClient {
    /// Spawn `bin` as an LSP server rooted at `root`, send `initialize` +
    /// `initialized`, and return the ready session. `request_timeout` bounds
    /// every individual request round-trip (the overlay adds its own overall
    /// pass budget on top).
    pub async fn spawn(bin: &Path, root: &Path, request_timeout: Duration) -> Result<Self> {
        Self::spawn_with_profile(bin, root, request_timeout, DEFAULT_PROFILE).await
    }

    pub async fn spawn_with_profile(
        bin: &Path,
        root: &Path,
        request_timeout: Duration,
        profile: LspClientProfile,
    ) -> Result<Self> {
        let mut child = Command::new(bin)
            .args(profile.server_args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn LSP server {bin:?}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("LSP child has no stdin"))?;
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("LSP child has no stdout"))?,
        );
        let mut me = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            request_timeout,
            encoding: PositionEncoding::Utf16, // LSP default until negotiated
            language_id: profile.language_id,
        };
        me.initialize(root, profile).await?;
        Ok(me)
    }

    #[allow(deprecated)] // `root_uri` is deprecated in favor of `workspace_folders`, but
    // rust-analyzer (and most servers) still honor it, and it's the simplest
    // correct single-root init for this overlay's one-shot session.
    async fn initialize(&mut self, root: &Path, profile: LspClientProfile) -> Result<()> {
        let root_uri = path_to_uri(root)?;
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    // Offer utf-8 first: rust-analyzer takes it (verified
                    // live), making our byte-offset column math exact.
                    position_encodings: Some(vec![
                        PositionEncodingKind::UTF8,
                        PositionEncodingKind::UTF16,
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut params = serde_json::to_value(params)?;
        if profile.include_workspace_folder {
            params["workspaceFolders"] = serde_json::json!([{
                "uri": root_uri.as_str(),
                "name": root.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
            }]);
        }
        if let Some(options) = profile.initialization_options_json {
            params["initializationOptions"] = serde_json::from_str(options)
                .context("invalid reviewed LSP initialization options JSON")?;
        }
        let result = self.request("initialize", params).await?;
        if let Ok(init) = serde_json::from_value::<InitializeResult>(result)
            && init.capabilities.position_encoding == Some(PositionEncodingKind::UTF8)
        {
            self.encoding = PositionEncoding::Utf8;
        }
        self.notify("initialized", serde_json::to_value(InitializedParams {})?)
            .await
    }

    /// `textDocument/didOpen` for `path` so the server has live content to
    /// resolve positions against. The overlay's per-file grouping already
    /// guarantees at most one call per file per session.
    /// `textDocument/didOpen` for `path` so the server has live content to
    /// resolve positions against. The overlay's per-file grouping already
    /// guarantees at most one call per file per session.
    pub async fn open_file(&mut self, path: &Path, uri: &Uri, text: &str) -> Result<()> {
        let language_id = (self.language_id)(path);
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id,
                version: 1,
                text: text.to_string(),
            },
        };
        self.notify("textDocument/didOpen", serde_json::to_value(params)?)
            .await
    }
    /// `textDocument/definition` at `(uri, line, character)` — 0-indexed,
    /// `character` in the negotiated `self.encoding`'s units.
    pub async fn definition(
        &mut self,
        uri: &Uri,
        line: u32,
        character: u32,
    ) -> Result<DefinitionOutcome> {
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        };
        let result = match self
            .request("textDocument/definition", serde_json::to_value(params)?)
            .await
        {
            Ok(v) => v,
            Err(e)
                if e.downcast_ref::<JsonRpcError>()
                    .is_some_and(|j| j.code == CONTENT_MODIFIED) =>
            {
                return Ok(DefinitionOutcome::Retryable);
            }
            Err(e) => return Err(e),
        };
        if result.is_null() {
            return Ok(DefinitionOutcome::NotFound);
        }
        let resp: GotoDefinitionResponse = serde_json::from_value(result)
            .with_context(|| "unparseable textDocument/definition response")?;
        Ok(match first_location(resp) {
            Some((uri, line)) => DefinitionOutcome::Resolved(uri, line),
            None => DefinitionOutcome::NotFound,
        })
    }

    /// Best-effort `shutdown`/`exit` + kill — never propagates an error, this
    /// runs on every exit path (including after a failed resolve loop or an
    /// expired pass budget) and a teardown failure must never mask the
    /// overlay's real result.
    pub async fn shutdown(&mut self) {
        let _ = self.request("shutdown", Value::Null).await;
        let _ = self.notify("exit", Value::Null).await;
        // Closing stdin lets a well-behaved stdio server finish processing the
        // final `exit` notification and flush any deterministic test/diagnostic
        // transcript before we resort to a hard kill.
        let _ = self.stdin.shutdown().await;
        if tokio::time::timeout(Duration::from_millis(250), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await?;
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.read_message())
                .await
                .map_err(|_| {
                    anyhow!(
                        "LSP request {method} timed out after {:?}",
                        self.request_timeout
                    )
                })??;
            // A message WITH a `method` is never a response, even when its
            // id numerically collides with ours (rust-analyzer's own request
            // ids start at 0 — observed colliding live). Requests receive a
            // reviewed, bounded reply; notifications are dropped.
            if let Some(m) = msg.get("method") {
                if let Some(their_id) = msg.get("id").cloned() {
                    let method = m.as_str().unwrap_or_default();
                    let reply = server_request_reply(
                        method,
                        msg.get("params").cloned().unwrap_or(Value::Null),
                        their_id,
                    );
                    tracing::debug!("replying to LSP server request {method}");
                    self.write_message(&reply).await?;
                }
                continue;
            }
            if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    return Err(anyhow::Error::new(JsonRpcError {
                        code,
                        message: format!("LSP error on {method}: {err}"),
                    }));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            // A response to a request we already gave up on (timed out
            // earlier) — drop it and keep reading for ours.
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&msg).await
    }

    async fn write_message(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_vec(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes()).await?;
        self.stdin.write_all(&body).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Value> {
        const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line).await?;
            if n == 0 {
                return Err(anyhow!("LSP server closed stdout"));
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break; // blank line ends the header block
            }
            headers.push_str(trimmed);
            headers.push('\n');
            if headers.len() > MAX_LSP_HEADER_BYTES {
                return Err(anyhow!("LSP header exceeds frame limit"));
            }
        }
        let len = parse_content_length(&headers)?;
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }
}

/// Parse LSP framing without allocating the declared body. The protocol allows
/// arbitrary extension headers, but exactly one bounded `Content-Length` is
/// mandatory and header names are case-insensitive.
fn parse_content_length(headers: &str) -> Result<usize> {
    const MAX_LSP_FRAME_BYTES: usize = 16 * 1024 * 1024;
    let mut content_length = None;
    for header in headers.lines() {
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        if content_length.is_some() {
            return Err(anyhow!("LSP message has duplicate Content-Length headers"));
        }
        let len = value.trim().parse::<usize>()?;
        if len > MAX_LSP_FRAME_BYTES {
            return Err(anyhow!(
                "LSP message exceeds frame limit of {MAX_LSP_FRAME_BYTES} bytes"
            ));
        }
        content_length = Some(len);
    }
    content_length.ok_or_else(|| anyhow!("LSP message missing Content-Length header"))
}

/// Server requests are an input boundary. CALM supports only the reviewed,
/// bounded no-op requests needed by provider profiles; it never executes a
/// command or reads arbitrary configuration on a server's behalf.
fn server_request_reply(method: &str, params: Value, id: Value) -> Value {
    const MAX_CONFIGURATION_ITEMS: usize = 32;
    let result = match method {
        "workspace/configuration" => {
            let Some(items) = params.get("items").and_then(Value::as_array) else {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32602, "message": "workspace/configuration requires items"},
                });
            };
            if items.len() > MAX_CONFIGURATION_ITEMS {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32602, "message": "workspace/configuration item limit exceeded"},
                });
            }
            Value::Array(vec![Value::Null; items.len()])
        }
        "client/registerCapability" => Value::Null,
        _ => {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "unsupported LSP server request"},
            });
        }
    };
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Typed JSON-RPC error so callers can branch on `code` (e.g. `-32801`).
#[derive(Debug)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for JsonRpcError {}

/// `Path` -> `file://` `Uri` — relative paths are resolved against the
/// current working directory first (LSP requires absolute URIs). `lsp_types`
/// 0.97's `Uri` (backed by `fluent_uri`, not the `url` crate) has no
/// `from_file_path` helper and its parser is RFC-3986-strict, so reserved
/// bytes (spaces above all — a checkout under `~/My Projects/` is common)
/// must be percent-encoded here or the parse fails and the caller silently
/// skips the file.
pub fn path_to_uri(path: &Path) -> Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let encoded = percent_encode_path(&abs.to_string_lossy());
    let s = format!("file://{encoded}");
    s.parse::<Uri>()
        .map_err(|e| anyhow!("not a valid file URI ({s:?}): {e}"))
}

/// Reverse of `path_to_uri` — `None` if `uri` isn't a `file://` URI.
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let raw = uri.as_str().strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(raw)))
}

/// Percent-encode a filesystem path for the path component of a `file://`
/// URI: RFC 3986 unreserved bytes and `/` pass through, everything else
/// (spaces, `#`, `?`, non-ASCII, ...) is `%XX`-encoded.
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let (Some(h), Some(l)) = (
                (bytes.get(i + 1).copied()).and_then(hex_val),
                (bytes.get(i + 2).copied()).and_then(hex_val),
            )
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn first_location(resp: GotoDefinitionResponse) -> Option<(Uri, u32)> {
    match resp {
        GotoDefinitionResponse::Scalar(loc) => Some((loc.uri, loc.range.start.line)),
        GotoDefinitionResponse::Array(locs) => {
            locs.into_iter().next().map(|l| (l.uri, l.range.start.line))
        }
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .next()
            .map(|l| (l.target_uri, l.target_selection_range.start.line)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn deterministic_mock_server_exercises_initialize_configuration_and_definition() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mock-lsp.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
emit() { body="$1"; printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"; }
emit '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"positionEncoding":"utf-8"}}}'
emit '{"jsonrpc":"2.0","id":99,"method":"workspace/configuration","params":{"items":[{},{}]}}'
emit '{"jsonrpc":"2.0","id":100,"method":"client/registerCapability","params":{"registrations":[]}}'
emit '{"jsonrpc":"2.0","id":2,"result":{"uri":"file:///tmp/definition.rs","range":{"start":{"line":7,"character":0},"end":{"line":7,"character":1}}}}'
emit '{"jsonrpc":"2.0","id":3,"result":null}'
printf '%s\n' "$@" > "$0.args"
cat > "$0.transcript"
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();

        let source = dir.path().join("sample.rs");
        std::fs::write(&source, "fn caller() { target(); }\n").unwrap();
        let uri = path_to_uri(&source).unwrap();
        let profile = LspClientProfile {
            server_args: &["--mock-profile"],
            language_id: |_| "mock-rust".to_string(),
            initialization_options_json: Some(r#"{"mockOption":true}"#),
            include_workspace_folder: true,
        };
        let mut client =
            LspClient::spawn_with_profile(&script, dir.path(), Duration::from_secs(1), profile)
                .await
                .unwrap();
        client
            .open_file(&source, &uri, "fn caller() { target(); }\n")
            .await
            .unwrap();
        assert!(matches!(
            client.definition(&uri, 0, 14).await.unwrap(),
            DefinitionOutcome::Resolved(_, 7)
        ));
        client.shutdown().await;
        assert_eq!(
            std::fs::read_to_string(format!("{}.args", script.display())).unwrap(),
            "--mock-profile\n"
        );
        let transcript =
            std::fs::read_to_string(format!("{}.transcript", script.display())).unwrap();
        assert!(transcript.contains("workspaceFolders"));
        assert!(transcript.contains("initializationOptions"));
        assert!(transcript.contains("mock-rust"));
    }

    #[test]
    fn percent_encoding_round_trips_a_path_with_spaces() {
        let p = "/home/user/My Projects/repo/src/main.rs";
        let encoded = percent_encode_path(p);
        assert_eq!(encoded, "/home/user/My%20Projects/repo/src/main.rs");
        assert_eq!(percent_decode(&encoded), p);
    }

    #[test]
    fn path_to_uri_accepts_a_path_with_spaces() {
        let uri = path_to_uri(Path::new("/tmp/with space/f.rs")).unwrap();
        assert_eq!(uri.as_str(), "file:///tmp/with%20space/f.rs");
        assert_eq!(
            uri_to_path(&uri).unwrap(),
            PathBuf::from("/tmp/with space/f.rs")
        );
    }

    #[test]
    fn unknown_server_request_receives_protocol_error_not_successful_null() {
        let reply = server_request_reply(
            "workspace/executeCommand",
            serde_json::json!({"command": "curl attacker.invalid"}),
            serde_json::json!(7),
        );

        assert_eq!(reply["id"], 7);
        assert_eq!(reply["error"]["code"], -32601);
        assert!(reply.get("result").is_none());
    }

    #[test]
    fn content_length_rejects_an_oversized_lsp_frame_before_allocation() {
        let error = parse_content_length("Content-Length: 16777217\r\n\r\n")
            .expect_err("a server must not be able to force an unbounded allocation");
        assert!(error.to_string().contains("frame limit"));
    }
}
