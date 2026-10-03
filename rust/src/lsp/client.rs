#![allow(clippy::wildcard_imports, clippy::default_trait_access)]

use lsp_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::capabilities::{SemanticBackendInfo, SemanticBackendKind, SemanticCapabilities};
use super::config::ResolvedServer;

const INIT_TIMEOUT_SECS: u64 = 60;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const SHUTDOWN_TIMEOUT_SECS: u64 = 5;

pub fn file_path_to_uri(path: &str) -> Result<Uri, String> {
    let abs = if path.starts_with('/') || (path.len() >= 2 && path.as_bytes()[1] == b':') {
        path.to_string()
    } else {
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| format!("Cannot resolve path '{path}': {e}"))?
    };
    let normalized = abs.replace('\\', "/");
    let uri_str = if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    };
    uri_str
        .parse::<Uri>()
        .map_err(|e| format!("Invalid URI: {e}"))
}

pub fn uri_to_file_path(uri: &Uri) -> Option<String> {
    let s = uri.as_str();
    s.strip_prefix("file://")
        .map(|p| urlencoding::decode(p).map_or_else(|_| p.to_string(), |d| d.to_string()))
        .map(|p| {
            // `file:///C:/x` → `C:/x`, not `/C:/x` (Windows drive URIs).
            let b = p.as_bytes();
            if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                p[1..].to_string()
            } else {
                p
            }
        })
}

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    response_rx: Receiver<Result<Value, String>>,
    next_id: AtomicI64,
    initialized: bool,
    /// Cleared by the reader thread once the server's stdout ends (exit/crash),
    /// so the router can evict a dead server without `&mut` access.
    alive: Arc<AtomicBool>,
    /// Server identity, negotiated capabilities and position encoding from
    /// the `initialize` handshake.
    info: SemanticBackendInfo,
    documents: DocumentStore,
    /// Per-request timeout; see `LspBackend::set_request_timeout`.
    request_timeout: Duration,
}

/// Open documents: URI → (version, content hash). Keeps repeated queries from
/// re-sending `didOpen` and turns content changes into `didChange`.
#[derive(Default)]
struct DocumentStore {
    documents: HashMap<String, (i32, u64)>,
    /// URIs in open order, oldest first — bounded by [`MAX_OPEN_DOCUMENTS`].
    open_order: VecDeque<String>,
}

impl DocumentStore {
    /// Records `text` for `uri` and says which notification brings the server
    /// up to date, plus a document to close when the bound is exceeded.
    fn sync(&mut self, uri: &str, text: &str) -> (DocumentSync, Option<String>) {
        let hash = content_hash(text);
        if let Some((version, known)) = self.documents.get_mut(uri) {
            if *known == hash {
                return (DocumentSync::Unchanged, None);
            }
            *version += 1;
            *known = hash;
            return (DocumentSync::Change(*version), None);
        }
        self.documents.insert(uri.to_string(), (1, hash));
        self.open_order.push_back(uri.to_string());
        let evicted = (self.open_order.len() > MAX_OPEN_DOCUMENTS)
            .then(|| self.open_order.pop_front())
            .flatten();
        if let Some(old) = &evicted {
            self.documents.remove(old);
        }
        (DocumentSync::Open, evicted)
    }
}

/// Upper bound on documents held open in one server; beyond it the oldest is
/// closed. Bounds server memory during bulk semantic enrichment.
const MAX_OPEN_DOCUMENTS: usize = 128;

/// What the document store must send for a `sync_document` call.
#[derive(Debug, PartialEq, Eq)]
enum DocumentSync {
    Open,
    Change(i32),
    Unchanged,
}

fn content_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// How `read_response` handles one incoming message.
#[derive(Debug, PartialEq)]
enum Incoming {
    /// A server request or notification: send the reply (if any), keep waiting.
    FromServer(Option<Value>),
    /// The awaited response (or its error).
    Response(Result<Option<Value>, String>),
    /// A response to another request (e.g. one that timed out): ignore.
    Other,
}

/// A message with `method` is a server request or notification — never our
/// response, even when its id equals ours (client and server ids are separate
/// id spaces).
fn route_incoming(msg: Value, expected_id: i64) -> Incoming {
    if msg.get("method").is_some() {
        return Incoming::FromServer(server_request_reply(&msg));
    }
    if msg.get("id").and_then(Value::as_i64) != Some(expected_id) {
        return Incoming::Other;
    }
    Incoming::Response(
        serde_json::from_value::<JsonRpcResponse>(msg)
            .map_err(|e| e.to_string())
            .and_then(|resp| match resp.error {
                Some(err) => Err(format!("LSP error: {}", err.message)),
                None => Ok(resp.result),
            }),
    )
}

/// Answers a server→client *request* (has both `id` and `method`). Servers
/// block on these, so each gets a reply: harmless acknowledgements succeed,
/// `workspace/configuration` gets one `null` per requested item (= "use your
/// defaults"), everything else is `MethodNotFound`.
fn server_request_reply(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method")?.as_str()?;
    let reply = match method {
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => {
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })
        }
        "workspace/configuration" => {
            let n = msg
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": vec![Value::Null; n] })
        }
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("lean-ctx does not handle {method}") }
        }),
    };
    Some(reply)
}

/// Credential-bearing variables are not inherited by language servers: they
/// read source and must not be able to reuse the user's tokens. This holds for
/// toolchain tokens too (`NPM_TOKEN`, `CARGO_REGISTRIES_*_TOKEN`); toolchain
/// *configuration* (`CARGO_HOME`, `GOPROXY`, `GOPRIVATE`, …) is kept, and
/// file-based credentials (`~/.cargo/credentials.toml`, `.npmrc`, `.netrc`)
/// remain available for private registry resolution.
///
/// Deliberately kept: `SSH_AUTH_SOCK` / askpass helpers. A server that runs
/// project code (build scripts, proc macros) executes as the user and can
/// reach the agent socket without the variable, so stripping it is no
/// isolation boundary — but it breaks `cargo metadata` / `go list` for
/// private git dependencies over SSH, i.e. the server's workspace load. The
/// scrub targets reusable secrets that could leak into server logs or crash
/// reports, not process isolation.
fn is_credential_env(name: &str) -> bool {
    const SECRET_MARKERS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "SECRET_KEY",
        "PRIVATE_KEY",
        "CREDENTIALS",
        "AUTH",
        "AUTH_CONFIG",
    ];
    let upper = name.to_ascii_uppercase();
    SECRET_MARKERS
        .iter()
        .any(|m| upper == *m || upper.ends_with(&format!("_{m}")))
}

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    params: Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    #[serde(rename = "id")]
    _id: Option<i64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    #[serde(rename = "code")]
    _code: i64,
    message: String,
}

fn read_one_message(reader: &mut BufReader<ChildStdout>) -> Result<Value, String> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .map_err(|e| format!("Read header: {e}"))?;
        if bytes_read == 0 {
            return Err("LSP server closed connection (EOF)".into());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
            content_length = val.parse().map_err(|e| format!("Parse length: {e}"))?;
        }
    }
    if content_length == 0 {
        return Err("Zero content length from LSP server".into());
    }
    let mut body = vec![0u8; content_length];
    std::io::Read::read_exact(reader, &mut body).map_err(|e| format!("Read body: {e}"))?;
    let text = String::from_utf8_lossy(&body);
    serde_json::from_str(&text).map_err(|e| format!("Parse response: {e}"))
}

fn spawn_reader(stdout: ChildStdout, alive: Arc<AtomicBool>) -> Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("lsp-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_one_message(&mut reader) {
                    Ok(msg) => {
                        if tx.send(Ok(msg)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        alive.store(false, Ordering::Release);
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        })
        .ok();
    rx
}

impl LspClient {
    /// Starts and initializes a server. `init_timeout` bounds the `initialize`
    /// handshake (default 60 s) — budgeted background work passes what is left
    /// of its own deadline.
    pub fn start(
        server: &ResolvedServer,
        root_uri: &Uri,
        init_timeout: Option<Duration>,
    ) -> Result<Self, String> {
        let config = &server.config;
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (name, _) in std::env::vars_os() {
            if name.to_str().is_some_and(is_credential_env) {
                cmd.env_remove(&name);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to start LSP server '{}': {e}", config.command))?;

        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;
        let alive = Arc::new(AtomicBool::new(true));
        let response_rx = spawn_reader(stdout, Arc::clone(&alive));

        let mut client = Self {
            child,
            stdin,
            response_rx,
            next_id: AtomicI64::new(1),
            initialized: false,
            alive,
            info: SemanticBackendInfo {
                kind: SemanticBackendKind::Lsp,
                server_name: None,
                server_version: None,
                capabilities: SemanticCapabilities::default(),
                utf8_positions: false,
            },
            documents: DocumentStore::default(),
            request_timeout: Duration::from_secs(REQUEST_TIMEOUT_SECS),
        };

        client.initialize(
            root_uri,
            init_timeout.unwrap_or(Duration::from_secs(INIT_TIMEOUT_SECS)),
            server.init_options.clone(),
        )?;
        // `serverInfo` is optional (typescript-language-server omits it); the
        // binary still distinguishes servers in identities and cache keys.
        if client.info.server_name.is_none() {
            client.info.server_name = Some(server.binary_name());
        }
        Ok(client)
    }

    fn check_alive(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Err(format!("LSP server exited: {status}")),
            Ok(None) => Ok(()),
            Err(e) => Err(format!("Cannot check LSP server status: {e}")),
        }
    }

    #[allow(deprecated)]
    fn initialize(
        &mut self,
        root_uri: &Uri,
        timeout: Duration,
        init_options: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let params = InitializeParams {
            root_uri: Some(root_uri.clone()),
            initialization_options: init_options,
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    rename: Some(RenameClientCapabilities {
                        dynamic_registration: Some(false),
                        prepare_support: Some(true),
                        ..Default::default()
                    }),
                    references: Some(DynamicRegistrationClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    definition: Some(GotoCapability {
                        dynamic_registration: Some(false),
                        link_support: Some(false),
                    }),
                    implementation: Some(GotoCapability {
                        dynamic_registration: Some(false),
                        link_support: Some(false),
                    }),
                    ..Default::default()
                }),
                // Prefer UTF-8 so tree-sitter byte columns map 1:1; servers
                // without 3.17 support fall back to the UTF-16 default.
                general: Some(GeneralClientCapabilities {
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

        let result = self.request_with_timeout::<request::Initialize>(params, timeout)?;
        self.info = SemanticBackendInfo {
            kind: SemanticBackendKind::Lsp,
            server_name: result.server_info.as_ref().map(|s| s.name.clone()),
            server_version: result
                .server_info
                .and_then(|s| s.version)
                .map(|v| crate::lsp::capabilities::compact_server_version(&v)),
            capabilities: SemanticCapabilities::from_server(&result.capabilities),
            utf8_positions: result.capabilities.position_encoding
                == Some(PositionEncodingKind::UTF8),
        };
        self.send_notification::<notification::Initialized>(InitializedParams {})?;
        self.initialized = true;
        Ok(())
    }

    pub fn info(&self) -> &SemanticBackendInfo {
        &self.info
    }

    /// Fails fast with a clear message when the server did not advertise the
    /// feature, instead of sending a request it cannot answer.
    fn require(&self, supported: bool, method: &str) -> Result<(), String> {
        if supported {
            return Ok(());
        }
        Err(format!(
            "UNSUPPORTED: language server '{}' does not provide {method}",
            self.info.server_name.as_deref().unwrap_or("unknown")
        ))
    }

    /// Brings the server's view of `uri` up to date: `didOpen` once, then
    /// `didChange` (full text) only when the content changed.
    pub fn did_open(&mut self, uri: &Uri, language_id: &str, text: &str) -> Result<(), String> {
        self.check_alive()?;
        let (sync, evicted) = self.documents.sync(uri.as_str(), text);
        if let Some(old) = evicted
            && let Ok(old_uri) = old.parse::<Uri>()
        {
            self.send_notification::<notification::DidCloseTextDocument>(
                DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: old_uri },
                },
            )?;
        }
        match sync {
            DocumentSync::Unchanged => Ok(()),
            DocumentSync::Open => self.send_notification::<notification::DidOpenTextDocument>(
                DidOpenTextDocumentParams {
                    text_document: TextDocumentItem {
                        uri: uri.clone(),
                        language_id: language_id.to_string(),
                        version: 1,
                        text: text.to_string(),
                    },
                },
            ),
            DocumentSync::Change(version) => self
                .send_notification::<notification::DidChangeTextDocument>(
                    DidChangeTextDocumentParams {
                        text_document: VersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version,
                        },
                        content_changes: vec![TextDocumentContentChangeEvent {
                            range: None,
                            range_length: None,
                            text: text.to_string(),
                        }],
                    },
                ),
        }
    }

    pub fn references(&mut self, uri: &Uri, position: Position) -> Result<Vec<Location>, String> {
        self.check_alive()?;
        self.require(self.info.capabilities.references, "textDocument/references")?;
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            context: ReferenceContext {
                include_declaration: true,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let result =
            self.request_with_timeout::<request::References>(params, self.request_timeout)?;
        Ok(result.unwrap_or_default())
    }

    pub fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<GotoDefinitionResponse, String> {
        self.check_alive()?;
        self.require(self.info.capabilities.definition, "textDocument/definition")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let result =
            self.request_with_timeout::<request::GotoDefinition>(params, self.request_timeout)?;
        Ok(result.unwrap_or(GotoDefinitionResponse::Array(vec![])))
    }

    pub fn rename(
        &mut self,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>, String> {
        self.check_alive()?;
        self.require(self.info.capabilities.rename, "textDocument/rename")?;
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };
        self.request_with_timeout::<request::Rename>(params, self.request_timeout)
    }

    pub fn implementations(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<Vec<Location>, String> {
        self.check_alive()?;
        self.require(
            self.info.capabilities.implementations,
            "textDocument/implementation",
        )?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let value = self.request_raw_with_timeout(
            "textDocument/implementation",
            serde_json::to_value(params).unwrap_or_default(),
            self.request_timeout,
        )?;
        match value {
            Some(v) => {
                let locations: Vec<Location> = serde_json::from_value(v).unwrap_or_default();
                Ok(locations)
            }
            None => Ok(vec![]),
        }
    }

    fn request_with_timeout<R: request::Request>(
        &mut self,
        params: R::Params,
        timeout: Duration,
    ) -> Result<R::Result, String>
    where
        R::Params: Serialize,
        R::Result: for<'de> Deserialize<'de>,
    {
        let value = self.request_raw_with_timeout(
            R::METHOD,
            serde_json::to_value(params).map_err(|e| e.to_string())?,
            timeout,
        )?;
        match value {
            Some(v) => serde_json::from_value(v).map_err(|e| format!("Deserialize error: {e}")),
            None => serde_json::from_value(Value::Null).map_err(|e| format!("Null result: {e}")),
        }
    }

    fn request_raw_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Option<Value>, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        self.send_message(&serde_json::to_value(req).map_err(|e| e.to_string())?)?;
        self.read_response(id, timeout)
    }

    fn send_notification<N: notification::Notification>(
        &mut self,
        params: N::Params,
    ) -> Result<(), String>
    where
        N::Params: Serialize,
    {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": serde_json::to_value(params).map_err(|e| e.to_string())?
        });
        self.send_message(&msg)
    }

    fn send_message(&mut self, msg: &Value) -> Result<(), String> {
        let body = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin
            .write_all(header.as_bytes())
            .map_err(|e| format!("Write to LSP server: {e}"))?;
        self.stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("Write to LSP server: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("Flush LSP server: {e}"))?;
        Ok(())
    }

    fn read_response(
        &mut self,
        expected_id: i64,
        timeout: Duration,
    ) -> Result<Option<Value>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "LSP response timeout ({}s) for request id={expected_id}",
                    timeout.as_secs()
                ));
            }

            match self.response_rx.recv_timeout(remaining) {
                Ok(Ok(msg)) => match route_incoming(msg, expected_id) {
                    Incoming::FromServer(reply) => {
                        if let Some(reply) = reply {
                            self.send_message(&reply)?;
                        }
                    }
                    Incoming::Response(result) => return result,
                    Incoming::Other => {}
                },
                Ok(Err(e)) => return Err(format!("LSP reader error: {e}")),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("LSP response timeout ({}s)", timeout.as_secs()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("LSP server connection lost".into());
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        if self.initialized {
            let _ = self.request_raw_with_timeout(
                "shutdown",
                Value::Null,
                Duration::from_secs(SHUTDOWN_TIMEOUT_SECS),
            );
            let _ = self.send_notification::<notification::Exit>(());
            self.initialized = false;
            self.reap(Duration::from_secs(SHUTDOWN_TIMEOUT_SECS));
        } else {
            // Never finished `initialize` (e.g. start-up exceeded its budget):
            // there is no protocol shutdown to wait for — kill it right away,
            // so a failed start costs no extra time on the caller's budget.
            self.reap(Duration::ZERO);
        }
    }

    /// Waits up to `grace` for the server to exit, then kills it. A server that
    /// ignores `exit` (or never finished `initialize`) can therefore neither
    /// block the caller — the idle reaper, `shutdown_all` — nor be left behind.
    fn reap(&mut self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl crate::lsp::backend::LspBackend for LspClient {
    fn open_file(
        &mut self,
        uri: &lsp_types::Uri,
        language_id: &str,
        text: &str,
    ) -> Result<(), String> {
        LspClient::did_open(self, uri, language_id, text)
    }
    fn references(
        &mut self,
        uri: &lsp_types::Uri,
        position: lsp_types::Position,
        _scope: &str,
    ) -> Result<Vec<lsp_types::Location>, String> {
        LspClient::references(self, uri, position)
    }
    fn definition(
        &mut self,
        uri: &lsp_types::Uri,
        position: lsp_types::Position,
    ) -> Result<lsp_types::GotoDefinitionResponse, String> {
        LspClient::definition(self, uri, position)
    }
    fn implementations(
        &mut self,
        uri: &lsp_types::Uri,
        position: lsp_types::Position,
        _scope: &str,
    ) -> Result<Vec<lsp_types::Location>, String> {
        LspClient::implementations(self, uri, position)
    }
    fn rename(
        &mut self,
        uri: &lsp_types::Uri,
        position: lsp_types::Position,
        new_name: &str,
    ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
        LspClient::rename(self, uri, position, new_name)
    }
    /// A server whose stdout closed (exit or crash) is stale; the router evicts
    /// it and starts a fresh one on the next call.
    fn is_stale(&self, _project_root: &str) -> bool {
        !self.alive.load(Ordering::Acquire)
    }
    fn set_request_timeout(&mut self, timeout: Option<Duration>) {
        self.request_timeout = timeout.unwrap_or(Duration::from_secs(REQUEST_TIMEOUT_SECS));
    }
    fn backend_info(&self) -> SemanticBackendInfo {
        self.info.clone()
    }
    // declaration/type_hierarchy/symbols_overview/format/inspections: Default-Err (Backing A).
}

#[cfg(test)]
mod tests {
    use super::{DocumentStore, DocumentSync, MAX_OPEN_DOCUMENTS, is_credential_env};
    use super::{Incoming, route_incoming, server_request_reply};

    #[test]
    fn document_store_opens_once_changes_on_edit_and_stays_bounded() {
        let mut docs = DocumentStore::default();
        assert_eq!(docs.sync("file:///a.rs", "v1"), (DocumentSync::Open, None));
        assert_eq!(
            docs.sync("file:///a.rs", "v1"),
            (DocumentSync::Unchanged, None)
        );
        assert_eq!(
            docs.sync("file:///a.rs", "v2"),
            (DocumentSync::Change(2), None)
        );
        for i in 0..MAX_OPEN_DOCUMENTS - 1 {
            docs.sync(&format!("file:///{i}.rs"), "x");
        }
        let (sync, evicted) = docs.sync("file:///overflow.rs", "x");
        assert_eq!(sync, DocumentSync::Open);
        assert_eq!(evicted.as_deref(), Some("file:///a.rs"), "oldest is closed");
        assert_eq!(
            docs.sync("file:///a.rs", "v2").0,
            DocumentSync::Open,
            "a closed document must be reopened, not changed"
        );
    }

    #[test]
    fn server_requests_get_a_reply_and_are_never_mistaken_for_responses() {
        let config = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "workspace/configuration",
            "params": { "items": [{}, {}] }
        });
        // Same numeric id as the client's in-flight request 1: still routed
        // to the server-request path, never returned as our response.
        let Incoming::FromServer(Some(reply)) = route_incoming(config, 1) else {
            panic!("server request must be answered, not returned as response");
        };
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"], serde_json::json!([null, null]));
        assert_eq!(
            route_incoming(
                serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": 7 }),
                1
            ),
            Incoming::Response(Ok(Some(serde_json::json!(7))))
        );
        assert_eq!(
            route_incoming(
                serde_json::json!({ "jsonrpc": "2.0", "id": 9, "result": 7 }),
                1
            ),
            Incoming::Other,
            "late response to another request is ignored"
        );

        let unknown = serde_json::json!({ "jsonrpc": "2.0", "id": "x", "method": "foo/bar" });
        assert_eq!(
            server_request_reply(&unknown).unwrap()["error"]["code"],
            -32601
        );

        let notification = serde_json::json!({ "jsonrpc": "2.0", "method": "$/progress" });
        assert!(server_request_reply(&notification).is_none());
    }

    #[test]
    fn credential_env_is_scrubbed_but_toolchain_config_is_kept() {
        for scrubbed in [
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "DB_PASSWORD",
            "CARGO_REGISTRIES_CORP_TOKEN",
            "NPM_TOKEN",
            "DOCKER_AUTH_CONFIG",
            "token",
        ] {
            assert!(is_credential_env(scrubbed), "{scrubbed} must be scrubbed");
        }
        for kept in [
            "PATH",
            "HOME",
            "CARGO_HOME",
            "GOPROXY",
            "GOPRIVATE",
            "GOAUTH",
            "SSH_AUTH_SOCK",
            "SSL_CERT_FILE",
            "VIRTUAL_ENV",
            "TOKENIZERS_PARALLELISM",
        ] {
            assert!(!is_credential_env(kept), "{kept} must be kept");
        }
    }
}
