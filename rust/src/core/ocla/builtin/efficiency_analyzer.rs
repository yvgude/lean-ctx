// SPDX-License-Identifier: Apache-2.0
//! BuiltinEfficiencyAnalyzer — computes ETPAO and duplication metrics.
//!
//! Wraps `core/mode_predictor.rs` behind the OCLA trait. Computes Effective
//! Tokens Per Accepted Outcome (ETPAO) from a sample of compression results
//! paired with acceptance signals.

use crate::core::ocla::traits::{EfficiencyAnalyzer, OclaService};
use crate::core::ocla::types::{
    EfficiencyAnalysis, EfficiencySample, OclaCapability, OclaCapabilityKind, OclaResult,
};
use crate::core::{io_boundary, tokens};
use std::path::Path;

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
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::EfficiencyAnalyzer)
    }
}

impl EfficiencyAnalyzer for BuiltinEfficiencyAnalyzer {
    fn analyze_efficiency(&self, sample: EfficiencySample) -> OclaResult<EfficiencyAnalysis> {
        let original_tokens = measured_original_tokens(&sample);
        let etpao = if sample.accepted == Some(true) && sample.delivered_tokens > 0 {
            Some(sample.delivered_tokens.saturating_mul(1000) / original_tokens.max(1))
        } else {
            None
        };

        let compression_rate = if original_tokens > 0 {
            let savings = original_tokens.saturating_sub(sample.delivered_tokens);
            #[allow(clippy::cast_possible_truncation)]
            let ratio = (savings.saturating_mul(1000) / original_tokens).min(1000) as u16;
            ratio
        } else {
            0
        };

        #[allow(clippy::cast_possible_truncation)]
        let cache_hit_rate = sample
            .cache_hits
            .saturating_mul(1000)
            .checked_div(sample.cache_reads)
            .unwrap_or(0)
            .min(1000) as u16;

        #[allow(clippy::cast_possible_truncation)]
        let duplicate_ratio = sample
            .cache_hits
            .saturating_mul(1000)
            .checked_div(sample.cache_reads)
            .unwrap_or(0)
            .min(1000) as u16;

        let mut recommendation_refs = Vec::new();
        if compression_rate < 300 {
            recommendation_refs.push("increase_compression_aggressiveness".into());
        }
        if cache_hit_rate > 800 {
            recommendation_refs.push("leverage_high_cache_hit_rate".into());
        }
        if matches!(etpao, Some(value) if value > 0 && value < 500) {
            recommendation_refs.push("review_low_etpao_reads".into());
        }

        Ok(EfficiencyAnalysis {
            etpao_milli: etpao,
            duplicate_ratio_milli: duplicate_ratio,
            compression_rate_milli: compression_rate,
            cache_hit_rate_milli: cache_hit_rate,
            recommendation_refs,
        })
    }
}

fn measured_original_tokens(sample: &EfficiencySample) -> u64 {
    let path = sample
        .context
        .content_ref
        .strip_prefix("file:")
        .unwrap_or(&sample.context.content_ref);
    if Path::new(path).is_file()
        && let Ok(content) = io_boundary::read_file_lossy(path)
    {
        return tokens::count_tokens(&content) as u64;
    }
    sample.original_tokens
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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            original_tokens: original,
            delivered_tokens: delivered,
            accepted,
            cache_hits: 0,
            cache_reads: 0,
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
        let mut input = sample(1000, 250, Some(true));
        input.cache_hits = 3;
        input.cache_reads = 4;

        let result = analyzer.analyze_efficiency(input).unwrap();

        assert_eq!(result.duplicate_ratio_milli, 750);
        assert_eq!(result.compression_rate_milli, 750);
    }

    #[test]
    fn recommendations_follow_efficiency_thresholds() {
        let mut input = sample(1000, 800, Some(true));
        input.cache_hits = 9;
        input.cache_reads = 10;

        let result = BuiltinEfficiencyAnalyzer::new()
            .analyze_efficiency(input)
            .unwrap();

        assert_eq!(
            result.recommendation_refs,
            vec![
                "increase_compression_aggressiveness",
                "leverage_high_cache_hit_rate",
            ]
        );

        let result = BuiltinEfficiencyAnalyzer::new()
            .analyze_efficiency(sample(1000, 100, Some(true)))
            .unwrap();
        assert_eq!(result.recommendation_refs, vec!["review_low_etpao_reads"]);
    }

    #[test]
    fn registry_with_builtins_analyzes_efficiency() {
        let registry = crate::core::ocla::OclaRegistry::with_builtins();
        let result = registry
            .efficiency_analyzer
            .analyze_efficiency(sample(1000, 200, Some(true)))
            .unwrap();

        assert_eq!(result.etpao_milli, Some(200));
        assert_eq!(result.compression_rate_milli, 800);
    }

    #[test]
    fn cache_hit_rate_uses_observed_reads() {
        let analyzer = BuiltinEfficiencyAnalyzer::new();
        let mut input = sample(1000, 400, Some(true));
        input.cache_hits = 3;
        input.cache_reads = 4;

        let result = analyzer.analyze_efficiency(input).unwrap();

        assert_eq!(result.cache_hit_rate_milli, 750);
    }

    #[test]
    fn original_tokens_are_measured_from_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.rs");
        std::fs::write(&path, "hello world").unwrap();
        let mut input = sample(1000, 1, Some(true));
        input.context.content_ref = format!("file:{}", path.display());

        let result = BuiltinEfficiencyAnalyzer::new()
            .analyze_efficiency(input)
            .unwrap();

        assert_eq!(result.etpao_milli, Some(500));
        assert_eq!(result.compression_rate_milli, 500);
    }
}
