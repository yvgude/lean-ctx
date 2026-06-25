use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, anyhow};
use axum::{
    Router,
    body::{self, Body},
    extract::{Extension, Json, Query, State},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::sse::{Event as SseEvent, KeepAlive, Sse},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::Stream;
use md5::{Digest, Md5};
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientJsonRpcMessage,
        ClientRequest, JsonRpcRequest, NumberOrString, ServerJsonRpcMessage, ServerResult,
    },
    service::{RequestContext, RoleServer, serve_directly},
    transport::{OneshotTransport, StreamableHttpService},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio::time::Duration;

use crate::tools::LeanCtxServer;

pub mod roles;
pub use roles::TeamRole;

#[cfg(test)]
mod tests;

const WORKSPACE_ARG_KEY: &str = "workspaceId";
const CHANNEL_ARG_KEY: &str = "channelId";
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
    /// Hosted-storage quota in bytes (`storageQuotaBytes` in `team.json`),
    /// rendered per plan by the control plane's provisioning bridge (#282).
    /// Omitted ⇒ the server defaults to the Team tier's 5 GiB; the
    /// `LEANCTX_TEAM_STORAGE_QUOTA_BYTES` env var overrides both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_quota_bytes: Option<u64>,
    /// Slack/Discord/generic webhook for the weekly team-ROI summary
    /// (`roiWebhookUrl` in `team.json`, GL #388). HTTPS only — the server
    /// refuses to start with a plaintext URL. Omitted ⇒ no webhook posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roi_webhook_url: Option<String>,
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
    /// Explicitly granted scopes. May be empty when a [`role`](Self::role) is set.
    #[serde(default)]
    pub scopes: Vec<TeamScope>,
    /// Optional RBAC role (EPIC 13.2). Expands to a scope set that is unioned
    /// with `scopes`. Lets admins grant `viewer`/`member`/`admin`/`owner`
    /// instead of hand-picking scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<roles::TeamRole>,
}

impl TeamTokenConfig {
    /// The effective scopes for this token: explicit scopes ∪ role-derived
    /// scopes. This is what authorization is evaluated against (EPIC 13.2).
    #[must_use]
    pub fn effective_scopes(&self) -> BTreeSet<TeamScope> {
        let mut s: BTreeSet<TeamScope> = self.scopes.iter().copied().collect();
        if let Some(role) = self.role {
            s.extend(role.scopes());
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamScope {
    Search,
    Graph,
    Artifacts,
    Index,
    Events,
    SessionMutations,
    Knowledge,
    Audit,
}

impl TeamScope {
    /// Every scope, used by role expansion (EPIC 13.2) to grant full access.
    #[must_use]
    pub fn all() -> &'static [TeamScope] {
        &[
            TeamScope::Search,
            TeamScope::Graph,
            TeamScope::Artifacts,
            TeamScope::Index,
            TeamScope::Events,
            TeamScope::SessionMutations,
            TeamScope::Knowledge,
            TeamScope::Audit,
        ]
    }
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
            // A token must grant access via explicit scopes and/or a role
            // (EPIC 13.2). An empty effective scope set is a misconfiguration.
            if t.effective_scopes().is_empty() {
                return Err(anyhow!("token '{id}' must have at least 1 scope or a role"));
            }
            parse_sha256_hex(&t.sha256_hex)
                .with_context(|| format!("token '{id}' invalid sha256Hex"))?;
        }

        if let Some(parent) = self.audit_log_path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            return Err(anyhow!(
                "auditLogPath parent does not exist: {}",
                parent.display()
            ));
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
pub struct TeamRequestContext {
    pub workspace_id: String,
}

#[derive(Clone)]
pub struct TeamState {
    auth: Arc<Vec<TeamTokenConfig>>,
    engine: Arc<TeamContextEngine>,
    audit: Arc<tokio::sync::Mutex<tokio::fs::File>>,
    pub savings_store_dir: Arc<tokio::sync::Mutex<std::path::PathBuf>>,
    /// Measurement roots for the billing-plane storage report (GL #463).
    pub storage_roots: super::team_billing::StorageRoots,
    /// 60 s cache for the measured storage report.
    pub storage_cache: Arc<tokio::sync::Mutex<super::team_billing::StorageCache>>,
}

#[derive(Clone)]
pub struct TeamAppState {
    concurrency: Arc<tokio::sync::Semaphore>,
    rate: Arc<super::RateLimiter>,
    timeout: Duration,
    pub team: Arc<TeamState>,
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
    #[serde(default)]
    channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolsQuery {
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    channel_id: Option<String>,
}

#[derive(Clone)]
struct TeamCtxServer {
    default_workspace_id: String,
    roots: Arc<HashMap<String, String>>,
}

impl TeamCtxServer {
    fn default_root(&self) -> &str {
        self.roots
            .get(&self.default_workspace_id)
            .expect("default workspace root")
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

    fn pick_workspace(
        &self,
        args: &mut Map<String, Value>,
    ) -> std::result::Result<(String, String), rmcp::ErrorData> {
        let ws = args
            .get(WORKSPACE_ARG_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or(self.default_workspace_id.as_str())
            .to_string();
        args.remove(WORKSPACE_ARG_KEY);

        let root = self
            .roots
            .get(&ws)
            .cloned()
            .ok_or_else(|| rmcp::ErrorData::invalid_params("unknown workspaceId", None))?;
        Self::rewrite_dot_paths(args, &root);
        Ok((ws, root))
    }

    fn make_server(&self, workspace_id: &str, channel_id: &str) -> LeanCtxServer {
        let root = self
            .roots
            .get(workspace_id)
            .cloned()
            .unwrap_or_else(|| self.default_root().to_string());
        LeanCtxServer::new_shared_with_context(&root, workspace_id, channel_id)
    }
}

impl ServerHandler for TeamCtxServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let s = self.make_server(&self.default_workspace_id, "default");
        <LeanCtxServer as ServerHandler>::get_info(&s)
    }

    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::InitializeResult, rmcp::ErrorData> {
        let s = self.make_server(&self.default_workspace_id, "default");
        <LeanCtxServer as ServerHandler>::initialize(&s, request, context).await
    }

    async fn list_tools(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let s = self.make_server(&self.default_workspace_id, "default");
        <LeanCtxServer as ServerHandler>::list_tools(&s, request, context).await
    }

    async fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, rmcp::ErrorData> {
        let mut args = request.arguments.take().unwrap_or_default();
        let (ws, root) = self.pick_workspace(&mut args)?;
        let channel = args
            .get(CHANNEL_ARG_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        args.remove(CHANNEL_ARG_KEY);
        // Re-apply dot path rewriting against the resolved root.
        Self::rewrite_dot_paths(&mut args, &root);
        request.arguments = Some(args);
        let s = LeanCtxServer::new_shared_with_context(&root, &ws, &channel);
        <LeanCtxServer as ServerHandler>::call_tool(&s, request, context).await
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
                ));
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
        // Search scope (read/discovery/analysis)
        "ctx_read" | "ctx_multi_read" | "ctx_smart_read" | "ctx_search" | "ctx_tree"
        | "ctx_outline" | "ctx_expand" | "ctx_delta" | "ctx_dedup" | "ctx_prefetch"
        | "ctx_preload" | "ctx_review" | "ctx_response" | "ctx_task" | "ctx_overview"
        | "ctx_architecture" | "ctx_benchmark" | "ctx_cost" | "ctx_intent" | "ctx_heatmap"
        | "ctx_gain" | "ctx_analyze" | "ctx_discover_tools" | "ctx_discover" | "ctx_symbol"
        | "ctx_index" | "ctx_metrics" | "ctx_cache" | "ctx_agent" => {
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
        "ctx_graph" | "ctx_impact" | "ctx_callgraph" | "ctx_routes" => {
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
        // Session-mutating tools
        "ctx_session" | "ctx_handoff" | "ctx_workflow" | "ctx_compress" | "ctx_share" => {
            s.insert(TeamScope::SessionMutations);
            Some(s)
        }
        // Knowledge tools
        "ctx_knowledge" | "ctx_knowledge_relations" => {
            s.insert(TeamScope::Knowledge);
            Some(s)
        }
        // Artifact + proof tools
        "ctx_artifacts" | "ctx_proof" | "ctx_verify" => {
            s.insert(TeamScope::Artifacts);
            Some(s)
        }
        _ => None,
    }
}

/// Records latency and server-error outcome of every team API request into
/// the process-global SLO store (GL #391). Runs as the outermost layer so the
/// measured latency matches what a client (or the synthetic probe) observes —
/// auth, rate limiting and the handler itself are all included. `/health` and
/// MCP fallback traffic stay unmeasured: the SLO gate is defined over the
/// `/v1` HTTP surface.
async fn team_slo_middleware(req: Request<Body>, next: Next) -> Response {
    let measured = {
        let p = req.uri().path();
        p.starts_with("/v1/") || p.starts_with("/api/v1/")
    };
    let start = std::time::Instant::now();
    let res = next.run(req).await;
    if measured {
        crate::core::team_slo::global().record_request(
            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            !res.status().is_server_error(),
        );
    }
    res
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
        return super::json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing Authorization header",
        );
    };
    let Ok(s) = h.to_str() else {
        return super::json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "malformed Authorization header",
        );
    };
    let Some(token) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    else {
        return super::json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Authorization must use the Bearer scheme",
        );
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
        return super::json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid bearer token",
        );
    };
    let tok_scopes: BTreeSet<TeamScope> = tok.effective_scopes();

    let workspace_id = req
        .headers()
        .get(WORKSPACE_HEADER)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| state.team.engine.server.default_workspace_id.clone());
    if !state.team.engine.server.roots.contains_key(&workspace_id) {
        return super::json_error(
            StatusCode::BAD_REQUEST,
            "unknown_workspace",
            "unknown workspace",
        );
    }
    let workspace_id_for_audit = workspace_id.clone();

    req.extensions_mut().insert(TeamAuthContext {
        token_id: tok.id.clone(),
        scopes: tok_scopes.clone(),
    });
    req.extensions_mut()
        .insert(TeamRequestContext { workspace_id });

    // Endpoint-level authz (non-tool endpoints).
    let path0 = req.uri().path();
    if path0 == "/v1/events" {
        let allow = tok_scopes.contains(&TeamScope::Events);
        let _ = audit_write(
            &state.team.audit,
            &tok.id,
            &workspace_id_for_audit,
            None,
            Some("events"),
            allow,
            if allow { None } else { Some("scope_denied") },
            None,
        )
        .await;
        if !allow {
            return super::json_error(
                StatusCode::FORBIDDEN,
                "scope_denied",
                "token lacks required scope: events",
            );
        }
    }

    if path0 == "/v1/metrics" {
        let allow = tok_scopes.contains(&TeamScope::Audit);
        let _ = audit_write(
            &state.team.audit,
            &tok.id,
            &workspace_id_for_audit,
            None,
            Some("metrics"),
            allow,
            if allow { None } else { Some("scope_denied") },
            None,
        )
        .await;
        if !allow {
            return super::json_error(
                StatusCode::FORBIDDEN,
                "scope_denied",
                "token lacks required scope: audit",
            );
        }
    }

    // Billing-plane reads (savings roll-up, storage/usage reports) share the
    // audit sensitivity class: owner/admin + the control plane's audit token.
    let audit_gated = match path0 {
        "/v1/savings/summary" => Some("savings_summary"),
        "/v1/storage" => Some("storage"),
        "/v1/usage" => Some("usage"),
        p if p.starts_with("/v1/savings/member/") => Some("savings_member"),
        _ => None,
    };
    if let Some(action) = audit_gated {
        let allow = tok_scopes.contains(&TeamScope::Audit);
        let _ = audit_write(
            &state.team.audit,
            &tok.id,
            &workspace_id_for_audit,
            None,
            Some(action),
            allow,
            if allow { None } else { Some("scope_denied") },
            None,
        )
        .await;
        if !allow {
            return super::json_error(
                StatusCode::FORBIDDEN,
                "scope_denied",
                "token lacks required scope: audit",
            );
        }
    }

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
            return super::json_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "could not read request body",
            );
        };

        let mut allow = false;
        let mut denied_reason: Option<String> = None;
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if v.is_array() {
                denied_reason = Some("batch_requests_not_supported".to_string());
                let _ = audit_write(
                    &state.team.audit,
                    &tok.id,
                    &workspace_id_for_audit,
                    None,
                    None,
                    false,
                    denied_reason.as_deref(),
                    None,
                )
                .await;
            } else {
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
                } else {
                    allow = true;
                }
            }
        }

        if !allow {
            return super::json_error(
                StatusCode::FORBIDDEN,
                "scope_denied",
                "token lacks required scope for this tool",
            );
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
            crate::core::agent_identity::hex_encode(&hasher.finalize())
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

/// Event-level audit entry: records who triggered which Context OS event.
async fn audit_event(
    file: &tokio::sync::Mutex<tokio::fs::File>,
    token_id: &str,
    workspace_id: &str,
    channel_id: &str,
    event_kind: &str,
    actor: Option<&str>,
    event_id: i64,
) -> Result<()> {
    let ts = chrono::Local::now().to_rfc3339();
    let rec = json!({
        "ts": ts,
        "type": "context_event",
        "tokenId": token_id,
        "workspaceId": workspace_id,
        "channelId": channel_id,
        "eventKind": event_kind,
        "actor": actor,
        "eventId": event_id,
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
    if !state.team.engine.server.roots.contains_key(&workspace_id) {
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
        return super::json_error(
            StatusCode::BAD_REQUEST,
            "unknown_workspace",
            "unknown workspace",
        );
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
            return super::json_error(
                StatusCode::BAD_REQUEST,
                "invalid_arguments",
                &format!("tool arguments must be a JSON object (got {other})"),
            );
        }
    };

    if let Value::Object(ref mut m) = args {
        m.insert(
            WORKSPACE_ARG_KEY.to_string(),
            Value::String(workspace_id.clone()),
        );
        if let Some(ch) = body.channel_id.as_deref()
            && !ch.trim().is_empty()
        {
            m.insert(
                CHANNEL_ARG_KEY.to_string(),
                Value::String(ch.trim().to_string()),
            );
        }
    }

    let required = required_scopes(&body.name, Some(&args));
    // Index-mutating calls (anything requiring the Index scope) reset the
    // hosted-index freshness baseline once they succeed (GL #391).
    let mutates_index = required
        .as_ref()
        .is_some_and(|reqs| reqs.contains(&TeamScope::Index));
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
        return super::json_error(
            StatusCode::FORBIDDEN,
            "scope_denied",
            "token lacks required scope for this tool",
        );
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
            if mutates_index {
                crate::core::team_slo::global().record_index_write();
            }
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
            {
                tracing::warn!("team tool call error: {e}");
                super::json_error(
                    StatusCode::BAD_REQUEST,
                    "tool_error",
                    "tool execution failed",
                )
            }
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
            super::json_error(
                StatusCode::GATEWAY_TIMEOUT,
                "request_timeout",
                "tool call timed out",
            )
        }
    }
}

async fn v1_events(
    State(state): State<TeamAppState>,
    Extension(auth): Extension<TeamAuthContext>,
    Extension(ctx): Extension<TeamRequestContext>,
    Query(q): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let ws = ctx.workspace_id;
    let ch = q.channel_id.unwrap_or_else(|| "default".to_string());
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(200).min(1000);

    let _ = audit_event(
        &state.team.audit,
        &auth.token_id,
        &ws,
        &ch,
        "sse_subscribe",
        None,
        since,
    )
    .await;

    let rt = crate::core::context_os::runtime();
    let replay = rt.bus.read(&ws, &ch, since, limit);
    let rx = if let Some(rx) = rt.bus.subscribe(&ws, &ch) {
        rx
    } else {
        tracing::warn!("SSE subscriber limit reached for {ws}/{ch}");
        let (_, rx) = tokio::sync::broadcast::channel::<crate::core::context_os::ContextEventV1>(1);
        rx
    };
    rt.metrics.record_sse_connect();
    rt.metrics.record_events_replayed(replay.len() as u64);
    rt.metrics.record_workspace_active(&ws);

    let bus = rt.bus.clone();
    let metrics = rt.metrics.clone();
    let pending: std::collections::VecDeque<crate::core::context_os::ContextEventV1> =
        replay.into();

    use crate::core::context_os::{RedactionLevel, redact_event_payload};
    let redaction = RedactionLevel::RefsOnly;

    let stream = futures::stream::unfold(
        (
            pending,
            rx,
            ws.clone(),
            ch.clone(),
            since,
            redaction,
            bus,
            metrics,
        ),
        |(mut pending, mut rx, ws, ch, mut last_id, redaction, bus, metrics)| async move {
            if let Some(mut ev) = pending.pop_front() {
                last_id = ev.id;
                redact_event_payload(&mut ev, redaction);
                let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                let evt = SseEvent::default()
                    .id(ev.id.to_string())
                    .event(ev.kind)
                    .data(data);
                return Some((
                    Ok(evt),
                    (pending, rx, ws, ch, last_id, redaction, bus, metrics),
                ));
            }

            loop {
                match rx.recv().await {
                    Ok(mut ev) if ev.id > last_id => {
                        last_id = ev.id;
                        redact_event_payload(&mut ev, redaction);
                        let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
                        let evt = SseEvent::default()
                            .id(ev.id.to_string())
                            .event(ev.kind)
                            .data(data);
                        return Some((
                            Ok(evt),
                            (pending, rx, ws, ch, last_id, redaction, bus, metrics),
                        ));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let missed = bus.read(&ws, &ch, last_id, skipped as usize);
                        metrics.record_events_replayed(missed.len() as u64);
                        for ev in missed {
                            last_id = last_id.max(ev.id);
                            pending.push_back(ev);
                        }
                    }
                }
            }
        },
    );

    let metrics_ref = rt.metrics.clone();
    let guarded = super::SseDisconnectGuard {
        inner: Box::pin(stream),
        metrics: metrics_ref,
    };

    Sse::new(guarded).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[derive(Debug, Deserialize)]
struct MetricsQuery {
    /// `?format=prometheus` switches to text exposition for scrape agents
    /// (Datadog openmetrics check, Prometheus, Grafana Alloy …).
    #[serde(default)]
    format: Option<String>,
}

async fn v1_team_metrics(
    State(_state): State<TeamAppState>,
    Query(q): Query<MetricsQuery>,
) -> Response {
    let slo = crate::core::team_slo::global().snapshot();

    if q.format.as_deref() == Some("prometheus") {
        return (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4",
            )],
            slo.to_prometheus(),
        )
            .into_response();
    }

    let rt = crate::core::context_os::runtime();
    let snap = rt.metrics.snapshot();
    let mut v = serde_json::to_value(snap).unwrap_or_default();
    if let Value::Object(ref mut m) = v {
        m.insert(
            "slo".to_string(),
            serde_json::to_value(&slo).unwrap_or_default(),
        );
    }
    (StatusCode::OK, Json(v)).into_response()
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

    let team_server = TeamCtxServer {
        default_workspace_id: cfg.default_workspace_id.clone(),
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

    let savings_dir = cfg
        .audit_log_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("savings");
    let workspace_roots: Vec<(String, std::path::PathBuf)> = cfg
        .workspaces
        .iter()
        .map(|w| (w.id.clone(), w.root.clone()))
        .collect();
    let team = Arc::new(TeamState {
        auth: Arc::new(cfg.tokens.clone()),
        engine,
        audit: Arc::new(tokio::sync::Mutex::new(audit_file)),
        savings_store_dir: Arc::new(tokio::sync::Mutex::new(savings_dir)),
        storage_roots: super::team_billing::storage_roots_from_config(
            &cfg.audit_log_path,
            &workspace_roots,
            cfg.storage_quota_bytes,
        ),
        storage_cache: Arc::new(tokio::sync::Mutex::new(
            super::team_billing::StorageCache::default(),
        )),
    });

    let state = TeamAppState {
        concurrency: Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrency.max(1))),
        rate: Arc::new(super::RateLimiter::new(cfg.max_rps, cfg.rate_burst)),
        timeout: Duration::from_millis(cfg.request_timeout_ms.max(1)),
        team,
        max_body_bytes: cfg.max_body_bytes,
    };

    let service_factory =
        move || -> std::result::Result<TeamCtxServer, std::io::Error> { Ok(team_server.clone()) };
    let mcp_http = StreamableHttpService::new(
        service_factory,
        Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        streamable_http_config(&cfg),
    );

    // Weekly team-ROI webhook (GL #388): validated at boot so a bad URL is a
    // loud startup error, not a silent weekly no-op.
    if let Some(url) = &cfg.roi_webhook_url {
        super::roi_webhook::validate_webhook_url(url)
            .map_err(|e| anyhow!("invalid roiWebhookUrl in team config: {e}"))?;
        let _ = super::roi_webhook::spawn_weekly_roi_webhook(state.clone(), url.clone());
        tracing::info!("team ROI webhook enabled (weekly)");
    }

    let app = Router::new()
        .route("/health", get(super::health))
        .route("/v1/manifest", get(v1_manifest))
        .route("/v1/tools", get(v1_tools))
        .route("/v1/tools/call", axum::routing::post(v1_tool_call))
        .route("/v1/events", get(v1_events))
        .route(
            "/v1/context/summary",
            get(super::context_views::v1_context_summary),
        )
        .route(
            "/v1/events/search",
            get(super::context_views::v1_events_search),
        )
        .route(
            "/v1/events/lineage",
            get(super::context_views::v1_event_lineage),
        )
        .route("/v1/metrics", get(v1_team_metrics))
        .route(
            "/v1/savings/summary",
            get(super::savings_summary::v1_savings_summary),
        )
        .route(
            "/v1/savings/member/{signer}",
            get(super::savings_summary::v1_savings_member),
        )
        .route("/v1/storage", get(super::team_billing::v1_storage))
        .route("/v1/usage", get(super::team_billing::v1_usage))
        .route(
            "/api/v1/savings/ingest",
            axum::routing::post(super::savings_ingest::v1_savings_ingest),
        )
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
        // Outermost: SLO measurement sees the full client-observed latency.
        .layer(middleware::from_fn(team_slo_middleware))
        .with_state(state);

    crate::core::team_slo::global().mark_started();

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
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("getrandom: {e}"))?;
    let token = hex_lower(&bytes);
    let hash = sha256_hex(token.as_bytes());
    Ok((token, hash))
}
