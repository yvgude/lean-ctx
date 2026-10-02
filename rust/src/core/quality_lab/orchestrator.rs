use std::sync::atomic::Ordering;

use crate::core::context_kernel::proxy_bridge;
use crate::core::context_quality::{ContextQualityReceiptV1, EvidenceTier, RecoveryPath};
use crate::core::telemetry::global_metrics;

use super::calibration::{CalibratedCount, compare_calibration};
use super::fidelity::assess_fidelity;

/// Compression quality and savings measured for one input/output pair.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct InputCompressionMetrics {
    pub modes_tested: usize,
    pub best_mode: String,
    pub best_savings_pct: f64,
    pub avg_savings_pct: f64,
    pub avg_preservation_score: f64,
    pub fidelity_class: String,
    pub quality_gate_passed: bool,
}

/// Hit rates and token savings across the three cache layers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct CacheEffectivenessMetrics {
    pub session_cache_hit_rate: f64,
    pub content_cache_hit_rate: f64,
    pub response_cache_hit_rate: f64,
    pub aggregate_hit_rate: f64,
    pub estimated_token_savings: u64,
}

/// Cross-family tokenizer calibration summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TokenizerCalibrationMetrics {
    pub families_tested: usize,
    pub max_cross_family_variance_pct: f64,
    pub dominant_family: String,
    pub dominant_accuracy: String,
}

/// Runtime effective-tokens-per-accepted-outcome summary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct EtpaoSummary {
    pub current_etpao: Option<f64>,
    pub savings_rate_pct: f64,
    pub total_events: u64,
    pub quality_gate: String,
}

/// Unified quality lab report aggregating all measurement pillars.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct QualityLabReport {
    pub schema_version: String,
    pub input_compression: InputCompressionMetrics,
    pub cache_effectiveness: CacheEffectivenessMetrics,
    pub tokenizer_calibration: TokenizerCalibrationMetrics,
    pub etpao: EtpaoSummary,
    /// Grades the *representation* (savings, cache reuse, structural fidelity). It is not
    /// a task-quality measure; that needs a model evaluation (`lean-ctx eval ab`).
    #[serde(alias = "overall_quality_grade")]
    pub representation_grade: RepresentationGrade,
    /// Kind of evidence this report is (v2). Task quality is never measured here.
    #[serde(default = "deterministic_tier")]
    pub evidence_tier: EvidenceTier,
    /// Retention/recovery/security receipt for the measured pair (v2). `None` when the
    /// lab ran without an original/compressed pair.
    #[serde(default)]
    pub context_quality: Option<ContextQualityReceiptV1>,
}

fn deterministic_tier() -> EvidenceTier {
    EvidenceTier::DeterministicQuality
}

/// Efficiency + fidelity grade of a compressed representation (v1 called this the
/// "quality grade" with a `Premium` top level, which read as a task-quality claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum RepresentationGrade {
    #[serde(alias = "Premium")]
    Excellent,
    Good,
    Acceptable,
    BelowThreshold,
}

#[derive(Debug, Clone, Copy)]
struct CacheCounts {
    session_hits: u64,
    session_misses: u64,
    content_hits: u64,
    content_misses: u64,
    response_hits: u64,
    response_misses: u64,
    tokens_saved: u64,
}

pub(crate) fn assess_input_compression(
    original: &str,
    compressed: &str,
    ext: &str,
) -> InputCompressionMetrics {
    let preservation = crate::core::preservation::measure(original, compressed, ext);
    let fidelity = assess_fidelity(original, compressed, ext);
    let input_tokens = crate::core::tokens::count_tokens(original) as u64;
    let output_tokens = crate::core::tokens::count_tokens(compressed) as u64;
    let savings_pct = token_savings_pct(input_tokens, output_tokens);

    InputCompressionMetrics {
        modes_tested: 1,
        best_mode: "provided".to_string(),
        best_savings_pct: savings_pct,
        avg_savings_pct: savings_pct,
        avg_preservation_score: preservation.overall(),
        fidelity_class: format!("{:?}", fidelity.class),
        quality_gate_passed: fidelity.passed_quality_gate,
    }
}

pub(crate) fn assess_cache_effectiveness() -> CacheEffectivenessMetrics {
    let counts = telemetry_counts();
    let session_requests = counts.session_hits.saturating_add(counts.session_misses);
    let content_requests = counts.content_hits.saturating_add(counts.content_misses);
    let response_requests = counts.response_hits.saturating_add(counts.response_misses);
    let total_hits = counts
        .session_hits
        .saturating_add(counts.content_hits)
        .saturating_add(counts.response_hits);
    let total_requests = session_requests
        .saturating_add(content_requests)
        .saturating_add(response_requests);

    CacheEffectivenessMetrics {
        session_cache_hit_rate: hit_rate(counts.session_hits, session_requests),
        content_cache_hit_rate: hit_rate(counts.content_hits, content_requests),
        response_cache_hit_rate: hit_rate(counts.response_hits, response_requests),
        aggregate_hit_rate: hit_rate(total_hits, total_requests),
        estimated_token_savings: if total_requests == 0 {
            0
        } else {
            counts.tokens_saved
        },
    }
}

pub(crate) fn assess_tokenizer_calibration(sample_text: &str) -> TokenizerCalibrationMetrics {
    let counts = compare_calibration(sample_text);
    let mut minimum = u64::MAX;
    let mut maximum = 0_u64;
    let mut dominant: Option<CalibratedCount> = None;

    for count in counts.iter().copied() {
        minimum = minimum.min(count.tokens);
        if dominant.is_none_or(|current| count.tokens > current.tokens) {
            dominant = Some(count);
        }
        maximum = maximum.max(count.tokens);
    }

    let variance = if maximum == 0 {
        0.0
    } else {
        maximum.saturating_sub(minimum) as f64 / maximum as f64 * 100.0
    };

    TokenizerCalibrationMetrics {
        families_tested: counts.len(),
        max_cross_family_variance_pct: variance,
        dominant_family: dominant.map_or_else(
            || "Unknown".to_string(),
            |count| format!("{:?}", count.family),
        ),
        dominant_accuracy: dominant.map_or_else(
            || "CharFallback".to_string(),
            |count| format!("{:?}", count.accuracy),
        ),
    }
}

pub(crate) fn compute_representation_grade(report: &QualityLabReport) -> RepresentationGrade {
    let input = &report.input_compression;
    let structural = matches!(input.fidelity_class.as_str(), "Exact" | "Structural");
    let savings = input.best_savings_pct;
    let cache = report.cache_effectiveness.aggregate_hit_rate;
    let etpao = report.etpao.savings_rate_pct;

    if savings >= 80.0 && cache >= 50.0 && structural && etpao >= 50.0 {
        RepresentationGrade::Excellent
    } else if savings >= 60.0 && cache >= 30.0 && structural {
        RepresentationGrade::Good
    } else if savings >= 40.0 && structural {
        RepresentationGrade::Acceptable
    } else {
        RepresentationGrade::BelowThreshold
    }
}

pub(crate) fn run_quality_lab(original: &str, compressed: &str, ext: &str) -> QualityLabReport {
    let input_compression = assess_input_compression(original, compressed, ext);
    let cache_effectiveness = assess_cache_effectiveness();
    let tokenizer_calibration = assess_tokenizer_calibration(original);
    let etpao = assess_etpao(input_compression.best_savings_pct);
    let mut report = QualityLabReport {
        schema_version: "lean-ctx.quality-lab/v2".to_string(),
        input_compression,
        cache_effectiveness,
        tokenizer_calibration,
        etpao,
        representation_grade: RepresentationGrade::BelowThreshold,
        evidence_tier: EvidenceTier::DeterministicQuality,
        // The lab sees only the pair, not a recovery handle: judge it strictly.
        context_quality: (!original.is_empty()).then(|| {
            ContextQualityReceiptV1::assess(
                ext,
                "provided",
                original,
                compressed,
                RecoveryPath::None,
                None,
            )
        }),
    };
    report.representation_grade = compute_representation_grade(&report);
    report
}

pub(crate) fn format_quality_report(report: &QualityLabReport) -> String {
    let mut out = format_lab_metrics(report);
    if let Some(receipt) = &report.context_quality {
        out.push_str("\n\n");
        out.push_str(receipt.render().trim_end());
    }
    out
}

fn format_lab_metrics(report: &QualityLabReport) -> String {
    format!(
        concat!(
            "Quality Lab ({})\n",
            "Input Compression\n",
            "  savings={:.1}% preservation={:.3} fidelity={} gate={}\n",
            "Cache Effectiveness\n",
            "  session={:.1}% content={:.1}% response={:.1}% ",
            "aggregate={:.1}%\n",
            "Tokenizer Calibration\n",
            "  families={} variance={:.1}% dominant={} ({})\n",
            "ETPAO\n",
            "  current={} savings={:.1}% events={} gate={}\n",
            "Representation Grade: {:?} (efficiency + structural fidelity)\n",
            "Task Quality: UNMEASURED (evidence tier {}; run `lean-ctx eval ab`)"
        ),
        report.schema_version,
        report.input_compression.best_savings_pct,
        report.input_compression.avg_preservation_score,
        report.input_compression.fidelity_class,
        gate_label(report.input_compression.quality_gate_passed),
        report.cache_effectiveness.session_cache_hit_rate,
        report.cache_effectiveness.content_cache_hit_rate,
        report.cache_effectiveness.response_cache_hit_rate,
        report.cache_effectiveness.aggregate_hit_rate,
        report.tokenizer_calibration.families_tested,
        report.tokenizer_calibration.max_cross_family_variance_pct,
        report.tokenizer_calibration.dominant_family,
        report.tokenizer_calibration.dominant_accuracy,
        format_etpao(report.etpao.current_etpao),
        report.etpao.savings_rate_pct,
        report.etpao.total_events,
        report.etpao.quality_gate,
        report.representation_grade,
        report.evidence_tier.code(),
    )
}

fn token_savings_pct(input_tokens: u64, output_tokens: u64) -> f64 {
    if input_tokens == 0 {
        return 0.0;
    }
    let retained = output_tokens as f64 / input_tokens as f64;
    ((1.0 - retained) * 100.0).clamp(0.0, 100.0)
}

fn hit_rate(hits: u64, requests: u64) -> f64 {
    if requests == 0 {
        0.0
    } else {
        hits as f64 / requests as f64 * 100.0
    }
}

fn telemetry_counts() -> CacheCounts {
    let metrics = global_metrics();
    let aggregate_hits = metrics.cache_hits.load(Ordering::Relaxed);
    let aggregate_misses = metrics.cache_misses.load(Ordering::Relaxed);
    let content = crate::core::content_cache::stats();
    let response = crate::core::ocla::response_cache::global_response_cache().stats();
    let classified_hits = content.hits.saturating_add(response.hits);
    let classified_misses = content.misses.saturating_add(response.misses);

    CacheCounts {
        session_hits: aggregate_hits.saturating_sub(classified_hits),
        session_misses: aggregate_misses.saturating_sub(classified_misses),
        content_hits: content.hits,
        content_misses: content.misses,
        response_hits: response.hits,
        response_misses: response.misses,
        tokens_saved: metrics.tokens_saved.load(Ordering::Relaxed),
    }
}

fn assess_etpao(input_savings_pct: f64) -> EtpaoSummary {
    let summary = proxy_bridge::etpao_summary();
    let has_data = summary.accepted_outcomes > 0;
    EtpaoSummary {
        current_etpao: has_data.then_some(summary.etpao),
        savings_rate_pct: input_savings_pct,
        total_events: summary.accepted_outcomes as u64,
        quality_gate: if has_data && input_savings_pct >= 50.0 {
            "PASS".to_string()
        } else if has_data {
            "BELOW_THRESHOLD".to_string()
        } else {
            "NO_DATA".to_string()
        },
    }
}

fn gate_label(passed: bool) -> &'static str {
    if passed { "PASS" } else { "FAIL" }
}

fn format_etpao(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |current| format!("{current:.1}"))
}

#[cfg(test)]
mod tests {
    use super::{
        CacheEffectivenessMetrics, EtpaoSummary, EvidenceTier, InputCompressionMetrics,
        QualityLabReport, RepresentationGrade, TokenizerCalibrationMetrics,
        assess_cache_effectiveness, assess_input_compression, assess_tokenizer_calibration,
        compute_representation_grade, format_quality_report, run_quality_lab,
    };

    const ORIGINAL: &str = r"pub fn process(items: &[Item]) -> Result<Vec<Output>, Error> {
    let mut outputs = Vec::with_capacity(items.len());
    for item in items {
        let validated = validate(item)?;
        outputs.push(transform(validated));
    }
    Ok(outputs)
}";

    const COMPRESSED: &str = r"pub fn process(items: &[Item]) -> Result<Vec<Output>, Error> {
    let outputs = items.iter().map(validate).map(transform).collect();
    Ok(outputs)
}";

    #[test]
    fn test_input_compression_assessment() {
        let metrics = assess_input_compression(ORIGINAL, COMPRESSED, "rs");
        assert_eq!(metrics.modes_tested, 1);
        assert!((0.0..=100.0).contains(&metrics.best_savings_pct));
        assert!((0.0..=1.0).contains(&metrics.avg_preservation_score));
        assert!(!metrics.fidelity_class.is_empty());
    }

    #[test]
    fn test_cache_effectiveness_valid_ranges() {
        let metrics = assess_cache_effectiveness();
        assert!((0.0..=100.0).contains(&metrics.session_cache_hit_rate));
        assert!((0.0..=100.0).contains(&metrics.content_cache_hit_rate));
        assert!((0.0..=100.0).contains(&metrics.response_cache_hit_rate));
        assert!((0.0..=100.0).contains(&metrics.aggregate_hit_rate));
    }

    #[test]
    fn test_tokenizer_calibration_variance() {
        let metrics = assess_tokenizer_calibration(ORIGINAL);
        assert!(metrics.families_tested > 1);
        assert!(metrics.max_cross_family_variance_pct >= 0.0);
        assert!(!metrics.dominant_family.is_empty());
    }

    #[test]
    fn test_excellent_grade_thresholds() {
        let report = report_with(85.0, 55.0, "Structural", 70.0);
        assert_eq!(
            compute_representation_grade(&report),
            RepresentationGrade::Excellent
        );
    }

    #[test]
    fn test_below_threshold_grade() {
        let report = report_with(25.0, 90.0, "Lossy", 80.0);
        assert_eq!(
            compute_representation_grade(&report),
            RepresentationGrade::BelowThreshold
        );
    }

    #[test]
    fn v1_report_with_premium_quality_grade_still_parses() {
        let mut json = serde_json::to_value(report_with(85.0, 55.0, "Exact", 70.0)).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("representation_grade");
        obj.remove("evidence_tier");
        obj.insert("overall_quality_grade".into(), "Premium".into());
        let decoded: QualityLabReport = serde_json::from_value(json).expect("v1 report parses");
        assert_eq!(decoded.representation_grade, RepresentationGrade::Excellent);
        assert_eq!(decoded.evidence_tier, EvidenceTier::DeterministicQuality);
    }

    #[test]
    fn test_format_report_never_implies_task_quality() {
        let mut report = report_with(85.0, 55.0, "Exact", 70.0);
        report.representation_grade = compute_representation_grade(&report);
        let output = format_quality_report(&report);
        assert!(output.contains("Input Compression"));
        assert!(output.contains("Representation Grade: Excellent"));
        assert!(output.contains("Task Quality: UNMEASURED"));
        assert!(!output.contains("Premium"));
    }

    #[test]
    fn test_run_quality_lab_integration() {
        let report = run_quality_lab(ORIGINAL, COMPRESSED, "rs");
        assert_eq!(report.schema_version, "lean-ctx.quality-lab/v2");
        assert!(report.tokenizer_calibration.families_tested > 1);
        assert!((0.0..=100.0).contains(&report.input_compression.best_savings_pct));
    }

    fn report_with(savings: f64, cache: f64, fidelity: &str, etpao: f64) -> QualityLabReport {
        QualityLabReport {
            schema_version: "lean-ctx.quality-lab/v2".to_string(),
            input_compression: InputCompressionMetrics {
                modes_tested: 1,
                best_mode: "provided".to_string(),
                best_savings_pct: savings,
                avg_savings_pct: savings,
                avg_preservation_score: 1.0,
                fidelity_class: fidelity.to_string(),
                quality_gate_passed: true,
            },
            cache_effectiveness: CacheEffectivenessMetrics {
                session_cache_hit_rate: cache,
                content_cache_hit_rate: cache,
                response_cache_hit_rate: cache,
                aggregate_hit_rate: cache,
                estimated_token_savings: 1_024,
            },
            tokenizer_calibration: TokenizerCalibrationMetrics {
                families_tested: 4,
                max_cross_family_variance_pct: 8.0,
                dominant_family: "Llama".to_string(),
                dominant_accuracy: "ProxyTokenizer".to_string(),
            },
            etpao: EtpaoSummary {
                current_etpao: Some(750.0),
                savings_rate_pct: etpao,
                total_events: 12,
                quality_gate: "PASS".to_string(),
            },
            representation_grade: RepresentationGrade::BelowThreshold,
            evidence_tier: EvidenceTier::DeterministicQuality,
            context_quality: None,
        }
    }
}
