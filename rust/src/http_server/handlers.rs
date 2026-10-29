use axum::{
    extract::{Extension, Json, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde_json::Value;

use super::{AppState, RequestConcurrencyLease};

#[path = "handoff_files.rs"]
mod handoff_files;
use handoff_files::{evict_oldest_files, persist_handoff_file};

const MAX_HANDOFF_PAYLOAD_BYTES: usize = 1_000_000;
const MAX_HANDOFF_FILES: usize = 50;
pub(super) const MAX_REMOTE_ENVELOPE_AGE_SECONDS: i64 = 300;
const MAX_REMOTE_ENVELOPE_FUTURE_SKEW_SECONDS: i64 = 30;
pub(super) const REMOTE_REPLAY_RETENTION_SECONDS: i64 =
    crate::core::a2a::relay::RELAY_MAX_DEADLINE_SECS
        + MAX_REMOTE_ENVELOPE_AGE_SECONDS
        + MAX_REMOTE_ENVELOPE_FUTURE_SKEW_SECONDS;

pub(super) async fn a2a_deliver(
    State(state): State<AppState>,
    Extension(permit): Extension<RequestConcurrencyLease>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    if !state.a2a_peers.peers.is_empty() {
        return relay_a2a_deliver(state, headers, body, permit).await;
    }
    let Some(secret) = state.a2a_signing_key.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "a2a_auth_not_configured"})),
        )
            .into_response();
    };
    let Ok(envelope) = crate::core::a2a_transport::parse_envelope(&body.to_string()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_envelope"})),
        )
            .into_response();
    };
    if envelope
        .metadata
        .contains_key(crate::core::a2a::relay::RELAY_METADATA_KEY)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "relay_peers_not_configured"})),
        )
            .into_response();
    }
    let now = chrono::Utc::now();
    if !valid_remote_authority(
        &envelope,
        secret.as_bytes(),
        state.a2a_recipient_id.as_deref(),
        state.a2a_tenant_id.as_deref(),
        state.a2a_project_id.as_deref(),
        now,
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_remote_authority"})),
        )
            .into_response();
    }
    let authenticated_id = match envelope.stable_delivery_id() {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!("a2a delivery identity rejected: {error}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_envelope"})),
            )
                .into_response();
        }
    };
    let Ok(legacy_id) = verified_legacy_delivery_id(&envelope, secret.as_bytes()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_legacy_delivery_id"})),
        )
            .into_response();
    };
    let generation = match state
        .remote_replays
        .reserve_with_alias(&authenticated_id, legacy_id.as_deref(), now)
        .await
    {
        Ok(super::remote_replay::Reservation::Reserved(generation)) => generation,
        Ok(super::remote_replay::Reservation::InFlight) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "delivery_in_progress"})),
            )
                .into_response();
        }
        Ok(super::remote_replay::Reservation::Conflict) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "delivery_identity_conflict"})),
            )
                .into_response();
        }
        Ok(super::remote_replay::Reservation::Completed) => {
            if envelope.content_type == crate::core::a2a_transport::TransportContentType::A2ATask
                && super::task_control::is_control(&envelope.payload_json)
            {
                // Recheck both authorities and durable ownership; do not turn
                // a transport replay marker into unsigned task status evidence.
                return a2a_handoff_offloaded(state, body, Some(authenticated_id), None, permit)
                    .await
                    .unwrap_or_else(|_| handoff_worker_unavailable())
                    .into_response();
            }
            return (
                StatusCode::OK,
                Json(serde_json::json!({"status": "already_received"})),
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!("a2a replay ledger unavailable: {error}");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "replay_protection_unavailable"})),
            )
                .into_response();
        }
    };

    let Ok(response) = a2a_handoff_offloaded(
        state.clone(),
        body,
        Some(authenticated_id.clone()),
        None,
        permit,
    )
    .await
    else {
        // An interrupted worker may already have persisted effects. Retain the
        // pending replay proof instead of admitting an uncertain duplicate.
        return handoff_worker_unavailable().into_response();
    };
    let response = response.into_response();
    if !response.status().is_success() {
        if let Err(error) = state
            .remote_replays
            .release_with_alias(&authenticated_id, legacy_id.as_deref(), generation)
            .await
        {
            tracing::error!("a2a replay reservation release failed: {error}");
        }
    } else if let Err(error) = state
        .remote_replays
        .complete_with_alias(&authenticated_id, legacy_id.as_deref(), generation)
        .await
    {
        tracing::error!("a2a replay reservation completion failed: {error}");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "replay_protection_unavailable"})),
        )
            .into_response();
    }
    response
}

fn verified_legacy_delivery_id(
    envelope: &crate::core::a2a_transport::TransportEnvelopeV1,
    secret: &[u8],
) -> Result<Option<String>, ()> {
    let id = envelope
        .metadata
        .get(crate::core::a2a_transport::LEGACY_DELIVERY_ID_METADATA);
    let sent_at = envelope
        .metadata
        .get(crate::core::a2a_transport::LEGACY_DELIVERY_SENT_AT_METADATA);
    let (Some(id), Some(sent_at)) = (id, sent_at) else {
        return if id.is_none() && sent_at.is_none() {
            Ok(None)
        } else {
            Err(())
        };
    };
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(());
    }
    let original_sent_at = chrono::DateTime::parse_from_rfc3339(sent_at)
        .map_err(|_| ())?
        .with_timezone(&chrono::Utc);
    let mut original = envelope.clone();
    original
        .metadata
        .remove(crate::core::a2a_transport::LEGACY_DELIVERY_ID_METADATA);
    original
        .metadata
        .remove(crate::core::a2a_transport::LEGACY_DELIVERY_SENT_AT_METADATA);
    original.sent_at = original_sent_at;
    original.signature = Some(id.to_ascii_lowercase());
    if !original.verify_signature(secret) {
        return Err(());
    }
    Ok(Some(id.to_ascii_lowercase()))
}

fn valid_remote_authority(
    envelope: &crate::core::a2a_transport::TransportEnvelopeV1,
    secret: &[u8],
    expected_recipient: Option<&str>,
    expected_tenant: Option<&str>,
    expected_project: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let age = now.signed_duration_since(envelope.sent_at);
    envelope.verify_signature(secret)
        && expected_recipient
            .is_some_and(|expected| envelope.recipient.as_deref() == Some(expected))
        && expected_tenant.is_some_and(|expected| {
            envelope.metadata.get("tenant_id").map(String::as_str) == Some(expected)
        })
        && expected_project.is_some_and(|expected| {
            envelope.metadata.get("project_id").map(String::as_str) == Some(expected)
        })
        && age.num_seconds() <= MAX_REMOTE_ENVELOPE_AGE_SECONDS
        && age.num_seconds() >= -MAX_REMOTE_ENVELOPE_FUTURE_SKEW_SECONDS
}

async fn relay_a2a_deliver(
    state: AppState,
    headers: HeaderMap,
    body: Value,
    permit: RequestConcurrencyLease,
) -> axum::response::Response {
    use crate::core::a2a::relay::{RELAY_PEER_HEADER, RelayPeerTableV1};
    use crate::core::a2a_transport::TransportEnvelopeV1;

    let Some(peer_id) = headers
        .get(RELAY_PEER_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing_peer_identity"})),
        )
            .into_response();
    };
    let Some(peer) = state.a2a_peers.peer(peer_id).cloned() else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unknown_peer"})),
        )
            .into_response();
    };
    let Some(authorization) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing_peer_bearer"})),
        )
            .into_response();
    };
    let Some(bearer) = authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "malformed_peer_bearer"})),
        )
            .into_response();
    };
    if !super::constant_time_eq(bearer.as_bytes(), peer.bearer_token.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_peer_bearer"})),
        )
            .into_response();
    }
    let envelope: TransportEnvelopeV1 =
        match crate::core::a2a_transport::parse_envelope(&body.to_string()) {
            Ok(envelope) => envelope,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_envelope"})),
                )
                    .into_response();
            }
        };
    let Some(record) = envelope.relay_record().ok().flatten() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "relay_record_required"})),
        )
            .into_response();
    };
    let Some(local_recipient) = state.a2a_recipient_id.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "recipient_not_configured"})),
        )
            .into_response();
    };
    if envelope.sender.agent_id != record.current_peer
        || record.current_peer != peer_id
        || envelope.content_type != record.content_type
        || envelope.metadata.get("tenant_id").map(String::as_str) != Some(record.tenant_id.as_str())
        || envelope.metadata.get("project_id").map(String::as_str)
            != Some(record.project_id.as_str())
        || record.next_peer != local_recipient
        || envelope.recipient.as_deref() != Some(record.final_recipient.as_str())
        || peer.recipient_id != local_recipient
        || !envelope.verify_signature(peer.channel_key.as_bytes())
        || record.verify_hop(peer.channel_key.as_str()).is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_relay_authority"})),
        )
            .into_response();
    }
    let Some(origin_peer) = state.a2a_peers.peer(&record.origin) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "unknown_origin"})),
        )
            .into_response();
    };
    if record
        .verify_origin(origin_peer.origin_public_key.as_str())
        .is_err()
        || record
            .verify_payload(envelope.payload_json.as_bytes())
            .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_origin_authority"})),
        )
            .into_response();
    }
    if state.a2a_tenant_id.as_deref() != Some(record.tenant_id.as_str())
        || state.a2a_project_id.as_deref() != Some(record.project_id.as_str())
        || !peer.allows(&record, envelope.payload_json.len())
        || !origin_peer.allows(&record, envelope.payload_json.len())
        || record
            .validate_at(
                chrono::Utc::now(),
                &record.current_peer,
                Some(local_recipient),
                peer.max_hops,
            )
            .is_err()
        || !matches!(
            envelope.content_type,
            crate::core::a2a_transport::TransportContentType::A2ATask
                | crate::core::a2a_transport::TransportContentType::ContextPackage
                | crate::core::a2a_transport::TransportContentType::EvidenceBundle
        )
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "relay_policy_denied"})),
        )
            .into_response();
    }
    if let Some(idempotency_key) = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        && idempotency_key != record.delivery_id
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "idempotency_key_mismatch"})),
        )
            .into_response();
    }
    if let Err(response) = check_relay_quota(&state, &record).await {
        return *response;
    }
    let now = chrono::Utc::now();
    let replay_id = relay_storage_id(&record);
    // Bind immutable signed content, independent of the hop or signature encoding.
    let Ok(fingerprint) = record.origin_fingerprint() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_origin_binding"})),
        )
            .into_response();
    };
    let generation = match state
        .remote_replays
        .reserve_relay(&replay_id, &record, &fingerprint, now)
        .await
    {
        Ok(super::relay_replay::Reservation::Reserved(generation)) => generation,
        Ok(super::relay_replay::Reservation::InFlight) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "delivery_in_progress"})),
            )
                .into_response();
        }
        Ok(super::relay_replay::Reservation::Conflict) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "delivery_identity_conflict"})),
            )
                .into_response();
        }
        Ok(super::relay_replay::Reservation::Completed) => {
            if envelope.content_type == crate::core::a2a_transport::TransportContentType::A2ATask {
                return match state
                    .remote_replays
                    .completed_relay_response(&replay_id, &record, &fingerprint)
                    .await
                {
                    Ok(Some(body)) => task_response_bytes(body),
                    Ok(None) => completed_without_retained_response(),
                    Err(error) => relay_replay_error(&error),
                };
            }
            return (
                StatusCode::OK,
                Json(serde_json::json!({"status": "already_received"})),
            )
                .into_response();
        }
        Err(error) => {
            tracing::error!("relay replay ledger unavailable: {error}");
            return relay_replay_error(&error);
        }
    };

    let response = if record.final_recipient == local_recipient {
        let Ok(response) = a2a_handoff_offloaded(
            state.clone(),
            body.clone(),
            Some(relay_storage_id(&record)),
            Some(record.clone()),
            permit,
        )
        .await
        else {
            // Keep the pending proof on ambiguous worker failure.
            return handoff_worker_unavailable().into_response();
        };
        response.into_response()
    } else {
        let next = match state.a2a_peers.route_for_recipient(&record.final_recipient) {
            Ok(next) => next.clone(),
            Err(error) => {
                tracing::warn!(error = %error, "relay route lookup failed");
                let _ = state
                    .remote_replays
                    .release_relay(&replay_id, &generation)
                    .await;
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "relay_route_not_found"})),
                )
                    .into_response();
            }
        };
        let mut forwarded = envelope.clone();
        let mut forwarded_record = record.clone();
        if forwarded_record
            .forward_from(
                local_recipient,
                &next.peer_id,
                chrono::Utc::now(),
                &next.channel_key,
            )
            .is_err()
        {
            let _ = state
                .remote_replays
                .release_relay(&replay_id, &generation)
                .await;
            return (
                StatusCode::LOOP_DETECTED,
                Json(serde_json::json!({"error": "relay_cycle_or_hop_cap"})),
            )
                .into_response();
        }
        forwarded.sender =
            crate::core::a2a_transport::AgentIdentityV1::from_current(local_recipient, "relay");
        forwarded.signature = None;
        // The authenticated inbound record has advanced one hop. Replace only
        // its reserved slot; attach_relay_record rejects accidental overwrites.
        forwarded
            .metadata
            .remove(crate::core::a2a::relay::RELAY_METADATA_KEY);
        if forwarded.attach_relay_record(&forwarded_record).is_err() {
            let _ = state
                .remote_replays
                .release_relay(&replay_id, &generation)
                .await;
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_relay_record"})),
            )
                .into_response();
        }
        let transport = match crate::core::a2a::remote_transport::RemoteTransport::for_peer_table(
            RelayPeerTableV1 {
                schema_version: state.a2a_peers.schema_version,
                peers: state.a2a_peers.peers.clone(),
            },
            local_recipient,
            state.timeout,
            cfg!(test),
        ) {
            Ok(transport) => transport,
            Err(error) => {
                tracing::error!(error = %error, "relay transport configuration unavailable");
                let _ = state
                    .remote_replays
                    .release_relay(&replay_id, &generation)
                    .await;
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({"error": "relay_transport_unavailable"})),
                )
                    .into_response();
            }
        };
        match transport.deliver_to(&next.peer_id, &forwarded).await {
            // The downstream's bytes are admitted under this server's project,
            // the one the authenticated origin request was scoped to.
            Ok(receipt) => crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(Some(std::path::PathBuf::from(&state.project_root))),
                || forwarded_delivery_response(receipt, &forwarded.content_type),
            ),
            Err(error) => {
                tracing::warn!(error = %error, "downstream relay delivery failed");
                // The downstream may have committed before the connection failed.
                // Task effects retain Pending; preserve the existing non-task DLQ path.
                if forwarded.content_type
                    != crate::core::a2a_transport::TransportContentType::A2ATask
                {
                    let _ = state
                        .remote_replays
                        .release_relay(&replay_id, &generation)
                        .await;
                }
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({"error": "downstream_delivery_failed"})),
                )
                    .into_response();
            }
        }
    };
    if !response.status().is_success() {
        if response
            .extensions()
            .get::<TaskResponseWithheld>()
            .is_some()
        {
            // The downstream answered; only its answer is not released.
            // Complete the delivery without a body: a duplicate is told so
            // and nothing is forwarded a second time.
            if let Err(error) = state
                .remote_replays
                .complete_relay(&replay_id, &generation)
                .await
            {
                tracing::error!("relay replay completion failed: {error}");
            }
            return response;
        }
        if record.final_recipient != local_recipient
            && envelope.content_type == crate::core::a2a_transport::TransportContentType::A2ATask
        {
            // A successful delivery without a usable task response is ambiguous too.
            return response;
        }
        let _ = state
            .remote_replays
            .release_relay(&replay_id, &generation)
            .await;
        return response;
    }
    let (response, completion) =
        if envelope.content_type == crate::core::a2a_transport::TransportContentType::A2ATask {
            let (parts, body) = response.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await else {
                return missing_replayed_task_response();
            };
            let Ok(body) = String::from_utf8(bytes.to_vec()) else {
                return missing_replayed_task_response();
            };
            if body.is_empty() {
                return missing_replayed_task_response();
            }
            let completion = state
                .remote_replays
                .complete_relay_with_response(&replay_id, &generation, body)
                .await;
            (
                axum::response::Response::from_parts(parts, axum::body::Body::from(bytes)),
                completion,
            )
        } else {
            let completion = state
                .remote_replays
                .complete_relay(&replay_id, &generation)
                .await;
            (response, completion)
        };
    if let Err(error) = completion {
        tracing::error!("relay replay completion failed: {error}");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "replay_protection_unavailable"})),
        )
            .into_response();
    }
    response
}

fn task_response_bytes(body: String) -> axum::response::Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

fn missing_replayed_task_response() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "task_response_unavailable"})),
    )
        .into_response()
}

fn relay_replay_error(error: &super::relay_replay::StoreError) -> axum::response::Response {
    use super::relay_replay::StoreError;
    match error {
        StoreError::CapacityExhausted {
            retry_after_seconds,
        } => (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                (*retry_after_seconds).max(1).to_string(),
            )],
            Json(serde_json::json!({"error": "relay_replay_capacity_exhausted"})),
        )
            .into_response(),
        StoreError::Expired | StoreError::InvalidInput(_) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_relay_replay_binding"})),
        )
            .into_response(),
        StoreError::MigrationRequired(_)
        | StoreError::LeaseMismatch
        | StoreError::Unavailable(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "replay_protection_unavailable"})),
        )
            .into_response(),
    }
}

async fn check_relay_quota(
    state: &AppState,
    record: &crate::core::a2a::relay::RelayRecordV1,
) -> Result<(), Box<axum::response::Response>> {
    use super::relay_rate::QuotaRejection;
    let unavailable = || {
        Box::new(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "relay_quota_unavailable"})),
            )
                .into_response(),
        )
    };
    let quotas = state.relay_quotas.as_ref().ok_or_else(unavailable)?;
    let result = {
        let mut quotas = tokio::time::timeout(std::time::Duration::from_millis(50), quotas.lock())
            .await
            .map_err(|_| unavailable())?;
        quotas.check_at(
            &record.current_peer,
            &record.origin,
            &record.tenant_id,
            &record.project_id,
            std::time::Instant::now(),
        )
    };
    match result {
        Ok(()) => Ok(()),
        Err(QuotaRejection::Limited { retry_after }) => {
            let seconds = retry_after
                .as_secs()
                .saturating_add(u64::from(retry_after.subsec_nanos() > 0))
                .max(1);
            Err(Box::new(
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, seconds.to_string())],
                    Json(serde_json::json!({"error": "relay_rate_limited"})),
                )
                    .into_response(),
            ))
        }
        Err(QuotaRejection::UnknownPeer | QuotaRejection::WrongScope) => Err(unavailable()),
    }
}

fn forwarded_delivery_response(
    receipt: crate::core::a2a::remote_transport::DeliveryReceipt,
    content_type: &crate::core::a2a_transport::TransportContentType,
) -> axum::response::Response {
    if *content_type == crate::core::a2a_transport::TransportContentType::A2ATask {
        // Preserve the final recipient's exact transcript across relay hops.
        // The origin verifies its signature; the relay must not re-sign it or
        // replace it with an acknowledgement that looks like task completion.
        let Some(raw) = receipt
            .unverified_task_response
            .filter(|raw| !raw.is_empty() && raw.len() <= 64 * 1024)
        else {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": "missing_task_response"})),
            )
                .into_response();
        };
        // Peer bytes are a carrier like any other: they pass the same carrier
        // admission as bridged tool output (current mode, project policy,
        // classification, secret rules) under the request project the caller
        // put in scope, before they reach the origin. Only unchanged bytes are
        // forwarded: a transcript the gateway would redact is refused, not
        // altered — a redacted signed transcript fails at the origin anyway
        // and would look like a fault of the peer. Both refusals carry reason
        // codes only.
        return match crate::core::context_admission::carrier::admit_carrier_text(&raw) {
            Ok(admitted) if admitted.text == raw => task_response_bytes(raw),
            Ok(_) => {
                withheld_task_response(&crate::core::context_admission::carrier::Withheld::new(&[
                    TASK_RESPONSE_REDACTION_REQUIRED,
                ]))
            }
            Err(withheld) => withheld_task_response(&withheld),
        };
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "forwarded", "delivery_id": receipt.envelope_id})),
    )
        .into_response()
}

/// Reason code of a forwarded task response that the gateway would deliver
/// only in redacted form. Distinct from the codes of a withheld response.
const TASK_RESPONSE_REDACTION_REQUIRED: &str = "relay.task_response_redaction_required";

/// Marks the refusal of a forwarded task response: the downstream has taken
/// the delivery, so it is completed (never forwarded again), without a body.
#[derive(Debug, Clone, Copy)]
struct TaskResponseWithheld;

/// Typed, content-free refusal of a forwarded task response.
fn withheld_task_response(
    withheld: &crate::core::context_admission::carrier::Withheld,
) -> axum::response::Response {
    let mut response = (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "error": "task_response_withheld",
            "reason_codes": withheld.codes(),
        })),
    )
        .into_response();
    response.extensions_mut().insert(TaskResponseWithheld);
    response
}

/// A duplicate of a delivery that completed, whose response was not retained
/// (withheld, not an acknowledgement, or completed before responses were
/// kept). Not "unavailable": retrying cannot produce it, and the delivery is
/// not executed again.
fn completed_without_retained_response() -> axum::response::Response {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": "task_response_not_retained",
            "status": "completed",
        })),
    )
        .into_response()
}

/// Verify signed task authority and durably materialize a remote A2A task.
///
/// Fail-closed in every branch: an unconfigured trust policy, an unparseable
/// descriptor, a descriptor that does not verify against the configured peer
/// key and capability grant, or a storage failure all return an error status.
/// Only a persisted task produces a success response, and the response states
/// truthfully whether the task was newly accepted or is an idempotent replay.
fn accept_remote_task(
    state: &AppState,
    envelope: &crate::core::a2a_transport::TransportEnvelopeV1,
    relay_record: Option<&crate::core::a2a::relay::RelayRecordV1>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    use crate::core::a2a::task::{
        MAX_TASK_DESCRIPTOR_BYTES, TaskAuthorityExpectationV1, TaskDescriptorV1, TaskStore,
    };

    if envelope.payload_json.len() > MAX_TASK_DESCRIPTOR_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "task_descriptor_too_large"})),
        ));
    }
    let recipient = relay_record
        .map(|record| record.final_recipient.as_str())
        .or(state.a2a_recipient_id.as_deref());
    let tenant_id = relay_record
        .map(|record| record.tenant_id.as_str())
        .or(state.a2a_tenant_id.as_deref());
    let project_id = relay_record
        .map(|record| record.project_id.as_str())
        .or(state.a2a_project_id.as_deref());
    let (Some(recipient), Some(tenant_id), Some(project_id)) = (recipient, tenant_id, project_id)
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "task_authority_not_configured"})),
        ));
    };
    if state.a2a_task_authority.peers.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "task_authority_not_configured"})),
        ));
    }
    let expectation = TaskAuthorityExpectationV1 {
        sender: relay_record
            .map(|record| record.origin.as_str())
            .unwrap_or(envelope.sender.agent_id.as_str()),
        recipient,
        tenant_id,
        project_id,
        now: chrono::Utc::now(),
    };
    if let Ok(request) = serde_json::from_str::<crate::core::a2a::task::TaskControlDescriptorV1>(
        &envelope.payload_json,
    ) {
        return super::task_control::accept(state, &request, &expectation);
    }
    let descriptor: TaskDescriptorV1 =
        serde_json::from_str(&envelope.payload_json).map_err(|error| {
            tracing::warn!("a2a task rejected: undecodable descriptor: {error}");
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_task_descriptor"})),
            )
        })?;

    // The envelope sender is only channel evidence; the descriptor must also
    // bind to a configured Ed25519 peer key that owns that agent id.
    if let Err(error) = descriptor.verify_authority(&state.a2a_task_authority, &expectation) {
        tracing::warn!("a2a task rejected: {error}");
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id: envelope.sender.agent_id.clone(),
            tool: "http:/a2a/task".to_string(),
            action: Some("task_authority_rejected".to_string()),
            input_hash: String::new(),
            output_tokens: 0,
            role: crate::core::roles::active_role_name(),
            event_type: crate::core::audit_trail::AuditEventType::SecurityViolation,
        });
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "unauthorized_task_authority",
                "reason": error.to_string(),
            })),
        ));
    }

    let path = TaskStore::scoped_path(&state.project_root).map_err(|error| {
        tracing::error!("a2a task store scope failed: {error}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "task_storage_unavailable"})),
        )
    })?;
    match TaskStore::materialize_remote_task(&path, &descriptor) {
        Ok(result) => Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "status": if result.duplicate { "duplicate" } else { "accepted" },
                "content_type": "a2a_task",
                "task_id": result.task_id,
            })),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            tracing::warn!("a2a task rejected: {error}");
            Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "idempotency_key_conflict"})),
            ))
        }
        // The store refuses a descriptor whose text the context gateway
        // withholds with `PermissionDenied` and a content-free message; that
        // is the sender's content, not a storage failure. A denial by the
        // operating system carries an OS code and stays a storage failure.
        Err(error) if task_text_withheld(&error) => {
            tracing::warn!("a2a task rejected: {error}");
            Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "task_text_withheld"})),
            ))
        }
        Err(error) => {
            tracing::error!("a2a task persistence failed: {error}");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "task_persist_failed"})),
            ))
        }
    }
}

/// Whether `TaskStore::materialize_remote_task` failed because the task text
/// was refused by the context gateway (typed, content-free), as opposed to a
/// file-system denial.
fn task_text_withheld(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied && error.raw_os_error().is_none()
}

/// Environment variable holding the shared HMAC secret for `/v1/a2a/handoff`.
///
/// It is the same key a sender passes to `lean-ctx pack send --secret <key>`:
/// the envelope signature is the transport HMAC over the canonical envelope
/// (`TransportEnvelopeV1::sign`). An empty or whitespace-only value counts as
/// not configured.
pub(super) const HANDOFF_SECRET_ENV: &str = "LEAN_CTX_HANDOFF_SECRET";

fn configured_handoff_secret() -> Option<String> {
    std::env::var(HANDOFF_SECRET_ENV)
        .ok()
        .filter(|secret| !secret.trim().is_empty())
}

/// Signature gate of `/v1/a2a/handoff`, applied before any payload handling.
///
/// - Secret configured (`LEAN_CTX_HANDOFF_SECRET`): the envelope must carry a
///   signature that verifies under it; unsigned or invalid → `401`.
/// - No secret, signed envelope: the signature cannot be checked, so the
///   envelope is refused (`401`, fail closed) instead of being treated as
///   authenticated.
/// - No secret, unsigned envelope: accepted as before (local, unauthenticated
///   handoff route; loopback-only unless a bearer token guards the server).
fn verify_handoff_envelope(
    body: &Value,
    secret: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    let unauthorized = |error: &str| {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": error })),
        )
    };
    let Ok(envelope) = crate::core::a2a_transport::parse_envelope(&body.to_string()) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_envelope"})),
        ));
    };
    match (secret, envelope.signature.is_some()) {
        (Some(secret), _) if envelope.verify_signature(secret.as_bytes()) => Ok(()),
        (Some(_), false) => Err(unauthorized("signature_required")),
        (Some(_), true) => Err(unauthorized("invalid_signature")),
        (None, true) => Err(unauthorized("handoff_secret_not_configured")),
        (None, false) => Ok(()),
    }
}

pub(super) async fn v1_a2a_handoff(
    State(state): State<AppState>,
    Extension(permit): Extension<RequestConcurrencyLease>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    if let Err(rejected) = verify_handoff_envelope(&body, configured_handoff_secret().as_deref()) {
        return rejected;
    }
    a2a_handoff_offloaded(state, body, None, None, permit)
        .await
        .unwrap_or_else(|_| handoff_worker_unavailable())
}

fn handoff_worker_unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "handoff_worker_unavailable"})),
    )
}

async fn a2a_handoff_offloaded(
    state: AppState,
    body: Value,
    delivery_id: Option<String>,
    record: Option<crate::core::a2a::relay::RelayRecordV1>,
    permit: RequestConcurrencyLease,
) -> Result<(StatusCode, Json<Value>), tokio::task::JoinError> {
    run_admitted_handoff(permit, move || {
        a2a_handoff_inner(
            State(state),
            Json(body),
            delivery_id.as_deref(),
            record.as_ref(),
        )
    })
    .await
}

async fn run_admitted_handoff(
    permit: RequestConcurrencyLease,
    work: impl FnOnce() -> (StatusCode, Json<Value>) + Send + 'static,
) -> Result<(StatusCode, Json<Value>), tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || {
        // The worker, not only its HTTP waiter, owns the admission slot.
        // A timed-out or disconnected request cannot free capacity early.
        let _permit = permit;
        work()
    })
    .await
}

fn relay_storage_id(record: &crate::core::a2a::relay::RelayRecordV1) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"leanctx.relay.storage.v1");
    for value in [
        &record.origin,
        &record.tenant_id,
        &record.project_id,
        &record.delivery_id,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    hex::encode(digest.finalize())
}

/// Global-only opt-in (`a2a_accept_untrusted_signers`): project-local config
/// never sets it (GHSA-gvg8-285q-m62g).
fn accept_untrusted_signers() -> bool {
    crate::core::config::Config::load().a2a_accept_untrusted_signers
}

/// 403 + `SecurityViolation` audit entry for a payload whose signature
/// verifies against a key that no trust anchor knows.
fn reject_untrusted_signer(sender_agent_id: &str, fingerprint: &str) -> (StatusCode, Json<Value>) {
    tracing::warn!("a2a handoff rejected: untrusted signer key {fingerprint}");
    crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
        agent_id: sender_agent_id.to_string(),
        tool: "http:/v1/a2a/handoff".to_string(),
        action: Some("import_untrusted_signer_rejected".to_string()),
        input_hash: String::new(),
        output_tokens: 0,
        role: crate::core::roles::active_role_name(),
        event_type: crate::core::audit_trail::AuditEventType::SecurityViolation,
    });
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({
            "error": "untrusted_signer",
            "signer_key_fingerprint": fingerprint,
            "hint": "pin the sender's key in trusted-signers.toml, or set \
                     a2a_accept_untrusted_signers = true in the global config",
        })),
    )
}

fn a2a_handoff_inner(
    State(state): State<AppState>,
    Json(body): Json<Value>,
    delivery_id: Option<&str>,
    relay_record: Option<&crate::core::a2a::relay::RelayRecordV1>,
) -> (StatusCode, Json<Value>) {
    let envelope = match crate::core::a2a_transport::parse_envelope(
        &serde_json::to_string(&body).unwrap_or_default(),
    ) {
        Ok(env) => env,
        Err(e) => {
            tracing::warn!("a2a handoff parse error: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid_envelope"})),
            );
        }
    };

    if envelope.payload_json.len() > MAX_HANDOFF_PAYLOAD_BYTES {
        tracing::warn!(
            "a2a handoff payload too large: {} bytes (limit {MAX_HANDOFF_PAYLOAD_BYTES})",
            envelope.payload_json.len()
        );
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "payload_too_large"})),
        );
    }

    let response = match envelope.content_type {
        crate::core::a2a_transport::TransportContentType::ContextPackage => {
            let Ok((manifest, _)) = crate::core::context_package::registry::parse_context_transfer(
                &envelope.payload_json,
            ) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_context_package"})),
                );
            };
            // GHSA-gvg8-285q-m62g: a valid signature from a key nobody
            // anchored proves nothing about the sender — anyone can mint one.
            if let Some(fingerprint) =
                crate::core::context_package::registry::untrusted_package_signer(&manifest)
                && !accept_untrusted_signers()
            {
                return reject_untrusted_signer(&envelope.sender.agent_id, &fingerprint);
            }
            let dir = std::path::Path::new(&state.project_root)
                .join(".lean-ctx")
                .join("handoffs")
                .join("packages");
            let _ = std::fs::create_dir_all(&dir);
            let name = delivery_id.map_or_else(
                || chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string(),
                str::to_string,
            );
            let out = dir.join(format!(
                "ctx-{name}.{}",
                crate::core::contracts::PACKAGE_EXTENSION
            ));
            if !out.exists() {
                evict_oldest_files(&dir, MAX_HANDOFF_FILES);
            }
            if let Err(e) =
                persist_handoff_file(&out, envelope.payload_json.as_bytes(), delivery_id)
            {
                tracing::error!("a2a handoff write failed: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "write_failed"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "received",
                    "content_type": "context_package",
                })),
            )
        }
        crate::core::a2a_transport::TransportContentType::HandoffBundle => {
            // Signature enforcement at the network boundary (GL #465): a
            // payload that is not a parseable bundle, or whose signature
            // material does not verify, is rejected fail-closed before it
            // ever touches disk. Unsigned bundles are rejected too.
            let bundle =
                match crate::core::handoff_transfer_bundle::parse_bundle_v1(&envelope.payload_json)
                {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("a2a handoff rejected: not a valid bundle: {e}");
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "invalid_bundle"})),
                        );
                    }
                };
            let signature =
                match crate::core::handoff_transfer_bundle::check_bundle_signature(&bundle) {
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Invalid(
                        reason,
                    ) => {
                        tracing::warn!("a2a handoff rejected: signature invalid: {reason}");
                        crate::core::audit_trail::record(
                            crate::core::audit_trail::AuditEntryData {
                                agent_id: envelope.sender.agent_id.clone(),
                                tool: "http:/v1/a2a/handoff".to_string(),
                                action: Some("import_signature_invalid".to_string()),
                                input_hash: String::new(),
                                output_tokens: 0,
                                role: crate::core::roles::active_role_name(),
                                event_type:
                                    crate::core::audit_trail::AuditEventType::SecurityViolation,
                            },
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "invalid_signature"})),
                        );
                    }
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Signed(v) => {
                        // GHSA-gvg8-285q-m62g: an untrusted or legacy (unbound)
                        // signature proves integrity only; the network boundary
                        // rejects it unless the user opted in globally.
                        if !v.trusted && !accept_untrusted_signers() {
                            return reject_untrusted_signer(
                                &envelope.sender.agent_id,
                                &v.key_fingerprint,
                            );
                        }
                        // Identity is only reported for an anchored key
                        // (GHSA-gvg8-285q-m62g); otherwise it is a claim.
                        serde_json::json!({
                            "status": v.status(),
                            "claimed_signer": v.claimed_signer,
                            "signer_key_fingerprint": v.key_fingerprint,
                            "trusted": v.trusted,
                        })
                    }
                    crate::core::handoff_transfer_bundle::BundleSignatureStatus::Unsigned => {
                        // GHSA-gvg8-285q-m62g: the network boundary never
                        // stores an unsigned bundle.
                        tracing::warn!("a2a handoff rejected: unsigned bundle");
                        crate::core::audit_trail::record(
                            crate::core::audit_trail::AuditEntryData {
                                agent_id: envelope.sender.agent_id.clone(),
                                tool: "http:/v1/a2a/handoff".to_string(),
                                action: Some("import_unsigned_rejected".to_string()),
                                input_hash: String::new(),
                                output_tokens: 0,
                                role: crate::core::roles::active_role_name(),
                                event_type:
                                    crate::core::audit_trail::AuditEventType::SecurityViolation,
                            },
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "unsigned_bundle"})),
                        );
                    }
                };

            let dir = std::path::Path::new(&state.project_root)
                .join(".lean-ctx")
                .join("handoffs");
            let _ = std::fs::create_dir_all(&dir);
            let name = delivery_id.map_or_else(
                || chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string(),
                str::to_string,
            );
            let out = dir.join(format!("received-{name}.json"));
            if !out.exists() {
                evict_oldest_files(&dir, MAX_HANDOFF_FILES);
            }
            if let Err(e) =
                persist_handoff_file(&out, envelope.payload_json.as_bytes(), delivery_id)
            {
                tracing::error!("a2a handoff write failed: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "write_failed"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "received",
                    "content_type": "handoff_bundle",
                    "signature": signature,
                })),
            )
        }
        crate::core::a2a_transport::TransportContentType::A2ATask => {
            if delivery_id.is_some() {
                // A remote task must be durably materialized under verified
                // authority or explicitly rejected. Never a success-shaped ACK
                // for a task that was not stored. The legacy local handoff route
                // has no authenticated delivery id and retains its prior receipt
                // semantics; it is not a remote authority boundary.
                match accept_remote_task(&state, &envelope, relay_record) {
                    Ok(accepted) => accepted,
                    Err(rejected) => return rejected,
                }
            } else {
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "status": "received",
                        "content_type": "a2a_task",
                    })),
                )
            }
        }
        crate::core::a2a_transport::TransportContentType::A2AMessage => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "received",
                "content_type": format!("{:?}", envelope.content_type),
            })),
        ),
        crate::core::a2a_transport::TransportContentType::EvidenceBundle => {
            let Ok(bundle) = serde_json::from_str::<
                crate::core::context_kernel::evidence_bundle::EvidenceBundle,
            >(&envelope.payload_json) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_evidence_bundle"})),
                );
            };
            if !bundle.verify() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid_evidence_bundle"})),
                );
            }
            let dir = std::path::Path::new(&state.project_root)
                .join(".lean-ctx")
                .join("handoffs")
                .join("evidence");
            let _ = std::fs::create_dir_all(&dir);
            let name = delivery_id.unwrap_or("legacy-evidence");
            let out = dir.join(format!("evidence-{name}.json"));
            if let Err(error) =
                persist_handoff_file(&out, envelope.payload_json.as_bytes(), delivery_id)
            {
                tracing::error!("a2a evidence write failed: {error}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "write_failed"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({"status": "received", "content_type": "evidence_bundle"})),
            )
        }
    };

    let rt = crate::core::context_os::runtime();
    let event_payload = serde_json::json!({
        "type": "handoff_received",
        "delivery_id": delivery_id,
        "content_type": format!("{:?}", envelope.content_type),
        "sender": envelope.sender.agent_id,
        "payload_size": envelope.payload_json.len(),
    });
    // A handoff addressed to one agent is directed: only that agent sees the
    // event, live and on replay. Unaddressed handoffs stay broadcast.
    let target_agents = envelope
        .recipient
        .as_deref()
        .filter(|recipient| !recipient.is_empty())
        .map(|recipient| vec![recipient.to_string()]);
    let kind = crate::core::context_os::ContextEventKindV1::SessionMutated;
    let actor = Some(envelope.sender.agent_id.as_str());
    let event_result = match (delivery_id, target_agents) {
        (Some(id), target_agents) => rt
            .bus
            .append_delivery_once(
                &state.project_root,
                "a2a",
                &kind,
                actor,
                event_payload,
                id,
                target_agents,
            )
            .map(|_| ()),
        (None, Some(target_agents)) => rt
            .bus
            .append_directed(
                &state.project_root,
                "a2a",
                &kind,
                actor,
                event_payload,
                target_agents,
            )
            .map(|_| ())
            .ok_or_else(|| "context bus append failed".to_string()),
        (None, None) => rt
            .bus
            .append(&state.project_root, "a2a", &kind, actor, event_payload)
            .map(|_| ())
            .ok_or_else(|| "context bus append failed".to_string()),
    };
    if let Err(error) = event_result {
        tracing::error!("a2a event write failed: {error}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "event_write_failed"})),
        );
    }
    response
}

/// `GET /v1/cache/stats` — live cross-agent cache and delivery metrics.
pub(super) async fn v1_cache_stats() -> impl axum::response::IntoResponse {
    let cache = crate::core::ocla::cache_coordinator::materialized_cache();
    use crate::core::ocla::cache_coordinator::CacheCoordinator as _;
    let stats = cache.stats();
    let delivery = crate::core::ocla::OclaRegistry::global()
        .delivery_registry
        .delivery_stats();
    let by_kind = {
        let mut m = serde_json::Map::new();
        for kind in &[
            "file_read",
            "shell_command",
            "search_query",
            "directory_walk",
            "composed_context",
        ] {
            m.insert(
                kind.to_string(),
                serde_json::json!({ "hits": 0_u64, "misses": 0_u64 }),
            );
        }
        m
    };
    let hit_rate = |hits: u64, misses: u64| {
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "l1": {
                "entries": cache.l1().len(),
                "hits": stats.l1_hits,
                "misses": stats.misses,
                "hit_rate": hit_rate(stats.l1_hits, stats.misses),
            },
            "l2": {
                "entries": cache.l2().len(),
                "hits": stats.l2_hits,
                "misses": stats.misses,
                "hit_rate": hit_rate(stats.l2_hits, stats.misses),
            },
            "l3": {
                "entries": cache.l3().len(),
                "bytes": cache.l3().len(),
                "hits": stats.l3_hits,
                "misses": stats.misses,
            },
            "delivery": {
                "total_stubs": delivery.stubs_served,
                "tokens_saved": delivery.tokens_saved,
                "references_served": stats.references_served,
            },
            "by_kind": by_kind,
        })),
    )
}

#[cfg(test)]
#[path = "relay_response_tests.rs"]
mod relay_response_tests;

#[cfg(test)]
#[path = "handlers_tests.rs"]
mod tests;
