//! Wire types for the lean-ctx `/v1` contract.
//!
//! These mirror the TypeScript SDK (`cookbook/sdk/src/types.ts`) and the server
//! structs so a Rust embedder sees the same shapes. Open-ended documents
//! (`manifest`, `capabilities`, `openapi.json`) are returned as
//! [`serde_json::Value`] so the client never breaks when the server adds keys.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Response of `GET /v1/tools` (paginated tool list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResponse {
    /// The tool descriptors for this page (opaque per-tool JSON).
    #[serde(default)]
    pub tools: Vec<Value>,
    /// Total number of tools available across all pages.
    #[serde(default)]
    pub total: u64,
    /// Offset this page started at.
    #[serde(default)]
    pub offset: u64,
    /// Page size requested.
    #[serde(default)]
    pub limit: u64,
}

/// Response envelope of `POST /v1/tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResponse {
    /// The raw MCP tool result (content blocks + optional structured content).
    pub result: Value,
}

/// A single context event delivered over `GET /v1/events` (SSE).
///
/// The wire format is camelCase; field names here use snake_case with serde
/// renames. `consistency_level` is kept as a `String` (not an enum) so new
/// levels never fail deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEventV1 {
    /// Monotonic event id within the workspace/channel stream.
    pub id: i64,
    /// Owning workspace.
    pub workspace_id: String,
    /// Owning channel.
    pub channel_id: String,
    /// Event kind (e.g. `tool_call`, `session_update`, `graph_build`).
    pub kind: String,
    /// Optional actor that produced the event.
    #[serde(default)]
    pub actor: Option<String>,
    /// RFC 3339 timestamp string.
    pub timestamp: String,
    /// Per-stream version counter.
    #[serde(default)]
    pub version: i64,
    /// Causal parent event id, when this event was emitted in a chain.
    #[serde(default)]
    pub parent_id: Option<i64>,
    /// Consistency level string (`local` | `eventual` | `strong`, forward-compatible).
    #[serde(default)]
    pub consistency_level: String,
    /// Event payload (redacted by default unless the bearer carries Audit scope).
    #[serde(default)]
    pub payload: Value,
    /// Optional targeted-agent allow-list for selective visibility.
    #[serde(default)]
    pub target_agents: Option<Vec<String>>,
}

/// Optional per-call workspace/channel override for tool calls and event streams.
#[derive(Debug, Clone, Default)]
pub struct CallContext {
    /// Override the client's default workspace for this call.
    pub workspace_id: Option<String>,
    /// Override the client's default channel for this call.
    pub channel_id: Option<String>,
}

impl CallContext {
    /// A context overriding only the workspace.
    #[must_use]
    pub fn workspace(workspace_id: impl Into<String>) -> Self {
        Self {
            workspace_id: Some(workspace_id.into()),
            channel_id: None,
        }
    }
}
