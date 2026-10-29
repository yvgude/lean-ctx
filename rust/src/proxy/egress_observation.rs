// SPDX-License-Identifier: Apache-2.0
//! Record what an outbound HTTP attempt actually observed, separately from admission.

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    EgressTransportFailureV1, EgressTransportObservationV1, EgressTransportObserverV1,
    EgressTransportResultV1, EgressTransportV1,
};

/// A response head is evidence of a response, never proof of model consumption.
/// A timeout may happen after bytes reached the server. Failure to save evidence
/// cannot undo that send; callers may return its response, but must not retry it.
pub(super) async fn record_http_attempt(
    admission_digest: Option<&Sha256Digest>,
    attempt: u32,
    response: &Result<reqwest::Response, reqwest::Error>,
) -> bool {
    let Some(admission_receipt_digest) = admission_digest.cloned() else {
        return true;
    };
    let (result, http_status, failure) = match response {
        Ok(response) => (
            EgressTransportResultV1::ResponseObserved,
            Some(response.status().as_u16()),
            None,
        ),
        Err(error) => (
            EgressTransportResultV1::NoResponseObserved,
            None,
            Some(if error.is_timeout() {
                EgressTransportFailureV1::Timeout
            } else if error.is_connect() {
                EgressTransportFailureV1::Connect
            } else {
                EgressTransportFailureV1::Other
            }),
        ),
    };
    persist(EgressTransportObservationV1 {
        schema_version: 1,
        admission_receipt_digest,
        attempt,
        transport: EgressTransportV1::Http,
        observer: EgressTransportObserverV1::EngineObserved,
        result,
        http_status,
        failure,
    })
    .await
}

/// One WebSocket frame handed to the upstream socket (AP10-C7). Each frame
/// has its own admission receipt, so it is that receipt's one attempt. A
/// completed send is what the transport can observe of a frame — that the
/// sink took it, never that the peer read it; a failed send observed no
/// hand-over.
pub(super) async fn record_websocket_frame(
    admission_digest: Option<&Sha256Digest>,
    sent: bool,
) -> bool {
    let Some(admission_receipt_digest) = admission_digest.cloned() else {
        return true;
    };
    let (result, failure) = if sent {
        (EgressTransportResultV1::ResponseObserved, None)
    } else {
        (
            EgressTransportResultV1::NoResponseObserved,
            Some(EgressTransportFailureV1::Other),
        )
    };
    persist(EgressTransportObservationV1 {
        schema_version: 1,
        admission_receipt_digest,
        attempt: 1,
        transport: EgressTransportV1::WebSocket,
        observer: EgressTransportObserverV1::EngineObserved,
        result,
        http_status: None,
        failure,
    })
    .await
}

async fn persist(observation: EgressTransportObservationV1) -> bool {
    let saved = matches!(
        tokio::task::spawn_blocking(move || {
            crate::core::context_admission::egress::append_engine_observation(&observation)
        })
        .await,
        Ok(Ok(()))
    );
    if !saved {
        tracing::warn!(
            "outbound response evidence unavailable; request may already have reached its destination"
        );
    }
    saved
}
