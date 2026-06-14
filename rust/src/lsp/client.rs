#![allow(clippy::wildcard_imports, clippy::default_trait_access)]

use lsp_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::config::LspServerConfig;

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
}

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    response_rx: Receiver<Result<Value, String>>,
    next_id: AtomicI64,
    initialized: bool,
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

fn spawn_reader(stdout: ChildStdout) -> Receiver<Result<Value, String>> {
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
    pub fn start(config: &LspServerConfig, root_uri: &Uri) -> Result<Self, String> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start LSP server '{}': {e}", config.command))?;

        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;
        let response_rx = spawn_reader(stdout);

        let mut client = Self {
            child,
            stdin,
            response_rx,
            next_id: AtomicI64::new(1),
            initialized: false,
        };

        client.initialize(root_uri)?;
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
    fn initialize(&mut self, root_uri: &Uri) -> Result<(), String> {
        let params = InitializeParams {
            root_uri: Some(root_uri.clone()),
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
                ..Default::default()
            },
            ..Default::default()
        };

        let _result = self.request_with_timeout::<request::Initialize>(
            params,
            Duration::from_secs(INIT_TIMEOUT_SECS),
        )?;
        self.send_notification::<notification::Initialized>(InitializedParams {})?;
        self.initialized = true;
        Ok(())
    }

    pub fn did_open(&mut self, uri: &Uri, language_id: &str, text: &str) -> Result<(), String> {
        self.check_alive()?;
        self.send_notification::<notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
    }

    pub fn references(&mut self, uri: &Uri, position: Position) -> Result<Vec<Location>, String> {
        self.check_alive()?;
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
        let result = self.request_with_timeout::<request::References>(
            params,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )?;
        Ok(result.unwrap_or_default())
    }

    pub fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<GotoDefinitionResponse, String> {
        self.check_alive()?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let result = self.request_with_timeout::<request::GotoDefinition>(
            params,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )?;
        Ok(result.unwrap_or(GotoDefinitionResponse::Array(vec![])))
    }

    pub fn rename(
        &mut self,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>, String> {
        self.check_alive()?;
        let params = RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                position,
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };
        self.request_with_timeout::<request::Rename>(
            params,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
    }

    pub fn implementations(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<Vec<Location>, String> {
        self.check_alive()?;
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
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
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

    fn read_response(&self, expected_id: i64, timeout: Duration) -> Result<Option<Value>, String> {
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
                Ok(Ok(msg)) => {
                    if msg.get("id").and_then(Value::as_i64) == Some(expected_id) {
                        let resp: JsonRpcResponse =
                            serde_json::from_value(msg).map_err(|e| e.to_string())?;
                        if let Some(err) = resp.error {
                            return Err(format!("LSP error: {}", err.message));
                        }
                        return Ok(resp.result);
                    }
                }
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
        let _ = self.request_raw_with_timeout(
            "shutdown",
            Value::Null,
            Duration::from_secs(SHUTDOWN_TIMEOUT_SECS),
        );
        let _ = self.send_notification::<notification::Exit>(());
        let _ = self.child.wait();
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        if self.initialized {
            self.shutdown();
        }
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
    // declaration/type_hierarchy/symbols_overview/format/inspections: Default-Err (Backing A).
}
