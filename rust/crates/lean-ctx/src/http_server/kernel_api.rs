// SPDX-License-Identifier: Apache-2.0
//! HTTP handlers exposing Context Kernel runtime state.

use axum::{Json, http::StatusCode};
use serde::{Deserialize, Serialize};

use lean_ctx::core::context_kernel::{
    envelope_wiring, kernel_config, live_dashboard, mcp_bridge, proxy_bridge,
};

/// Point-in-time ETPAO values for the proxy and MCP hot paths.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct EtpaoResponse {
    /// Current proxy effective tokens per accepted outcome.
    pub proxy_etpao: f64,
    /// Current MCP effective tokens per accepted outcome.
    pub mcp_etpao: f64,
    /// Arithmetic mean of the proxy and MCP ETPAO values.
    pub combined_etpao: f64,
}

/// Returns the live Context Kernel dashboard snapshot.
#[allow(clippy::unused_async)]
pub async fn dashboard() -> Json<serde_json::Value> {
    let snapshot = live_dashboard::snapshot_json();
    let report = lean_ctx::core::context_kernel::dashboard_report::generate_report();
    let mut base = serde_json::from_str::<serde_json::Value>(&snapshot)
        .unwrap_or_else(|_| serde_json::json!({}));
    if let (Some(obj), Ok(report_val)) = (base.as_object_mut(), serde_json::to_value(&report)) {
        obj.insert("report".to_string(), report_val);
    }
    Json(base)
}

/// Returns current ETPAO values for both active integration paths.
#[allow(clippy::unused_async)]
pub async fn etpao() -> Json<EtpaoResponse> {
    let proxy_etpao = proxy_bridge::current_etpao();
    let mcp_etpao = mcp_bridge::mcp_etpao();
    Json(EtpaoResponse {
        proxy_etpao,
        mcp_etpao,
        combined_etpao: f64::midpoint(proxy_etpao, mcp_etpao),
    })
}

/// Returns the current Context Kernel runtime feature configuration.
#[allow(clippy::unused_async)]
pub async fn get_config() -> Json<serde_json::Value> {
    Json(
        serde_json::to_value(kernel_config::features()).unwrap_or_else(|error| {
            serde_json::json!({
                "error": "invalid kernel configuration",
                "detail": error.to_string(),
            })
        }),
    )
}

/// Replaces the Context Kernel runtime feature configuration.
#[allow(clippy::unused_async)]
pub async fn set_config(
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let features = serde_json::from_value(body).map_err(|_| StatusCode::BAD_REQUEST)?;
    kernel_config::update_features(features);
    Ok(get_config().await)
}

/// Returns aggregate evidence from the active Context Kernel pipeline.
#[allow(clippy::unused_async)]
pub async fn evidence() -> Json<serde_json::Value> {
    Json(
        serde_json::to_value(envelope_wiring::evidence_summary()).unwrap_or_else(|error| {
            serde_json::json!({
                "error": "invalid kernel evidence",
                "detail": error.to_string(),
            })
        }),
    )
}

/// Clears live kernel evidence and ETPAO state.
#[allow(clippy::unused_async)]
pub async fn reset_state() -> Json<&'static str> {
    envelope_wiring::reset_evidence();
    proxy_bridge::reset_state();
    mcp_bridge::reset_mcp_state();
    Json("ok")
}

/// Returns the aggregated Context Kernel health report.
#[allow(clippy::unused_async)]
pub async fn health() -> Json<serde_json::Value> {
    let json_str = lean_ctx::core::context_kernel::health_api::health_json();
    Json(serde_json::from_str(&json_str).unwrap_or_else(|error| {
        serde_json::json!({
            "error": "invalid health snapshot",
            "detail": error.to_string(),
        })
    }))
}

/// Returns a structured kernel dashboard report.
#[allow(clippy::unused_async)]
pub async fn report() -> Json<serde_json::Value> {
    let r = lean_ctx::core::context_kernel::dashboard_report::generate_report();
    Json(serde_json::to_value(&r).unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() })))
}

#[cfg(test)]
mod tests {
    use axum::{body::to_bytes, response::IntoResponse};

    use super::*;

    async fn response_json(response: impl IntoResponse) -> serde_json::Value {
        let response = response.into_response();
        let bytes = to_bytes(response.into_body(), 1_000_000)
            .await
            .expect("response body should be readable");
        serde_json::from_slice(&bytes).expect("response body should contain JSON")
    }

    #[tokio::test]
    async fn dashboard_returns_valid_json() {
        let value = response_json(dashboard().await).await;
        assert!(value.is_object());
    }

    #[tokio::test]
    async fn etpao_returns_numbers() {
        let value = response_json(etpao().await).await;
        assert!(value["proxy_etpao"].is_f64());
        assert!(value["mcp_etpao"].is_f64());
        assert!(value["combined_etpao"].is_f64());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn config_roundtrip() {
        let _guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = response_json(get_config().await).await;
        let mut modified = original.clone();
        modified["content_dedup"] = serde_json::Value::Bool(
            !original["content_dedup"]
                .as_bool()
                .expect("content_dedup should be boolean"),
        );

        let response = set_config(Json(modified.clone()))
            .await
            .expect("valid configuration should be accepted");
        assert_eq!(response_json(response).await, modified);
        assert_eq!(response_json(get_config().await).await, modified);

        let _ = set_config(Json(original))
            .await
            .expect("original configuration should be restored");
    }

    #[tokio::test]
    async fn evidence_returns_summary() {
        let value = response_json(evidence().await).await;
        for field in [
            "proxy_requests",
            "mcp_calls",
            "total_envelopes",
            "chain_entries",
            "compression_ratio",
            "kernel_hit_rate",
        ] {
            assert!(value.get(field).is_some(), "missing field: {field}");
        }
    }
}
