use std::collections::{HashMap, HashSet};

use axum::Extension;
use axum::{Json, extract::Query, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::core::context_os::redaction::{RedactionLevel, redact_event_payload};
use crate::core::context_os::{ContextEventKindV1, ContextEventV1};

use super::team::TeamRequestContext;

/// When running behind the team server, the workspace is bound to the
/// authenticated token's header. The query parameter is ignored.
/// In standalone mode (no `TeamRequestContext`), the query parameter is used.
fn resolve_workspace(query_ws: Option<String>, team_ctx: Option<&TeamRequestContext>) -> String {
    if let Some(ctx) = team_ctx {
        return ctx.workspace_id.clone();
    }
    query_ws.unwrap_or_else(|| "default".to_string())
}

#[derive(Deserialize)]
pub struct SummaryQuery {
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
    #[serde(rename = "channelId")]
    pub channel_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSummary {
    pub workspace_id: String,
    pub channel_id: String,
    pub total_events: usize,
    pub latest_version: i64,
    pub active_agents: Vec<String>,
    pub recent_decisions: Vec<DecisionSummary>,
    pub knowledge_delta: Vec<KnowledgeDelta>,
    pub conflict_alerts: Vec<ConflictAlert>,
    pub event_counts_by_kind: HashMap<String, usize>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSummary {
    pub agent: String,
    pub tool: String,
    pub action: Option<String>,
    pub reasoning: Option<String>,
    pub timestamp: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDelta {
    pub category: String,
    pub key: String,
    pub agent: String,
    pub timestamp: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictAlert {
    pub category: String,
    pub key: String,
    pub agents: Vec<String>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
    #[serde(rename = "channelId")]
    pub channel_id: Option<String>,
    pub limit: Option<usize>,
}

pub async fn v1_events_search(
    team_ctx: Option<Extension<TeamRequestContext>>,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    let ws = resolve_workspace(q.workspace_id, team_ctx.as_ref().map(|e| &e.0));
    let limit = q.limit.unwrap_or(20).min(100);

    let rt = crate::core::context_os::runtime();
    let mut results = rt.bus.search(&ws, q.channel_id.as_deref(), &q.q, limit);
    for ev in &mut results {
        redact_event_payload(ev, RedactionLevel::Summary);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "query": q.q,
            "workspaceId": ws,
            "channelId": q.channel_id,
            "results": results,
            "count": results.len(),
        })),
    )
}

#[derive(Deserialize)]
pub struct LineageQuery {
    pub id: i64,
    pub depth: Option<usize>,
    #[serde(rename = "workspaceId")]
    pub workspace_id: Option<String>,
}

pub async fn v1_event_lineage(
    team_ctx: Option<Extension<TeamRequestContext>>,
    Query(q): Query<LineageQuery>,
) -> impl IntoResponse {
    let ws = resolve_workspace(q.workspace_id.clone(), team_ctx.as_ref().map(|e| &e.0));
    let depth = q.depth.unwrap_or(20).min(50);

    let rt = crate::core::context_os::runtime();
    let mut chain = rt.bus.lineage(q.id, &ws, depth);
    for ev in &mut chain {
        redact_event_payload(ev, RedactionLevel::Summary);
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "eventId": q.id,
            "chain": chain,
            "depth": chain.len(),
        })),
    )
}

pub async fn v1_context_summary(
    team_ctx: Option<Extension<TeamRequestContext>>,
    Query(q): Query<SummaryQuery>,
) -> impl IntoResponse {
    let ws = resolve_workspace(q.workspace_id, team_ctx.as_ref().map(|e| &e.0));
    let ch = q.channel_id.unwrap_or_else(|| "default".to_string());
    let limit = q.limit.unwrap_or(100).min(500);

    let rt = crate::core::context_os::runtime();
    let mut events = rt.bus.read(&ws, &ch, 0, limit);
    for ev in &mut events {
        redact_event_payload(ev, RedactionLevel::Summary);
    }

    let summary = build_summary(&ws, &ch, &events);
    (
        StatusCode::OK,
        Json(serde_json::to_value(summary).unwrap_or_default()),
    )
}

fn build_summary(ws: &str, ch: &str, events: &[ContextEventV1]) -> ContextSummary {
    let mut agents: HashSet<String> = HashSet::new();
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut decisions = Vec::new();
    let mut knowledge_deltas = Vec::new();
    let mut latest_version: i64 = 0;

    // Track knowledge writes per category/key for conflict detection.
    let mut knowledge_writers: HashMap<(String, String), HashSet<String>> = HashMap::new();

    for ev in events {
        if let Some(ref actor) = ev.actor {
            agents.insert(actor.clone());
        }
        *kind_counts.entry(ev.kind.clone()).or_insert(0) += 1;
        latest_version = latest_version.max(ev.version);

        let p = &ev.payload;
        let tool = p
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let action = p.get("action").and_then(|v| v.as_str()).map(String::from);
        let reasoning = {
            let mut payload_clone = p.clone();
            crate::core::context_os::redact_payload_value(
                &mut payload_clone,
                crate::core::context_os::RedactionLevel::Summary,
            );
            payload_clone
                .get("reasoning")
                .and_then(|v| v.as_str())
                .map(String::from)
        };

        if ev.kind == ContextEventKindV1::SessionMutated.as_str()
            || ev.kind == ContextEventKindV1::KnowledgeRemembered.as_str()
        {
            decisions.push(DecisionSummary {
                agent: ev.actor.clone().unwrap_or_default(),
                tool: tool.clone(),
                action: action.clone(),
                reasoning,
                timestamp: ev.timestamp.to_rfc3339(),
            });
        }

        if ev.kind == ContextEventKindV1::KnowledgeRemembered.as_str() {
            let cat = p
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let key = p
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            knowledge_deltas.push(KnowledgeDelta {
                category: cat.clone(),
                key: key.clone(),
                agent: ev.actor.clone().unwrap_or_default(),
                timestamp: ev.timestamp.to_rfc3339(),
            });

            if let Some(ref actor) = ev.actor {
                knowledge_writers
                    .entry((cat, key))
                    .or_default()
                    .insert(actor.clone());
            }
        }
    }

    let conflict_alerts: Vec<ConflictAlert> = knowledge_writers
        .into_iter()
        .filter(|(_, writers)| writers.len() > 1)
        .map(|((cat, key), writers)| ConflictAlert {
            category: cat,
            key,
            agents: writers.into_iter().collect(),
        })
        .collect();

    let recent_limit = 10;
    let decisions: Vec<_> = if decisions.len() > recent_limit {
        decisions[decisions.len() - recent_limit..].to_vec()
    } else {
        decisions
    };

    ContextSummary {
        workspace_id: ws.to_string(),
        channel_id: ch.to_string(),
        total_events: events.len(),
        latest_version,
        active_agents: agents.into_iter().collect(),
        recent_decisions: decisions,
        knowledge_delta: knowledge_deltas,
        conflict_alerts,
        event_counts_by_kind: kind_counts,
    }
}
