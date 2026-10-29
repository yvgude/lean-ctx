//! POST /v1/quality-lab — run Quality Lab analysis on a text pair.

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::admitted::{self, GatewayRefusal, Payload};
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};
use crate::core::quality_lab::orchestrator::{QualityLabReport, run_quality_lab};

#[derive(Deserialize)]
pub(crate) struct QualityLabRequest {
    #[serde(default)]
    pub original: String,
    #[serde(default)]
    pub compressed: String,
    #[serde(default = "default_ext")]
    pub ext: String,
}

fn default_ext() -> String {
    "rs".to_string()
}

#[derive(Serialize)]
pub(crate) struct QualityLabResponse {
    pub report: QualityLabReport,
}

/// Destination label of the quality lab in receipts. Like `/v1/compress`, the
/// reply goes to a caller whose onward destination this proxy cannot see.
const QUALITY_LAB_DESTINATION: &str = "quality-lab";

/// The report is mostly counts and grades, but it is computed from the
/// caller's text and echoes the caller's `ext` verbatim
/// (`context_quality.transformation.source_type`). The pair is therefore
/// admitted by the gateway before it is analysed, the analysis only sees the
/// admitted form, and the reply bytes are bound to the receipt.
pub(crate) async fn handler(Json(req): Json<QualityLabRequest>) -> Response {
    match admitted_report(&req).await {
        Ok(response) => response,
        Err(refusal) => refusal.response("OpenAI"),
    }
}

async fn admitted_report(req: &QualityLabRequest) -> Result<Response, GatewayRefusal> {
    // An array, so no member is taken for protocol structure by its key.
    let mut document = serde_json::json!([req.original, req.compressed, req.ext]);
    let original_bytes = document.to_string().len();
    let target = EgressTarget {
        provider: QUALITY_LAB_DESTINATION,
        model: None,
        upstream_base: "",
    };
    let mut admission = admitted::admit(
        Payload::Document(&document),
        &target,
        TrustedDestinationBinding::default(),
    )?;
    if admission.is_refused() {
        return Err(admitted::record_refusal(admission, original_bytes, None).await);
    }
    if let Some(admitted_document) = admission.take_rewritten() {
        document = admitted_document;
    }
    let text = |index: usize| document[index].as_str().unwrap_or_default();
    let report = run_quality_lab(text(0), text(1), text(2));
    let reply = serde_json::to_vec(&QualityLabResponse { report })
        .map_err(|_| GatewayRefusal::unavailable("egress.encode_failed"))?;
    admitted::bind_delivery(admission, &reply, original_bytes).await?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        reply,
    )
        .into_response())
}

pub(crate) async fn handler_get() -> Response {
    let report = run_quality_lab("", "", "");
    Json(QualityLabResponse { report }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report echoes the caller's `ext`; a protected value in it — or in
    /// the analysed pair — must not come back in the reply.
    #[tokio::test]
    async fn reply_never_echoes_a_protected_value() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let response = handler(Json(QualityLabRequest {
            original: format!("deploy with {key} today\n").repeat(8),
            compressed: "deploy today".to_owned(),
            ext: key.to_owned(),
        }))
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let reply = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("reply body");
        let reply = String::from_utf8_lossy(&reply);
        assert!(reply.contains("\"report\""), "{reply}");
        assert!(!reply.contains(key), "the reply echoes the value: {reply}");
    }
}
