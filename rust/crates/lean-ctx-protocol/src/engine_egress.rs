// SPDX-License-Identifier: Apache-2.0
//! Egress admission for an operator-owned Engine process (`engine egress-admit`).
//!
//! A host that forwards model requests itself (for example an organization's
//! inference gateway) hands the request to the local Engine, which applies the
//! same admission the lean-ctx proxy applies before a request leaves: secrets
//! and personal data are masked, withheld content is never forwarded. The
//! answer carries the admitted body, the request's most sensitive
//! classification and the decision receipt. What the host does with the
//! classification (destination rules) is the host's policy, not the Engine's.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::Sha256Digest;
use crate::common::ValidationError;
use crate::context_gateway::{
    ClassificationV1, ContextDecisionReceiptV1, DeliveryOutcomeV1, EgressTransportFailureV1,
    EgressTransportObservationV1, EgressTransportObserverV1, EgressTransportResultV1,
    EgressTransportV1,
};

pub const ENGINE_EGRESS_SCHEMA_VERSION: u32 = 1;
/// Opt-in preparation response that links an admission receipt for later reports.
pub const ENGINE_EGRESS_PREPARE_SCHEMA_VERSION: u32 = 2;
/// Maximum per-admission transport attempts persisted or host-reported.
pub const MAX_EGRESS_ATTEMPTS: u32 = 32;
/// Upper bound for one request document (matches the proxy's body bound).
pub const MAX_ENGINE_EGRESS_REQUEST_BYTES: usize = 32 * 1024 * 1024;
/// Host reports contain only bounded status metadata, never request content.
pub const MAX_ENGINE_EGRESS_REPORT_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEgressAdmissionRequestV1 {
    pub schema_version: u32,
    /// Provider label as the request's wire shape names it (`anthropic`,
    /// `openai`, ...). Informational: it is recorded in the receipt.
    pub provider: String,
    /// Base URL the host will send the request to. Locality (local vs.
    /// remote) is derived from it; anything not provably local is remote.
    pub upstream_base: String,
    /// The model request document exactly as the host would forward it.
    pub body: Value,
}

impl EngineEgressAdmissionRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != ENGINE_EGRESS_SCHEMA_VERSION {
            return Err(ValidationError::new("unsupported egress schema version"));
        }
        if self.provider.trim().is_empty() || self.provider.len() > 64 {
            return Err(ValidationError::new("egress provider label is invalid"));
        }
        if self.upstream_base.trim().is_empty() || self.upstream_base.len() > 2048 {
            return Err(ValidationError::new("egress upstream base is invalid"));
        }
        if !self.body.is_object() {
            return Err(ValidationError::new("egress body must be a JSON object"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressDispositionV1 {
    /// Forward the request unchanged.
    Forward,
    /// Forward `body` instead of the original (values were masked).
    Rewritten,
    /// Do not forward anything.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEgressAdmissionResponseV1 {
    pub schema_version: u32,
    pub disposition: EgressDispositionV1,
    /// The body to forward; absent when the request is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Content-free refusal reason (reason codes only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// Most sensitive classification found in the request. Absent when the
    /// gateway inspected nothing (disabled, or an opaque payload): a host must
    /// then treat the request as unclassified, never as public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<ClassificationV1>,
    /// The decision receipt; absent when the gateway is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ContextDecisionReceiptV1>,
}

impl EngineEgressAdmissionResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != ENGINE_EGRESS_SCHEMA_VERSION {
            return Err(ValidationError::new("unsupported egress schema version"));
        }
        let consistent = match self.disposition {
            EgressDispositionV1::Refused => self.body.is_none() && self.refusal.is_some(),
            EgressDispositionV1::Forward | EgressDispositionV1::Rewritten => {
                self.body.is_some() && self.refusal.is_none()
            }
        };
        if !consistent {
            return Err(ValidationError::new(
                "egress body and refusal must match the disposition",
            ));
        }
        if let Some(receipt) = &self.receipt {
            receipt.validate()?;
            let outcome_matches = match self.disposition {
                EgressDispositionV1::Forward | EgressDispositionV1::Rewritten => {
                    receipt.outcome == DeliveryOutcomeV1::Delivered
                }
                EgressDispositionV1::Refused => receipt.outcome != DeliveryOutcomeV1::Delivered,
            };
            if !outcome_matches {
                return Err(ValidationError::new(
                    "egress disposition does not match its decision receipt outcome",
                ));
            }
        }
        Ok(())
    }
}

/// Explicit opt-in wire contract for hosts that need to report transport
/// observations. The legacy `egress-admit` response remains V1 and unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEgressPrepareResponseV2 {
    pub schema_version: u32,
    pub decision: EngineEgressAdmissionResponseV1,
    /// Exact UTF-8 JSON bytes to send. Hosts must send this string verbatim,
    /// not reserialize `decision.body` (which could change the admitted bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_body: Option<String>,
    /// Content-addressed preparation evidence; this is not a send claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_receipt_digest: Option<Sha256Digest>,
}

impl EngineEgressPrepareResponseV2 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != ENGINE_EGRESS_PREPARE_SCHEMA_VERSION {
            return Err(ValidationError::new(
                "unsupported egress preparation schema version",
            ));
        }
        self.decision.validate()?;
        match self.decision.disposition {
            EgressDispositionV1::Refused => {
                if self.prepared_body.is_some() {
                    return Err(ValidationError::new(
                        "a refused preparation cannot carry prepared bytes",
                    ));
                }
            }
            EgressDispositionV1::Forward | EgressDispositionV1::Rewritten => {
                let prepared = self.prepared_body.as_ref().ok_or_else(|| {
                    ValidationError::new("a dispatchable preparation must carry exact bytes")
                })?;
                let parsed: Value = serde_json::from_str(prepared)
                    .map_err(|_| ValidationError::new("prepared bytes are not JSON"))?;
                if Some(&parsed) != self.decision.body.as_ref() {
                    return Err(ValidationError::new(
                        "prepared bytes do not match the admitted decision body",
                    ));
                }
                let digest = digest_bytes(prepared.as_bytes())?;
                if let Some(receipt) = &self.decision.receipt
                    && receipt.final_context.as_ref() != Some(&digest)
                {
                    return Err(ValidationError::new(
                        "prepared bytes do not match the decision receipt digest",
                    ));
                }
            }
        }
        if self.admission_receipt_digest.is_some() && self.decision.receipt.is_none() {
            return Err(ValidationError::new(
                "egress admission digest requires its decision receipt",
            ));
        }
        Ok(())
    }
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut digest, "{byte:02x}")
            .map_err(|_| ValidationError::new("egress digest encoding failed"))?;
    }
    Sha256Digest::new(digest)
}

/// Content-free transport facts reported by a host after it attempted to send
/// an exact request that the Engine previously recorded as Prepared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEgressHostReportRequestV1 {
    pub schema_version: u32,
    pub admission_receipt_digest: Sha256Digest,
    pub attempt: u32,
    pub transport: EgressTransportV1,
    pub result: EgressTransportResultV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<EgressTransportFailureV1>,
}

impl EngineEgressHostReportRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != ENGINE_EGRESS_SCHEMA_VERSION {
            return Err(ValidationError::new("unsupported egress schema version"));
        }
        if self.attempt > MAX_EGRESS_ATTEMPTS {
            return Err(ValidationError::new(
                "egress attempt exceeds the supported limit",
            ));
        }
        let report = EgressTransportObservationV1 {
            schema_version: self.schema_version,
            admission_receipt_digest: self.admission_receipt_digest.clone(),
            attempt: self.attempt,
            transport: self.transport,
            observer: EgressTransportObserverV1::HostReported,
            result: self.result,
            http_status: self.http_status,
            failure: self.failure,
        };
        report.validate()
    }
}

/// The stored host report. The Engine sets `HostReported`; clients cannot
/// submit an observer value in the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineEgressHostReportResponseV1 {
    pub schema_version: u32,
    pub observation_digest: Sha256Digest,
    pub observation: EgressTransportObservationV1,
}

impl EngineEgressHostReportResponseV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != ENGINE_EGRESS_SCHEMA_VERSION {
            return Err(ValidationError::new("unsupported egress schema version"));
        }
        self.observation.validate()?;
        if self.observation.observer != EgressTransportObserverV1::HostReported
            || self.observation.canonical_digest()? != self.observation_digest
        {
            return Err(ValidationError::new(
                "host report response does not match its stored observation",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision_receipt(outcome: DeliveryOutcomeV1) -> ContextDecisionReceiptV1 {
        let final_context = (outcome == DeliveryOutcomeV1::Delivered)
            .then(|| Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap());
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "receipt_id": "egress-test",
            "mode": "developer",
            "principal": {"kind": "unknown"},
            "destination": {"provider": "openai", "locality": "remote"},
            "sources": {"inspected": 0, "permitted": 0, "selected": 0, "blocked": 0},
            "decisions": [],
            "security": {
                "redactions": 0, "blocked_objects": 0, "quarantined_objects": 0,
                "injection_signals": 0, "incomplete_coverage": 0
            },
            "tokens": {"original": 0, "delivered": 0},
            "final_context": final_context,
            "outcome": outcome,
            "duration_us": 0
        }))
        .unwrap()
    }

    fn decision(
        disposition: EgressDispositionV1,
        receipt: Option<ContextDecisionReceiptV1>,
    ) -> EngineEgressAdmissionResponseV1 {
        let refused = disposition == EgressDispositionV1::Refused;
        EngineEgressAdmissionResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            disposition,
            body: (!refused).then(|| serde_json::json!({"model": "m"})),
            refusal: refused.then(|| "request withheld".to_owned()),
            classification: Some(ClassificationV1::Internal),
            receipt,
        }
    }

    #[test]
    fn decision_receipt_outcome_must_match_egress_disposition() {
        for disposition in [EgressDispositionV1::Forward, EgressDispositionV1::Rewritten] {
            assert!(
                decision(
                    disposition,
                    Some(decision_receipt(DeliveryOutcomeV1::Delivered))
                )
                .validate()
                .is_ok()
            );
            for outcome in [DeliveryOutcomeV1::Withheld, DeliveryOutcomeV1::Failed] {
                assert!(
                    decision(disposition, Some(decision_receipt(outcome)))
                        .validate()
                        .is_err(),
                    "{disposition:?} with {outcome:?} receipt"
                );
            }
        }
        for outcome in [DeliveryOutcomeV1::Withheld, DeliveryOutcomeV1::Failed] {
            assert!(
                decision(
                    EgressDispositionV1::Refused,
                    Some(decision_receipt(outcome))
                )
                .validate()
                .is_ok()
            );
        }
        assert!(
            decision(
                EgressDispositionV1::Refused,
                Some(decision_receipt(DeliveryOutcomeV1::Delivered))
            )
            .validate()
            .is_err()
        );
        // Disabled or best-effort operation may omit a durable decision receipt.
        assert!(
            decision(EgressDispositionV1::Forward, None)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn a_refusal_never_carries_a_body_and_a_forward_always_does() {
        let mut response = EngineEgressAdmissionResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            disposition: EgressDispositionV1::Refused,
            body: None,
            refusal: Some("request withheld by the context gateway (x)".into()),
            classification: Some(ClassificationV1::Restricted),
            receipt: None,
        };
        assert!(response.validate().is_ok());
        response.body = Some(serde_json::json!({"model": "m"}));
        assert!(response.validate().is_err(), "refused with a body");
        response.disposition = EgressDispositionV1::Forward;
        response.refusal = None;
        assert!(response.validate().is_ok());
        response.body = None;
        assert!(response.validate().is_err(), "forward without a body");
    }

    #[test]
    fn preparation_link_uses_a_separate_strict_versioned_response() {
        let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let decision = EngineEgressAdmissionResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            disposition: EgressDispositionV1::Forward,
            body: Some(serde_json::json!({"model": "m"})),
            refusal: None,
            classification: Some(ClassificationV1::Internal),
            receipt: None,
        };
        let response = EngineEgressPrepareResponseV2 {
            schema_version: ENGINE_EGRESS_PREPARE_SCHEMA_VERSION,
            decision,
            prepared_body: Some(r#"{ "model" : "m" }"#.into()),
            admission_receipt_digest: None,
        };
        assert!(response.validate().is_ok());
        let legacy = serde_json::to_value(&response.decision).unwrap();
        assert!(legacy.get("admission_receipt_digest").is_none());
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["prepared_body"], r#"{ "model" : "m" }"#);
        let round_trip: EngineEgressPrepareResponseV2 =
            serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            round_trip.prepared_body.as_deref(),
            Some(r#"{ "model" : "m" }"#),
            "hosts send these exact UTF-8 bytes instead of reserializing the body"
        );
        let reserialized =
            serde_json::to_string(round_trip.decision.body.as_ref().unwrap()).unwrap();
        assert_ne!(
            round_trip.prepared_body.as_deref(),
            Some(reserialized.as_str())
        );
        let mut invalid = response.clone();
        invalid.schema_version = ENGINE_EGRESS_SCHEMA_VERSION;
        assert!(invalid.validate().is_err());
        invalid.schema_version = ENGINE_EGRESS_PREPARE_SCHEMA_VERSION;
        invalid.admission_receipt_digest = Some(digest);
        assert!(
            invalid.validate().is_err(),
            "digest needs a decision receipt"
        );
        let mut value = wire;
        value["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EngineEgressPrepareResponseV2>(value).is_err());
    }

    #[test]
    fn requests_need_an_object_body_and_the_current_schema() {
        let request = EngineEgressAdmissionRequestV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            provider: "openai".into(),
            upstream_base: "https://api.openai.com".into(),
            body: serde_json::json!({"model": "m", "messages": []}),
        };
        assert!(request.validate().is_ok());
        let mut array = request.clone();
        array.body = serde_json::json!([]);
        assert!(array.validate().is_err());
        let mut future = request;
        future.schema_version = 2;
        assert!(future.validate().is_err());
    }

    #[test]
    fn host_report_request_cannot_choose_its_observer() {
        let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let request = EngineEgressHostReportRequestV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            admission_receipt_digest: digest,
            attempt: 1,
            transport: EgressTransportV1::Http,
            result: EgressTransportResultV1::ResponseObserved,
            http_status: Some(503),
            failure: None,
        };
        assert!(request.validate().is_ok());
        let mut value = serde_json::to_value(&request).unwrap();
        value["observer"] = serde_json::json!("engine_observed");
        assert!(serde_json::from_value::<EngineEgressHostReportRequestV1>(value).is_err());

        let mut over_limit = request;
        over_limit.attempt = MAX_EGRESS_ATTEMPTS + 1;
        assert!(over_limit.validate().is_err());
    }

    #[test]
    fn host_report_response_is_bound_and_cannot_claim_engine_observation() {
        let digest = Sha256Digest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let observation = EgressTransportObservationV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            admission_receipt_digest: digest,
            attempt: 1,
            transport: EgressTransportV1::Http,
            observer: EgressTransportObserverV1::HostReported,
            result: EgressTransportResultV1::ResponseObserved,
            http_status: Some(200),
            failure: None,
        };
        let response = EngineEgressHostReportResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            observation_digest: observation.canonical_digest().unwrap(),
            observation,
        };
        assert!(response.validate().is_ok());
        let mut spoofed = response;
        spoofed.observation.observer = EgressTransportObserverV1::EngineObserved;
        assert!(spoofed.validate().is_err());
    }
}
