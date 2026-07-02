//! `GET /api/admin/status` (enterprise#46) — the gateway's live health/config
//! card for the admin dashboard.
//!
//! Everything here is *observed*, not configured wishful thinking: the store
//! block runs a real query against `usage_events` (connected = the query
//! succeeded just now), the drop counter is the live fail-open counter from
//! `proxy::usage_sink`, and the provider list mirrors the resolved registry —
//! including whether each injection credential is actually present in the
//! environment.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};

use super::admin_api::AdminState;

/// One registry provider as shown on the status card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderStatus {
    pub id: String,
    /// Wire shape label (`anthropic` | `openai` | `gemini`).
    pub shape: String,
    pub base_url: String,
    /// Whether the gateway injects its own upstream key for this provider.
    pub injects_credential: bool,
    /// `injects_credential` and the env var is actually set and non-empty.
    pub credential_present: bool,
    /// Billed as local inference (shadow rate) — declared flag or loopback URL.
    #[serde(default)]
    pub local: bool,
}

/// Store (Postgres) health, measured by a live query at request time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoreStatus {
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_ts: Option<String>,
    /// Fail-open drops since process start (`usage_sink` saturation counter).
    pub dropped_events: u64,
}

/// Response of `GET /api/admin/status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StatusResponse {
    pub version: String,
    pub uptime_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seats: Option<u32>,
    pub store: StoreStatus,
    pub providers: Vec<ProviderStatus>,
    pub routing_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_model: Option<String>,
    pub local_shadow_rate_per_mtok: f64,
}

pub(super) async fn get_status(State(state): State<Arc<AdminState>>) -> Response {
    Json(build_status(&state).await).into_response()
}

/// Assembles the status snapshot. Never fails: a broken store shows up as
/// `connected: false`, not as an error response (the card must render during
/// incidents — that is when it matters most).
pub async fn build_status(state: &AdminState) -> StatusResponse {
    let store = match probe_store(&state.pool).await {
        Ok((events_total, last_event_ts)) => StoreStatus {
            connected: true,
            events_total: Some(events_total),
            last_event_ts,
            dropped_events: crate::proxy::usage_sink::dropped_count(),
        },
        Err(e) => {
            tracing::debug!("admin status store probe failed: {e:#}");
            StoreStatus {
                connected: false,
                events_total: None,
                last_event_ts: None,
                dropped_events: crate::proxy::usage_sink::dropped_count(),
            }
        }
    };
    StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.started_at.elapsed().as_secs(),
        org_label: state.org_label.clone(),
        seats: state.seats,
        store,
        providers: state.providers.clone(),
        routing_enabled: state.routing_enabled,
        reference_model: state.reference_model.clone(),
        local_shadow_rate_per_mtok: state.local_shadow_rate,
    }
}

async fn probe_store(pool: &deadpool_postgres::Pool) -> anyhow::Result<(i64, Option<String>)> {
    let client = pool.get().await?;
    let row = client
        .query_one(
            "SELECT count(*) AS n, max(ts) AS last FROM usage_events",
            &[],
        )
        .await?;
    let last: Option<chrono::DateTime<chrono::Utc>> = row.get("last");
    Ok((row.get("n"), last.map(|t| t.to_rfc3339())))
}

/// Derives the provider status list from the resolved registry, checking each
/// injection env var *now* (a rotated-away key shows up immediately).
#[must_use]
pub fn provider_statuses(
    providers: &[crate::core::config::ResolvedProvider],
) -> Vec<ProviderStatus> {
    providers
        .iter()
        .map(|p| {
            let credential_present = p.api_key_env.as_deref().is_some_and(|env_name| {
                std::env::var(env_name).is_ok_and(|v| !v.trim().is_empty())
            });
            ProviderStatus {
                id: p.id.clone(),
                shape: p.shape.as_str().to_string(),
                base_url: p.base_url.clone(),
                injects_credential: p.api_key_env.is_some(),
                credential_present,
                local: p.local,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ResolvedProvider, WireShape};

    #[test]
    fn provider_status_reflects_env_presence() {
        let providers = vec![
            ResolvedProvider {
                id: "local".into(),
                shape: WireShape::OpenAi,
                base_url: "http://127.0.0.1:11434".into(),
                api_key_env: None,
                local: true,
            },
            ResolvedProvider {
                id: "foundry".into(),
                shape: WireShape::OpenAi,
                base_url: "https://example.services.ai.azure.com/models".into(),
                api_key_env: Some("LEANCTX_TEST_STATUS_KEY_UNSET".into()),
                local: false,
            },
        ];
        let statuses = provider_statuses(&providers);
        assert_eq!(statuses.len(), 2);
        assert!(!statuses[0].injects_credential);
        assert!(!statuses[0].credential_present);
        assert_eq!(statuses[0].shape, "openai");
        assert!(statuses[0].local, "declared local flag must surface");
        assert!(statuses[1].injects_credential);
        assert!(
            !statuses[1].credential_present,
            "unset env var must show as missing credential"
        );
        assert!(!statuses[1].local);
    }

    #[test]
    fn status_response_shape_round_trips() {
        let resp = StatusResponse {
            version: "3.8.18".into(),
            uptime_secs: 42,
            org_label: Some("Zühlke Engineering AG".into()),
            seats: Some(800),
            store: StoreStatus {
                connected: true,
                events_total: Some(1234),
                last_event_ts: Some("2026-07-02T09:00:00+00:00".into()),
                dropped_events: 0,
            },
            providers: vec![],
            routing_enabled: true,
            reference_model: Some("claude-opus-4.5".into()),
            local_shadow_rate_per_mtok: 0.25,
        };
        let json = serde_json::to_value(&resp).expect("serializes");
        assert_eq!(json["store"]["connected"], true);
        assert_eq!(json["seats"], 800);
        let parsed: StatusResponse = serde_json::from_value(json).expect("round-trips");
        assert_eq!(parsed, resp);
    }
}
