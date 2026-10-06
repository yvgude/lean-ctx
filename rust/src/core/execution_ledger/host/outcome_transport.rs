// SPDX-License-Identifier: Apache-2.0
//! Restricted wire adapter over the single operator outcome authority.

use anyhow::Result;
use lean_ctx_protocol::{
    EngineOutcomeRequestV1, EngineOutcomeResponseV1, EngineOutcomeSignalTypeV1,
    EngineOutcomeSignalValueV1, Sha256Digest,
};

use super::{HostOutcomeRequest, HostReceiptAuthority};
use crate::core::outcome::signals::{OutcomeSignal, SignalType, SignalValue};

impl HostReceiptAuthority {
    pub(crate) fn observe_engine_outcome(
        &self,
        request: &EngineOutcomeRequestV1,
    ) -> Result<EngineOutcomeResponseV1> {
        request.validate()?;
        let signals = request
            .signals
            .iter()
            .map(|signal| {
                let kind = match signal.signal_type {
                    EngineOutcomeSignalTypeV1::BuildSuccess => SignalType::BuildSuccess,
                    EngineOutcomeSignalTypeV1::TestsPassing => SignalType::TestsPassing,
                    EngineOutcomeSignalTypeV1::LintClean => SignalType::LintClean,
                    EngineOutcomeSignalTypeV1::TypecheckPassing => SignalType::TypecheckPassing,
                    EngineOutcomeSignalTypeV1::HumanAcceptance => SignalType::HumanAcceptance,
                    EngineOutcomeSignalTypeV1::PrMerge => SignalType::PrMerge,
                    EngineOutcomeSignalTypeV1::CiPassing => SignalType::CiPassing,
                    EngineOutcomeSignalTypeV1::Correction => SignalType::Correction,
                    EngineOutcomeSignalTypeV1::Rollback => SignalType::Rollback,
                };
                let value = match signal.value {
                    EngineOutcomeSignalValueV1::Boolean(value) => SignalValue::Boolean(value),
                    EngineOutcomeSignalValueV1::Count(value) => SignalValue::Count(value),
                    EngineOutcomeSignalValueV1::Unknown => SignalValue::Unknown,
                };
                OutcomeSignal::new(kind, value)
            })
            .collect();
        let result = self.observe_outcome_bound(
            &HostOutcomeRequest {
                schema_version: 1,
                receipt_digest: request.receipt_digest.clone(),
                context_decision_digest: request.context_decision_digest.clone(),
                signals,
                // Remote/SDK attestation never opts a tenant into personal learning.
                learn: false,
            },
            Some(&request.binding),
        )?;
        let receipt_document_json = self.published_receipt_json(&result.publication)?;
        let response = EngineOutcomeResponseV1 {
            schema_version: 1,
            original_receipt_digest: request.receipt_digest.clone(),
            receipt_id: Sha256Digest::new(result.publication.receipt_id)?,
            receipt_digest: Sha256Digest::new(result.publication.receipt_digest)?,
            acceptance: result.acceptance,
            already_recorded: result.already_recorded,
            receipt_document_json,
        };
        response.validate_against(request)?;
        Ok(response)
    }
}
