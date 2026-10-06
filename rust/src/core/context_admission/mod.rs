// SPDX-License-Identifier: Apache-2.0
//! Context admission: the one local decision every governed source passes
//! *before* it is cached, compressed, indexed or delivered.
//!
//! `admit` runs the built-in detectors (secrets, checksum-validated PII,
//! prompt injection, classification markers, secret-like paths) over the
//! original text, decides one [`ContextDispositionV1`], applies the required
//! transformations and re-checks the result. Blocking signals are evaluated
//! before any redaction can hide them, and a redaction that leaves a
//! detectable secret behind withholds the content instead (fail closed).
//!
//! The result carries the canonical `context_gateway` vocabulary — signals
//! with honest coverage, reason codes, classification — and never the matched
//! values. Defaults are on (owner decision E2); each detector can be tuned in
//! the global `[context_gateway]` section, which project-local configuration
//! cannot weaken.

pub(crate) mod capture;
mod clean;
mod detectors;
pub mod egress;
pub mod hud;
mod notes;
pub mod provider;
pub(crate) mod receipt_store;
pub mod recovery;
mod semantic;
pub mod stores;

use std::path::Path;

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDecisionV1, ContextDispositionV1, DetectorSignalV1, GatewayModeV1,
    ReasonCodeV1, TransformationKindV1, TrustLevelV1,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::config::SecretDetectionConfig;
use crate::core::input_filters::FilterAction;

/// `[context_gateway]` — global only; never merged from a project-local file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextGatewayConfig {
    /// Master switch. `LEAN_CTX_CONTEXT_GATEWAY=off` overrides it.
    pub enabled: bool,
    pub mode: GatewayModeV1,
    /// Credentials and keys (patterns from `[secret_detection]`).
    pub secrets: FilterAction,
    /// Checksum-validated PII only: AHV, IBAN, payment cards.
    pub pii: FilterAction,
    /// Prompt-injection heuristic (OWASP LLM01).
    pub injection: FilterAction,
    /// Explicit classification markings (`CONFIDENTIAL`, `classification: …`).
    pub classification: FilterAction,
    /// Bytes each detector may inspect per object; beyond it, coverage is
    /// `partial`. Governed and sovereign modes withhold partially inspected
    /// content, developer mode delivers it with a visible note.
    pub max_inspected_bytes: usize,
    /// Per-detector time budget; a detector that runs out reports `timed_out`.
    pub detector_timeout_ms: u64,
    /// Where redaction counts are shown (see `hud`).
    pub hud: hud::HudPlacement,
}

/// Same bound as policy-pack content inspection (`policy::content`).
pub const DEFAULT_MAX_INSPECTED_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_DETECTOR_TIMEOUT_MS: u64 = 2_000;

impl Default for ContextGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: GatewayModeV1::Developer,
            secrets: FilterAction::Redact,
            pii: FilterAction::Redact,
            injection: FilterAction::Warn,
            classification: FilterAction::Warn,
            max_inspected_bytes: DEFAULT_MAX_INSPECTED_BYTES,
            detector_timeout_ms: DEFAULT_DETECTOR_TIMEOUT_MS,
            hud: hud::HudPlacement::Auto,
        }
    }
}

impl ContextGatewayConfig {
    /// Effective master switch, honoring the `LEAN_CTX_CONTEXT_GATEWAY`
    /// kill switch (`0|false|off` disables).
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        if let Ok(value) = std::env::var("LEAN_CTX_CONTEXT_GATEWAY") {
            return !matches!(value.trim(), "0" | "false" | "off");
        }
        self.enabled
    }
}

/// Everything `admit` needs, resolved once from the global configuration.
#[derive(Debug, Clone)]
pub struct AdmissionPolicy {
    pub gateway: ContextGatewayConfig,
    /// Pattern source for the secrets detector. An explicit
    /// `secret_detection.enabled = false` turns the secrets detector off.
    pub secret_patterns: SecretDetectionConfig,
    /// Optional licensed semantic detectors (advisory; see `semantic`).
    pub(crate) semantic: Option<semantic::SemanticDetector>,
}

impl AdmissionPolicy {
    #[must_use]
    pub fn from_config(config: &crate::core::config::Config) -> Self {
        Self {
            gateway: config.context_gateway.clone(),
            secret_patterns: config.secret_detection.clone(),
            semantic: crate::core::intelligence_runtime::semantic_detectors::detector(),
        }
    }

    /// Built-in detectors only, from explicit settings.
    #[must_use]
    pub fn builtin(gateway: ContextGatewayConfig, secret_patterns: SecretDetectionConfig) -> Self {
        Self {
            gateway,
            secret_patterns,
            semantic: None,
        }
    }

    fn secrets_action(&self) -> FilterAction {
        if self.secret_patterns.enabled {
            self.gateway.secrets
        } else {
            FilterAction::Off
        }
    }

    fn actions(&self) -> detectors::Actions {
        detectors::Actions {
            secrets: self.secrets_action(),
            pii: self.gateway.pii,
            injection: self.gateway.injection,
            classification: self.gateway.classification,
        }
    }

    fn budget(&self) -> detectors::Budget {
        detectors::Budget {
            max_bytes: self.gateway.max_inspected_bytes,
            deadline: std::time::Duration::from_millis(self.gateway.detector_timeout_ms),
        }
    }

    /// Canonical bytes of everything that decides an admission: the gateway
    /// settings and the secret patterns. Keys the clean memo and is hashed into
    /// every receipt's policy reference.
    fn fingerprint(&self) -> Vec<u8> {
        let value = serde_json::json!({
            "gateway": &self.gateway,
            "secret_patterns": &self.secret_patterns,
            "semantic": self.semantic.is_some(),
        });
        serde_json::to_vec(&value).unwrap_or_default()
    }

    /// Governed and sovereign modes treat every enabled detector as mandatory:
    /// content it could not fully inspect is withheld.
    fn detectors_mandatory(&self) -> bool {
        self.gateway.mode != GatewayModeV1::Developer
    }
}

/// Content-free counts for the HUD line and the receipt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdmissionCounts {
    pub secrets: u32,
    pub pii: u32,
    pub injection: u32,
    pub markings: u32,
    /// Enabled detectors that did not inspect the whole object.
    pub incomplete: u32,
    /// Findings of the optional semantic detectors.
    pub semantic: u32,
}

/// The outcome of admitting one object.
#[derive(Debug, Clone)]
pub struct Admission {
    /// `None` when the content is withheld; never the original then.
    pub text: Option<String>,
    pub classification: ClassificationV1,
    pub trust: TrustLevelV1,
    pub decision: ContextDecisionV1,
    pub counts: AdmissionCounts,
}

impl Admission {
    #[must_use]
    pub fn disposition(&self) -> ContextDispositionV1 {
        self.decision.disposition
    }

    /// Whether the delivered text may enter derived stores (caches, indexes,
    /// archives). Restricted content never does (owner decision E3).
    #[must_use]
    pub fn storable(&self) -> bool {
        self.text.is_some() && self.classification < ClassificationV1::Restricted
    }

    /// One stable, content-free line describing what the gateway did, or
    /// `None` when it changed and flagged nothing.
    #[must_use]
    pub fn hud_line(&self) -> Option<String> {
        self.hud_line_placed(true)
    }

    /// [`Self::hud_line`] for the model-facing output. With
    /// `redaction_counts == false` (the host shows lean-ctx out of band),
    /// counts of masked values are left to the status line: the markers in the
    /// content already tell the model, the human sees the count elsewhere.
    /// Detected-but-delivered values, untrusted content and incomplete
    /// inspection are always stated, because the model must act on them.
    #[must_use]
    pub fn hud_line_placed(&self, redaction_counts: bool) -> Option<String> {
        let c = self.counts;
        let mut parts = Vec::new();
        let redacted = self.decision.disposition == ContextDispositionV1::AllowRedacted;
        let verb = if redacted { "redacted" } else { "detected" };
        let show_values = !redacted || redaction_counts;
        if c.secrets > 0 && show_values {
            parts.push(format!("{} secret(s) {verb}", c.secrets));
        }
        if c.pii > 0 && show_values {
            parts.push(format!("{} PII value(s) {verb}", c.pii));
        }
        if c.injection > 0 {
            parts.push(format!("{} prompt-injection signal(s)", c.injection));
        }
        if c.markings > 0 {
            parts.push(format!("{} classification marking(s)", c.markings));
        }
        if c.incomplete > 0 {
            let reasons: Vec<&str> = self
                .decision
                .reason_codes
                .iter()
                .map(ReasonCodeV1::as_str)
                .filter(|code| code.starts_with("coverage.") || code.starts_with("detector."))
                .collect();
            parts.push(format!("not fully inspected ({})", reasons.join(", ")));
        }
        if c.semantic > 0 {
            parts.push(format!("{} semantic finding(s)", c.semantic));
        }
        let semantic_gaps: Vec<&str> = self
            .decision
            .reason_codes
            .iter()
            .map(ReasonCodeV1::as_str)
            .filter(|code| matches!(*code, "semantic.partial" | "semantic.unavailable"))
            .collect();
        if !semantic_gaps.is_empty() {
            parts.push(format!(
                "semantic check incomplete ({})",
                semantic_gaps.join(", ")
            ));
        }
        if self.text.is_none() {
            let reasons: Vec<&str> = self
                .decision
                .reason_codes
                .iter()
                .map(ReasonCodeV1::as_str)
                .collect();
            return Some(format!(
                "[lean-ctx gateway: content withheld — {}]",
                reasons.join(", ")
            ));
        }
        (!parts.is_empty()).then(|| format!("[lean-ctx gateway: {}]", parts.join(" · ")))
    }
}

/// Admit `text` (from `path`, when it came from a file) under `policy`.
#[must_use]
pub fn admit(text: &str, path: Option<&Path>, policy: &AdmissionPolicy) -> Admission {
    let object = content_digest(text);
    let gateway = &policy.gateway;
    if !gateway.enabled_effective() {
        return Admission {
            text: Some(text.to_owned()),
            classification: gateway.mode.unclassified(),
            trust: TrustLevelV1::Unknown,
            decision: ContextDecisionV1 {
                object,
                disposition: ContextDispositionV1::Allow,
                reason_codes: vec![reason("gateway.disabled")],
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            counts: AdmissionCounts::default(),
        };
    }

    let actions = policy.actions();
    let findings = detectors::inspect(
        text,
        path,
        &actions,
        &policy.secret_patterns,
        policy.budget(),
    );
    let mut counts = findings.counts();

    let mut reasons = Vec::new();
    let mut disposition = ContextDispositionV1::Allow;
    // A detector that did not inspect everything never reports "clean": its
    // reason is always recorded, and mandatory detectors withhold the object.
    for incomplete in findings.incomplete() {
        reasons.push(incomplete);
        if policy.detectors_mandatory() {
            disposition = ContextDispositionV1::Deny;
        }
    }
    let mut transformations = Vec::new();
    for (action, hits, code) in [
        (actions.secrets, counts.secrets, "secret"),
        (actions.pii, counts.pii, "pii"),
        (actions.injection, counts.injection, "injection"),
        (actions.classification, counts.markings, "classification"),
    ] {
        if hits == 0 {
            continue;
        }
        let step = match action {
            FilterAction::Block => {
                reasons.push(reason(&format!("{code}.blocked")));
                ContextDispositionV1::Deny
            }
            // A marking classifies the whole object; there is no span to mask.
            FilterAction::Redact if code == "classification" => {
                reasons.push(reason(&format!("{code}.blocked")));
                ContextDispositionV1::Deny
            }
            FilterAction::Redact => {
                reasons.push(reason(&format!("{code}.redacted")));
                if !transformations.contains(&TransformationKindV1::Redaction) {
                    transformations.push(TransformationKindV1::Redaction);
                }
                ContextDispositionV1::AllowRedacted
            }
            FilterAction::Warn => {
                reasons.push(reason(&format!("{code}.flagged")));
                ContextDispositionV1::Allow
            }
            FilterAction::Off => continue,
        };
        disposition = disposition.most_restrictive(step);
    }

    // Optional semantic detectors: advisory, so an incomplete run is reported
    // but only an actual finding can tighten the decision.
    let semantic = policy
        .semantic
        .map(|detect| semantic::run(text, detect, &actions, gateway.max_inspected_bytes));
    if let Some(outcome) = &semantic {
        counts.semantic = outcome.hits;
        for (step, code) in &outcome.steps {
            reasons.push(code.clone());
            if *step == ContextDispositionV1::AllowRedacted
                && !transformations.contains(&TransformationKindV1::Redaction)
            {
                transformations.push(TransformationKindV1::Redaction);
            }
            disposition = disposition.most_restrictive(*step);
        }
        if let Some(code) = &outcome.incomplete {
            reasons.push(code.clone());
        }
    }

    let mut delivered = if disposition == ContextDispositionV1::Deny {
        None
    } else {
        // Semantic spans refer to the original text, so they are masked first.
        let semantic_masked = semantic
            .as_ref()
            .filter(|outcome| !outcome.redactions.is_empty())
            .map(|outcome| semantic::redact(text, &outcome.redactions));
        let source = semantic_masked.as_deref().unwrap_or(text);
        Some(findings.redact(source, &actions, &policy.secret_patterns))
    };
    // Fail closed: a redaction that leaves a detectable secret or PII value
    // behind must not be delivered as if it were clean. Unchanged text was
    // already inspected by the same detectors, so only a rewrite is re-checked.
    if let Some(output) = &delivered
        && output != text
        && detectors::residual(output, &actions, &policy.secret_patterns)
    {
        reasons.push(reason("redaction.incomplete"));
        disposition = ContextDispositionV1::Deny;
        delivered = None;
    }

    let classification = findings.classification(gateway.mode, &actions);
    let semantic_injection = semantic.as_ref().is_some_and(|outcome| {
        outcome
            .steps
            .iter()
            .any(|(_, code)| code.as_str().starts_with("semantic.prompt_injection"))
    });
    let trust = if counts.injection > 0 || semantic_injection {
        TrustLevelV1::Untrusted
    } else {
        TrustLevelV1::Internal
    };
    let mut signals: Vec<DetectorSignalV1> = findings.signals();
    if let Some(outcome) = semantic {
        signals.push(outcome.signal);
    }
    Admission {
        text: delivered,
        classification,
        trust,
        decision: ContextDecisionV1 {
            object,
            disposition,
            reason_codes: reasons,
            signals,
            required_transformations: transformations,
        },
        counts,
    }
}

/// Admit a source the agent is about to read, at the read choke point.
///
/// Returns the admitted text, or a content-free `PermissionDenied` naming the
/// reason codes. With `preserve_bytes` (signed snapshots) any rewrite is
/// refused instead of silently changing a digest-bound source.
pub fn admit_source(
    text: &str,
    path: &str,
    preserve_bytes: bool,
) -> Result<String, std::io::Error> {
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    let fingerprint = policy.fingerprint();
    let capture = capture::current();
    if let Some(capture) = &capture {
        capture.bind_policy(policy.gateway.mode, fingerprint_digest(&fingerprint));
    }
    // Clean memo: only built-in detectors are pure functions of the input.
    let memo_key = (policy.semantic.is_none() && policy.gateway.enabled_effective())
        .then(|| clean::key(&fingerprint, path, text));
    if let Some(key) = &memo_key
        && let Some(decision) = clean::get(key)
    {
        if let Some(capture) = &capture {
            capture.record(&decision, AdmissionCounts::default());
        }
        return Ok(text.to_owned());
    }
    let admission = admit(text, Some(Path::new(path)), &policy);
    if let Some(capture) = &capture {
        capture.record(&admission.decision, admission.counts);
    }
    if let Some(key) = memo_key
        && admission.disposition() == ContextDispositionV1::Allow
        && admission.decision.reason_codes.is_empty()
        && admission.text.as_deref() == Some(text)
    {
        clean::insert(key, admission.decision.clone());
    }
    let Some(admitted) = admission.text.as_deref() else {
        let reasons: Vec<&str> = admission
            .decision
            .reason_codes
            .iter()
            .map(ReasonCodeV1::as_str)
            .collect();
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "source content withheld by the context gateway ({})",
                reasons.join(", ")
            ),
        ));
    };
    if preserve_bytes && admitted != text {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "source content withheld: the context gateway would rewrite a digest-bound source",
        ));
    }
    let redaction_counts = policy.gateway.hud.redaction_counts_in_band();
    if let Some(line) = admission.hud_line_placed(redaction_counts) {
        notes::record(admitted, line);
    }
    Ok(admission.text.unwrap_or_default())
}

/// Admit an object no built-in detector can read (an image). Every enabled
/// detector reports `unsupported`; developer mode delivers it with a note,
/// governed and sovereign modes withhold it.
#[must_use]
pub fn admit_uninspectable(path: Option<&Path>, bytes: u64, policy: &AdmissionPolicy) -> Admission {
    let gateway = &policy.gateway;
    let object = content_digest(&format!("uninspectable:{bytes}"));
    let actions = policy.actions();
    let signals = detectors::unsupported_signals(&actions, bytes);
    let required = signals.iter().any(|signal| {
        signal.coverage.kind == lean_ctx_protocol::context_gateway::CoverageKindV1::Unsupported
    });
    if !gateway.enabled_effective() || !required {
        return admit("", path, policy);
    }
    let restricted_path =
        path.is_some_and(|p| crate::core::io_boundary::is_secret_like(p).is_some());
    let mandatory = policy.detectors_mandatory();
    Admission {
        text: (!mandatory).then(String::new),
        classification: if restricted_path {
            ClassificationV1::Restricted
        } else {
            gateway.mode.unclassified()
        },
        trust: TrustLevelV1::Unknown,
        decision: ContextDecisionV1 {
            object,
            disposition: if mandatory {
                ContextDispositionV1::Deny
            } else {
                ContextDispositionV1::Allow
            },
            reason_codes: vec![reason("coverage.unsupported_media")],
            signals,
            required_transformations: Vec::new(),
        },
        counts: AdmissionCounts {
            incomplete: 1,
            ..AdmissionCounts::default()
        },
    }
}

/// Media gate for the read path: `Ok(note)` to deliver with a visible note,
/// or a content-free `PermissionDenied` when the media must be withheld.
pub fn admit_media(path: &str, bytes: u64) -> Result<Option<String>, std::io::Error> {
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    let admission = admit_uninspectable(Some(Path::new(path)), bytes, &policy);
    if let Some(capture) = capture::current() {
        capture.bind_policy(
            policy.gateway.mode,
            fingerprint_digest(&policy.fingerprint()),
        );
        capture.record(&admission.decision, admission.counts);
    }
    if admission.text.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "media withheld by the context gateway (coverage.unsupported_media)",
        ));
    }
    Ok((admission.counts.incomplete > 0)
        .then(|| "[lean-ctx gateway: media not inspected (coverage.unsupported_media)]".to_owned()))
}

/// Forget every memoized clean admission (measurements of cold reads).
#[cfg(test)]
pub(crate) fn clear_clean_memo() {
    clean::clear();
}

/// The HUD line recorded when exactly `admitted` was produced, if any.
#[must_use]
pub fn note_for(admitted: &str) -> Option<String> {
    notes::lookup(admitted)
}

fn reason(code: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(code).expect("built-in reason codes are valid")
}

/// SHA-256 of the canonical policy fingerprint: the receipt's policy digest.
fn fingerprint_digest(fingerprint: &[u8]) -> Sha256Digest {
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(fingerprint));
    Sha256Digest::new(format!("sha256:{digest}")).expect("SHA-256 digest is canonical")
}

fn content_digest(text: &str) -> Sha256Digest {
    let digest = crate::core::agent_identity::hex_encode(&Sha256::digest(text.as_bytes()));
    Sha256Digest::new(format!("sha256:{digest}")).expect("SHA-256 digest is canonical")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod receipt_tests;

#[cfg(test)]
mod store_tests;

#[cfg(test)]
mod egress_tests;
