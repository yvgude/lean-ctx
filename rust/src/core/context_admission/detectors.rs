// SPDX-License-Identifier: Apache-2.0
//! Built-in detectors behind `admit`, adapted from the existing scanners:
//! `secret_detection` (with the user's custom and exclude patterns),
//! checksum-validated `input_filters::pii`, the `output_sanitizer` injection
//! heuristic, `input_filters::classification` markings and
//! `io_boundary::is_secret_like` for paths.
//!
//! Detectors scan line-aligned chunks under a byte budget and a per-detector
//! deadline. Every built-in pattern is line-local, so line-aligned chunks
//! without overlap find exactly what a whole-text scan finds. Coverage is
//! reported as it happened: `complete` only when every byte was inspected,
//! `partial` when the budget or the deadline stopped the scan, `failed` when a
//! configured rule could not run, `not_required` when the detector is off.
//! Only counts leave this module, never matched values.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use lean_ctx_protocol::context_gateway::{
    ClassificationV1, CoverageKindV1, DetectorCategoryV1, DetectorCoverageV1, DetectorRefV1,
    DetectorSignalV1, DetectorStatusV1, GatewayModeV1, ReasonCodeV1, SeverityV1,
};
use lean_ctx_protocol::{ProtocolReference, SemanticVersion};

use super::AdmissionCounts;
use crate::core::config::SecretDetectionConfig;
use crate::core::input_filters::{FilterAction, classification, injection, pii};
use crate::core::secret_detection;

/// Version of the built-in detector set; bump when a detector's semantics change.
const BUILTIN_VERSION: &str = "1.1.0";

/// Target chunk size; chunks end on a line boundary, so a chunk can be longer.
pub(super) const CHUNK_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub(super) struct Actions {
    pub(super) secrets: FilterAction,
    pub(super) pii: FilterAction,
    pub(super) injection: FilterAction,
    pub(super) classification: FilterAction,
}

/// How much one admission may inspect.
#[derive(Debug, Clone, Copy)]
pub(super) struct Budget {
    pub(super) max_bytes: usize,
    pub(super) deadline: Duration,
}

/// One detector's run over the chunked text.
#[derive(Debug, Clone)]
struct Run {
    hits: u32,
    latency_us: u64,
    bytes_inspected: u64,
    chunks_inspected: u32,
    status: DetectorStatusV1,
    /// Set when a configured rule could not run (the run is then `failed`).
    failure: Option<&'static str>,
}

impl Run {
    fn complete(&self, bytes_total: u64) -> bool {
        self.status == DetectorStatusV1::Completed
            && self.failure.is_none()
            && self.bytes_inspected == bytes_total
    }
}

/// What the detectors found in the original text.
#[derive(Debug, Default)]
pub(super) struct Findings {
    bytes_total: u64,
    chunks_total: u32,
    secrets: Option<Run>,
    secret_path: Option<Run>,
    pii: Option<Run>,
    injection: Option<Run>,
    markings: Option<(Run, ClassificationV1)>,
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn micros(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// Split `text` into chunks of about [`CHUNK_BYTES`] that end on a line
/// boundary (or at the end of the text).
fn line_chunks(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end < text.len() {
            end = text[end..]
                .find('\n')
                .map_or(text.len(), |offset| end + offset + 1);
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// Run one detector over the chunks until the budget or the deadline stops it.
fn run_chunked(chunks: &[&str], budget: Budget, mut detect: impl FnMut(&str) -> usize) -> Run {
    let started = Instant::now();
    let mut run = Run {
        hits: 0,
        latency_us: 0,
        bytes_inspected: 0,
        chunks_inspected: 0,
        status: DetectorStatusV1::Completed,
        failure: None,
    };
    let mut inspected_bytes = 0usize;
    for chunk in chunks {
        if inspected_bytes + chunk.len() > budget.max_bytes {
            break;
        }
        if started.elapsed() >= budget.deadline {
            run.status = DetectorStatusV1::TimedOut;
            break;
        }
        run.hits = run.hits.saturating_add(saturating_u32(detect(chunk)));
        inspected_bytes += chunk.len();
        run.chunks_inspected += 1;
    }
    run.bytes_inspected = u64::try_from(inspected_bytes).unwrap_or(u64::MAX);
    run.latency_us = micros(started.elapsed());
    run
}

/// Scanner configuration for detection: never rewrites, always reports.
fn detect_only(patterns: &SecretDetectionConfig) -> SecretDetectionConfig {
    SecretDetectionConfig {
        enabled: true,
        redact: false,
        ..patterns.clone()
    }
}

fn secret_hits(text: &str, patterns: &SecretDetectionConfig) -> usize {
    secret_detection::scan_and_redact(text, &detect_only(patterns))
        .1
        .len()
}

fn pii_hits(text: &str) -> usize {
    pii::redact_checksummed(text).1.iter().map(|(_, n)| n).sum()
}

fn default_matcher() -> &'static classification::Matcher {
    static MATCHER: OnceLock<classification::Matcher> = OnceLock::new();
    MATCHER.get_or_init(|| classification::Matcher::new(&[]))
}

/// The classification an explicit marking asserts.
fn marking_level(label: &str) -> ClassificationV1 {
    match label {
        "TOP SECRET" | "SECRET" | "RESTRICTED" => ClassificationV1::Restricted,
        "INTERNAL ONLY" => ClassificationV1::Internal,
        _ => ClassificationV1::Confidential,
    }
}

/// A configured custom secret pattern that does not compile would otherwise be
/// skipped silently by the scanner — the run must say so instead.
fn invalid_custom_pattern(patterns: &SecretDetectionConfig) -> bool {
    patterns
        .custom_patterns
        .iter()
        .any(|pattern| regex::Regex::new(pattern).is_err())
}

pub(super) fn inspect(
    text: &str,
    path: Option<&Path>,
    actions: &Actions,
    patterns: &SecretDetectionConfig,
    budget: Budget,
) -> Findings {
    let chunks = line_chunks(text);
    let mut findings = Findings {
        bytes_total: u64::try_from(text.len()).unwrap_or(u64::MAX),
        chunks_total: saturating_u32(chunks.len()),
        ..Findings::default()
    };
    if !actions.secrets.is_off() {
        let mut run = run_chunked(&chunks, budget, |chunk| secret_hits(chunk, patterns));
        if invalid_custom_pattern(patterns) {
            run.status = DetectorStatusV1::Failed;
            run.failure = Some("detector.invalid_pattern");
        }
        findings.secrets = Some(run);
        if let Some(path) = path {
            let started = Instant::now();
            let secret_like = crate::core::io_boundary::is_secret_like(path).is_some();
            findings.secret_path = Some(Run {
                hits: u32::from(secret_like),
                latency_us: micros(started.elapsed()),
                bytes_inspected: findings.bytes_total,
                chunks_inspected: findings.chunks_total,
                status: DetectorStatusV1::Completed,
                failure: None,
            });
        }
    }
    if !actions.pii.is_off() {
        findings.pii = Some(run_chunked(&chunks, budget, pii_hits));
    }
    if !actions.injection.is_off() {
        findings.injection = Some(run_chunked(&chunks, budget, injection::detect));
    }
    if !actions.classification.is_off() {
        let mut labels = std::collections::BTreeSet::new();
        let run = run_chunked(&chunks, budget, |chunk| {
            let found = default_matcher().detect(chunk);
            let n = found.len();
            labels.extend(found);
            n
        });
        let level = labels
            .iter()
            .map(|label| marking_level(label))
            .max()
            .unwrap_or(ClassificationV1::Public);
        let run = Run {
            hits: saturating_u32(labels.len()),
            ..run
        };
        findings.markings = Some((run, level));
    }
    findings
}

impl Findings {
    fn runs(&self) -> [(Option<&Run>, &'static str); 4] {
        [
            (self.secrets.as_ref(), "secrets"),
            (self.pii.as_ref(), "pii"),
            (self.injection.as_ref(), "injection"),
            (self.markings.as_ref().map(|(run, _)| run), "classification"),
        ]
    }

    pub(super) fn counts(&self) -> AdmissionCounts {
        let hits = |run: Option<&Run>| run.map_or(0, |r| r.hits);
        AdmissionCounts {
            secrets: hits(self.secrets.as_ref()),
            pii: hits(self.pii.as_ref()),
            injection: hits(self.injection.as_ref()),
            markings: hits(self.markings.as_ref().map(|(run, _)| run)),
            incomplete: saturating_u32(self.incomplete().len()),
            semantic: 0,
        }
    }

    /// One reason per enabled detector that did not inspect the whole object.
    pub(super) fn incomplete(&self) -> Vec<ReasonCodeV1> {
        let mut reasons = Vec::new();
        for (run, _) in self.runs() {
            let Some(run) = run else { continue };
            if run.complete(self.bytes_total) {
                continue;
            }
            let code = match (run.failure, run.status) {
                (Some(failure), _) => failure,
                (None, DetectorStatusV1::TimedOut) => "detector.timed_out",
                _ => "coverage.partial",
            };
            let reason = ReasonCodeV1::new(code).expect("built-in reason codes are valid");
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
        }
        reasons
    }

    fn secret_path(&self) -> bool {
        self.secret_path.as_ref().is_some_and(|r| r.hits > 0)
    }

    /// Apply every `redact` action to the whole text. Secrets go last so no
    /// other marker can split a credential and hide it from the secrets pass.
    pub(super) fn redact(
        &self,
        text: &str,
        actions: &Actions,
        patterns: &SecretDetectionConfig,
    ) -> String {
        let counts = self.counts();
        let mut out = text.to_owned();
        if actions.injection == FilterAction::Redact && counts.injection > 0 {
            out = injection::redact(&out).0;
        }
        if actions.pii == FilterAction::Redact && counts.pii > 0 {
            out = pii::redact_checksummed(&out).0;
        }
        if actions.secrets == FilterAction::Redact && counts.secrets > 0 {
            let forced = SecretDetectionConfig {
                enabled: true,
                redact: true,
                ..patterns.clone()
            };
            out = secret_detection::scan_and_redact(&out, &forced).0;
        }
        out
    }

    /// Classification of the delivered object.
    pub(super) fn classification(
        &self,
        mode: GatewayModeV1,
        actions: &Actions,
    ) -> ClassificationV1 {
        let counts = self.counts();
        let mut level = mode.unclassified();
        if self.secret_path() {
            level = level.join(ClassificationV1::Restricted);
        }
        if let Some((run, marked)) = &self.markings
            && run.hits > 0
        {
            level = level.join(*marked);
        }
        // Values that are detected but deliberately delivered keep their weight.
        if counts.secrets > 0 && actions.secrets == FilterAction::Warn {
            level = level.join(ClassificationV1::Restricted);
        }
        if counts.pii > 0 && actions.pii == FilterAction::Warn {
            level = level.join(ClassificationV1::Confidential);
        }
        level
    }

    pub(super) fn signals(&self) -> Vec<DetectorSignalV1> {
        let scope = Scope {
            bytes_total: self.bytes_total,
            chunks_total: self.chunks_total,
        };
        let mut signals = vec![signal(
            "builtin.secrets",
            DetectorCategoryV1::Secret,
            SeverityV1::Critical,
            self.secrets.as_ref(),
            scope,
        )];
        if self.secret_path.is_some() {
            signals.push(signal(
                "builtin.secret_path",
                DetectorCategoryV1::Classification,
                SeverityV1::High,
                self.secret_path.as_ref(),
                scope,
            ));
        }
        signals.push(signal(
            "builtin.pii_checksum",
            DetectorCategoryV1::Pii,
            SeverityV1::High,
            self.pii.as_ref(),
            scope,
        ));
        signals.push(signal(
            "builtin.injection",
            DetectorCategoryV1::PromptInjection,
            SeverityV1::High,
            self.injection.as_ref(),
            scope,
        ));
        signals.push(signal(
            "builtin.classification_marking",
            DetectorCategoryV1::Classification,
            SeverityV1::Medium,
            self.markings.as_ref().map(|(run, _)| run),
            scope,
        ));
        signals
    }
}

#[derive(Debug, Clone, Copy)]
struct Scope {
    bytes_total: u64,
    chunks_total: u32,
}

fn detector_ref(id: &str) -> DetectorRefV1 {
    DetectorRefV1 {
        id: ProtocolReference::new(id).expect("built-in detector ids are valid"),
        version: SemanticVersion::new(BUILTIN_VERSION).expect("built-in version is valid"),
    }
}

fn reason(code: &str) -> Option<ReasonCodeV1> {
    Some(ReasonCodeV1::new(code).expect("built-in reason codes are valid"))
}

fn signal(
    id: &str,
    category: DetectorCategoryV1,
    severity: SeverityV1,
    run: Option<&Run>,
    scope: Scope,
) -> DetectorSignalV1 {
    let Some(run) = run else {
        return DetectorSignalV1 {
            detector: detector_ref(id),
            category,
            severity: SeverityV1::Info,
            confidence_milli: None,
            calibrated: false,
            evidence_count: 0,
            coverage: DetectorCoverageV1 {
                kind: CoverageKindV1::NotRequired,
                bytes_total: scope.bytes_total,
                bytes_inspected: 0,
                chunks_total: scope.chunks_total,
                chunks_inspected: 0,
                reason: None,
            },
            status: DetectorStatusV1::Skipped,
            latency_us: 0,
        };
    };
    let (kind, coverage_reason) = if let Some(failure) = run.failure {
        (CoverageKindV1::Failed, reason(failure))
    } else if run.complete(scope.bytes_total) {
        (CoverageKindV1::Complete, None)
    } else if run.status == DetectorStatusV1::TimedOut {
        (CoverageKindV1::Partial, reason("detector.timed_out"))
    } else {
        (CoverageKindV1::Partial, reason("coverage.budget_exhausted"))
    };
    DetectorSignalV1 {
        detector: detector_ref(id),
        category,
        severity: if run.hits > 0 {
            severity
        } else {
            SeverityV1::Info
        },
        confidence_milli: None,
        calibrated: false,
        evidence_count: run.hits,
        coverage: DetectorCoverageV1 {
            kind,
            bytes_total: scope.bytes_total,
            bytes_inspected: run.bytes_inspected,
            chunks_total: scope.chunks_total,
            chunks_inspected: run.chunks_inspected,
            reason: coverage_reason,
        },
        status: run.status,
        latency_us: run.latency_us,
    }
}

/// Signals for an object no built-in detector can read (an image): every
/// enabled detector reports `unsupported`, never a clean `complete`.
pub(super) fn unsupported_signals(actions: &Actions, bytes: u64) -> Vec<DetectorSignalV1> {
    let scope = Scope {
        bytes_total: bytes,
        chunks_total: 1,
    };
    [
        (
            actions.secrets,
            "builtin.secrets",
            DetectorCategoryV1::Secret,
        ),
        (actions.pii, "builtin.pii_checksum", DetectorCategoryV1::Pii),
        (
            actions.injection,
            "builtin.injection",
            DetectorCategoryV1::PromptInjection,
        ),
        (
            actions.classification,
            "builtin.classification_marking",
            DetectorCategoryV1::Classification,
        ),
    ]
    .into_iter()
    .map(|(action, id, category)| {
        if action.is_off() {
            return signal(id, category, SeverityV1::Info, None, scope);
        }
        DetectorSignalV1 {
            detector: detector_ref(id),
            category,
            severity: SeverityV1::Info,
            confidence_milli: None,
            calibrated: false,
            evidence_count: 0,
            coverage: DetectorCoverageV1 {
                kind: CoverageKindV1::Unsupported,
                bytes_total: scope.bytes_total,
                bytes_inspected: 0,
                chunks_total: 1,
                chunks_inspected: 0,
                reason: reason("coverage.unsupported_media"),
            },
            status: DetectorStatusV1::Skipped,
            latency_us: 0,
        }
    })
    .collect()
}

#[cfg(test)]
pub(super) fn secret_hits_for_bench(text: &str, patterns: &SecretDetectionConfig) -> usize {
    secret_hits(text, patterns)
}

#[cfg(test)]
pub(super) fn markings_for_bench(text: &str) -> usize {
    default_matcher().detect(text).len()
}

#[cfg(test)]
pub(super) fn chunks_for_test(text: &str) -> Vec<&str> {
    line_chunks(text)
}

/// True when a redacting detector can still find something in `output`.
pub(super) fn residual(output: &str, actions: &Actions, patterns: &SecretDetectionConfig) -> bool {
    (actions.secrets == FilterAction::Redact && secret_hits(output, patterns) > 0)
        || (actions.pii == FilterAction::Redact && pii_hits(output) > 0)
}
