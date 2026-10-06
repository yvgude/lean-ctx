// SPDX-License-Identifier: Apache-2.0

//! Via-specific, content-free savings receipt with non-overlapping attribution.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::common::{
    ValidationError, deserialize_schema_version, validate_bounded_opaque_identifier,
    validate_schema_version,
};
use crate::{MAX_SAFE_INTEGER, ProtocolReference, Sha256Digest};

pub const VIA_SAVINGS_RECEIPT_SCHEMA_V1: u32 = 1;
pub const MAX_VIA_RECEIPT_CONTEXTS: usize = 64;
pub const MAX_VIA_RECEIPT_PROOFS: usize = 64;

const RECEIPT_ID_DOMAIN: &[u8] = b"leanctx.via.savings-receipt.v1\0";
const MAX_ID_BYTES: usize = 256;
const MAX_VERSION_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaReceiptModeV1 {
    Bypass,
    Shadow,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaUsageStateV1 {
    Measured,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaQualityStateV1 {
    Passed,
    Failed,
    NotEvaluated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaFallbackReasonV1 {
    BrainUnavailable,
    BrainTimedOut,
    PlanRejected,
    ProviderUnavailable,
    StreamIncomplete,
    MeasurementUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViaBaselineDefinitionV1 {
    BypassActual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaVersionRefV1 {
    pub version: String,
    pub digest: Sha256Digest,
}

impl ViaVersionRefV1 {
    fn validate(&self, field: &str) -> Result<(), ValidationError> {
        validate_text(&self.version, field, MAX_VERSION_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaReceiptIdentityV1 {
    pub invocation_id: ProtocolReference,
    pub provider_id: String,
    pub model_id: String,
}

impl ViaReceiptIdentityV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_logical_reference(&self.invocation_id, "Via receipt invocation_id")?;
        validate_text(&self.provider_id, "Via receipt provider_id", MAX_ID_BYTES)?;
        validate_text(&self.model_id, "Via receipt model_id", MAX_ID_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaReceiptVersionsV1 {
    pub policy: ViaVersionRefV1,
    pub operator: ViaVersionRefV1,
    pub methodology: ViaVersionRefV1,
}

impl ViaReceiptVersionsV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        self.policy.validate("policy version")?;
        self.operator.validate("operator version")?;
        self.methodology.validate("methodology version")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaContextAttributionV1 {
    pub context_id: ProtocolReference,
    pub raw_observed_tokens: u64,
    pub source_selected_tokens: u64,
    pub representation_delivered_tokens: u64,
    pub reuse_delivered_tokens: u64,
    pub provider_context_final_tokens: u64,
    pub raw_observed_bytes: u64,
    pub remote_bytes_transferred: u64,
}

impl ViaContextAttributionV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_logical_reference(&self.context_id, "Via context_id")?;
        for (field, value) in [
            ("raw_observed_tokens", self.raw_observed_tokens),
            ("source_selected_tokens", self.source_selected_tokens),
            (
                "representation_delivered_tokens",
                self.representation_delivered_tokens,
            ),
            ("reuse_delivered_tokens", self.reuse_delivered_tokens),
            (
                "provider_context_final_tokens",
                self.provider_context_final_tokens,
            ),
            ("raw_observed_bytes", self.raw_observed_bytes),
            ("remote_bytes_transferred", self.remote_bytes_transferred),
        ] {
            validate_safe_integer(value, field)?;
        }
        if self.source_selected_tokens > self.raw_observed_tokens
            || self.representation_delivered_tokens > self.source_selected_tokens
            || self.reuse_delivered_tokens > self.representation_delivered_tokens
            || self.provider_context_final_tokens > self.reuse_delivered_tokens
        {
            return Err(ValidationError::new(
                "Via token checkpoints must be monotonically non-increasing",
            ));
        }
        if self.remote_bytes_transferred > self.raw_observed_bytes {
            return Err(ValidationError::new(
                "remote_bytes_transferred exceeds raw_observed_bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaStageTotalsV1 {
    pub source_avoidance_tokens: u64,
    pub representation_reduction_tokens: u64,
    pub upload_reuse_tokens: u64,
    pub inference_reduction_tokens: u64,
}

impl ViaStageTotalsV1 {
    pub fn from_contexts(contexts: &[ViaContextAttributionV1]) -> Result<Self, ValidationError> {
        let mut totals = Self {
            source_avoidance_tokens: 0,
            representation_reduction_tokens: 0,
            upload_reuse_tokens: 0,
            inference_reduction_tokens: 0,
        };
        for context in contexts {
            context.validate()?;
            totals.source_avoidance_tokens = checked_add(
                totals.source_avoidance_tokens,
                checked_sub(
                    context.raw_observed_tokens,
                    context.source_selected_tokens,
                    "source avoidance",
                )?,
                "source avoidance total",
            )?;
            totals.representation_reduction_tokens = checked_add(
                totals.representation_reduction_tokens,
                checked_sub(
                    context.source_selected_tokens,
                    context.representation_delivered_tokens,
                    "representation reduction",
                )?,
                "representation reduction total",
            )?;
            totals.upload_reuse_tokens = checked_add(
                totals.upload_reuse_tokens,
                checked_sub(
                    context.representation_delivered_tokens,
                    context.reuse_delivered_tokens,
                    "upload reuse",
                )?,
                "upload reuse total",
            )?;
            totals.inference_reduction_tokens = checked_add(
                totals.inference_reduction_tokens,
                checked_sub(
                    context.reuse_delivered_tokens,
                    context.provider_context_final_tokens,
                    "inference reduction",
                )?,
                "inference reduction total",
            )?;
        }
        Ok(totals)
    }

    pub fn total_avoided_tokens(&self) -> Result<u64, ValidationError> {
        checked_sum(
            [
                self.source_avoidance_tokens,
                self.representation_reduction_tokens,
                self.upload_reuse_tokens,
                self.inference_reduction_tokens,
            ],
            "stage total",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaBandwidthV1 {
    pub raw_observed_bytes: u64,
    pub remote_bytes_transferred: u64,
    pub avoided_bytes: u64,
}

impl ViaBandwidthV1 {
    pub fn from_contexts(contexts: &[ViaContextAttributionV1]) -> Result<Self, ValidationError> {
        let raw_observed_bytes = contexts.iter().try_fold(0_u64, |sum, context| {
            checked_add(sum, context.raw_observed_bytes, "raw byte total")
        })?;
        let remote_bytes_transferred = contexts.iter().try_fold(0_u64, |sum, context| {
            checked_add(sum, context.remote_bytes_transferred, "remote byte total")
        })?;
        let avoided_bytes = checked_sub(
            raw_observed_bytes,
            remote_bytes_transferred,
            "avoided bytes",
        )?;
        Ok(Self {
            raw_observed_bytes,
            remote_bytes_transferred,
            avoided_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaProviderUsageV1 {
    pub state: ViaUsageStateV1,
    pub uncached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub total_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl ViaProviderUsageV1 {
    pub fn measured(
        uncached_input_tokens: u64,
        cache_write_input_tokens: u64,
        cache_read_input_tokens: u64,
        output_tokens: u64,
    ) -> Result<Self, ValidationError> {
        let total_input_tokens = checked_sum(
            [
                uncached_input_tokens,
                cache_write_input_tokens,
                cache_read_input_tokens,
            ],
            "provider input total",
        )?;
        Ok(Self {
            state: ViaUsageStateV1::Measured,
            uncached_input_tokens: Some(uncached_input_tokens),
            cache_write_input_tokens: Some(cache_write_input_tokens),
            cache_read_input_tokens: Some(cache_read_input_tokens),
            total_input_tokens: Some(total_input_tokens),
            output_tokens: Some(output_tokens),
        })
    }

    pub fn unavailable() -> Self {
        Self {
            state: ViaUsageStateV1::Unavailable,
            uncached_input_tokens: None,
            cache_write_input_tokens: None,
            cache_read_input_tokens: None,
            total_input_tokens: None,
            output_tokens: None,
        }
    }

    pub(crate) fn validate(&self, field: &str) -> Result<(), ValidationError> {
        let values = [
            self.uncached_input_tokens,
            self.cache_write_input_tokens,
            self.cache_read_input_tokens,
            self.total_input_tokens,
            self.output_tokens,
        ];
        match self.state {
            ViaUsageStateV1::Unavailable => {
                if values.iter().any(Option::is_some) {
                    return Err(ValidationError::new(format!(
                        "{field} unavailable usage must omit every counter"
                    )));
                }
            }
            ViaUsageStateV1::Measured | ViaUsageStateV1::Estimated => {
                if values.iter().any(Option::is_none) {
                    return Err(ValidationError::new(format!(
                        "{field} measured/estimated usage requires every counter"
                    )));
                }
                for value in values.into_iter().flatten() {
                    validate_safe_integer(value, field)?;
                }
                let expected = checked_sum(
                    [
                        self.uncached_input_tokens.unwrap_or_default(),
                        self.cache_write_input_tokens.unwrap_or_default(),
                        self.cache_read_input_tokens.unwrap_or_default(),
                    ],
                    "provider input components",
                )?;
                if self.total_input_tokens != Some(expected) {
                    return Err(ValidationError::new(format!(
                        "{field} total_input_tokens does not match components"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaProviderUsageComparisonV1 {
    pub baseline_actual: ViaProviderUsageV1,
    pub treatment_actual: ViaProviderUsageV1,
    pub counterfactual_final_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaCacheArmV1 {
    pub context_size_tokens: u64,
    pub uncached_input_tokens: u64,
    pub cache_write_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub uncached_input_cost_micros: u64,
    pub cache_write_cost_micros: u64,
    pub cache_read_cost_micros: u64,
    pub total_billed_input_cost_micros: u64,
}

impl ViaCacheArmV1 {
    fn validate(&self, field: &str) -> Result<(), ValidationError> {
        let tokens = checked_sum(
            [
                self.uncached_input_tokens,
                self.cache_write_input_tokens,
                self.cache_read_input_tokens,
            ],
            field,
        )?;
        if tokens != self.context_size_tokens {
            return Err(ValidationError::new(format!(
                "{field} context size does not match cache token components"
            )));
        }
        let cost = checked_sum(
            [
                self.uncached_input_cost_micros,
                self.cache_write_cost_micros,
                self.cache_read_cost_micros,
            ],
            field,
        )?;
        if cost != self.total_billed_input_cost_micros {
            return Err(ValidationError::new(format!(
                "{field} billed cost does not match cache cost components"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaCacheEconomicsV1 {
    pub cost_basis: ViaVersionRefV1,
    pub baseline: ViaCacheArmV1,
    pub treatment: ViaCacheArmV1,
    pub adverse_cost_justification: Option<Sha256Digest>,
}

impl ViaCacheEconomicsV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        self.cost_basis.validate("cache cost basis")?;
        self.baseline.validate("baseline cache arm")?;
        self.treatment.validate("treatment cache arm")?;
        if self.treatment.total_billed_input_cost_micros
            > self.baseline.total_billed_input_cost_micros
            && self.adverse_cost_justification.is_none()
        {
            return Err(ValidationError::new(
                "adverse cache economics require a digest-bound justification",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaQualityResultV1 {
    pub state: ViaQualityStateV1,
    pub evidence_digest: Option<Sha256Digest>,
}

impl ViaQualityResultV1 {
    fn validate(&self) -> Result<(), ValidationError> {
        match self.state {
            ViaQualityStateV1::Passed | ViaQualityStateV1::Failed
                if self.evidence_digest.is_none() =>
            {
                Err(ValidationError::new(
                    "evaluated quality requires an evidence digest",
                ))
            }
            ViaQualityStateV1::NotEvaluated if self.evidence_digest.is_some() => Err(
                ValidationError::new("not-evaluated quality must omit evidence"),
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaFallbackV1 {
    pub reason: ViaFallbackReasonV1,
    pub evidence_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaLatencyV1 {
    pub via_latency_micros: u64,
    pub provider_latency_micros: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaSavingsReceiptPayloadV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub identity: ViaReceiptIdentityV1,
    pub versions: ViaReceiptVersionsV1,
    pub requested_mode: ViaReceiptModeV1,
    pub effective_mode: ViaReceiptModeV1,
    pub baseline_definition: ViaBaselineDefinitionV1,
    pub input_body_digest: Sha256Digest,
    pub forward_body_digest: Sha256Digest,
    pub contexts: Vec<ViaContextAttributionV1>,
    pub stage_totals: ViaStageTotalsV1,
    pub provider_usage: ViaProviderUsageComparisonV1,
    pub provider_cache_metrics: Option<ViaCacheEconomicsV1>,
    pub via_bandwidth: ViaBandwidthV1,
    pub latency: ViaLatencyV1,
    pub quality_result: ViaQualityResultV1,
    pub fallback: Option<ViaFallbackV1>,
    pub counterfactual_avoided_tokens: Option<u64>,
    pub actual_avoided_tokens: u64,
    pub proof_refs: Vec<Sha256Digest>,
}

impl ViaSavingsReceiptPayloadV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        if self.schema_version != VIA_SAVINGS_RECEIPT_SCHEMA_V1 {
            return Err(ValidationError::new(
                "unsupported Via savings receipt schema",
            ));
        }
        self.identity.validate()?;
        self.versions.validate()?;
        if self.contexts.is_empty() || self.contexts.len() > MAX_VIA_RECEIPT_CONTEXTS {
            return Err(ValidationError::new(
                "Via receipt contexts must contain 1..=64 items",
            ));
        }
        let mut context_ids = BTreeSet::new();
        for context in &self.contexts {
            context.validate()?;
            if !context_ids.insert(context.context_id.as_str()) {
                return Err(ValidationError::new("duplicate Via context attribution"));
            }
        }
        if self
            .contexts
            .windows(2)
            .any(|pair| pair[0].context_id.as_str() >= pair[1].context_id.as_str())
        {
            return Err(ValidationError::new(
                "Via receipt contexts must use stable context_id order",
            ));
        }
        let expected_stages = ViaStageTotalsV1::from_contexts(&self.contexts)?;
        if self.stage_totals != expected_stages {
            return Err(ValidationError::new(
                "stage totals do not match adjacent context checkpoints",
            ));
        }
        let expected_bandwidth = ViaBandwidthV1::from_contexts(&self.contexts)?;
        if self.via_bandwidth != expected_bandwidth {
            return Err(ValidationError::new(
                "Via bandwidth does not match context byte checkpoints",
            ));
        }
        validate_safe_integer(self.latency.via_latency_micros, "Via latency")?;
        if let Some(value) = self.latency.provider_latency_micros {
            validate_safe_integer(value, "provider latency")?;
        }
        self.quality_result.validate()?;
        validate_proofs(&self.proof_refs)?;
        self.provider_usage
            .baseline_actual
            .validate("baseline provider usage")?;
        self.provider_usage
            .treatment_actual
            .validate("treatment provider usage")?;
        if let Some(value) = self.provider_usage.counterfactual_final_input_tokens {
            validate_safe_integer(value, "counterfactual provider input")?;
        }
        if let Some(cache) = &self.provider_cache_metrics {
            cache.validate()?;
            validate_cache_usage_match(cache, &self.provider_usage)?;
        }
        validate_evidence_bindings(self)?;
        self.validate_mode_reconciliation()
    }

    fn validate_mode_reconciliation(&self) -> Result<(), ValidationError> {
        let raw = sum_context_tokens(&self.contexts, |value| value.raw_observed_tokens)?;
        let final_context =
            sum_context_tokens(&self.contexts, |value| value.provider_context_final_tokens)?;
        let avoided = self.stage_totals.total_avoided_tokens()?;
        if avoided != checked_sub(raw, final_context, "total avoided tokens")? {
            return Err(ValidationError::new(
                "stage totals do not telescope to raw minus provider final",
            ));
        }
        let baseline = self.provider_usage.baseline_actual.total_input_tokens;
        let treatment = self.provider_usage.treatment_actual.total_input_tokens;
        if self.provider_usage.baseline_actual.state == ViaUsageStateV1::Measured
            && baseline != Some(raw)
        {
            return Err(ValidationError::new(
                "measured BYPASS usage does not match raw observed tokens",
            ));
        }
        let valid_transition = matches!(
            (self.requested_mode, self.effective_mode),
            (ViaReceiptModeV1::Bypass, ViaReceiptModeV1::Bypass)
                | (ViaReceiptModeV1::Shadow, ViaReceiptModeV1::Shadow)
                | (ViaReceiptModeV1::Shadow, ViaReceiptModeV1::Bypass)
                | (ViaReceiptModeV1::On, ViaReceiptModeV1::On)
                | (ViaReceiptModeV1::On, ViaReceiptModeV1::Bypass)
        );
        if !valid_transition {
            return Err(ValidationError::new(
                "invalid requested/effective Via mode transition",
            ));
        }
        if (self.requested_mode != self.effective_mode) != self.fallback.is_some() {
            return Err(ValidationError::new(
                "changed effective mode requires exactly one fallback",
            ));
        }
        match self.effective_mode {
            ViaReceiptModeV1::Bypass => {
                if avoided != 0
                    || self.input_body_digest != self.forward_body_digest
                    || self.provider_usage.treatment_actual != self.provider_usage.baseline_actual
                    || self.counterfactual_avoided_tokens.is_some()
                    || self.actual_avoided_tokens != 0
                {
                    return Err(ValidationError::new(
                        "BYPASS/fallback receipt cannot claim or apply savings",
                    ));
                }
            }
            ViaReceiptModeV1::Shadow => {
                if self.provider_usage.treatment_actual != self.provider_usage.baseline_actual
                    || self.input_body_digest != self.forward_body_digest
                    || self.provider_usage.counterfactual_final_input_tokens != Some(final_context)
                    || self.counterfactual_avoided_tokens != Some(avoided)
                    || self.actual_avoided_tokens != 0
                {
                    return Err(ValidationError::new(
                        "SHADOW must preserve actual usage and separate its counterfactual",
                    ));
                }
            }
            ViaReceiptModeV1::On => {
                if self
                    .provider_usage
                    .counterfactual_final_input_tokens
                    .is_some()
                {
                    return Err(ValidationError::new(
                        "ON receipt must reconcile the applied provider context directly",
                    ));
                }
                if self.provider_usage.treatment_actual.state == ViaUsageStateV1::Measured
                    && treatment != Some(final_context)
                {
                    return Err(ValidationError::new(
                        "measured ON usage does not match provider final tokens",
                    ));
                }
                let claim_allowed = self.quality_result.state == ViaQualityStateV1::Passed
                    && self.fallback.is_none()
                    && self.provider_usage.baseline_actual.state == ViaUsageStateV1::Measured
                    && self.provider_usage.treatment_actual.state == ViaUsageStateV1::Measured
                    && self.provider_cache_metrics.is_some();
                let expected_actual = if claim_allowed { avoided } else { 0 };
                if self.actual_avoided_tokens != expected_actual
                    || self.counterfactual_avoided_tokens.is_some()
                {
                    return Err(ValidationError::new(
                        "ON actual savings do not match quality/usage/cache gates",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViaSavingsReceiptV1 {
    pub receipt_id: Sha256Digest,
    pub payload: ViaSavingsReceiptPayloadV1,
}

impl ViaSavingsReceiptV1 {
    pub fn new(payload: ViaSavingsReceiptPayloadV1) -> Result<Self, ValidationError> {
        payload.validate()?;
        let receipt_id = derive_receipt_id(&payload)?;
        Ok(Self {
            receipt_id,
            payload,
        })
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        self.payload.validate()?;
        if self.receipt_id != derive_receipt_id(&self.payload)? {
            return Err(ValidationError::new(
                "Via savings receipt_id does not match canonical payload",
            ));
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| ValidationError::new(format!("encode Via receipt: {error}")))
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ValidationError> {
        let receipt: Self = serde_json::from_slice(bytes)
            .map_err(|error| ValidationError::new(format!("decode Via receipt: {error}")))?;
        receipt.validate()?;
        Ok(receipt)
    }
}

fn derive_receipt_id(
    payload: &ViaSavingsReceiptPayloadV1,
) -> Result<Sha256Digest, ValidationError> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| ValidationError::new(format!("encode Via receipt payload: {error}")))?;
    let mut hash = Sha256::new();
    hash.update(RECEIPT_ID_DOMAIN);
    hash.update(bytes);
    let mut value = String::from("sha256:");
    for byte in hash.finalize() {
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(value)
}

fn validate_cache_usage_match(
    cache: &ViaCacheEconomicsV1,
    usage: &ViaProviderUsageComparisonV1,
) -> Result<(), ValidationError> {
    for (field, arm, observed) in [
        ("baseline", &cache.baseline, &usage.baseline_actual),
        ("treatment", &cache.treatment, &usage.treatment_actual),
    ] {
        if observed.state != ViaUsageStateV1::Measured
            || observed.uncached_input_tokens != Some(arm.uncached_input_tokens)
            || observed.cache_write_input_tokens != Some(arm.cache_write_input_tokens)
            || observed.cache_read_input_tokens != Some(arm.cache_read_input_tokens)
            || observed.total_input_tokens != Some(arm.context_size_tokens)
        {
            return Err(ValidationError::new(format!(
                "{field} cache metrics do not match measured provider usage"
            )));
        }
    }
    Ok(())
}

fn validate_proofs(proofs: &[Sha256Digest]) -> Result<(), ValidationError> {
    if proofs.is_empty() || proofs.len() > MAX_VIA_RECEIPT_PROOFS {
        return Err(ValidationError::new(
            "Via receipt proof_refs must contain 1..=64 digests",
        ));
    }
    let mut unique = BTreeSet::new();
    for proof in proofs {
        if !unique.insert(proof.as_str()) {
            return Err(ValidationError::new("duplicate Via receipt proof digest"));
        }
    }
    if proofs
        .windows(2)
        .any(|pair| pair[0].as_str() >= pair[1].as_str())
    {
        return Err(ValidationError::new(
            "Via receipt proof_refs must use stable digest order",
        ));
    }
    Ok(())
}

fn validate_evidence_bindings(payload: &ViaSavingsReceiptPayloadV1) -> Result<(), ValidationError> {
    let proofs = payload
        .proof_refs
        .iter()
        .map(Sha256Digest::as_str)
        .collect::<BTreeSet<_>>();
    let mut required = Vec::new();
    if let Some(digest) = payload.quality_result.evidence_digest.as_ref() {
        required.push(("quality evidence", digest));
    }
    if let Some(fallback) = payload.fallback.as_ref() {
        required.push(("fallback evidence", &fallback.evidence_digest));
    }
    if let Some(cache) = payload.provider_cache_metrics.as_ref() {
        required.push(("cache cost basis", &cache.cost_basis.digest));
        if let Some(digest) = cache.adverse_cost_justification.as_ref() {
            required.push(("cache justification", digest));
        }
    }
    for (field, digest) in required {
        if !proofs.contains(digest.as_str()) {
            return Err(ValidationError::new(format!(
                "{field} digest is absent from proof_refs"
            )));
        }
    }
    Ok(())
}

fn validate_logical_reference(
    value: &ProtocolReference,
    field: &str,
) -> Result<(), ValidationError> {
    validate_bounded_opaque_identifier(value.as_str(), field)?;
    validate_text(value.as_str(), field, MAX_ID_BYTES)
}

fn validate_text(value: &str, field: &str, maximum: usize) -> Result<(), ValidationError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(ValidationError::new(format!(
            "{field} must be non-empty, printable and at most {maximum} bytes"
        )));
    }
    if value.starts_with('/')
        || value.starts_with("~/")
        || value.contains("../")
        || value.contains('\\')
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain a filesystem path"
        )));
    }
    Ok(())
}

fn validate_safe_integer(value: u64, field: &str) -> Result<(), ValidationError> {
    if value > MAX_SAFE_INTEGER {
        return Err(ValidationError::new(format!(
            "{field} exceeds the cross-language safe integer ceiling"
        )));
    }
    Ok(())
}

fn checked_add(left: u64, right: u64, field: &str) -> Result<u64, ValidationError> {
    let value = left
        .checked_add(right)
        .ok_or_else(|| ValidationError::new(format!("{field} overflowed")))?;
    validate_safe_integer(value, field)?;
    Ok(value)
}

fn checked_sub(left: u64, right: u64, field: &str) -> Result<u64, ValidationError> {
    left.checked_sub(right)
        .ok_or_else(|| ValidationError::new(format!("{field} underflowed")))
}

fn checked_sum<const N: usize>(values: [u64; N], field: &str) -> Result<u64, ValidationError> {
    values
        .into_iter()
        .try_fold(0_u64, |sum, value| checked_add(sum, value, field))
}

fn sum_context_tokens(
    contexts: &[ViaContextAttributionV1],
    select: impl Fn(&ViaContextAttributionV1) -> u64,
) -> Result<u64, ValidationError> {
    contexts.iter().try_fold(0_u64, |sum, context| {
        checked_add(sum, select(context), "context token total")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: u8) -> Sha256Digest {
        Sha256Digest::new(format!("sha256:{}", format!("{seed:02x}").repeat(32))).unwrap()
    }

    fn context() -> ViaContextAttributionV1 {
        ViaContextAttributionV1 {
            context_id: ProtocolReference::new("context-1").unwrap(),
            raw_observed_tokens: 40_000,
            source_selected_tokens: 8_000,
            representation_delivered_tokens: 6_500,
            reuse_delivered_tokens: 4_500,
            provider_context_final_tokens: 3_900,
            raw_observed_bytes: 160_000,
            remote_bytes_transferred: 15_600,
        }
    }

    fn usage(total: u64) -> ViaProviderUsageV1 {
        ViaProviderUsageV1::measured(total, 0, 0, 8).unwrap()
    }

    fn cache(total: u64, cost: u64) -> ViaCacheArmV1 {
        ViaCacheArmV1 {
            context_size_tokens: total,
            uncached_input_tokens: total,
            cache_write_input_tokens: 0,
            cache_read_input_tokens: 0,
            uncached_input_cost_micros: cost,
            cache_write_cost_micros: 0,
            cache_read_cost_micros: 0,
            total_billed_input_cost_micros: cost,
        }
    }

    fn payload(mode: ViaReceiptModeV1) -> ViaSavingsReceiptPayloadV1 {
        let contexts = if mode == ViaReceiptModeV1::Bypass {
            vec![ViaContextAttributionV1 {
                source_selected_tokens: 40_000,
                representation_delivered_tokens: 40_000,
                reuse_delivered_tokens: 40_000,
                provider_context_final_tokens: 40_000,
                remote_bytes_transferred: 160_000,
                ..context()
            }]
        } else {
            vec![context()]
        };
        let stage_totals = ViaStageTotalsV1::from_contexts(&contexts).unwrap();
        let via_bandwidth = ViaBandwidthV1::from_contexts(&contexts).unwrap();
        let final_tokens = contexts[0].provider_context_final_tokens;
        let treatment = if mode == ViaReceiptModeV1::Shadow {
            usage(40_000)
        } else {
            usage(final_tokens)
        };
        ViaSavingsReceiptPayloadV1 {
            schema_version: 1,
            identity: ViaReceiptIdentityV1 {
                invocation_id: ProtocolReference::new("invocation-1").unwrap(),
                provider_id: "anthropic".to_owned(),
                model_id: "claude-haiku".to_owned(),
            },
            versions: ViaReceiptVersionsV1 {
                policy: ViaVersionRefV1 {
                    version: "1.0.0".to_owned(),
                    digest: digest(1),
                },
                operator: ViaVersionRefV1 {
                    version: "1.0.0".to_owned(),
                    digest: digest(2),
                },
                methodology: ViaVersionRefV1 {
                    version: "1.0.0".to_owned(),
                    digest: digest(3),
                },
            },
            requested_mode: mode,
            effective_mode: mode,
            baseline_definition: ViaBaselineDefinitionV1::BypassActual,
            input_body_digest: digest(4),
            forward_body_digest: if mode == ViaReceiptModeV1::On {
                digest(5)
            } else {
                digest(4)
            },
            contexts,
            stage_totals,
            provider_usage: ViaProviderUsageComparisonV1 {
                baseline_actual: usage(40_000),
                treatment_actual: treatment,
                counterfactual_final_input_tokens: (mode == ViaReceiptModeV1::Shadow)
                    .then_some(final_tokens),
            },
            provider_cache_metrics: (mode == ViaReceiptModeV1::On).then(|| ViaCacheEconomicsV1 {
                cost_basis: ViaVersionRefV1 {
                    version: "2026-08-28".to_owned(),
                    digest: digest(10),
                },
                baseline: cache(40_000, 4_000),
                treatment: cache(final_tokens, 390),
                adverse_cost_justification: None,
            }),
            via_bandwidth,
            latency: ViaLatencyV1 {
                via_latency_micros: 200,
                provider_latency_micros: Some(2_000),
            },
            quality_result: ViaQualityResultV1 {
                state: if mode == ViaReceiptModeV1::On {
                    ViaQualityStateV1::Passed
                } else {
                    ViaQualityStateV1::NotEvaluated
                },
                evidence_digest: (mode == ViaReceiptModeV1::On).then(|| digest(6)),
            },
            fallback: None,
            counterfactual_avoided_tokens: (mode == ViaReceiptModeV1::Shadow).then_some(36_100),
            actual_avoided_tokens: if mode == ViaReceiptModeV1::On {
                36_100
            } else {
                0
            },
            proof_refs: if mode == ViaReceiptModeV1::On {
                vec![digest(6), digest(7), digest(8), digest(10)]
            } else {
                vec![digest(7), digest(8)]
            },
        }
    }

    #[test]
    fn authority_example_attributes_each_token_once() {
        let totals = ViaStageTotalsV1::from_contexts(&[context()]).unwrap();
        assert_eq!(totals.source_avoidance_tokens, 32_000);
        assert_eq!(totals.representation_reduction_tokens, 1_500);
        assert_eq!(totals.upload_reuse_tokens, 2_000);
        assert_eq!(totals.inference_reduction_tokens, 600);
        assert_eq!(totals.total_avoided_tokens().unwrap(), 36_100);
        ViaSavingsReceiptV1::new(payload(ViaReceiptModeV1::On)).unwrap();
    }

    #[test]
    fn bypass_and_shadow_never_claim_actual_savings() {
        for mode in [ViaReceiptModeV1::Bypass, ViaReceiptModeV1::Shadow] {
            let receipt = ViaSavingsReceiptV1::new(payload(mode)).unwrap();
            assert_eq!(receipt.payload.actual_avoided_tokens, 0);
        }
    }

    #[test]
    fn unavailable_usage_is_distinct_from_measured_zero() {
        ViaProviderUsageV1::unavailable()
            .validate("unavailable")
            .unwrap();
        let measured_zero = ViaProviderUsageV1::measured(0, 0, 0, 0).unwrap();
        assert_ne!(ViaProviderUsageV1::unavailable(), measured_zero);
        let mut invalid = ViaProviderUsageV1::unavailable();
        invalid.total_input_tokens = Some(0);
        assert!(invalid.validate("invalid").is_err());
    }

    #[test]
    fn wp019_measured_reduction_reconciles() {
        let mut value = payload(ViaReceiptModeV1::On);
        value.contexts[0] = ViaContextAttributionV1 {
            context_id: ProtocolReference::new("wp019-context").unwrap(),
            raw_observed_tokens: 165,
            source_selected_tokens: 165,
            representation_delivered_tokens: 165,
            reuse_delivered_tokens: 165,
            provider_context_final_tokens: 138,
            raw_observed_bytes: 660,
            remote_bytes_transferred: 552,
        };
        value.stage_totals = ViaStageTotalsV1::from_contexts(&value.contexts).unwrap();
        value.via_bandwidth = ViaBandwidthV1::from_contexts(&value.contexts).unwrap();
        value.provider_usage.baseline_actual = usage(165);
        value.provider_usage.treatment_actual = usage(138);
        value.provider_cache_metrics = Some(ViaCacheEconomicsV1 {
            cost_basis: ViaVersionRefV1 {
                version: "2026-08-28".to_owned(),
                digest: digest(10),
            },
            baseline: cache(165, 165),
            treatment: cache(138, 138),
            adverse_cost_justification: None,
        });
        value.actual_avoided_tokens = 27;
        ViaSavingsReceiptV1::new(value).unwrap();
    }

    #[test]
    fn rejects_non_monotone_duplicate_and_adverse_unjustified_data() {
        let mut non_monotone = context();
        non_monotone.source_selected_tokens = non_monotone.raw_observed_tokens + 1;
        assert!(non_monotone.validate().is_err());

        let mut duplicate = payload(ViaReceiptModeV1::On);
        duplicate.contexts.push(duplicate.contexts[0].clone());
        duplicate.stage_totals = ViaStageTotalsV1::from_contexts(&duplicate.contexts).unwrap();
        duplicate.via_bandwidth = ViaBandwidthV1::from_contexts(&duplicate.contexts).unwrap();
        assert!(duplicate.validate().is_err());

        let mut adverse = payload(ViaReceiptModeV1::On);
        adverse.provider_cache_metrics.as_mut().unwrap().treatment = cache(3_900, 5_000);
        assert!(adverse.validate().is_err());
        adverse
            .provider_cache_metrics
            .as_mut()
            .unwrap()
            .adverse_cost_justification = Some(digest(9));
        adverse.proof_refs.insert(3, digest(9));
        ViaSavingsReceiptV1::new(adverse).unwrap();
    }

    #[test]
    fn quality_fallback_and_identity_gate_actual_claims() {
        let mut failed = payload(ViaReceiptModeV1::On);
        failed.quality_result.state = ViaQualityStateV1::Failed;
        assert!(failed.validate().is_err());
        failed.actual_avoided_tokens = 0;
        ViaSavingsReceiptV1::new(failed).unwrap();

        let mut fallback = payload(ViaReceiptModeV1::Bypass);
        fallback.requested_mode = ViaReceiptModeV1::On;
        fallback.fallback = Some(ViaFallbackV1 {
            reason: ViaFallbackReasonV1::BrainTimedOut,
            evidence_digest: digest(9),
        });
        fallback.proof_refs.push(digest(9));
        ViaSavingsReceiptV1::new(fallback).unwrap();

        let receipt = ViaSavingsReceiptV1::new(payload(ViaReceiptModeV1::On)).unwrap();
        let same = ViaSavingsReceiptV1::new(receipt.payload.clone()).unwrap();
        assert_eq!(receipt.receipt_id, same.receipt_id);
        let bytes = receipt.canonical_json().unwrap();
        assert_eq!(ViaSavingsReceiptV1::from_json(&bytes).unwrap(), receipt);
        let mut tampered = receipt;
        tampered.payload.latency.via_latency_micros += 1;
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn strict_wire_and_privacy_canaries() {
        let receipt = ViaSavingsReceiptV1::new(payload(ViaReceiptModeV1::On)).unwrap();
        let bytes = receipt.canonical_json().unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unknown".to_owned(), serde_json::Value::Bool(true));
        assert!(ViaSavingsReceiptV1::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
        let wire = String::from_utf8(bytes).unwrap();
        for canary in [
            "sk-secret-canary",
            "prompt-secret-canary",
            "/private/secret/path",
            "authorization",
            "cookie-secret-canary",
            "request-body-secret-canary",
        ] {
            assert!(!wire.contains(canary));
        }

        let mut path = payload(ViaReceiptModeV1::On);
        path.identity.invocation_id = ProtocolReference::new("/private/secret/path").unwrap();
        assert!(path.validate().is_err());

        let mut oversized = context();
        oversized.raw_observed_tokens = MAX_SAFE_INTEGER + 1;
        assert!(oversized.validate().is_err());
    }
}
