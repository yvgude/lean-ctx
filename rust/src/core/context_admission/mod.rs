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
pub(crate) mod carrier;
mod clean;
pub(crate) mod cross_line;
mod detectors;
pub mod egress;
pub mod hud;
mod notes;
pub mod provider;
pub(crate) mod receipt_store;
pub mod recovery;
pub(crate) mod semantic;
pub(crate) mod semantic_deferred;
pub(crate) mod stored;
pub mod stores;
pub mod withheld;

use std::path::Path;

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDecisionV1, ContextDispositionV1, CoverageKindV1,
    DestinationConstraintV1, DestinationLocalityV1, DestinationV1, DetectorSignalV1, GatewayModeV1,
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
    /// Checksum-validated PII only: payment cards.
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
    /// Additional placement constraints applied to every final egress target.
    /// These remain active when content scanning is disabled.
    pub destination_constraints: Vec<DestinationConstraintV1>,
    /// Optional target allow-list. Provider and locality are always checked;
    /// model/account/region are checked when the configured entry supplies them.
    /// Unknown locality never matches. Account references are configured labels,
    /// not provider-verified identities.
    pub allowed_destinations: Vec<DestinationV1>,
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
            destination_constraints: Vec::new(),
            allowed_destinations: Vec::new(),
            hud: hud::HudPlacement::Auto,
        }
    }
}

impl ContextGatewayConfig {
    /// Effective master switch. The `LEAN_CTX_CONTEXT_GATEWAY` kill switch
    /// (`0|false|off`) is a developer convenience: it disables admission only
    /// in developer mode. A governed or sovereign deployment is configured by
    /// its operator in the global file; a process environment cannot switch
    /// its protection off, so the variable is ignored there (and reported, see
    /// [`Self::kill_switch`]). Any other value still forces the gateway on.
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        self.enabled_under(EnvSwitch::read())
    }

    /// The effective switch under one captured environment state. Admission
    /// uses only this form, with the state its `AdmissionPolicy` captured:
    /// the live environment is read once per snapshot, never per decision.
    #[must_use]
    pub(crate) fn enabled_under(&self, env: EnvSwitch) -> bool {
        match self.kill_switch_under(env) {
            KillSwitch::Unset | KillSwitch::Ignored => self.enabled,
            KillSwitch::ForcedOn => true,
            KillSwitch::Honoured => false,
        }
    }

    /// What the `LEAN_CTX_CONTEXT_GATEWAY` environment variable does under
    /// this configuration (for `doctor` and the admission log). Reads the
    /// live environment.
    #[must_use]
    pub fn kill_switch(&self) -> KillSwitch {
        self.kill_switch_under(EnvSwitch::read())
    }

    fn kill_switch_under(&self, env: EnvSwitch) -> KillSwitch {
        match env {
            EnvSwitch::Unset => return KillSwitch::Unset,
            EnvSwitch::On => return KillSwitch::ForcedOn,
            EnvSwitch::Off => {}
        }
        if self.mode == GatewayModeV1::Developer {
            KillSwitch::Honoured
        } else {
            static REPORTED: std::sync::Once = std::sync::Once::new();
            REPORTED.call_once(|| {
                tracing::warn!(
                    "LEAN_CTX_CONTEXT_GATEWAY=off ignored: the context gateway runs in {:?} mode",
                    self.mode
                );
            });
            KillSwitch::Ignored
        }
    }

    /// Destination policy is a hard authorization gate, independent of the
    /// optional content-detector switch.
    #[must_use]
    pub(crate) fn has_destination_policy(&self) -> bool {
        !self.destination_constraints.is_empty() || !self.allowed_destinations.is_empty()
    }

    /// Check the final, trusted destination against configured placement rules.
    #[must_use]
    pub(crate) fn allows_destination(&self, actual: &DestinationV1) -> bool {
        if actual.locality == DestinationLocalityV1::Unknown
            || !actual.satisfies_all(&self.destination_constraints)
        {
            return false;
        }
        self.allowed_destinations.is_empty()
            || self.allowed_destinations.iter().any(|expected| {
                expected.locality != DestinationLocalityV1::Unknown
                    && expected.provider == actual.provider
                    && expected.locality == actual.locality
                    && expected.organization_managed == actual.organization_managed
                    && expected
                        .model
                        .as_ref()
                        .is_none_or(|reference| actual.model.as_ref() == Some(reference))
                    && expected
                        .account_ref
                        .as_ref()
                        .is_none_or(|reference| actual.account_ref.as_ref() == Some(reference))
                    && expected
                        .region
                        .as_ref()
                        .is_none_or(|reference| actual.region.as_ref() == Some(reference))
            })
    }
}

/// The raw state of `LEAN_CTX_CONTEXT_GATEWAY`, read once and carried in the
/// admission snapshot. It is the only environment-derived input of admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvSwitch {
    Unset,
    /// `0|false|off`.
    Off,
    /// Any other value.
    On,
}

impl EnvSwitch {
    /// The live environment, now.
    pub(crate) fn read() -> Self {
        match std::env::var("LEAN_CTX_CONTEXT_GATEWAY") {
            Err(_) => Self::Unset,
            Ok(value) if matches!(value.trim(), "0" | "false" | "off") => Self::Off,
            Ok(_) => Self::On,
        }
    }
}

/// Effect of the `LEAN_CTX_CONTEXT_GATEWAY` environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSwitch {
    Unset,
    /// Set to a value other than `0|false|off`: the gateway is on.
    ForcedOn,
    /// `off` in developer mode: admission is disabled (`gateway.disabled`).
    Honoured,
    /// `off` in governed or sovereign mode: has no effect.
    Ignored,
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
    /// Set when the snapshot this policy was resolved from could not apply the
    /// global configuration, so `gateway` holds built-in defaults, not the
    /// operator's settings. The configured mode is unknown: nothing is
    /// admitted in this state, in any mode.
    load_failure: Option<crate::core::config::ConfigLoadFailure>,
    /// The environment switch as it was when this snapshot was taken.
    env_switch: EnvSwitch,
}

impl AdmissionPolicy {
    /// Resolve from a configuration snapshot. Whether the operator's settings
    /// were applied is read from that same snapshot (`Config::load_failure`),
    /// never from the process-global parse diagnostic.
    #[must_use]
    pub fn from_config(config: &crate::core::config::Config) -> Self {
        Self {
            gateway: config.context_gateway.clone(),
            secret_patterns: config.secret_detection.clone(),
            semantic: semantic::SemanticDetector::for_snapshot(config),
            load_failure: config.load_failure,
            env_switch: EnvSwitch::read(),
        }
    }

    /// Built-in detectors only, from explicit settings.
    #[must_use]
    pub fn builtin(gateway: ContextGatewayConfig, secret_patterns: SecretDetectionConfig) -> Self {
        Self {
            gateway,
            secret_patterns,
            semantic: None,
            load_failure: None,
            env_switch: EnvSwitch::read(),
        }
    }

    /// Whether this policy stands in for a global configuration that could
    /// not be loaded (see [`Self::from_config`]).
    #[must_use]
    pub fn config_unloadable(&self) -> bool {
        self.load_failure.is_some()
    }

    /// Reason codes of the refusal for an unloadable configuration: the one
    /// stable admission code, then the loader's specific code. Never the
    /// parse error text, which can quote file content.
    pub(crate) fn config_refusal_codes(&self) -> Option<[&'static str; 2]> {
        self.load_failure
            .map(|failure| ["gateway.config_unloadable", failure.code()])
    }

    /// The master switch as admission applies it: an unloadable configuration
    /// can never be "disabled", whatever its default values or the
    /// environment say.
    #[must_use]
    pub fn enabled_effective(&self) -> bool {
        self.config_unloadable() || self.gateway_switch()
    }

    /// The gateway's own switch under the environment state this snapshot
    /// captured when it was built. The fingerprint and every decision taken
    /// with this policy use exactly this value; the live environment is not
    /// consulted again. Whether the snapshot is still current is a separate,
    /// explicit question (`egress::PolicyEpoch::is_current`).
    fn gateway_switch(&self) -> bool {
        self.gateway.enabled_under(self.env_switch)
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
        let mut value = serde_json::json!({
            "gateway": &self.gateway,
            "secret_patterns": &self.secret_patterns,
            // `false` without a detector (bytes unchanged); otherwise the
            // identity of the runtime this policy's detector is bound to.
            "semantic": self
                .semantic
                .as_ref()
                .map_or(serde_json::Value::Bool(false), semantic::SemanticDetector::identity),
        });
        // Added only in the failure state, so every digest recorded under a
        // loadable configuration keeps its bytes.
        if let Some(failure) = self.load_failure
            && let Some(map) = value.as_object_mut()
        {
            map.insert("config_unloadable".to_owned(), failure.code().into());
        }
        // The effective switch is an input of every decision, and the
        // environment can move it without touching a configuration byte
        // (`LEAN_CTX_CONTEXT_GATEWAY`). It is bound whenever it differs from
        // the configured value — and only then, so digests taken without an
        // environment override keep their bytes.
        let effective = self.enabled_effective();
        if effective != self.gateway.enabled
            && let Some(map) = value.as_object_mut()
        {
            map.insert("enabled_effective".to_owned(), effective.into());
        }
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
    /// 1 when the object carries prompt-injection signals: an object is
    /// flagged once, however many of its lines matched.
    pub injection: u32,
    /// Which pattern families flagged it
    /// ([`crate::core::output_sanitizer::InjectionCategory`] bits).
    pub injection_patterns: u8,
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
        render_hud(
            self.counts,
            &self.decision,
            self.text.is_none(),
            redaction_counts,
        )
    }
}

/// The HUD line for `counts` under `decision`; see [`Admission::hud_line_placed`].
fn render_hud(
    c: AdmissionCounts,
    decision: &ContextDecisionV1,
    withheld: bool,
    redaction_counts: bool,
) -> Option<String> {
    let mut parts = Vec::new();
    let redacted = decision.disposition == ContextDispositionV1::AllowRedacted;
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
    let codes = || decision.reason_codes.iter().map(ReasonCodeV1::as_str);
    if c.incomplete > 0 {
        let reasons: Vec<&str> = codes()
            .filter(|code| code.starts_with("coverage.") || code.starts_with("detector."))
            .collect();
        parts.push(format!("not fully inspected ({})", reasons.join(", ")));
    }
    if c.semantic > 0 {
        parts.push(format!("{} semantic finding(s)", c.semantic));
    }
    let semantic_gaps: Vec<&str> = codes()
        .filter(|code| {
            matches!(*code, "semantic.partial" | "semantic.unavailable")
                || (code.starts_with("semantic.") && code.ends_with(".not_inspected"))
        })
        .collect();
    if !semantic_gaps.is_empty() {
        parts.push(format!(
            "semantic check incomplete ({})",
            semantic_gaps.join(", ")
        ));
    }
    if withheld {
        let reasons: Vec<&str> = codes().collect();
        return Some(format!(
            "[lean-ctx gateway: content withheld — {}]",
            reasons.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| format!("[lean-ctx gateway: {}]", parts.join(" · ")))
}

/// Admit `text` (from `path`, when it came from a file) under `policy`.
#[must_use]
pub fn admit(text: &str, path: Option<&Path>, policy: &AdmissionPolicy) -> Admission {
    let object = content_digest(text);
    let gateway = &policy.gateway;
    if let Some(codes) = policy.config_refusal_codes() {
        // The operator's mode and detector actions are unknown. Running the
        // built-in defaults here would silently turn a governed installation
        // into a developer one.
        return Admission {
            text: None,
            classification: gateway.mode.unclassified(),
            trust: TrustLevelV1::Unknown,
            decision: ContextDecisionV1 {
                object,
                disposition: ContextDispositionV1::Deny,
                reason_codes: codes.iter().map(|code| reason(code)).collect(),
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            counts: AdmissionCounts::default(),
        };
    }
    if !policy.gateway_switch() {
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
        .as_ref()
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
            // A finding, not the statement that the category was not inspected.
            .any(|(_, code)| {
                let code = code.as_str();
                code.starts_with("semantic.prompt_injection") && !code.ends_with(".not_inspected")
            })
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

/// Capture facts from a shell process. The flags describe raw byte streams,
/// before decoding or rendering; marker text is never used to infer completeness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OutputCompleteness {
    pub truncated: bool,
    pub timed_out: bool,
    pub reader_incomplete: bool,
    pub cancelled: bool,
    /// Set conservatively when captured bytes required lossy text decoding.
    /// On Windows the legacy ACP decoder may also be lossy for some code pages.
    pub decode_lossy: bool,
}

impl OutputCompleteness {
    pub(crate) const fn complete() -> Self {
        Self {
            truncated: false,
            timed_out: false,
            reader_incomplete: false,
            cancelled: false,
            decode_lossy: false,
        }
    }

    pub(crate) fn is_complete(self) -> bool {
        !(self.truncated
            || self.timed_out
            || self.reader_incomplete
            || self.cancelled
            || self.decode_lossy)
    }

    fn reason_codes(self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.truncated {
            reasons.push("coverage.output_truncated");
        }
        if self.timed_out {
            reasons.push("coverage.output_timed_out");
        }
        if self.reader_incomplete {
            reasons.push("coverage.output_reader_incomplete");
        }
        if self.cancelled {
            reasons.push("coverage.output_cancelled");
        }
        if self.decode_lossy {
            reasons.push("coverage.output_decode_lossy");
        }
        reasons
    }
}

/// Result of inspecting tool output. A missing text means delivery is denied;
/// rendered then contains only the stable, content-free gateway notice.
#[derive(Debug, Clone)]
pub(crate) struct ToolOutputAdmission {
    pub text: Option<String>,
    pub notice: Option<String>,
}

impl ToolOutputAdmission {
    pub(crate) fn withheld(&self) -> bool {
        self.text.is_none()
    }

    pub(crate) fn rendered(&self) -> String {
        match (&self.text, &self.notice) {
            (Some(text), Some(notice)) if text.is_empty() => notice.clone(),
            (Some(text), Some(notice)) => format!("{notice}\n{text}"),
            (Some(text), None) => text.clone(),
            (None, Some(notice)) => notice.clone(),
            (None, None) => "[lean-ctx gateway: content withheld]".to_string(),
        }
    }
}

pub(crate) fn admit_tool_output(
    text: &str,
    completeness: OutputCompleteness,
) -> ToolOutputAdmission {
    // Check active project policy against the original bytes before Gateway
    // redaction can hide a phrase that policy must block.
    let project_original = crate::core::policy::content::protect_active(text);
    let project_rewrote = project_original
        .as_ref()
        .is_ok_and(|admitted| admitted.as_ref() != text);
    let policy = carrier::current_policy();
    let mut admission = admit(text, None, &policy);
    mark_capture_incomplete(&mut admission, completeness, &policy);

    let gateway_output = admission.text.clone();
    let text = project_original
        .as_deref()
        .ok()
        .zip(gateway_output.as_deref())
        .and_then(|(project_original, gateway_output)| {
            compose_active_gateway_output(project_original, gateway_output, &policy, completeness)
        });
    let policy_withheld = project_original.is_err() || (gateway_output.is_some() && text.is_none());
    if let Some(admitted) = &text {
        admission.text = Some(admitted.clone());
        if project_rewrote
            || gateway_output
                .as_deref()
                .is_some_and(|gateway| gateway != admitted)
        {
            mark_project_redaction(&mut admission);
        }
    } else if policy_withheld {
        mark_policy_withheld(&mut admission, "policy.content_withheld");
    }

    let capture = capture::current();
    if let Some(capture) = &capture {
        capture.bind_policy(
            policy.gateway.mode,
            fingerprint_digest(&policy.fingerprint()),
        );
        capture.record(&admission.decision, admission.counts);
    }
    let mut notice = admission.hud_line_placed(policy.gateway.hud.redaction_counts_in_band());
    if policy_withheld {
        notice.get_or_insert_with(|| "[lean-ctx policy: content withheld]".to_string());
    }
    ToolOutputAdmission { text, notice }
}

fn mark_project_redaction(admission: &mut Admission) {
    admission.decision.disposition = admission
        .decision
        .disposition
        .most_restrictive(ContextDispositionV1::AllowRedacted);
    let reason = reason("policy.content_redacted");
    if !admission.decision.reason_codes.contains(&reason) {
        admission.decision.reason_codes.push(reason);
    }
    if !admission
        .decision
        .required_transformations
        .contains(&TransformationKindV1::Redaction)
    {
        admission
            .decision
            .required_transformations
            .push(TransformationKindV1::Redaction);
    }
}

fn mark_policy_withheld(admission: &mut Admission, reason_code: &str) {
    admission.text = None;
    admission.decision.disposition = ContextDispositionV1::Deny;
    let reason = reason(reason_code);
    if !admission.decision.reason_codes.contains(&reason) {
        admission.decision.reason_codes.push(reason);
    }
}

/// Compose the project-policy and Gateway transformations without losing either
/// detector's view of the original input. If their order changes the result,
/// withholding is safer than publishing text whose provenance is ambiguous.
pub(crate) fn compose_active_gateway_output(
    project_original: &str,
    gateway_output: &str,
    policy: &AdmissionPolicy,
    completeness: OutputCompleteness,
) -> Option<String> {
    let project_after_gateway = crate::core::policy::content::protect_active(gateway_output)
        .ok()?
        .into_owned();
    let mut gateway_after_project = admit(project_original, None, policy);
    mark_capture_incomplete(&mut gateway_after_project, completeness, policy);
    let gateway_after_project = gateway_after_project.text?;
    (project_after_gateway == gateway_after_project).then_some(project_after_gateway)
}

pub(super) fn admit_output_for_storage(
    text: &str,
    completeness: OutputCompleteness,
    policy: &AdmissionPolicy,
) -> Option<String> {
    // Incomplete process output is never cached or retained for later polls,
    // even in Developer mode where the originating call may see a warning.
    if !completeness.is_complete() {
        return None;
    }
    let project_original = crate::core::policy::content::protect_active(text).ok()?;
    let mut admission = admit(text, None, policy);
    mark_capture_incomplete(&mut admission, completeness, policy);
    if !admission.storable() {
        return None;
    }
    let gateway_output = admission.text?;
    compose_active_gateway_output(
        project_original.as_ref(),
        &gateway_output,
        policy,
        completeness,
    )
}

/// A finished background job's output, kept until a status poll delivers it
/// (#2164). Only a held pipe (a process the command left running) is
/// tolerated; deliveries still pass `admit_tool_output` with the real flags,
/// and truncated, timed-out, cancelled or lossy captures stay unretained.
pub(super) fn admit_output_for_poll(
    text: &str,
    completeness: OutputCompleteness,
    policy: &AdmissionPolicy,
) -> Option<String> {
    let without_reader = OutputCompleteness {
        reader_incomplete: false,
        ..completeness
    };
    if !completeness.reader_incomplete {
        return admit_output_for_storage(text, completeness, policy);
    }
    if !without_reader.is_complete() || (policy.enabled_effective() && policy.detectors_mandatory())
    {
        return None;
    }
    admit_output_for_storage(text, without_reader, policy)
}

fn mark_capture_incomplete(
    admission: &mut Admission,
    completeness: OutputCompleteness,
    policy: &AdmissionPolicy,
) {
    if completeness.is_complete() || !policy.enabled_effective() {
        return;
    }
    for code in completeness.reason_codes() {
        let reason = reason(code);
        if !admission.decision.reason_codes.contains(&reason) {
            admission.decision.reason_codes.push(reason);
        }
    }
    let partial_reason = reason("coverage.output_partial");
    if !admission.decision.reason_codes.contains(&partial_reason) {
        admission.decision.reason_codes.push(partial_reason.clone());
    }
    for signal in &mut admission.decision.signals {
        if signal.coverage.kind != CoverageKindV1::Complete {
            continue;
        }
        // The process capture knows only the prefix length. One unknown byte is
        // the protocol-valid lower bound for the omitted tail; it is not a claim
        // that the command emitted exactly one additional byte.
        signal.coverage.kind = CoverageKindV1::Partial;
        signal.coverage.bytes_total = signal.coverage.bytes_inspected.saturating_add(1);
        signal.coverage.chunks_total = signal.coverage.chunks_inspected.saturating_add(1);
        signal.coverage.reason = Some(partial_reason.clone());
    }
    admission.counts.incomplete = admission.counts.incomplete.max(1);
    if policy.detectors_mandatory() {
        admission.decision.disposition = ContextDispositionV1::Deny;
        admission.text = None;
    }
}

/// Source text admitted for delivery, with the original classification's
/// persistence decision preserved for downstream caches and indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceAdmission {
    pub(crate) text: String,
    pub(crate) storable: bool,
    /// Classification assigned by the original Gateway inspection.
    pub(crate) classification: ClassificationV1,
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
    admit_source_with_storage(text, path, preserve_bytes, OutputCompleteness::complete())
        .map(|admission| admission.text)
}

/// Admit a source and preserve whether its original Gateway classification may
/// enter derived stores. Incomplete reads never seed the clean memo or stores.
pub(crate) fn admit_source_with_storage(
    text: &str,
    path: &str,
    preserve_bytes: bool,
    completeness: OutputCompleteness,
) -> Result<SourceAdmission, std::io::Error> {
    admit_source_inner(text, Some(path), preserve_bytes, completeness, None)
}

/// Admit an in-memory source without assigning it a synthetic filesystem path.
pub(crate) fn admit_context_source(text: &str) -> Result<SourceAdmission, std::io::Error> {
    admit_source_inner(text, None, false, OutputCompleteness::complete(), None)
}

/// [`admit_context_source`] for text that is bound by a digest or signature
/// and therefore cannot be delivered rewritten: a mask is a refusal, and it
/// is recorded as one (never as an admitted redaction of a copy that did not
/// leave).
pub(crate) fn admit_bound_context_source(text: &str) -> Result<SourceAdmission, std::io::Error> {
    admit_source_inner(text, None, true, OutputCompleteness::complete(), None)
}

/// Admit a rooted source with one caller-captured Gateway snapshot. Hosted
/// generation builders use this so a multi-file build cannot mix policies if
/// global configuration changes during the scan; publication must reauthorize
/// the same snapshot afterward.
pub(crate) fn admit_source_with_snapshot(
    text: &str,
    path: &str,
    preserve_bytes: bool,
    completeness: OutputCompleteness,
    snapshot: &stores::StoreAdmission,
) -> Result<SourceAdmission, std::io::Error> {
    admit_source_inner(
        text,
        Some(path),
        preserve_bytes,
        completeness,
        Some(snapshot.admission_policy()),
    )
}

fn admit_source_inner(
    text: &str,
    path: Option<&str>,
    preserve_bytes: bool,
    completeness: OutputCompleteness,
    policy_snapshot: Option<&AdmissionPolicy>,
) -> Result<SourceAdmission, std::io::Error> {
    admit_source_typed(text, path, preserve_bytes, completeness, policy_snapshot).map_err(
        |reasons| {
            let hint = withheld_remedy(&reasons);
            let reasons: Vec<&str> = reasons.iter().map(ReasonCodeV1::as_str).collect();
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "source content withheld by the context gateway ({}){hint}",
                    reasons.join(", ")
                ),
            )
        },
    )
}

/// The setting behind a withheld read, so the user can tell an intended policy
/// from a misconfiguration (814 refusals in 30 days carried only the code —
/// focus analysis 2026-10-15). Fixed text: never content, never a path.
fn withheld_remedy(reasons: &[ReasonCodeV1]) -> &'static str {
    let has = |code: &str| reasons.iter().any(|reason| reason.as_str() == code);
    if has("gateway.config_unloadable") {
        " — the global config.toml could not be loaded; fix it (lean-ctx doctor) to restore reads"
    } else if has("coverage.partial") || has("coverage.budget_exhausted") {
        " — the object exceeds [context_gateway] max_inspected_bytes and governed/sovereign mode \
         withholds what detectors cannot fully inspect"
    } else if has("detector.timed_out") {
        " — a detector exceeded [context_gateway] detector_timeout_ms in governed/sovereign mode"
    } else if has("classification.blocked") {
        " — a classification marking matched [context_gateway] classification = redact|block"
    } else if has("secret.blocked") {
        " — a secret matched [context_gateway] secrets = \"block\""
    } else if has("injection.blocked") {
        " — prompt-injection text matched [context_gateway] injection = \"block\""
    } else if has("redaction.incomplete") {
        " — a redaction left a detectable value behind, so nothing is delivered"
    } else if has("policy.content_withheld") {
        " — withheld by the project or organization policy"
    } else {
        ""
    }
}

/// [`admit_source_inner`] with the refusal as typed reason codes, for carriers
/// that render their own content-free message (see `carrier`).
fn admit_source_typed(
    text: &str,
    path: Option<&str>,
    preserve_bytes: bool,
    completeness: OutputCompleteness,
    policy_snapshot: Option<&AdmissionPolicy>,
) -> Result<SourceAdmission, Vec<ReasonCodeV1>> {
    // Inspect original content before a Gateway redaction or clean memo can
    // remove a signal required by the active project policy.
    let project_original = crate::core::policy::content::protect_active(text);
    let project_rewrote = project_original
        .as_ref()
        .is_ok_and(|admitted| admitted.as_ref() != text);
    let policy = policy_snapshot
        .cloned()
        .unwrap_or_else(carrier::current_policy);
    let fingerprint = policy.fingerprint();
    let capture = capture::current();
    if let Some(capture) = &capture {
        capture.bind_policy(policy.gateway.mode, fingerprint_digest(&fingerprint));
    }
    // Clean memo: only complete reads and built-in detectors are pure functions
    // of the input. Partial or lossy reads must be inspected with their flags.
    let memo_key = path.and_then(|path| {
        (completeness.is_complete()
            && project_original.is_ok()
            && !project_rewrote
            && policy.semantic.is_none()
            && !policy.config_unloadable()
            && policy.gateway_switch())
        .then(|| clean::key(&fingerprint, path, text))
    });
    if let Some(key) = &memo_key
        && let Some(decision) = clean::get_source(key)
    {
        if let Some(capture) = &capture {
            capture.record(&decision.decision, AdmissionCounts::default());
        }
        return Ok(SourceAdmission {
            text: project_original
                .expect("memo key requires accepted project policy")
                .into_owned(),
            storable: decision.storable,
            classification: decision.classification,
        });
    }

    let mut admission = admit(text, path.map(Path::new), &policy);
    mark_capture_incomplete(&mut admission, completeness, &policy);
    let storable = completeness.is_complete() && admission.storable();
    let gateway_admitted = admission.text.clone();
    let mut project_admitted = project_original
        .as_deref()
        .ok()
        .zip(gateway_admitted.as_deref())
        .and_then(|(project_original, gateway_admitted)| {
            compose_active_gateway_output(project_original, gateway_admitted, &policy, completeness)
        });
    let mut refusal_reason = if project_original.is_err()
        || (gateway_admitted.is_some() && project_admitted.is_none())
    {
        Some("policy.content_withheld")
    } else {
        None
    };

    if let Some(admitted) = &project_admitted {
        if project_rewrote
            || gateway_admitted
                .as_deref()
                .is_some_and(|gateway| gateway != admitted)
        {
            admission.text = Some(admitted.clone());
            mark_project_redaction(&mut admission);
        }
    }
    if preserve_bytes
        && let Some(gateway_admitted) = gateway_admitted.as_deref()
        && let Some(project_admitted_text) = project_admitted.as_deref()
        && preserve_bytes_rewrite_reason(
            text,
            gateway_admitted,
            project_admitted_text,
            project_rewrote,
        )
        .is_some()
    {
        project_admitted = None;
        refusal_reason = Some("source.bytes_not_preserved");
    }
    if let Some(reason_code) = refusal_reason {
        mark_policy_withheld(&mut admission, reason_code);
    }
    if let Some(capture) = &capture {
        capture.record(&admission.decision, admission.counts);
    }

    let Some(project_admitted) = project_admitted else {
        return Err(admission.decision.reason_codes);
    };

    if let Some(key) = memo_key
        && admission.disposition() == ContextDispositionV1::Allow
        && admission.decision.reason_codes.is_empty()
        && admission.text.as_deref() == Some(text)
        && project_admitted == text
    {
        clean::insert_source(
            key,
            admission.decision.clone(),
            admission.classification,
            admission.storable(),
        );
    }
    let redaction_counts = policy.gateway.hud.redaction_counts_in_band();
    if admission.hud_line_placed(redaction_counts).is_some() {
        notes::record(
            project_admitted.as_ref(),
            notes::Note {
                counts: admission.counts,
                decision: admission.decision.clone(),
                redaction_counts,
            },
        );
    }
    Ok(SourceAdmission {
        text: project_admitted,
        storable,
        classification: admission.classification,
    })
}

fn preserve_bytes_rewrite_reason(
    source: &str,
    gateway_admitted: &str,
    project_admitted: &str,
    project_rewrote_original: bool,
) -> Option<&'static str> {
    if project_rewrote_original || project_admitted != gateway_admitted {
        Some("source content withheld: an active policy would rewrite a digest-bound source")
    } else if gateway_admitted != source {
        Some("source content withheld: the context gateway would rewrite a digest-bound source")
    } else {
        None
    }
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
    if policy.config_unloadable() || !policy.gateway_switch() || !required {
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
    let policy = carrier::current_policy();
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

/// The HUD line recorded when exactly `admitted` was produced, stated for
/// `delivered`: the part of `admitted` a tool actually returns (a line
/// window, a compressed view). Masked values are counted from the markers
/// `delivered` carries, never above the object's own counts, so a window
/// without a masked value gets no redaction count — and no line when nothing
/// else applies (#2135). Every other part of the note describes the object.
#[must_use]
pub fn note_for(admitted: &str, delivered: &str) -> Option<String> {
    let note = notes::lookup(admitted)?;
    let mut counts = note.counts;
    if note.decision.disposition == ContextDispositionV1::AllowRedacted {
        let pii = crate::core::input_filters::pii::checksummed_marker_count(delivered);
        let other = delivered
            .matches("[REDACTED:")
            .count()
            .saturating_sub(pii + delivered.matches(INJECTION_MARKER).count());
        counts.pii = counts.pii.min(saturating_u32(pii));
        counts.secrets = counts.secrets.min(saturating_u32(other));
    }
    render_hud(counts, &note.decision, false, note.redaction_counts)
}

const INJECTION_MARKER: &str = "[REDACTED:prompt-injection]";

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn reason(code: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(code).expect("built-in reason codes are valid")
}

/// Record in the running call's receipt that a derived copy admitted before
/// (at its cache fill) was asked of its source again at this serve and
/// refused: the after side of the revocation boundary (C04). The decision is
/// a denial whose reason is the source state (`source.revoked`, …); its
/// object is one fixed digest for every such refusal, so the receipt names
/// neither the item nor its text and reads the same for every item withheld
/// this way (K02/S02). No capture: nothing is recorded.
pub(crate) fn record_recheck_refusal(
    admission: &stores::StoreAdmission,
    validity: lean_ctx_protocol::context_lineage::SourceValidityV1,
) {
    let Some(capture) = capture::current() else {
        return;
    };
    let policy = admission.admission_policy();
    capture.bind_policy(
        policy.gateway.mode,
        fingerprint_digest(&policy.fingerprint()),
    );
    let decision = ContextDecisionV1 {
        object: fingerprint_digest(b"source-recheck-refused"),
        disposition: ContextDispositionV1::Deny,
        reason_codes: vec![reason(validity.reason_code())],
        signals: Vec::new(),
        required_transformations: Vec::new(),
    };
    capture.record(&decision, AdmissionCounts::default());
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
mod shell_admission_regression_tests {
    use super::*;

    #[test]
    fn rendered_notice_uses_a_newline_and_survives_empty_text() {
        let admission = ToolOutputAdmission {
            text: Some("body".to_string()),
            notice: Some("[notice]".to_string()),
        };
        assert_eq!(admission.rendered(), "[notice]\nbody");

        let empty = ToolOutputAdmission {
            text: Some(String::new()),
            notice: Some("[notice]".to_string()),
        };
        assert_eq!(empty.rendered(), "[notice]");
    }

    #[test]
    fn incomplete_capture_is_never_persistable() {
        let policy = AdmissionPolicy::builtin(
            ContextGatewayConfig::default(),
            SecretDetectionConfig::default(),
        );
        let incomplete = OutputCompleteness {
            reader_incomplete: true,
            ..OutputCompleteness::complete()
        };
        assert!(
            admit_output_for_storage("partial output", incomplete, &policy).is_none(),
            "an incomplete prefix must not seed a cache"
        );
    }

    /// #2164: a background job whose command exited while a process it left
    /// running still holds the pipe keeps its captured output for the status
    /// poll; every other gap, and a mandatory gateway, still withholds it.
    #[test]
    fn background_poll_keeps_a_capture_whose_only_gap_is_a_held_pipe() {
        let developer = AdmissionPolicy::builtin(
            ContextGatewayConfig::default(),
            SecretDetectionConfig::default(),
        );
        let held_pipe = OutputCompleteness {
            reader_incomplete: true,
            ..OutputCompleteness::complete()
        };
        assert_eq!(
            admit_output_for_poll("hello stdout\nboom stderr", held_pipe, &developer).as_deref(),
            Some("hello stdout\nboom stderr")
        );
        for other_gap in [
            OutputCompleteness {
                truncated: true,
                ..held_pipe
            },
            OutputCompleteness {
                timed_out: true,
                ..held_pipe
            },
            OutputCompleteness {
                cancelled: true,
                ..OutputCompleteness::complete()
            },
        ] {
            assert!(admit_output_for_poll("partial", other_gap, &developer).is_none());
        }

        let governed = AdmissionPolicy::builtin(
            ContextGatewayConfig {
                mode: GatewayModeV1::Governed,
                ..ContextGatewayConfig::default()
            },
            SecretDetectionConfig::default(),
        );
        if governed.enabled_effective() {
            assert!(admit_output_for_poll("partial", held_pipe, &governed).is_none());
        }
    }

    #[test]
    fn mandatory_gateway_marks_and_withholds_incomplete_capture() {
        let gateway = ContextGatewayConfig {
            mode: GatewayModeV1::Governed,
            ..ContextGatewayConfig::default()
        };
        let policy = AdmissionPolicy::builtin(gateway, SecretDetectionConfig::default());
        let incomplete = OutputCompleteness {
            truncated: true,
            ..OutputCompleteness::complete()
        };
        let mut admission = admit("partial output", None, &policy);
        mark_capture_incomplete(&mut admission, incomplete, &policy);

        if policy.enabled_effective() {
            assert!(admission.text.is_none());
            assert!(
                admission
                    .decision
                    .reason_codes
                    .iter()
                    .any(|code| code.as_str() == "coverage.output_partial")
            );
        }
    }

    /// AWS's documented example access key, assembled so the source carries
    /// no literal the public secret scanner flags.
    fn aws_documentation_key() -> String {
        ["AK", "IA", "IOSFODNN7EXAMPLE"].concat()
    }

    #[test]
    fn overlapping_policy_and_gateway_redactions_withhold_noncomposable_output() {
        let pack = crate::core::policy::parse(&format!(
            "name = \"composition-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[redaction]\ncustomer = 'customer {}'",
            aws_documentation_key()
        ))
        .expect("valid policy");
        let _project_policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
            crate::core::policy::resolve(&pack).expect("resolved policy"),
        ));
        let original = format!("customer {}", aws_documentation_key());
        let original = original.as_str();
        let project_original = crate::core::policy::content::protect_active(original)
            .expect("project redaction is allowed")
            .into_owned();
        let policy = AdmissionPolicy::builtin(
            ContextGatewayConfig::default(),
            SecretDetectionConfig::default(),
        );
        let gateway_output = admit(original, None, &policy)
            .text
            .expect("developer gateway redacts and delivers");
        assert_ne!(project_original.as_str(), original);
        assert_ne!(gateway_output.as_str(), original);
        assert!(
            compose_active_gateway_output(
                &project_original,
                &gateway_output,
                &policy,
                OutputCompleteness::complete(),
            )
            .is_none(),
            "overlapping redactions that depend on order must be withheld"
        );
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod receipt_tests;

#[cfg(test)]
mod store_tests;

#[cfg(test)]
mod egress_tests;

#[cfg(test)]
mod egress_api_tests;
