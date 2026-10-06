use crate::common::{
    ReceiptId, TaskId, ValidationError, deserialize_milliunit, deserialize_schema_version,
    validate_milliunit, validate_schema_version,
};
use crate::execution::{ContextBalanceV1, ExecutionReceiptV1};
use crate::{EvidenceRefV1, MoneyV1, UsageBreakdownV1};
use serde::{Deserialize, Serialize};

/// What OSS emits. NOT VerifiedSavings (that's proprietary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavingsObservationV1 {
    pub observation_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub original_usage: UsageBreakdownV1,
    pub actual_usage: UsageBreakdownV1,
    pub local_cost_estimate: MoneyV1,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub observed_at: String,
    pub runtime_version: String,
    pub sequence_number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementMethod {
    ProviderReported,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavingsReceiptV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub savings_id: String,
    pub task_id: TaskId,
    pub baseline_receipt_id: ReceiptId,
    pub treatment_receipt_id: ReceiptId,
    pub baseline_cost_micros: u64,
    pub treatment_cost_micros: u64,
    pub avoided_cost_micros: u64,
    pub baseline_tokens: ContextBalanceV1,
    pub treatment_tokens: ContextBalanceV1,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub token_savings_ratio_milli: u16,
    pub quality_preserved: bool,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub quality_baseline_score_milli: u16,
    #[serde(deserialize_with = "deserialize_milliunit")]
    pub quality_treatment_score_milli: u16,
    pub measurement_method: MeasurementMethod,
    pub context_strategy: String,
    pub methodology_version: String,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub decision_refs: Vec<String>,
    pub signature: String,
}

impl SavingsReceiptV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_schema_version(self.schema_version)?;
        self.baseline_tokens.validate()?;
        self.treatment_tokens.validate()?;
        validate_milliunit(self.token_savings_ratio_milli, "token_savings_ratio_milli")?;
        validate_milliunit(
            self.quality_baseline_score_milli,
            "quality_baseline_score_milli",
        )?;
        validate_milliunit(
            self.quality_treatment_score_milli,
            "quality_treatment_score_milli",
        )?;

        if self.avoided_cost_micros > self.baseline_cost_micros {
            return Err(ValidationError::new(
                "avoided_cost_micros exceeds baseline_cost_micros",
            ));
        }

        if self.quality_preserved
            != (self.quality_treatment_score_milli >= self.quality_baseline_score_milli)
        {
            return Err(ValidationError::new(
                "quality_preserved does not match quality scores",
            ));
        }

        Ok(())
    }

    pub fn compute_from_arms(
        baseline: &ExecutionReceiptV1,
        treatment: &ExecutionReceiptV1,
    ) -> Self {
        let baseline_cost_micros = baseline.actual_cost_micros;
        let treatment_cost_micros = treatment.actual_cost_micros;
        let baseline_provider_tokens = baseline.context_balance.provider_billed_tokens;
        let treatment_provider_tokens = treatment.context_balance.provider_billed_tokens;
        let savings_id = format!(
            "savings-{}-{}",
            baseline.receipt_id.as_str(),
            treatment.receipt_id.as_str()
        );

        let mut evidence_refs = baseline.evidence_refs.clone();
        evidence_refs.extend(treatment.evidence_refs.iter().cloned());
        let mut decision_refs = baseline.decision_refs.clone();
        decision_refs.extend(treatment.decision_refs.iter().cloned());

        Self {
            schema_version: baseline.schema_version,
            savings_id,
            task_id: baseline.task_id.clone(),
            baseline_receipt_id: baseline.receipt_id.clone(),
            treatment_receipt_id: treatment.receipt_id.clone(),
            baseline_cost_micros,
            treatment_cost_micros,
            avoided_cost_micros: baseline_cost_micros.saturating_sub(treatment_cost_micros),
            baseline_tokens: baseline.context_balance.clone(),
            treatment_tokens: treatment.context_balance.clone(),
            token_savings_ratio_milli: savings_ratio_milli(
                baseline_provider_tokens,
                treatment_provider_tokens,
            ),
            quality_preserved: true,
            quality_baseline_score_milli: 0,
            quality_treatment_score_milli: 0,
            measurement_method: MeasurementMethod::ProviderReported,
            context_strategy: String::new(),
            methodology_version: "savings-receipt-v1".to_owned(),
            evidence_refs,
            decision_refs,
            signature: String::new(),
        }
    }
}

fn savings_ratio_milli(baseline_tokens: u64, treatment_tokens: u64) -> u16 {
    if baseline_tokens == 0 {
        return 0;
    }

    let avoided_tokens = baseline_tokens.saturating_sub(treatment_tokens) as u128;
    ((avoided_tokens * 1000) / baseline_tokens as u128).min(1000) as u16
}

#[cfg(test)]
mod tests {
    use crate::{MoneyV1, SavingsObservationV1, UsageBreakdownV1};

    #[test]
    fn serialization_round_trip() {
        let observation = SavingsObservationV1 {
            observation_id: "obs-1".to_owned(),
            provider_id: "provider".to_owned(),
            model_id: "model".to_owned(),
            original_usage: UsageBreakdownV1::default(),
            actual_usage: UsageBreakdownV1::default(),
            local_cost_estimate: MoneyV1 {
                currency: "USD".to_owned(),
                coefficient: 1,
                scale: 4,
            },
            evidence_refs: Vec::new(),
            observed_at: "2026-01-01T00:00:00Z".to_owned(),
            runtime_version: "1.0.0".to_owned(),
            sequence_number: 1,
        };
        let json = serde_json::to_string(&observation).expect("observation should serialize");
        let decoded = serde_json::from_str(&json).expect("observation should deserialize");
        assert_eq!(observation, decoded);
    }
}

#[cfg(test)]
mod savings_receipt_tests {
    use super::*;
    use crate::PlanId;

    fn id<T>(value: &str) -> T
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Debug,
    {
        value.parse().expect("valid identifier")
    }

    fn balance(provider_billed_tokens: u64) -> ContextBalanceV1 {
        ContextBalanceV1 {
            original_tokens: provider_billed_tokens,
            materialized_tokens: provider_billed_tokens,
            delivered_tokens: provider_billed_tokens,
            provider_billed_tokens,
        }
    }

    fn execution(
        receipt_id: &str,
        actual_cost_micros: u64,
        provider_billed_tokens: u64,
    ) -> ExecutionReceiptV1 {
        ExecutionReceiptV1 {
            schema_version: 1,
            receipt_id: id(receipt_id),
            task_id: id("task-1"),
            plan_id: PlanId::try_from("plan-1").expect("valid plan id"),
            context_balance: balance(provider_billed_tokens),
            fresh_input_tokens: provider_billed_tokens,
            cached_input_tokens: 0,
            output_tokens: 10,
            reasoning_tokens: 0,
            requested_model: "model-1".to_owned(),
            selected_model: "model-1".to_owned(),
            provider: "provider-1".to_owned(),
            capability_id: None,
            capability_version: None,
            model_calls: 1,
            retries: 0,
            latency_ms: 10,
            actual_cost_micros,
            baseline_cost_micros: actual_cost_micros,
            avoided_cost_micros: 0,
            etpao_milli: 0,
            outcome_ref: None,
            knowledge_refs: Vec::new(),
            decision_refs: vec!["decision-1".to_owned()],
            evidence_refs: Vec::new(),
            signature: "signature-1".to_owned(),
            executor_agent_id: None,
            attempt_id: None,
            context_plan_id: None,
            context_receipt_ref: None,
            capability_bindings: Vec::new(),
            observations: crate::ExecutionObservationsV1 {
                model_calls: Some(1),
                retries: Some(0),
                latency_ms: Some(10),
                actual_cost_micros: Some(actual_cost_micros),
            },
            extensions: Default::default(),
        }
    }

    #[test]
    fn serialization_round_trip() {
        let receipt = SavingsReceiptV1 {
            schema_version: 1,
            savings_id: "savings-1".to_owned(),
            task_id: id("task-1"),
            baseline_receipt_id: id("receipt-baseline"),
            treatment_receipt_id: id("receipt-treatment"),
            baseline_cost_micros: 10_000,
            treatment_cost_micros: 5_000,
            avoided_cost_micros: 5_000,
            baseline_tokens: balance(1_000),
            treatment_tokens: balance(600),
            token_savings_ratio_milli: 400,
            quality_preserved: true,
            quality_baseline_score_milli: 900,
            quality_treatment_score_milli: 900,
            measurement_method: MeasurementMethod::ProviderReported,
            context_strategy: "semantic".to_owned(),
            methodology_version: "method-v1".to_owned(),
            evidence_refs: Vec::new(),
            decision_refs: vec!["decision-1".to_owned()],
            signature: "signature-1".to_owned(),
        };

        let json = serde_json::to_string(&receipt).expect("receipt should serialize");
        let decoded: SavingsReceiptV1 =
            serde_json::from_str(&json).expect("receipt should deserialize");
        assert_eq!(receipt, decoded);
    }

    #[test]
    fn validation_rejects_avoided_cost_above_baseline() {
        let mut receipt = SavingsReceiptV1::compute_from_arms(
            &execution("receipt-baseline", 1_000, 1_000),
            &execution("receipt-treatment", 500, 500),
        );
        receipt.avoided_cost_micros = receipt.baseline_cost_micros + 1;

        assert!(receipt.validate().is_err());
    }

    #[test]
    fn compute_from_arms_uses_cost_and_token_deltas() {
        let baseline = execution("receipt-baseline", 10_000, 1_000);
        let treatment = execution("receipt-treatment", 6_000, 600);

        let receipt = SavingsReceiptV1::compute_from_arms(&baseline, &treatment);

        assert_eq!(receipt.task_id, baseline.task_id);
        assert_eq!(receipt.baseline_receipt_id, baseline.receipt_id);
        assert_eq!(receipt.treatment_receipt_id, treatment.receipt_id);
        assert_eq!(receipt.baseline_cost_micros, 10_000);
        assert_eq!(receipt.treatment_cost_micros, 6_000);
        assert_eq!(receipt.avoided_cost_micros, 4_000);
        assert_eq!(receipt.token_savings_ratio_milli, 400);
        assert_eq!(receipt.baseline_tokens, baseline.context_balance);
        assert_eq!(receipt.treatment_tokens, treatment.context_balance);
        assert!(receipt.validate().is_ok());
    }
}
