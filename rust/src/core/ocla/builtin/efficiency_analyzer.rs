//! BuiltinEfficiencyAnalyzer — computes ETPAO and duplication metrics.
//!
//! Wraps `core/mode_predictor.rs` behind the OCLA trait. Computes Effective
//! Tokens Per Accepted Outcome (ETPAO) from a sample of compression results
//! paired with acceptance signals.

use crate::core::ocla::traits::{EfficiencyAnalyzer, OclaService};
use crate::core::ocla::types::{
    EfficiencyAnalysis, EfficiencySample, OclaCapability, OclaCapabilityKind, OclaResult,
};

pub struct BuiltinEfficiencyAnalyzer;

impl BuiltinEfficiencyAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinEfficiencyAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinEfficiencyAnalyzer {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::EfficiencyAnalyzer)
    }
}

impl EfficiencyAnalyzer for BuiltinEfficiencyAnalyzer {
    fn analyze_efficiency(&self, sample: EfficiencySample) -> OclaResult<EfficiencyAnalysis> {
        let etpao = if sample.accepted == Some(true) && sample.delivered_tokens > 0 {
            Some(sample.delivered_tokens.saturating_mul(1000) / sample.original_tokens.max(1))
        } else {
            None
        };

        let dup_ratio = if sample.original_tokens > 0 {
            let savings = sample
                .original_tokens
                .saturating_sub(sample.delivered_tokens);
            #[allow(clippy::cast_possible_truncation)]
            let ratio = (savings.saturating_mul(1000) / sample.original_tokens) as u16;
            ratio
        } else {
            0
        };

        Ok(EfficiencyAnalysis {
            etpao_milli: etpao,
            duplicate_ratio_milli: dup_ratio,
            recommendation_refs: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn sample(original: u64, delivered: u64, accepted: Option<bool>) -> EfficiencySample {
        EfficiencySample {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            original_tokens: original,
            delivered_tokens: delivered,
            accepted,
        }
    }

    #[test]
    fn etpao_computed_when_accepted() {
        let analyzer = BuiltinEfficiencyAnalyzer::new();
        let result = analyzer
            .analyze_efficiency(sample(1000, 300, Some(true)))
            .unwrap();
        assert_eq!(result.etpao_milli, Some(300));
    }

    #[test]
    fn etpao_none_when_rejected() {
        let analyzer = BuiltinEfficiencyAnalyzer::new();
        let result = analyzer
            .analyze_efficiency(sample(1000, 300, Some(false)))
            .unwrap();
        assert_eq!(result.etpao_milli, None);
    }

    #[test]
    fn duplicate_ratio() {
        let analyzer = BuiltinEfficiencyAnalyzer::new();
        let result = analyzer
            .analyze_efficiency(sample(1000, 250, Some(true)))
            .unwrap();
        assert_eq!(result.duplicate_ratio_milli, 750);
    }
}
