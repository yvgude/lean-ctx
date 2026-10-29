// SPDX-License-Identifier: Apache-2.0
//! HTTP transport for telemetry and product feedback: the API base URL (with
//! the test-process loopback guard), the telemetry-v2 batch send, remote
//! telemetry deletion and anonymous product feedback.

/// Base URL of the lean-ctx cloud API. Unit tests and cargo-launched binaries
/// get the discard port instead of any non-loopback endpoint, so test fixtures
/// (telemetry probes, feedback, stats, wrapped cards) can never reach
/// production; their requests fail fast with "connection refused".
pub fn api_url() -> String {
    const LOOPBACK_SINK: &str = "http://127.0.0.1:9";
    let configured =
        std::env::var("LEAN_CTX_API_URL").unwrap_or_else(|_| "https://api.leanctx.com".to_string());
    if cloud_endpoint_allowed(&configured, launched_by_cargo()) {
        configured
    } else {
        LOOPBACK_SINK.to_string()
    }
}

/// Send one volunteered product-feedback submission. No authentication.
///
/// Anonymous like the telemetry send, and for the same reason: there is no account to
/// attach this to, and requiring one would silence exactly the users worth
/// hearing from. The payload carries only what the person typed into the form,
/// plus the installation id and version so answers can be read in context —
/// never anything derived from their code, paths or usage.
///
/// The server may answer 429 when one installation has already sent several
/// today; that is surfaced to the caller rather than swallowed, so the UI can
/// say what happened instead of reporting a send that was dropped.
pub fn submit_product_feedback(payload: &serde_json::Value) -> Result<String, String> {
    let url = format!("{}/api/feedback/product", api_url());

    let resp = ureq::post(&url)
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(payload).map_err(|e| format!("JSON error: {e}"))?)
        .map_err(|e| format!("Could not send feedback: {e}"))?;

    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid JSON: {e}"))?;

    Ok(json["message"].as_str().unwrap_or("Thanks").to_string())
}

/// Cargo exports `CARGO_PKG_NAME` to `cargo test` / `cargo run` processes and
/// every child they spawn, so integration tests driving the binary carry it too.
fn launched_by_cargo() -> bool {
    std::env::var("CARGO_PKG_NAME").is_ok_and(|name| name == env!("CARGO_PKG_NAME"))
}

/// Unit tests and cargo-launched binaries may only talk to a loopback
/// endpoint; otherwise test fixtures would pollute production data.
fn cloud_endpoint_allowed(base_url: &str, launched_by_cargo: bool) -> bool {
    if !cfg!(test) && !launched_by_cargo {
        return true;
    }
    let Some(authority) = base_url
        .strip_prefix("http://")
        .or_else(|| base_url.strip_prefix("https://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next())
    else {
        return false;
    };
    if authority.contains('@') {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Send one validated telemetry-v2 daily batch. No authentication required.
pub fn telemetry_v2_batch(
    batch: &crate::core::telemetry_v2::TelemetryBatchV2,
) -> Result<String, String> {
    telemetry_v2_batch_with_timeout(batch, std::time::Duration::from_secs(10))
}

pub(crate) fn telemetry_v2_batch_with_timeout(
    batch: &crate::core::telemetry_v2::TelemetryBatchV2,
    timeout: std::time::Duration,
) -> Result<String, String> {
    // A sender may have waited for another lease since its initial precheck.
    let config = crate::core::config::Config::try_load_global()
        .map_err(|_| "Telemetry configuration unavailable".to_string())?;
    let do_not_track = crate::core::host_env::var("DO_NOT_TRACK");
    let telemetry_override = crate::core::host_env::var("LEAN_CTX_TELEMETRY");
    if !config
        .telemetry
        .send_eligible(do_not_track.as_deref(), telemetry_override.as_deref())
    {
        return Err("Telemetry sending is disabled".to_string());
    }
    if crate::core::telemetry_consent::running_in_ci() {
        return Err("Telemetry is not sent from CI".to_string());
    }
    batch
        .validate()
        .map_err(|error| format!("Telemetry validation failed: {error:?}"))?;
    let url = format!("{}/api/telemetry/v2/batch", api_url());
    let response = ureq::post(&url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .header("Content-Type", "application/json")
        .send(&serde_json::to_vec(batch).map_err(|error| format!("JSON error: {error}"))?)
        .map_err(|error| format!("Telemetry v2 batch failed: {error}"))?;
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|error| format!("Failed to read response: {error}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|error| format!("Invalid JSON: {error}"))?;
    crate::core::telemetry_notices::remember(crate::core::telemetry_notices::from_response(&json));
    Ok(json["message"].as_str().unwrap_or("OK").to_string())
}

/// Delete every server-side aggregate associated with one installation ID.
pub fn delete_remote_telemetry(
    installation_id: &str,
    deletion_token: &str,
) -> Result<bool, String> {
    let installation_id = uuid::Uuid::parse_str(installation_id)
        .map_err(|_| "Invalid installation ID".to_string())?;
    let url = format!(
        "{}/api/telemetry/v2/installations/{installation_id}",
        api_url()
    );
    if deletion_token.len() != 64
        || !deletion_token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("Invalid telemetry deletion credential".to_string());
    }
    let response = ureq::delete(&url)
        .header(
            "Authorization",
            &format!("TelemetryDelete v1.{deletion_token}"),
        )
        .call()
        .map_err(|error| format!("Remote telemetry deletion failed: {error}"))?;
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .read_to_string()
        .map_err(|error| format!("Failed to read deletion response: {error}"))?;
    deletion_confirmed(status, &body)
}

fn deletion_confirmed(status: u16, body: &str) -> Result<bool, String> {
    if status == 204 {
        return Ok(false);
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("Invalid deletion response: {error}"))?;
    Ok(value.get("deleted").and_then(serde_json::Value::as_bool) == Some(true))
}

#[cfg(test)]
#[path = "telemetry_transport_tests.rs"]
mod tests;
