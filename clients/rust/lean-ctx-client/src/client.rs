//! The blocking [`LeanCtxClient`] over the `/v1` HTTP contract.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{HttpError, LeanCtxError, Result};
use crate::events::EventStream;
use crate::tool_text::tool_result_to_text;
use crate::types::{CallContext, ListToolsResponse, ToolCallResponse};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Query parameters for [`LeanCtxClient::subscribe_events`].
#[derive(Debug, Clone, Default)]
pub struct EventQuery {
    /// Workspace to stream (defaults to the client's workspace).
    pub workspace_id: Option<String>,
    /// Channel to stream (defaults to the client's channel).
    pub channel_id: Option<String>,
    /// Replay events with id strictly greater than this (SSE `since`).
    pub since: Option<i64>,
    /// Cap on the number of replayed events.
    pub limit: Option<u64>,
}

/// A thin, blocking client for a lean-ctx server's `/v1` surface.
///
/// Construct via [`LeanCtxClient::new`] (anonymous) or
/// [`LeanCtxClient::builder`] (bearer token, default workspace/channel,
/// timeout). All methods are blocking; wrap calls in your own thread pool or
/// `spawn_blocking` when used from async code.
#[derive(Debug, Clone)]
pub struct LeanCtxClient {
    base_url: String,
    bearer_token: Option<String>,
    workspace_id: Option<String>,
    channel_id: Option<String>,
    agent: ureq::Agent,
}

impl LeanCtxClient {
    /// Create a client for `base_url` with no auth and default timeout.
    ///
    /// # Errors
    /// Returns [`LeanCtxError::Config`] when `base_url` is empty.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Self::builder(base_url).build()
    }

    /// Start a [`LeanCtxClientBuilder`] for `base_url`.
    pub fn builder(base_url: impl AsRef<str>) -> LeanCtxClientBuilder {
        LeanCtxClientBuilder::new(base_url)
    }

    /// The normalized base URL (no trailing slash).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /health` — liveness probe, returns the body text (`ok`).
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure or non-2xx status.
    pub fn health(&self) -> Result<String> {
        let url = self.url("/health");
        let req = self.with_auth(self.agent.get(&url).set("Accept", "text/plain"), None);
        match req.call() {
            Ok(resp) => resp.into_string().map_err(|e| decode("GET", url, &e)),
            Err(e) => Err(self.map_err("GET", url, e)),
        }
    }

    /// `GET /v1/manifest` — the full MCP manifest as raw JSON.
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure, non-2xx status, or decode failure.
    pub fn manifest(&self) -> Result<Value> {
        self.get_json("/v1/manifest")
    }

    /// `GET /v1/capabilities` — instance capability discovery document.
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure, non-2xx status, or decode failure.
    pub fn capabilities(&self) -> Result<Value> {
        self.get_json("/v1/capabilities")
    }

    /// `GET /v1/openapi.json` — the OpenAPI 3.0 spec for this server's surface.
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure, non-2xx status, or decode failure.
    pub fn openapi(&self) -> Result<Value> {
        self.get_json("/v1/openapi.json")
    }

    /// `GET /v1/tools` — a paginated page of tool descriptors.
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure, non-2xx status, or decode failure.
    pub fn list_tools(&self, offset: Option<u64>, limit: Option<u64>) -> Result<ListToolsResponse> {
        let mut q = Vec::new();
        if let Some(o) = offset {
            q.push(("offset".to_string(), o.to_string()));
        }
        if let Some(l) = limit {
            q.push(("limit".to_string(), l.to_string()));
        }
        self.get_json(&format!("/v1/tools{}", encode_query(&q)))
    }

    /// `POST /v1/tools/call` — execute a tool, returning its raw MCP result.
    ///
    /// `args` must be a JSON object when present. `ctx` overrides the client's
    /// default workspace/channel for this call only.
    ///
    /// # Errors
    /// [`LeanCtxError::Config`] when `args` is not an object;
    /// otherwise [`LeanCtxError`] on transport failure, non-2xx status, or
    /// decode failure.
    pub fn call_tool(
        &self,
        name: &str,
        args: Option<Value>,
        ctx: Option<&CallContext>,
    ) -> Result<Value> {
        let mut body = serde_json::Map::new();
        body.insert("name".to_string(), Value::String(name.to_string()));
        if let Some(a) = args {
            if !a.is_object() {
                return Err(LeanCtxError::Config(
                    "tool arguments must be a JSON object".to_string(),
                ));
            }
            body.insert("arguments".to_string(), a);
        }

        let ws = ctx
            .and_then(|c| c.workspace_id.clone())
            .or_else(|| self.workspace_id.clone());
        let ch = ctx
            .and_then(|c| c.channel_id.clone())
            .or_else(|| self.channel_id.clone());
        if let Some(w) = &ws {
            body.insert("workspaceId".to_string(), Value::String(w.clone()));
        }
        if let Some(c) = &ch {
            body.insert("channelId".to_string(), Value::String(c.clone()));
        }

        let url = self.url("/v1/tools/call");
        let req = self.with_auth(
            self.agent.post(&url).set("Accept", "application/json"),
            ws.as_deref(),
        );
        match req.send_json(Value::Object(body)) {
            Ok(resp) => {
                let parsed: ToolCallResponse =
                    resp.into_json().map_err(|e| decode("POST", url, &e))?;
                Ok(parsed.result)
            }
            Err(e) => Err(self.map_err("POST", url, e)),
        }
    }

    /// Like [`LeanCtxClient::call_tool`] but flattens the MCP result to text.
    ///
    /// # Errors
    /// Same as [`LeanCtxClient::call_tool`].
    pub fn call_tool_text(
        &self,
        name: &str,
        args: Option<Value>,
        ctx: Option<&CallContext>,
    ) -> Result<String> {
        let result = self.call_tool(name, args, ctx)?;
        Ok(tool_result_to_text(&result))
    }

    /// `GET /v1/events` — open a blocking SSE stream of context events.
    ///
    /// # Errors
    /// [`LeanCtxError`] on transport failure or non-2xx status while opening the
    /// stream. Per-event I/O errors surface during iteration.
    pub fn subscribe_events(&self, params: &EventQuery) -> Result<EventStream> {
        let ws = params
            .workspace_id
            .clone()
            .or_else(|| self.workspace_id.clone());
        let ch = params
            .channel_id
            .clone()
            .or_else(|| self.channel_id.clone());

        let mut q = Vec::new();
        if let Some(w) = &ws {
            q.push(("workspaceId".to_string(), w.clone()));
        }
        if let Some(c) = &ch {
            q.push(("channelId".to_string(), c.clone()));
        }
        if let Some(s) = params.since {
            q.push(("since".to_string(), s.to_string()));
        }
        if let Some(l) = params.limit {
            q.push(("limit".to_string(), l.to_string()));
        }

        let url = self.url(&format!("/v1/events{}", encode_query(&q)));
        let req = self.with_auth(
            self.agent.get(&url).set("Accept", "text/event-stream"),
            ws.as_deref(),
        );
        match req.call() {
            Ok(resp) => Ok(EventStream::new(resp.into_reader())),
            Err(e) => Err(self.map_err("GET", url, e)),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn with_auth(&self, mut req: ureq::Request, workspace: Option<&str>) -> ureq::Request {
        if let Some(token) = &self.bearer_token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        if let Some(ws) = workspace {
            req = req.set("x-leanctx-workspace", ws);
        }
        req
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.url(path);
        let req = self.with_auth(self.agent.get(&url).set("Accept", "application/json"), None);
        match req.call() {
            Ok(resp) => resp.into_json::<T>().map_err(|e| decode("GET", url, &e)),
            Err(e) => Err(self.map_err("GET", url, e)),
        }
    }

    fn map_err(&self, method: &str, url: String, err: ureq::Error) -> LeanCtxError {
        match err {
            ureq::Error::Status(status, resp) => http_error(method, url, status, resp),
            ureq::Error::Transport(t) => LeanCtxError::Transport {
                method: method.to_string(),
                url,
                message: t.to_string(),
            },
        }
    }
}

/// Builder for [`LeanCtxClient`].
#[derive(Debug, Clone)]
pub struct LeanCtxClientBuilder {
    base_url: String,
    bearer_token: Option<String>,
    workspace_id: Option<String>,
    channel_id: Option<String>,
    timeout: Option<Duration>,
}

impl LeanCtxClientBuilder {
    fn new(base_url: impl AsRef<str>) -> Self {
        Self {
            base_url: base_url.as_ref().to_string(),
            bearer_token: None,
            workspace_id: None,
            channel_id: None,
            timeout: None,
        }
    }

    /// Set the bearer token sent as `Authorization: Bearer …`.
    #[must_use]
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        let t = token.into();
        self.bearer_token = if t.trim().is_empty() { None } else { Some(t) };
        self
    }

    /// Set the default workspace applied to tool calls and event streams.
    #[must_use]
    pub fn workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        let w = workspace_id.into();
        self.workspace_id = if w.trim().is_empty() { None } else { Some(w) };
        self
    }

    /// Set the default channel applied to tool calls and event streams.
    #[must_use]
    pub fn channel_id(mut self, channel_id: impl Into<String>) -> Self {
        let c = channel_id.into();
        self.channel_id = if c.trim().is_empty() { None } else { Some(c) };
        self
    }

    /// Override the per-request timeout (default 30s).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the client.
    ///
    /// # Errors
    /// Returns [`LeanCtxError::Config`] when the base URL is empty.
    pub fn build(self) -> Result<LeanCtxClient> {
        let base_url = normalize_base_url(&self.base_url)?;
        let timeout = self.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout(timeout)
            .build();
        Ok(LeanCtxClient {
            base_url,
            bearer_token: self.bearer_token,
            workspace_id: self.workspace_id,
            channel_id: self.channel_id,
            agent,
        })
    }
}

fn normalize_base_url(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(LeanCtxError::Config("base_url is required".to_string()));
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn decode(method: &str, url: String, err: &std::io::Error) -> LeanCtxError {
    LeanCtxError::Decode {
        method: method.to_string(),
        url,
        message: err.to_string(),
    }
}

fn http_error(method: &str, url: String, status: u16, resp: ureq::Response) -> LeanCtxError {
    let content_type = resp.content_type().to_string();
    let mut message = format!("HTTP {status} {method} {url}");
    let mut error_code = None;
    let mut body = None;

    if content_type.contains("application/json") {
        if let Ok(v) = resp.into_json::<Value>() {
            if let Some(s) = v.get("error").and_then(Value::as_str) {
                let s = s.trim();
                if !s.is_empty() {
                    message = s.to_string();
                }
            }
            if let Some(c) = v.get("error_code").and_then(Value::as_str) {
                let c = c.trim();
                if !c.is_empty() {
                    error_code = Some(c.to_string());
                }
            }
            body = Some(v);
        }
    } else if let Ok(text) = resp.into_string() {
        let t = text.trim();
        if !t.is_empty() {
            message = t.to_string();
        }
        body = Some(Value::String(text));
    }

    LeanCtxError::http(HttpError {
        status,
        method: method.to_string(),
        url,
        message,
        error_code,
        body,
    })
}

fn encode_query(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let mut out = String::from("?");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_rejects_base_url() {
        assert_eq!(
            normalize_base_url("http://localhost:7777/").unwrap(),
            "http://localhost:7777"
        );
        assert!(normalize_base_url("   ").is_err());
    }

    #[test]
    fn builder_blanks_become_none() {
        let c = LeanCtxClient::builder("http://x")
            .bearer_token("  ")
            .workspace_id("")
            .build()
            .unwrap();
        assert!(c.bearer_token.is_none());
        assert!(c.workspace_id.is_none());
        assert_eq!(c.base_url(), "http://x");
    }

    #[test]
    fn query_is_percent_encoded() {
        let q = encode_query(&[("workspaceId".into(), "a b/c".into())]);
        assert_eq!(q, "?workspaceId=a%20b%2Fc");
        assert_eq!(encode_query(&[]), "");
    }
}
