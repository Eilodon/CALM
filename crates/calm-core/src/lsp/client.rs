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
}

impl LspClient {
    /// Spawn `bin` as an LSP server rooted at `root`, send `initialize` +
    /// `initialized`, and return the ready session. `request_timeout` bounds
    /// every individual request round-trip (the overlay adds its own overall
    /// pass budget on top).
    pub async fn spawn(bin: &Path, root: &Path, request_timeout: Duration) -> Result<Self> {
        let mut child = Command::new(bin)
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
        };
        me.initialize(root).await?;
        Ok(me)
    }

    #[allow(deprecated)] // `root_uri` is deprecated in favor of `workspace_folders`, but
    // rust-analyzer (and most servers) still honor it, and it's the simplest
    // correct single-root init for this overlay's one-shot session.
    async fn initialize(&mut self, root: &Path) -> Result<()> {
        let root_uri = path_to_uri(root)?;
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri),
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
        let result = self
            .request("initialize", serde_json::to_value(params)?)
            .await?;
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
    pub async fn open_file(&mut self, path: &Path, uri: &Uri, text: &str) -> Result<()> {
        let language_id = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| match ext {
                "rs" => "rust",
                other => other,
            })
            .unwrap_or("rust")
            .to_string();
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
        let _ = self.child.kill().await;
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
            // ids start at 0 — observed colliding live). Requests get a
            // `null` stub reply (verified sufficient live) so the server
            // never stalls waiting on us; notifications are dropped.
            if let Some(m) = msg.get("method") {
                if let Some(their_id) = msg.get("id").cloned() {
                    tracing::debug!("stub-replying null to server request {m}");
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": their_id,
                        "result": Value::Null,
                    });
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
        let mut content_length: Option<usize> = None;
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
            if let Some(v) = trimmed.strip_prefix("Content-Length:") {
                content_length = Some(v.trim().parse()?);
            }
        }
        let len =
            content_length.ok_or_else(|| anyhow!("LSP message missing Content-Length header"))?;
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }
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
}
