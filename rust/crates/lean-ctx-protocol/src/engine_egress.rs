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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::ValidationError;
use crate::context_gateway::{ClassificationV1, ContextDecisionReceiptV1};

pub const ENGINE_EGRESS_SCHEMA_VERSION: u32 = 1;
/// Upper bound for one request document (matches the proxy's body bound).
pub const MAX_ENGINE_EGRESS_REQUEST_BYTES: usize = 32 * 1024 * 1024;

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
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
