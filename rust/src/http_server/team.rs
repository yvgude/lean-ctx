use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::{
    body::{self, Body},
    extract::{Extension, Json, Query, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use md5::{Digest, Md5};
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientJsonRpcMessage,
        ClientRequest, JsonRpcRequest, NumberOrString, ServerJsonRpcMessage, ServerResult,
    },
    service::{serve_directly, RequestContext, RoleServer},
    transport::{OneshotTransport, StreamableHttpService},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::time::Duration;

use crate::tools::LeanCtxServer;

const WORKSPACE_ARG_KEY: &str = "workspaceId";
const WORKSPACE_HEADER: &str = "x-leanctx-workspace";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamServerConfig {
    pub host: String,
    pub port: u16,
    pub default_workspace_id: String,
    pub workspaces: Vec<TeamWorkspaceConfig>,
    #[serde(default)]
    pub tokens: Vec<TeamTokenConfig>,
    pub audit_log_path: PathBuf,
    #[serde(default)]
    pub disable_host_check: bool,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_max_rps")]
    pub max_rps: u32,
    #[serde(default = "default_rate_burst")]
    pub rate_burst: u32,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default)]
    pub stateful_mode: bool,
    #[serde(default = "default_true")]
    pub json_response: bool,
}

fn default_true() -> bool {
    true
}
fn default_max_body_bytes() -> usize {
    2 * 1024 * 1024
}
fn default_max_concurrency() -> usize {
    32
}
fn default_max_rps() -> u32 {
    50
}
fn default_rate_burst() -> u32 {
    100
}
fn default_request_timeout_ms() -> u64 {
    30_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamWorkspaceConfig {
    pub id: String,
    pub label: Option<String>,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTokenConfig {
    pub id: String,
    /// Stored as lowercase hex of SHA-256(token).
    pub sha256_hex: String,
    pub scopes: Vec<TeamScope>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamScope {
    Search,
    Graph,
    Artifacts,
    Index,
}

impl TeamServerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let cfg: Self =
            serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let s = serde_json::to_string_pretty(self).context("serialize TeamServerConfig")?;
        std::fs::write(path, format!("{s}\n")).with_context(|| format!("write {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.workspaces.is_empty() {
            return Err(anyhow!("team server requires at least 1 workspace"));
        }
        let mut ws_ids = BTreeSet::new();
        for ws in &self.workspaces {
            let id = ws.id.trim();
            if id.is_empty() {
                return Err(anyhow!("workspace id must be non-empty"));
            }
            if !ws_ids.insert(id.to_string()) {
                return Err(anyhow!("duplicate workspace id: {id}"));
            }
            if !ws.root.exists() {
                return Err(anyhow!(
                    "workspace root does not exist: {}",
                    ws.root.display()
                ));
            }
        }
        if !ws_ids.contains(self.default_workspace_id.trim()) {
            return Err(anyhow!(
                "defaultWorkspaceId '{}' not found in workspaces",
                self.default_workspace_id
            ));
        }

        let mut token_ids = BTreeSet::new();
        for t in &self.tokens {
            let id = t.id.trim();
            if id.is_empty() {
                return Err(anyhow!("token id must be non-empty"));
            }
            if !token_ids.insert(id.to_string()) {
                return Err(anyhow!("duplicate token id: {id}"));
            }
            if t.scopes.is_empty() {
                return Err(anyhow!("token '{id}' must have at least 1 scope"));
            }
            parse_sha256_hex(&t.sha256_hex)
                .with_context(|| format!("token '{id}' invalid sha256Hex"))?;
        }

        if let Some(parent) = self.audit_log_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                return Err(anyhow!(
                    "auditLogPath parent does not exist: {}",
                    parent.display()
                ));
            }
        }
        Ok(())
    }

    pub fn validate_for_serve(&self) -> Result<()> {
        self.validate()?;
        if self.tokens.is_empty() {
            return Err(anyhow!("team server requires at least 1 token"));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct TeamAuthContext {
    token_id: String,
    scopes: BTreeSet<TeamScope>,
}

#[derive(Clone)]
struct TeamRequestContext {
    workspace_id: String,
}

#[derive(Clone)]
struct TeamState {
    auth: Arc<Vec<TeamTokenConfig>>,
    engine: Arc<TeamContextEngine>,
    audit: Arc<tokio::sync::Mutex<tokio::fs::File>>,
}

#[derive(Clone)]
struct TeamAppState {
    concurrency: Arc<tokio::sync::Semaphore>,
    rate: Arc<super::RateLimiter>,
    timeout: Duration,
    team: Arc<TeamState>,
    max_body_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallBody {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
    #[serde(default)]
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolsQuery {
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Clone)]
struct TeamCtxServer {
    default_workspace_id: String,
    workspaces: Arc<HashMap<String, LeanCtxServer>>,
    roots: Arc<HashMap<String, String>>,
}

impl TeamCtxServer {
    fn default_server(&self) -> &LeanCtxServer {
        self.workspaces
            .get(&self.default_workspace_id)
            .expect("default workspace")
    }

    fn rewrite_dot_paths(args: &mut Map<String, Value>, root: &str) {
        for k in ["path", "target_directory", "targetDirectory"] {
            let Some(Value::String(s)) = args.get(k) else {
                continue;
            };
            let t = s.trim();
            if t.is_empty() || t == "." {
                args.insert(k.to_string(), Value::String(root.to_string()));
            }
        }
    }

    fn pick_workspace<'a>(
        &'a self,
        args: &mut Map<String, Value>,
    ) -> std::result::Result<&'a LeanCtxServer, rmcp::ErrorData> {
        let ws = args
            .get(WORKSPACE_ARG_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or(self.default_workspace_id.as_str())
            .to_string();
        args.remove(WORKSPACE_ARG_KEY);

        if let Some(root) = self.roots.get(&ws) {
            Self::rewrite_dot_paths(args, root);
        }

        self.workspaces
            .get(&ws)
            .ok_or_else(|| rmcp::ErrorData::invalid_params("unknown workspaceId", None))
    }
}

impl ServerHandler for TeamCtxServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        <LeanCtxServer as ServerHandler>::get_info(self.default_server())
    }

    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::InitializeResult, rmcp::ErrorData> {
        <LeanCtxServer as ServerHandler>::initialize(self.default_server(), request, context).await
    }

    async fn list_tools(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        <LeanCtxServer as ServerHandler>::list_tools(self.default_server(), request, context).await
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let mut args = request.arguments.take().unwrap_or_default();
        let target = self.pick_workspace(&mut args)?;
        request.arguments = Some(args);
        <LeanCtxServer as ServerHandler>::call_tool(target, request, context).await
    }
}

struct TeamContextEngine {
    server: TeamCtxServer,
    next_id: AtomicI64,
}

impl TeamContextEngine {
    fn new(server: TeamCtxServer) -> Self {
        Self {
            server,
            next_id: AtomicI64::new(1),
        }
    }

    fn manifest_value() -> Value {
        crate::core::mcp_manifest::manifest_value()
    }

    async fn call_tool_value(&self, name: &str, arguments: Option<Value>) -> Result<Value> {
        let result = self.call_tool_result(name, arguments).await?;
        serde_json::to_value(result).map_err(|e| anyhow!("serialize CallToolResult: {e}"))
    }

    async fn call_tool_result(
        &self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<CallToolResult> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req_id = NumberOrString::Number(id);

        let args_obj: Map<String, Value> = match arguments {
            None => Map::new(),
            Some(Value::Object(m)) => m,
            Some(other) => {
                return Err(anyhow!(
                    "tool arguments must be a JSON object (got {other})"
                ))
            }
        };

        let params = CallToolRequestParams::new(name.to_string()).with_arguments(args_obj);
        let call: CallToolRequest = CallToolRequest::new(params);
        let client_req = ClientRequest::CallToolRequest(call);
        let msg = ClientJsonRpcMessage::Request(JsonRpcRequest::new(req_id, client_req));

        let (transport, mut rx) = OneshotTransport::<RoleServer>::new(msg);
        let service = serve_directly(self.server.clone(), transport, None);
        tokio::spawn(async move {
            let _ = service.waiting().await;
        });

        let Some(server_msg) = rx.recv().await else {
            return Err(anyhow!("no response from tool call"));
        };

        match server_msg {
            ServerJsonRpcMessage::Response(r) => match r.result {
                ServerResult::CallToolResult(result) => Ok(result),
                other => Err(anyhow!("unexpected server result: {other:?}")),
            },
            ServerJsonRpcMessage::Error(e) => Err(anyhow!("{e:?}")).context("tool call error"),
            ServerJsonRpcMessage::Notification(_) => Err(anyhow!("unexpected notification")),
            ServerJsonRpcMessage::Request(_) => Err(anyhow!("unexpected request")),
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
    String::from_utf8(out).unwrap_or_default()
}

fn parse_sha256_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() != 64 {
        return Err(anyhow!("sha256 hex must be 64 chars"));
    }
    let mut out = Vec::with_capacity(32);
    let bytes = s.as_bytes();
    let to_nibble = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    for i in (0..64).step_by(2) {
        let hi = to_nibble(bytes[i]).ok_or_else(|| anyhow!("invalid hex"))?;
        let lo = to_nibble(bytes[i + 1]).ok_or_else(|| anyhow!("invalid hex"))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn required_scopes(tool_name: &str, args: Option<&Value>) -> Option<BTreeSet<TeamScope>> {
    if matches!(tool_name, "ctx_shell" | "ctx_execute" | "ctx_edit") {
        return None;
    }

    if tool_name == "ctx" {
        let Value::Object(m) = args? else {
            return None;
        };
        let sub = m.get("tool")?.as_str()?.trim();
        if sub.is_empty() {
            return None;
        }
        let canonical = if sub.starts_with("ctx_") {
            sub.to_string()
        } else {
            format!("ctx_{sub}")
        };
        let mut m2 = m.clone();
        m2.remove("tool");
        return required_scopes(&canonical, Some(&Value::Object(m2)));
    }

    let mut s = BTreeSet::new();
    match tool_name {
        // Search scope (read/discovery)
        "ctx_read" | "ctx_multi_read" | "ctx_smart_read" | "ctx_search" | "ctx_tree"
        | "ctx_outline" | "ctx_expand" | "ctx_delta" | "ctx_dedup" | "ctx_prefetch"
        | "ctx_preload" | "ctx_review" | "ctx_response" | "ctx_task" | "ctx_overview" => {
            s.insert(TeamScope::Search);
            Some(s)
        }
        // Pack needs search + graph (it includes impact/graph-derived context)
        "ctx_pack" => {
            s.insert(TeamScope::Search);
            s.insert(TeamScope::Graph);
            Some(s)
        }
        // Graph scope
        "ctx_graph" | "ctx_graph_diagram" | "ctx_impact" | "ctx_callgraph" | "ctx_callers"
        | "ctx_callees" | "ctx_routes" => {
            s.insert(TeamScope::Graph);

            if tool_name == "ctx_graph" {
                let action = args
                    .and_then(|v| v.get("action"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if matches!(
                    action,
                    "index-build"
                        | "index-build-full"
                        | "index-build-background"
                        | "index-build-full-background"
                ) {
                    s.insert(TeamScope::Index);
                }
            }

            Some(s)
        }
        "ctx_semantic_search" => {
            s.insert(TeamScope::Search);
            if args
                .and_then(|v| v.get("artifacts"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                s.insert(TeamScope::Artifacts);
            }
            if args
                .and_then(|v| v.get("action"))
                .and_then(|v| v.as_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("reindex"))
            {
                s.insert(TeamScope::Index);
            }
            Some(s)
        }
        _ => None,
    }
}

async fn team_rate_limit_middleware(
    State(state): State<TeamAppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if !state.rate.allow().await {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    next.run(req).await
}

async fn team_concurrency_middleware(
    State(state): State<TeamAppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let Ok(permit) = state.concurrency.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let resp = next.run(req).await;
    drop(permit);
    resp
}

async fn team_auth_middleware(
    State(state): State<TeamAppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }

    let Some(h) = req.headers().get(header::AUTHORIZATION) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(s) = h.to_str() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(token) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let token_hash = sha256_hex(token.as_bytes());
    let mut matched: Option<TeamTokenConfig> = None;
    for t in state.team.auth.iter() {
        if super::constant_time_eq(token_hash.as_bytes(), t.sha256_hex.as_bytes()) {
            matched = Some(t.clone());
            break;
        }
    }
    let Some(tok) = matched else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let tok_scopes: BTreeSet<TeamScope> = tok.scopes.iter().copied().collect();

    let workspace_id = req
        .headers()
        .get(WORKSPACE_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| state.team.engine.server.default_workspace_id.clone());
    if !state
        .team
        .engine
        .server
        .workspaces
        .contains_key(&workspace_id)
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let workspace_id_for_audit = workspace_id.clone();

    req.extensions_mut().insert(TeamAuthContext {
        token_id: tok.id.clone(),
        scopes: tok_scopes.clone(),
    });
    req.extensions_mut()
        .insert(TeamRequestContext { workspace_id });

    // Tool-level authz for MCP fallback (tools/call).
    let path = req.uri().path().to_string();
    if path != "/v1/tools/call"
        && path != "/v1/tools"
        && path != "/v1/manifest"
        && path != "/health"
    {
        if req.method() != axum::http::Method::POST {
            return next.run(req).await;
        }

        let (parts, body0) = req.into_parts();
        let Ok(bytes) = body::to_bytes(body0, state.max_body_bytes).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };

        let mut allow = true;
        let mut denied_reason: Option<String> = None;
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
            if method.eq_ignore_ascii_case("tools/call") {
                let tool = v
                    .pointer("/params/name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                let args = v.pointer("/params/arguments");
                let req_scopes = required_scopes(tool, args);
                allow = match req_scopes {
                    None => false,
                    Some(reqs) => reqs.is_subset(&tok_scopes),
                };
                if !allow {
                    denied_reason = Some("scope_denied".to_string());
                }
                let _ = audit_write(
                    &state.team.audit,
                    &tok.id,
                    &workspace_id_for_audit,
                    Some(tool),
                    Some(method),
                    allow,
                    denied_reason.as_deref(),
                    args,
                )
                .await;
            }
        }

        if !allow {
            return StatusCode::FORBIDDEN.into_response();
        }

        req = Request::from_parts(parts, Body::from(bytes));
    }

    next.run(req).await
}

async fn audit_write(
    file: &tokio::sync::Mutex<tokio::fs::File>,
    token_id: &str,
    workspace_id: &str,
    tool: Option<&str>,
    method: Option<&str>,
    allowed: bool,
    denied_reason: Option<&str>,
    args: Option<&Value>,
) -> Result<()> {
    let args_hash = args
        .map(|a| {
            let s = a.to_string();
            let mut hasher = Md5::new();
            hasher.update(s.as_bytes());
            format!("{:x}", hasher.finalize())
        })
        .unwrap_or_default();

    let ts = chrono::Local::now().to_rfc3339();
    let rec = json!({
        "ts": ts,
        "tokenId": token_id,
        "workspaceId": workspace_id,
        "tool": tool,
        "method": method,
        "allowed": allowed,
        "deniedReason": denied_reason,
        "argumentsMd5": args_hash,
    });

    let mut guard = file.lock().await;
    guard.write_all(rec.to_string().as_bytes()).await?;
    guard.write_all(b"\n").await?;
    guard.flush().await?;
    Ok(())
}

async fn v1_manifest(State(_state): State<TeamAppState>) -> impl IntoResponse {
    let v = TeamContextEngine::manifest_value();
    (StatusCode::OK, Json(v))
}

async fn v1_tools(
    State(_state): State<TeamAppState>,
    Query(q): Query<ToolsQuery>,
) -> impl IntoResponse {
    let v = TeamContextEngine::manifest_value();
    let tools = v
        .get("tools")
        .and_then(|t| t.get("granular"))
        .cloned()
        .unwrap_or(Value::Array(vec![]));

    let all = tools.as_array().cloned().unwrap_or_default();
    let total = all.len();
    let offset = q.offset.unwrap_or(0).min(total);
    let limit = q.limit.unwrap_or(200).min(500);
    let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();

    (
        StatusCode::OK,
        Json(json!({
            "tools": page,
            "total": total,
            "offset": offset,
            "limit": limit,
        })),
    )
}

async fn v1_tool_call(
    State(state): State<TeamAppState>,
    Extension(auth): Extension<TeamAuthContext>,
    Extension(ctx): Extension<TeamRequestContext>,
    Json(body): Json<ToolCallBody>,
) -> impl IntoResponse {
    let workspace_id = body
        .workspace_id
        .clone()
        .unwrap_or_else(|| ctx.workspace_id.clone());
    if !state
        .team
        .engine
        .server
        .workspaces
        .contains_key(&workspace_id)
    {
        let _ = audit_write(
            &state.team.audit,
            &auth.token_id,
            &workspace_id,
            Some(&body.name),
            Some("/v1/tools/call"),
            false,
            Some("unknown_workspace"),
            body.arguments.as_ref(),
        )
        .await;
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown_workspace" })),
        )
            .into_response();
    }

    let mut args = match body.arguments {
        None => Value::Object(Map::new()),
        Some(Value::Object(m)) => Value::Object(m),
        Some(other) => {
            let _ = audit_write(
                &state.team.audit,
                &auth.token_id,
                &workspace_id,
                Some(&body.name),
                Some("/v1/tools/call"),
                false,
                Some("invalid_arguments"),
                Some(&other),
            )
            .await;
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("tool arguments must be a JSON object (got {other})") })),
            )
                .into_response();
        }
    };

    if let Value::Object(ref mut m) = args {
        m.insert(
            WORKSPACE_ARG_KEY.to_string(),
            Value::String(workspace_id.clone()),
        );
    }

    let required = required_scopes(&body.name, Some(&args));
    let allowed = match required {
        None => false,
        Some(reqs) => reqs.is_subset(&auth.scopes),
    };
    if !allowed {
        let _ = audit_write(
            &state.team.audit,
            &auth.token_id,
            &workspace_id,
            Some(&body.name),
            Some("/v1/tools/call"),
            false,
            Some("scope_denied"),
            Some(&args),
        )
        .await;
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "scope_denied" })),
        )
            .into_response();
    }

    let tool_name = body.name.clone();
    let call = tokio::time::timeout(
        state.timeout,
        state
            .team
            .engine
            .call_tool_value(&tool_name, Some(args.clone())),
    )
    .await;

    match call {
        Ok(Ok(v)) => {
            let _ = audit_write(
                &state.team.audit,
                &auth.token_id,
                &workspace_id,
                Some(&tool_name),
                Some("/v1/tools/call"),
                true,
                None,
                Some(&args),
            )
            .await;
            (StatusCode::OK, Json(json!({ "result": v }))).into_response()
        }
        Ok(Err(e)) => {
            let _ = audit_write(
                &state.team.audit,
                &auth.token_id,
                &workspace_id,
                Some(&tool_name),
                Some("/v1/tools/call"),
                true,
                Some("tool_error"),
                Some(&args),
            )
            .await;
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response()
        }
        Err(_) => {
            let _ = audit_write(
                &state.team.audit,
                &auth.token_id,
                &workspace_id,
                Some(&tool_name),
                Some("/v1/tools/call"),
                true,
                Some("request_timeout"),
                Some(&args),
            )
            .await;
            (
                StatusCode::GATEWAY_TIMEOUT,
                Json(json!({ "error": "request_timeout" })),
            )
                .into_response()
        }
    }
}

fn streamable_http_config(cfg: &TeamServerConfig) -> rmcp::transport::StreamableHttpServerConfig {
    let mut out = rmcp::transport::StreamableHttpServerConfig::default()
        .with_stateful_mode(cfg.stateful_mode)
        .with_json_response(cfg.json_response);

    if cfg.disable_host_check {
        out = out.disable_allowed_hosts();
        return out;
    }
    if !cfg.allowed_hosts.is_empty() {
        out = out.with_allowed_hosts(cfg.allowed_hosts.clone());
        return out;
    }
    let host = cfg.host.trim();
    if host == "127.0.0.1" || host == "localhost" || host == "::1" {
        out.allowed_hosts.push(host.to_string());
    }
    out
}

pub async fn serve_team(cfg: TeamServerConfig) -> Result<()> {
    cfg.validate_for_serve()?;

    let addr: std::net::SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid host/port")?;

    let mut workspaces: HashMap<String, LeanCtxServer> = HashMap::new();
    for ws in &cfg.workspaces {
        let root_s = ws.root.to_string_lossy().to_string();
        workspaces.insert(
            ws.id.clone(),
            LeanCtxServer::new_with_project_root(Some(&root_s)),
        );
    }
    let team_server = TeamCtxServer {
        default_workspace_id: cfg.default_workspace_id.clone(),
        workspaces: Arc::new(workspaces),
        roots: Arc::new(
            cfg.workspaces
                .iter()
                .map(|w| (w.id.clone(), w.root.to_string_lossy().to_string()))
                .collect(),
        ),
    };
    let engine = Arc::new(TeamContextEngine::new(team_server.clone()));

    let audit_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.audit_log_path)
        .await
        .with_context(|| format!("open audit log {}", cfg.audit_log_path.display()))?;

    let team = Arc::new(TeamState {
        auth: Arc::new(cfg.tokens.clone()),
        engine,
        audit: Arc::new(tokio::sync::Mutex::new(audit_file)),
    });

    let state = TeamAppState {
        concurrency: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency.max(1))),
        rate: Arc::new(super::RateLimiter::new(cfg.max_rps, cfg.rate_burst)),
        timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
        team,
        max_body_bytes: cfg.max_body_bytes,
    };

    let service_factory = move || Ok(team_server.clone());
    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        streamable_http_config(&cfg),
    );

    let app = Router::new()
        .route("/health", get(super::health))
        .route("/v1/manifest", get(v1_manifest))
        .route("/v1/tools", get(v1_tools))
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .fallback_service(mcp_http)
        .layer(axum::extract::DefaultBodyLimit::max(cfg.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            team_rate_limit_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            team_concurrency_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            team_auth_middleware,
        ))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;

    tracing::info!(
        "lean-ctx TEAM server listening on http://{addr} (workspaces={}, audit={})",
        cfg.workspaces.len(),
        cfg.audit_log_path.display()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("team http server")?;
    Ok(())
}

pub fn create_token() -> Result<(String, String)> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).with_context(|| "getrandom")?;
    let token = hex_lower(&bytes);
    let hash = sha256_hex(token.as_bytes());
    Ok((token, hash))
}

#[cfg(test)]
mod tests {
    use super::super::RateLimiter;
    use super::*;
    use tower::ServiceExt;

    fn cfg_two(tmp: &tempfile::TempDir) -> TeamServerConfig {
        let ws1 = tmp.path().join("ws1");
        let ws2 = tmp.path().join("ws2");
        std::fs::create_dir_all(&ws1).unwrap();
        std::fs::create_dir_all(&ws2).unwrap();
        std::fs::write(ws1.join("ws1_marker.txt"), "1").unwrap();
        std::fs::write(ws2.join("ws2_marker.txt"), "2").unwrap();

        TeamServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            default_workspace_id: "ws1".to_string(),
            workspaces: vec![
                TeamWorkspaceConfig {
                    id: "ws1".to_string(),
                    label: None,
                    root: ws1,
                },
                TeamWorkspaceConfig {
                    id: "ws2".to_string(),
                    label: None,
                    root: ws2,
                },
            ],
            tokens: vec![TeamTokenConfig {
                id: "t1".to_string(),
                sha256_hex: sha256_hex(b"secret"),
                scopes: vec![TeamScope::Search],
            }],
            audit_log_path: tmp.path().join("audit.jsonl"),
            disable_host_check: true,
            allowed_hosts: vec![],
            max_body_bytes: 2 * 1024 * 1024,
            max_concurrency: 4,
            max_rps: 100,
            rate_burst: 100,
            request_timeout_ms: 30_000,
            stateful_mode: false,
            json_response: true,
        }
    }

    async fn build_app(cfg: TeamServerConfig) -> Router {
        let mut workspaces: HashMap<String, LeanCtxServer> = HashMap::new();
        for ws in &cfg.workspaces {
            let root_s = ws.root.to_string_lossy().to_string();
            workspaces.insert(
                ws.id.clone(),
                LeanCtxServer::new_with_project_root(Some(&root_s)),
            );
        }
        let team_server = TeamCtxServer {
            default_workspace_id: cfg.default_workspace_id.clone(),
            workspaces: Arc::new(workspaces),
            roots: Arc::new(
                cfg.workspaces
                    .iter()
                    .map(|w| (w.id.clone(), w.root.to_string_lossy().to_string()))
                    .collect(),
            ),
        };
        let engine = Arc::new(TeamContextEngine::new(team_server));
        let audit_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.audit_log_path)
            .await
            .unwrap();
        let team = Arc::new(TeamState {
            auth: Arc::new(cfg.tokens.clone()),
            engine,
            audit: Arc::new(tokio::sync::Mutex::new(audit_file)),
        });
        let state = TeamAppState {
            concurrency: Arc::new(tokio::sync::Semaphore::new(4)),
            rate: Arc::new(RateLimiter::new(100, 100)),
            timeout: Duration::from_millis(30_000),
            team,
            max_body_bytes: 2 * 1024 * 1024,
        };

        Router::new()
            .route("/v1/tools/call", axum::routing::post(v1_tool_call))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                team_auth_middleware,
            ))
            .with_state(state)
    }

    #[tokio::test]
    async fn missing_bearer_token_is_401() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_two(&tmp);
        let app = build_app(cfg).await;

        let body = json!({"name":"ctx_tree","arguments":{"path":".","depth":1}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/tools/call")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn workspace_header_routes_tool_call_and_audits() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_two(&tmp);
        let audit_path = cfg.audit_log_path.clone();
        let app = build_app(cfg).await;

        let body = json!({"name":"ctx_tree","arguments":{"path":".","depth":2}}).to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/tools/call")
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer secret")
            .header("x-leanctx-workspace", "ws2")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let all = v.to_string();
        assert!(all.contains("ws2_marker.txt"));
        assert!(!all.contains("ws1_marker.txt"));

        let log = std::fs::read_to_string(&audit_path).unwrap_or_default();
        assert!(log.contains("\"tokenId\":\"t1\""));
        assert!(log.contains("\"workspaceId\":\"ws2\""));
        assert!(log.contains("\"tool\":\"ctx_tree\""));
    }
}
