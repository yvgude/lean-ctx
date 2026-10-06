// SPDX-License-Identifier: Apache-2.0
//! Host side of the optional semantic detectors (licensed runtime). They are
//! advisory: the built-in detectors remain the mandatory floor, so a missing,
//! failing or partial semantic run is reported but never blocks on its own.
//! The host applies redactions from validated spans; a runtime returns no text.

use lean_ctx_protocol::context_gateway::{
    ContextDispositionV1, CoverageKindV1, DetectorCategoryV1, DetectorCoverageV1, DetectorRefV1,
    DetectorSignalV1, DetectorStatusV1, ReasonCodeV1, SeverityV1,
};
use lean_ctx_protocol::{ProtocolReference, SemanticVersion};

use super::detectors::Actions;
use crate::core::input_filters::FilterAction;
use crate::core::intelligence_runtime::semantic_detectors::{
    MAX_SOURCE_BYTES, SemanticCategory, SemanticReport,
};

pub(crate) type SemanticDetector =
    fn(&str) -> crate::core::intelligence_runtime::Result<SemanticReport>;

/// What one semantic run contributes to the admission decision.
pub(super) struct SemanticOutcome {
    pub(super) signal: DetectorSignalV1,
    pub(super) hits: u32,
    /// `(disposition step, reason)` per category with findings.
    pub(super) steps: Vec<(ContextDispositionV1, ReasonCodeV1)>,
    /// Spans to mask, in the original text, ascending and disjoint.
    pub(super) redactions: Vec<(usize, usize, SemanticCategory)>,
    /// Set when the run did not inspect the whole text or failed.
    pub(super) incomplete: Option<ReasonCodeV1>,
}

fn reason(code: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(code).expect("built-in reason codes are valid")
}

fn action_for(category: SemanticCategory, actions: &Actions) -> FilterAction {
    match category {
        SemanticCategory::Secret => actions.secrets,
        SemanticCategory::Pii => actions.pii,
        SemanticCategory::PromptInjection => actions.injection,
    }
}

/// The longest line-aligned prefix within both bounds.
fn inspectable_prefix(text: &str, max_bytes: usize) -> &str {
    let limit = max_bytes.min(MAX_SOURCE_BYTES);
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match text[..end].rfind('\n') {
        Some(newline) => &text[..=newline],
        None => "",
    }
}

pub(super) fn run(
    text: &str,
    detect: SemanticDetector,
    actions: &Actions,
    max_bytes: usize,
) -> SemanticOutcome {
    let started = std::time::Instant::now();
    let prefix = inspectable_prefix(text, max_bytes);
    let total = u64::try_from(text.len()).unwrap_or(u64::MAX);
    let report = if text.is_empty() {
        // Nothing to inspect: never start a runtime for it.
        Some(SemanticReport {
            bytes_inspected: 0,
            findings: Vec::new(),
        })
    } else if prefix.is_empty() {
        None
    } else {
        detect(prefix).ok()
    };
    let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let detector = DetectorRefV1 {
        id: ProtocolReference::new("pro.semantic_detectors").expect("valid detector id"),
        version: SemanticVersion::new("1.0.0").expect("valid version"),
    };

    let Some(report) = report else {
        return SemanticOutcome {
            signal: DetectorSignalV1 {
                detector,
                category: DetectorCategoryV1::Custom,
                severity: SeverityV1::Info,
                confidence_milli: None,
                calibrated: false,
                evidence_count: 0,
                coverage: DetectorCoverageV1 {
                    kind: CoverageKindV1::Failed,
                    bytes_total: total,
                    bytes_inspected: 0,
                    chunks_total: 1,
                    chunks_inspected: 0,
                    reason: Some(reason("semantic.unavailable")),
                },
                status: DetectorStatusV1::Failed,
                latency_us,
            },
            hits: 0,
            steps: Vec::new(),
            redactions: Vec::new(),
            incomplete: Some(reason("semantic.unavailable")),
        };
    };

    let mut steps = Vec::new();
    let mut redactions = Vec::new();
    let mut hits = 0u32;
    let mut confidence = None::<u16>;
    for finding in &report.findings {
        let action = action_for(finding.category, actions);
        if action.is_off() {
            continue;
        }
        hits = hits.saturating_add(1);
        confidence = Some(confidence.map_or(finding.confidence_milli, |c| {
            c.max(finding.confidence_milli)
        }));
        let category = finding.category.as_str();
        let step = match action {
            FilterAction::Block => (
                ContextDispositionV1::Deny,
                reason(&format!("semantic.{category}.blocked")),
            ),
            FilterAction::Redact => {
                redactions.push((finding.start, finding.end, finding.category));
                (
                    ContextDispositionV1::AllowRedacted,
                    reason(&format!("semantic.{category}.redacted")),
                )
            }
            FilterAction::Warn | FilterAction::Off => (
                ContextDispositionV1::Allow,
                reason(&format!("semantic.{category}.flagged")),
            ),
        };
        if !steps.contains(&step) {
            steps.push(step);
        }
    }

    let inspected = u64::try_from(report.bytes_inspected).unwrap_or(u64::MAX);
    let complete = inspected == total;
    SemanticOutcome {
        signal: DetectorSignalV1 {
            detector,
            category: DetectorCategoryV1::Custom,
            severity: if hits > 0 {
                SeverityV1::High
            } else {
                SeverityV1::Info
            },
            confidence_milli: confidence,
            calibrated: false,
            evidence_count: hits,
            coverage: DetectorCoverageV1 {
                kind: if complete {
                    CoverageKindV1::Complete
                } else {
                    CoverageKindV1::Partial
                },
                bytes_total: total,
                bytes_inspected: inspected,
                chunks_total: 1,
                chunks_inspected: u32::from(inspected > 0 || total == 0),
                reason: (!complete).then(|| reason("coverage.budget_exhausted")),
            },
            status: DetectorStatusV1::Completed,
            latency_us,
        },
        hits,
        steps,
        redactions,
        incomplete: (!complete).then(|| reason("semantic.partial")),
    }
}

/// Mask validated spans of the original text; spans are disjoint and ascending.
pub(super) fn redact(text: &str, spans: &[(usize, usize, SemanticCategory)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end, category) in spans {
        out.push_str(&text[cursor..start]);
        out.push_str(&format!("[REDACTED:{}]", category.as_str()));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}
