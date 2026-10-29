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
use super::semantic_deferred;
use crate::core::input_filters::FilterAction;
use crate::core::intelligence_runtime::semantic_coverage::{self, Declared, Scope, Unknown};
use crate::core::intelligence_runtime::semantic_detectors::{
    CategorySet, MAX_SOURCE_BYTES, NotInspected, SemanticCategory, SemanticReport,
};

/// A detector that answers from a fixed function (tests).
#[cfg(test)]
pub(crate) type Inspect = fn(&str) -> crate::core::intelligence_runtime::Result<SemanticReport>;

/// The semantic detector of one admission policy, together with the runtime
/// settings it was resolved from. Both come from the policy's configuration
/// snapshot: every text of an admission is inspected with the same runtime,
/// and that runtime's identity is part of the policy fingerprint.
#[derive(Debug, Clone)]
pub(crate) struct SemanticDetector(Bound);

#[derive(Debug, Clone)]
enum Bound {
    Runtime(std::sync::Arc<crate::core::config::IntelligenceRuntimeConfig>),
    /// A fixed function, and what its model set would declare.
    #[cfg(test)]
    Fixed(Inspect, Option<Declared>),
}

impl SemanticDetector {
    /// The licensed detector for this configuration snapshot, when its
    /// runtime is enabled, accepted and permitted; `None` otherwise.
    pub(super) fn for_snapshot(config: &crate::core::config::Config) -> Option<Self> {
        crate::core::intelligence_runtime::semantic_detectors::bound(config)
            .map(|runtime| Self(Bound::Runtime(runtime)))
    }

    /// A fixed detector whose set declares nothing (an earlier descriptor):
    /// asked, but every category it examines is reported as not inspected.
    #[cfg(test)]
    pub(crate) fn fixed(inspect: Inspect) -> Self {
        Self(Bound::Fixed(inspect, None))
    }

    /// A fixed detector whose set declares `declared`.
    #[cfg(test)]
    pub(crate) fn fixed_declaring(inspect: Inspect, declared: Declared) -> Self {
        Self(Bound::Fixed(inspect, Some(declared)))
    }

    /// What the bound model set declares; `None` when that cannot be read.
    fn declared(&self) -> Option<Declared> {
        match &self.0 {
            Bound::Runtime(runtime) => {
                crate::core::intelligence_runtime::semantic_detectors::declared(runtime)
            }
            #[cfg(test)]
            Bound::Fixed(_, declared) => declared.clone(),
        }
    }

    /// Bound to explicit runtime settings, without the permission check.
    #[cfg(test)]
    pub(crate) fn runtime(runtime: crate::core::config::IntelligenceRuntimeConfig) -> Self {
        Self(Bound::Runtime(std::sync::Arc::new(runtime)))
    }

    /// The report and the categories it speaks for. A category outside that
    /// set was not examined by the runtime's models.
    pub(super) fn inspect(
        &self,
        text: &str,
    ) -> Result<(SemanticReport, CategorySet), NotInspected> {
        match &self.0 {
            Bound::Runtime(runtime) => {
                crate::core::intelligence_runtime::semantic_detectors::inspect_bound(runtime, text)
            }
            #[cfg(test)]
            Bound::Fixed(inspect, _) => inspect(text)
                .map(|report| (report, CategorySet::ALL))
                .map_err(|_| NotInspected::Unavailable),
        }
    }

    /// What the policy fingerprint binds for this detector: a digest of the
    /// effective runtime settings (root, manifest, trust pins, switches). Two
    /// policies with different runtimes never share an epoch, a memo entry or
    /// a receipt's policy reference.
    pub(super) fn identity(&self) -> serde_json::Value {
        match &self.0 {
            Bound::Runtime(runtime) => {
                let bytes =
                    serde_json::to_vec(runtime.as_ref()).expect("runtime settings serialize");
                format!("runtime:blake3:{}", blake3::hash(&bytes).to_hex()).into()
            }
            #[cfg(test)]
            Bound::Fixed(..) => true.into(),
        }
    }
}

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
pub(super) fn inspectable_prefix(text: &str, max_bytes: usize) -> &str {
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
    detect: &SemanticDetector,
    actions: &Actions,
    max_bytes: usize,
) -> SemanticOutcome {
    let started = std::time::Instant::now();
    let prefix = inspectable_prefix(text, max_bytes);
    let total = u64::try_from(text.len()).unwrap_or(u64::MAX);
    let detector = DetectorRefV1 {
        id: ProtocolReference::new("pro.semantic_detectors").expect("valid detector id"),
        version: SemanticVersion::new("1.0.0").expect("valid version"),
    };
    // The categories the set is declared for on this text, and the
    // dimensions along which it declares nothing.
    let (mut within, mut unknown) = (CategorySet::ALL, Unknown::default());
    // Stored context: a full check reached later replaces the bounded run,
    // and a bounded run that stays partial is queued for one.
    let mut deferred = None;
    let report = if text.is_empty() {
        // Nothing to inspect: never start a runtime for it.
        Ok((
            SemanticReport {
                bytes_inspected: 0,
                findings: Vec::new(),
            },
            CategorySet::ALL,
        ))
    } else if prefix.is_empty() {
        Err(NotInspected::Unavailable)
    } else {
        match semantic_coverage::scope(detect.declared().as_ref(), prefix) {
            Scope::Outside(code) => return outside_coverage(code, detector, total, actions),
            Scope::Inside {
                categories,
                unknown: undeclared,
            } => {
                (within, unknown) = (categories, undeclared);
                deferred = semantic_deferred::in_stored_context()
                    .then(|| semantic_deferred::key(detect, text));
                match deferred.as_ref().and_then(semantic_deferred::verdict) {
                    Some(verdict) => Ok((verdict.report, verdict.examined)),
                    None => detect.inspect(prefix),
                }
            }
        }
    };
    let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);

    let (report, examined) = match report {
        Ok(inspected) => inspected,
        // Not inspected, whatever the cause: the decision carries the one
        // code `semantic.unavailable`; the coverage reason tells a worker
        // that is still loading its models (`semantic.warming`) from one
        // that failed.
        Err(not_inspected) => {
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
                        reason: Some(reason(not_inspected.coverage_reason())),
                    },
                    status: DetectorStatusV1::Failed,
                    latency_us,
                },
                hits: 0,
                steps: Vec::new(),
                redactions: Vec::new(),
                incomplete: Some(reason("semantic.unavailable")),
            };
        }
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

    // A category the operator has switched on but the runtime's models do not
    // examine is named in the decision's reason codes. The absence of a
    // finding for it says nothing, and nothing here lets it read as clean.
    // A category examined by a model that is not declared for this text is
    // not inspected either: its findings still mask, its silence says nothing.
    let examined = examined.intersection(within);
    let mut all_examined = true;
    for category in [
        SemanticCategory::Secret,
        SemanticCategory::Pii,
        SemanticCategory::PromptInjection,
    ] {
        if !examined.contains(category) && !action_for(category, actions).is_off() {
            all_examined = false;
            steps.push((
                ContextDispositionV1::Allow,
                reason(&format!("semantic.{}.not_inspected", category.as_str())),
            ));
        }
    }

    let inspected = u64::try_from(report.bytes_inspected).unwrap_or(u64::MAX);
    let complete = inspected == total;
    if !complete && let Some(key) = deferred {
        semantic_deferred::enqueue(key, detect, text);
    }
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
                // Byte coverage first; a complete run that left a configured
                // category unexamined says so here as well.
                // A set that declares no languages or document types is
                // asked (its findings mask) but claims no category; that
                // its coverage is unknown is said here, before a left-out
                // category (which the decision's
                // `semantic.<category>.not_inspected` codes name anyway).
                reason: if !complete {
                    Some(reason("coverage.budget_exhausted"))
                } else if let Some(unknown) = unknown.reason() {
                    Some(reason(unknown))
                } else if !all_examined {
                    Some(reason("semantic.categories_not_inspected"))
                } else {
                    None
                },
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

/// A text outside every declaration of the set: no model is asked, nothing
/// is masked by it, and every category the operator has switched on is
/// named as not inspected, with `code` as the reason. Like any incomplete
/// semantic run this is advisory; the built-in floor decides as always.
fn outside_coverage(
    code: &str,
    detector: DetectorRefV1,
    total: u64,
    actions: &Actions,
) -> SemanticOutcome {
    let steps = [
        SemanticCategory::Secret,
        SemanticCategory::Pii,
        SemanticCategory::PromptInjection,
    ]
    .into_iter()
    .filter(|category| !action_for(*category, actions).is_off())
    .map(|category| {
        (
            ContextDispositionV1::Allow,
            reason(&format!("semantic.{}.not_inspected", category.as_str())),
        )
    })
    .collect();
    SemanticOutcome {
        signal: DetectorSignalV1 {
            detector,
            category: DetectorCategoryV1::Custom,
            severity: SeverityV1::Info,
            confidence_milli: None,
            calibrated: false,
            evidence_count: 0,
            coverage: DetectorCoverageV1 {
                kind: CoverageKindV1::Unsupported,
                bytes_total: total,
                bytes_inspected: 0,
                chunks_total: 1,
                chunks_inspected: 0,
                reason: Some(reason(code)),
            },
            status: DetectorStatusV1::Skipped,
            latency_us: 0,
        },
        hits: 0,
        steps,
        redactions: Vec::new(),
        incomplete: Some(reason(code)),
    }
}

/// Mask validated spans of the original text; spans are disjoint and ascending.
pub(super) fn redact(text: &str, spans: &[(usize, usize, SemanticCategory)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for &(start, end, category) in spans {
        out.push_str(&text[cursor..start]);
        // One kind, one marker: the built-in filter has always written
        // `prompt-injection`; the wire name of the category is not a marker.
        let kind = match category {
            SemanticCategory::PromptInjection => "prompt-injection",
            other => other.as_str(),
        };
        out.push_str(&format!("[REDACTED:{kind}]"));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}
