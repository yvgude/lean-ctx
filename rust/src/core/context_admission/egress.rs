// SPDX-License-Identifier: Apache-2.0
//! Final egress control in the BYOK proxy (G6, plan cases 41–46).
//!
//! Everything the proxy forwards to a model provider passes the gateway one
//! last time, after compression, routing, translation and every cache-safety
//! revert — on exactly the request body that leaves the machine:
//!
//! - every content string of the request (system prompt, messages, tool
//!   results, tool arguments; OpenAI Chat/Responses, Anthropic Messages,
//!   Gemini) is admitted under the current policy; masked values are
//!   rewritten in place, withheld objects are replaced by a content-free
//!   marker;
//! - content the model provider has signed or encrypted (thinking signatures,
//!   `encrypted_content`) is inspected but never rewritten: altering it would
//!   break the request, so a finding there is delivered and flagged in
//!   developer mode and refuses the request in governed/sovereign mode;
//! - media (images, inline data) cannot be inspected and is reported as such;
//!   governed/sovereign mode refuses it;
//! - the destination is part of the decision: content classified `restricted`
//!   never goes to a remote model (Community rule; organisation matrices are
//!   Enterprise);
//! - a body the proxy cannot parse is forwarded only as an honestly marked
//!   opaque payload (developer mode) and refused otherwise;
//! - one Decision Receipt per request binds the policy digest, the destination
//!   and the SHA-256 of the forwarded bytes.
//!
//! Prompt-cache stability (#498/#1912): admission is a pure function of
//! (policy, text), so the same prefix is rewritten to the same bytes on every
//! turn, and a request with nothing to change is forwarded byte-identical.

use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "http-server")]
use lean_ctx_protocol::context_gateway::EgressTransportObservationV1;
use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDecisionReceiptV1, ContextDecisionV1, ContextDispositionV1,
    DestinationLocalityV1, DestinationV1, EgressAdmissionOutcomeV1, EgressAdmissionReceiptV1,
    ReasonCodeV1,
};
use lean_ctx_protocol::{ProtocolReference, Sha256Digest};
use serde_json::Value;

use super::capture::{self, AdmissionCapture, CallIdentity, Delivered};
use super::receipt_store::keyed::KeyedRefs;
use super::{
    AdmissionCounts, AdmissionPolicy, admit, admit_uninspectable, fingerprint_digest, reason,
};

/// Receipts of proxy requests are listed under this key (`lean-ctx inspect
/// --proxy`), separate from any project.
pub const PROXY_RECEIPT_KEY: &str = "<proxy>";

/// Who dispatches an admitted egress request — what its receipt is indexed
/// under and what its audit entries and trace sidecar name (G08).
#[derive(Debug, Clone, Copy)]
pub(crate) enum EgressCaller<'a> {
    /// The lean-ctx proxy: index `<proxy>`, tool `proxy`, sidecar transport
    /// `proxy` with the provider label as its endpoint.
    Proxy,
    /// A host that dispatches the request itself (`lean-ctx engine
    /// egress-admit` / `egress-prepare`, a lean-ctx client command): indexed
    /// under the project of the request, tool and sidecar endpoint are the
    /// host `operation`, sidecar transport `host_egress`.
    Host {
        /// The project root the request was admitted for; canonicalised by
        /// the receipt store (`receipt_store::roots`).
        project_root: &'a str,
        /// The operation that asked: a short plain word (`egress-prepare`,
        /// `team`). It must pass the sidecar's word check.
        operation: &'a str,
    },
}

impl EgressCaller<'_> {
    fn receipt_index(&self) -> &str {
        match self {
            Self::Proxy => PROXY_RECEIPT_KEY,
            Self::Host { project_root, .. } => project_root,
        }
    }

    fn tool(&self) -> &str {
        match self {
            Self::Proxy => "proxy",
            Self::Host { operation, .. } => operation,
        }
    }
}

/// Where the request goes.
#[derive(Debug, Clone, Copy)]
pub struct EgressTarget<'a> {
    /// Provider label as the proxy meters it (`Anthropic`, `OpenAI`, …).
    pub provider: &'a str,
    pub model: Option<&'a str>,
    /// The effective upstream base URL, after routing.
    pub upstream_base: &'a str,
}

/// Destination metadata derived from a trusted configured route. Caller headers
/// and credentials never populate this structure.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrustedDestinationBinding<'a> {
    /// Registry provider identity, when the request selected a configured route.
    pub provider_id: Option<&'a str>,
    /// Configured locality; when present it takes precedence over URL inference.
    pub local: Option<bool>,
    /// Non-secret operator account label. This is not provider-verified identity.
    pub account_ref: Option<&'a str>,
    /// Configured destination region; Bedrock supplies its signing region.
    pub region: Option<&'a str>,
}

/// The destination the receipt names. Locality comes from the upstream host;
/// anything not provably on this machine is remote.
#[must_use]
pub fn destination(target: &EgressTarget<'_>) -> DestinationV1 {
    destination_with_binding(target, TrustedDestinationBinding::default())
}

/// Build a receipt destination from the effective endpoint plus trusted route
/// metadata. A route's registry ID and configured locality outrank its wire
/// shape label and the URL heuristic.
#[must_use]
pub fn destination_with_binding(
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> DestinationV1 {
    let reference = |raw: &str| {
        ProtocolReference::new(raw.to_owned()).ok().or_else(|| {
            let digest = blake3::hash(raw.as_bytes()).to_hex();
            ProtocolReference::new(digest[..32].to_owned()).ok()
        })
    };
    DestinationV1 {
        provider: reference(
            &binding
                .provider_id
                .unwrap_or(target.provider)
                .to_ascii_lowercase(),
        )
        .unwrap_or_else(|| ProtocolReference::new("provider").expect("valid reference")),
        model: target.model.and_then(reference),
        locality: binding.local.map_or_else(
            || locality(target.upstream_base),
            |local| {
                if local {
                    DestinationLocalityV1::Local
                } else {
                    DestinationLocalityV1::Remote
                }
            },
        ),
        organization_managed: false,
        account_ref: binding.account_ref.and_then(reference),
        region: binding.region.and_then(reference),
    }
}

fn locality(upstream_base: &str) -> DestinationLocalityV1 {
    let Some(host) = reqwest::Url::parse(upstream_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return DestinationLocalityV1::Unknown;
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let local = match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => bare == "localhost" || bare.ends_with(".localhost"),
    };
    if local {
        DestinationLocalityV1::Local
    } else {
        DestinationLocalityV1::Remote
    }
}

/// What happens to the request body.
#[derive(Debug)]
pub enum EgressBody {
    /// Nothing to change: forward the bytes exactly as prepared.
    Unchanged,
    /// Forward this admitted document instead.
    Rewritten(Value),
    /// Do not forward. Content-free, names the reason codes.
    Refused(String),
}

/// The egress decision for one request, with the capture its receipt is
/// built from once the forwarded bytes are known.
#[derive(Debug)]
pub struct EgressOutcome {
    pub body: EgressBody,
    /// Most sensitive classification found in the request (information-flow
    /// join over every inspected string). `None` when nothing was inspected.
    pub classification: Option<ClassificationV1>,
    capture: Option<Arc<AdmissionCapture>>,
    destination: DestinationV1,
    policy_fingerprint: Vec<u8>,
    /// Frozen with admission, so a later config change cannot weaken the gate.
    receipt_required: bool,
    /// Keyed references of the caller the route told apart (AP10-C6) and of
    /// the request head the bytes are sent with (AP10-C2), for the receipt's
    /// trace sidecar; set by the proxy after the decision.
    refs: KeyedRefs,
}

/// Content-free lifecycle evidence returned after admission records persist.
#[derive(Debug, Clone, Default)]
pub(crate) struct EgressEvidence {
    pub(crate) decision_receipt: Option<ContextDecisionReceiptV1>,
    pub(crate) admission: Option<EgressAdmissionReceiptV1>,
    pub(crate) admission_digest: Option<Sha256Digest>,
}

impl EgressOutcome {
    fn passthrough(destination: DestinationV1, policy_fingerprint: Vec<u8>) -> Self {
        Self {
            body: EgressBody::Unchanged,
            classification: None,
            capture: None,
            destination,
            policy_fingerprint,
            receipt_required: false,
            refs: KeyedRefs::default(),
        }
    }

    /// The receipt duty frozen with this admission.
    #[cfg(any(test, feature = "http-server"))]
    pub(crate) fn receipt_required(&self) -> bool {
        self.receipt_required
    }

    /// Name the caller this decision was taken for by its keyed reference
    /// (`receipt_store::keyed`). Decides nothing.
    #[must_use]
    #[cfg(any(test, feature = "http-server"))]
    pub(crate) fn for_principal(mut self, principal_ref: Option<String>) -> Self {
        self.refs.principal = principal_ref;
        self
    }

    /// Name the request head the bytes will be sent with by its keyed
    /// reference. Decides nothing.
    #[must_use]
    #[cfg(any(test, feature = "http-server"))]
    pub(crate) fn with_request_head(mut self, head_ref: Option<String>) -> Self {
        self.refs.request_head = head_ref;
        self
    }

    /// Re-read the Gateway configuration before durable preparation or output.
    /// The project fingerprint is separately verified by `PublicationAuthority`.
    pub(crate) fn policy_epoch_current_for_project(&self, project_fingerprint: &[u8]) -> bool {
        let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
        let current = combined_policy_fingerprint(&policy.fingerprint(), project_fingerprint);
        self.policy_fingerprint == current
    }
}

/// The walk of one admission over a request document: positions, provider
/// shapes and the per-string decisions.
#[path = "egress_walk.rs"]
mod walk;

pub use walk::{PartKind, SealedPart, SealedRequest, SealedVerdict};
use walk::{Position, SealedWalk, Walk, Wire};

/// Run `admission` — one call of an `admit_request*` function — with
/// `resolver` answering for the provider-sealed items the request returns
/// (thinking signatures, encrypted reasoning, thought signatures). It is
/// called once, after the walk and only where full inspection is demanded,
/// with the items and the parts of the request that are not host-authored
/// text, and returns one verdict per item; without a resolver such an item is of unknown origin and
/// refused there. The resolver gets the exact sealed bytes and must keep
/// none of them.
pub fn with_sealed_resolver<T>(
    resolver: Box<dyn Fn(&SealedRequest<'_>) -> Vec<SealedVerdict>>,
    admission: impl FnOnce() -> T,
) -> T {
    walk::with_resolver(resolver, admission)
}

/// Admit a parsed request body before it is forwarded to `target`.
#[must_use]
pub fn admit_request(doc: &Value, target: &EgressTarget<'_>) -> EgressOutcome {
    admit_request_inner(doc, target, None, TrustedDestinationBinding::default())
}

/// Admit a proxy body with trusted provider-route metadata.
#[must_use]
pub fn admit_request_with_binding(
    doc: &Value,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    admit_request_inner(doc, target, None, binding)
}

/// Admit a host request under the project policy captured by the caller's
/// explicit source view. The caller binds and revalidates that view around both
/// admission and receipt persistence.
pub(crate) fn admit_request_for_project(
    doc: &Value,
    target: &EgressTarget<'_>,
    project_fingerprint: &[u8],
) -> EgressOutcome {
    admit_request_inner(
        doc,
        target,
        Some(project_fingerprint),
        TrustedDestinationBinding::default(),
    )
}

#[cfg(any(test, feature = "http-server"))]
pub(crate) fn admit_request_for_project_with_binding(
    doc: &Value,
    target: &EgressTarget<'_>,
    project_fingerprint: &[u8],
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    admit_request_inner(doc, target, Some(project_fingerprint), binding)
}

/// Whether the walk decided on at least one object and withheld every one:
/// no itemized decision delivers content and none was counted beyond them.
fn withholds_everything(capture: &AdmissionCapture) -> bool {
    let decided: Vec<_> = capture
        .itemized()
        .into_iter()
        .map(|(decision, _)| decision)
        .filter(|decision| !capture::is_note(decision))
        .collect();
    !decided.is_empty()
        && capture.counted_only() == 0
        && decided
            .iter()
            .all(|decision| !decision.disposition.delivers_content())
}

/// The one decision of an admission that inspected nothing but was made under
/// an observed organization decision: the document as it leaves, permitted,
/// with the fixed reason for bytes without a source decision. It claims no
/// inspected source and carries no counts.
fn record_policy_only(
    capture: &AdmissionCapture,
    doc: &Value,
    policy: &AdmissionPolicy,
    policy_fingerprint: &[u8],
) {
    capture.record_unsourced(
        fingerprint_digest(doc.to_string().as_bytes()),
        policy.gateway.mode,
        fingerprint_digest(policy_fingerprint),
    );
}

fn admit_request_inner(
    doc: &Value,
    target: &EgressTarget<'_>,
    project_fingerprint: Option<&[u8]>,
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    let destination = destination_with_binding(target, binding);
    // ONE snapshot: configuration and environment are read here and nowhere
    // else in this admission. The fingerprint, the switch and every decision
    // of the walk below use `policy`.
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    #[cfg(test)]
    failpoint::after_snapshot();
    // The project policy belongs to the same snapshot: resolved once here and
    // used for every string of the walk, so one admission never sees two
    // project-policy states.
    // The organization decision the verifier stated comes from that same
    // resolution; nothing about it is read again for this admission.
    let (project_policy, org_observation) = match project_fingerprint {
        Some(_) => crate::core::policy::runtime::active_observed(),
        None => (None, None),
    };
    let project_policy_active = project_policy.is_some();
    let destination_policy_active = policy.gateway.has_destination_policy();
    let gateway_fingerprint = policy.fingerprint();
    let fingerprint = project_fingerprint.map_or_else(
        || gateway_fingerprint.clone(),
        |project| combined_policy_fingerprint(&gateway_fingerprint, project),
    );
    let gateway_enabled = policy.enabled_effective();
    // A verified organization decision is in force for this admission even
    // when it leaves no floor (a signed unpin) and nothing else is active:
    // such a request is not an unrecorded passthrough. Without an observed
    // decision — no organization — nothing changes here.
    let org_observed = org_observation.is_some();
    if !gateway_enabled && !project_policy_active && !destination_policy_active && !org_observed {
        return EgressOutcome::passthrough(destination, fingerprint);
    }
    let capture = AdmissionCapture::new();
    capture.bind_policy(policy.gateway.mode, fingerprint_digest(&fingerprint));
    // The pack of this admission's snapshot, by name, for the trace sidecar.
    if let Some(active) = &project_policy {
        capture.note_pack(&active.resolved.name, &active.resolved.version);
    }
    // Noted with the snapshot it was observed in, and only for an admission
    // that resolved a project: also when nothing was observed, so that a
    // capture merged from admissions under different decisions shows it.
    if project_fingerprint.is_some() {
        capture.note_org_policy(org_observation);
    }
    let policy_fingerprint = fingerprint.clone();
    if let Some(codes) = policy.config_refusal_codes() {
        // Mode and destination rules are unknown: nothing leaves, rather than
        // forwarding a body in which every string was replaced by a notice.
        capture.record(
            &ContextDecisionV1 {
                object: fingerprint_digest(doc.to_string().as_bytes()),
                disposition: ContextDispositionV1::Deny,
                reason_codes: codes.iter().map(|code| reason(code)).collect(),
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            AdmissionCounts::default(),
        );
        return EgressOutcome {
            body: EgressBody::Refused(format!(
                "request withheld by the context gateway ({})",
                codes.join(", ")
            )),
            classification: None,
            capture: Some(capture),
            destination,
            policy_fingerprint,
            receipt_required: true,
            refs: KeyedRefs::default(),
        };
    }
    if destination_policy_active {
        let allowed = policy.gateway.allows_destination(&destination);
        let object = fingerprint_digest(doc.to_string().as_bytes());
        capture.record(
            &ContextDecisionV1 {
                object,
                disposition: if allowed {
                    ContextDispositionV1::Allow
                } else {
                    ContextDispositionV1::Deny
                },
                reason_codes: vec![reason(if allowed {
                    "destination.constraint_satisfied"
                } else {
                    "destination.constraint_unsatisfied"
                })],
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            AdmissionCounts::default(),
        );
        if !allowed {
            return EgressOutcome {
                body: EgressBody::Refused(
                    "request withheld by the context gateway (destination.constraint_unsatisfied)"
                        .to_owned(),
                ),
                classification: None,
                capture: Some(capture),
                destination,
                policy_fingerprint,
                receipt_required: true,
                refs: KeyedRefs::default(),
            };
        }
    }
    if !gateway_enabled && !project_policy_active {
        // Here only because an organization decision was observed (no
        // destination rule recorded anything): nothing of the request was
        // inspected, and the receipt says exactly that — the document left
        // without a source decision. No source, token or redaction count is
        // made up for it; the observation itself is in the sidecar.
        if org_observed && capture.is_empty() {
            record_policy_only(&capture, doc, &policy, &policy_fingerprint);
        }
        return EgressOutcome {
            body: EgressBody::Unchanged,
            classification: None,
            capture: Some(capture),
            destination,
            policy_fingerprint,
            receipt_required: true,
            refs: KeyedRefs::default(),
        };
    }
    let mut admitted = doc.clone();
    let mut walk = Walk {
        policy: &policy,
        fingerprint,
        capture: &capture,
        remote: destination.locality != DestinationLocalityV1::Local,
        mandatory: policy.detectors_mandatory(),
        project: project_policy,
        wire: Wire::of(target.provider),
        anthropic_safeguards: target.provider.eq_ignore_ascii_case("anthropic"),
        refused: None,
        rewritten: false,
        classification: None,
        nodes: 0,
        partial: false,
        sealed: SealedWalk::default(),
        provider: destination.provider.as_str().to_owned(),
        at: walk::Location::default(),
        withheld_classification: None,
    };
    walk.walk(&mut admitted, Position::Root, false, 0);
    // Provider-sealed items are decided last: whether one may leave depends
    // on every other part of the request having been admitted.
    walk.settle_sealed();
    // The project policy of this admission's snapshot was held for the whole
    // walk. A document with no string to evaluate never asked it anything,
    // so its signed horizon is checked here, for every document, before
    // anything is let out.
    if walk.refused.is_none()
        && walk
            .project
            .as_ref()
            .is_some_and(|active| !active.within_horizon())
    {
        walk.refused = Some("request withheld by the context gateway (policy.expired)".into());
    }
    // A document with nothing to decide on (no string at all) leaves the walk
    // with an empty capture. Under an observed organization decision it is
    // still recorded as what it is; every other empty capture stays empty.
    if org_observed && walk.refused.is_none() && capture.is_empty() {
        record_policy_only(&capture, doc, &policy, &policy_fingerprint);
    }
    let body = match (walk.refused.take(), walk.rewritten) {
        (Some(refusal), _) => EgressBody::Refused(refusal),
        // A rewrite in which every decided object was withheld leaves nothing
        // admitted to send: it is the refusal the call routes give the same
        // content, receipted as withheld — not a request of markers whose
        // "delivered" receipt cannot be sealed (H01).
        (None, true) if withholds_everything(&capture) => EgressBody::Refused(format!(
            "request withheld by the context gateway ({})",
            capture
                .deny_reason_codes()
                .iter()
                .map(ReasonCodeV1::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        (None, true) => EgressBody::Rewritten(admitted),
        (None, false) => EgressBody::Unchanged,
    };
    // What leaves is classified by what leaves: a block withheld in place for
    // the destination does not classify the forwarded request (the proxy's
    // joined check would otherwise refuse it whole again); a refused request
    // states everything it carried.
    let classification = match (&body, walk.withheld_classification) {
        (EgressBody::Refused(_), Some(withheld)) => Some(
            walk.classification
                .map_or(withheld, |seen| seen.join(withheld)),
        ),
        _ => walk.classification,
    };
    EgressOutcome {
        body,
        classification,
        capture: Some(capture),
        destination,
        policy_fingerprint,
        // The one "receipt store is mandatory" predicate MCP and CLI use,
        // under this admission's policy snapshot, plus the proxy's own
        // destination-policy condition.
        // An observed organization decision is named in the receipt's sealed
        // trace, so its receipt is owed as well.
        receipt_required: super::receipt_store::mandatory_under(&policy, destination_policy_active)
            || org_observed,
        refs: KeyedRefs::default(),
    }
}

/// The policy an egress decision was taken under, detached from the outcome so
/// it can be re-checked after the outcome was consumed (at bind, and before
/// every dispatch). It covers the gateway settings, the secret patterns, the
/// presence of the semantic detectors and the configuration load state; for a
/// project outcome also the project fingerprint the caller supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEpoch {
    fingerprint: Vec<u8>,
}

impl PolicyEpoch {
    /// Whether the gateway policy read NOW, combined with `project_fingerprint`
    /// exactly as at admission (`None` for outcomes admitted without a project,
    /// including opaque ones), is still the one this epoch was taken under.
    #[must_use]
    pub fn is_current(&self, project_fingerprint: Option<&[u8]>) -> bool {
        let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
        self.matches(&policy, project_fingerprint)
    }

    /// Whether `policy` (one admission snapshot) with `project_fingerprint` is
    /// the policy of this epoch. The gateway fingerprint binds every effective
    /// input: settings, secret patterns, the effective switch including the
    /// environment override, semantic-detector presence and the load state.
    /// Project, root and role are bound through the caller's fingerprint.
    pub(crate) fn matches(
        &self,
        policy: &AdmissionPolicy,
        project_fingerprint: Option<&[u8]>,
    ) -> bool {
        let gateway = policy.fingerprint();
        let current = project_fingerprint.map_or_else(
            || gateway.clone(),
            |project| combined_policy_fingerprint(&gateway, project),
        );
        self.fingerprint == current
    }
}

impl EgressOutcome {
    /// The organization decision this admission noted from its own snapshot
    /// (`None`: it has no capture). For tests of the callers that decide
    /// which admission runs.
    #[cfg(test)]
    pub(crate) fn org_evidence(&self) -> Option<super::capture::OrgEvidence> {
        self.capture.as_ref().map(|capture| capture.org_policy())
    }

    /// The policy epoch of this decision; bound for every outcome, opaque and
    /// refused ones included.
    #[must_use]
    pub fn policy_epoch(&self) -> PolicyEpoch {
        PolicyEpoch {
            fingerprint: self.policy_fingerprint.clone(),
        }
    }

    /// One outcome for two admissions of the same request: `self` is the
    /// admission of the caller's content (original classification, redaction
    /// provenance), `later` the admission of the final content that leaves.
    /// The result has ONE capture holding both decision sets, the higher of
    /// the two classifications, and the body and destination of `later`. A
    /// refusal in either admission refuses the joined outcome.
    ///
    /// Only outcomes of the IDENTICAL policy epoch are joined: the same
    /// gateway snapshot (settings, secret patterns, effective switch, detector
    /// presence, load state) and the same project fingerprint. Two admissions
    /// taken under different epochs do not describe one decision; they are not
    /// merged. The result is then `later` refused with `policy.changed` —
    /// a typed refusal whose receipt carries that deny decision.
    #[must_use]
    pub fn join(self, later: Self) -> Self {
        if self.policy_fingerprint != later.policy_fingerprint {
            return later.refuse(&[reason("policy.changed")]);
        }
        let capture = match (self.capture, later.capture) {
            (Some(earlier), Some(final_capture)) => {
                let joined = AdmissionCapture::new();
                joined.absorb(&earlier);
                joined.absorb(&final_capture);
                if let Some((mode, digest)) = final_capture
                    .bound_policy()
                    .or_else(|| earlier.bound_policy())
                {
                    joined.bind_policy(mode, digest);
                }
                Some(joined)
            }
            (earlier, final_capture) => final_capture.or(earlier),
        };
        let classification = match (self.classification, later.classification) {
            (Some(earlier), Some(final_class)) => Some(earlier.join(final_class)),
            (earlier, final_class) => final_class.or(earlier),
        };
        let body = if matches!(self.body, EgressBody::Refused(_))
            && !matches!(later.body, EgressBody::Refused(_))
        {
            self.body
        } else {
            later.body
        };
        Self {
            body,
            classification,
            capture,
            destination: later.destination,
            policy_fingerprint: later.policy_fingerprint,
            receipt_required: self.receipt_required || later.receipt_required,
            refs: later.refs.or(self.refs),
        }
    }

    /// Reason codes of a refused outcome, taken from the deny decisions its
    /// capture holds (empty when the outcome is not refused). Callers use this
    /// instead of parsing the refusal text.
    #[must_use]
    pub fn refusal_codes(&self) -> Vec<ReasonCodeV1> {
        match (&self.body, &self.capture) {
            (EgressBody::Refused(_), Some(capture)) => capture.deny_reason_codes(),
            _ => Vec::new(),
        }
    }

    /// The ids of the withheld blocks a refusal is about — those with a
    /// recorded explanation (`lean-ctx gateway why <id>`), in decision
    /// order, at most eight. Empty for an outcome that is not refused.
    #[must_use]
    pub fn refused_blocks(&self) -> Vec<String> {
        const MAX: usize = 8;
        let (EgressBody::Refused(_), Some(capture)) = (&self.body, &self.capture) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = Vec::new();
        for (decision, _) in capture.itemized() {
            if decision.disposition.delivers_content() {
                continue;
            }
            if let Some(id) = super::withheld::block_id(&decision.object)
                && !ids.contains(&id)
                && super::withheld::lookup(&id).is_some()
            {
                ids.push(id);
                if ids.len() == MAX {
                    break;
                }
            }
        }
        ids
    }

    /// Refuse after admission (the policy changed, an authority is no longer
    /// current): records a deny decision with `reason_codes` in the capture,
    /// so the refusal receipt contains it, and replaces the body by the
    /// content-free refusal. The receipt becomes mandatory.
    #[must_use]
    pub fn refuse(mut self, reason_codes: &[ReasonCodeV1]) -> Self {
        let capture = self.capture.take().unwrap_or_else(|| {
            // A passthrough outcome had no capture; bind the epoch it was
            // decided under and the mode in force now.
            let capture = AdmissionCapture::new();
            let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
            capture.bind_policy(
                policy.gateway.mode,
                fingerprint_digest(&self.policy_fingerprint),
            );
            capture
        });
        capture.record(
            &ContextDecisionV1 {
                object: fingerprint_digest(b"egress-refused-after-admission"),
                disposition: ContextDispositionV1::Deny,
                reason_codes: reason_codes.to_vec(),
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            AdmissionCounts::default(),
        );
        let codes: Vec<&str> = reason_codes.iter().map(ReasonCodeV1::as_str).collect();
        self.body = EgressBody::Refused(format!(
            "request withheld by the context gateway ({})",
            codes.join(", ")
        ));
        self.capture = Some(capture);
        self.receipt_required = true;
        self
    }

    /// [`Self::refuse`] for callers outside this module, which name reason
    /// codes as text.
    #[must_use]
    pub fn refuse_with(self, reason_codes: &[&str]) -> Self {
        let codes: Vec<ReasonCodeV1> = reason_codes.iter().map(|code| reason(code)).collect();
        self.refuse(&codes)
    }

    /// State something about the request in its receipt without deciding
    /// anything: one allow decision per entry of `reason_codes`, on the fixed
    /// note object, carrying that code. A note names no content and is no
    /// source — the receipt's source counts do not move
    /// (`capture::note_object`). Body, classification and whether a receipt
    /// is required stay as they are; an outcome that has no capture (nothing
    /// was decided under a policy) records nothing.
    #[must_use]
    pub fn note_with(self, reason_codes: &[&str]) -> Self {
        if let Some(capture) = self.capture.as_ref() {
            for code in reason_codes {
                capture.record(
                    &ContextDecisionV1 {
                        object: capture::note_object(),
                        disposition: ContextDispositionV1::Allow,
                        reason_codes: vec![reason(code)],
                        signals: Vec::new(),
                        required_transformations: Vec::new(),
                    },
                    AdmissionCounts::default(),
                );
            }
        }
        self
    }
}

/// The decision for one text that is about to leave for `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextVerdict {
    /// Forward the text as it is.
    Delivered,
    /// Forward `text` instead; `reason_codes` name what was masked.
    Redacted {
        text: String,
        reason_codes: Vec<ReasonCodeV1>,
    },
    /// Do not forward. Content-free.
    Withheld { reason_codes: Vec<ReasonCodeV1> },
}

/// Admit one plain text for `target` with a typed verdict, for callers that
/// would otherwise wrap it in a JSON string and match the withheld marker.
/// Gateway layer only (configuration state, destination rule, detectors,
/// restricted-to-remote): it takes no project fingerprint and writes no
/// receipt — a caller that needs either uses the document functions.
#[must_use]
pub fn admit_text(
    text: &str,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> TextVerdict {
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    if let Some(codes) = policy.config_refusal_codes() {
        return TextVerdict::Withheld {
            reason_codes: codes.iter().map(|code| reason(code)).collect(),
        };
    }
    let destination = destination_with_binding(target, binding);
    if policy.gateway.has_destination_policy() && !policy.gateway.allows_destination(&destination) {
        return TextVerdict::Withheld {
            reason_codes: vec![reason("destination.constraint_unsatisfied")],
        };
    }
    if text.is_empty() || !policy.enabled_effective() {
        return TextVerdict::Delivered;
    }
    let admission = admit(text, None, &policy);
    let mut reason_codes = admission.decision.reason_codes;
    match admission.text {
        None => TextVerdict::Withheld { reason_codes },
        Some(_)
            if destination.locality != DestinationLocalityV1::Local
                && admission.classification >= ClassificationV1::Restricted =>
        {
            reason_codes.push(reason("destination.remote_restricted"));
            TextVerdict::Withheld { reason_codes }
        }
        Some(delivered) if delivered == text => TextVerdict::Delivered,
        Some(delivered) => TextVerdict::Redacted {
            text: delivered,
            reason_codes,
        },
    }
}

fn combined_policy_fingerprint(gateway: &[u8], project: &[u8]) -> Vec<u8> {
    let mut combined = b"lean-ctx-egress-project-policy-v1\0".to_vec();
    combined.extend_from_slice(&(gateway.len() as u64).to_be_bytes());
    combined.extend_from_slice(gateway);
    combined.extend_from_slice(&(project.len() as u64).to_be_bytes());
    combined.extend_from_slice(project);
    combined
}

/// A body the proxy cannot parse (opaque content encoding, not JSON). It is
/// never claimed as inspected: developer mode forwards it marked as opaque,
/// governed and sovereign mode refuse it.
#[must_use]
pub fn admit_opaque(bytes: usize, target: &EgressTarget<'_>) -> EgressOutcome {
    admit_opaque_with_binding(bytes, target, TrustedDestinationBinding::default())
}

/// Opaque-body admission with the same trusted destination metadata used for
/// parsed proxy requests.
#[must_use]
pub fn admit_opaque_with_binding(
    bytes: usize,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    admit_opaque_inner(bytes, target, None, binding)
}

/// Opaque-body admission under the project policy captured by the caller's
/// source view: the outcome's policy epoch and receipt digest bind the project
/// fingerprint, exactly as `admit_request_for_project_with_binding` does for a
/// parsed body. The bytes themselves stay uninspected — a project content rule
/// cannot be evaluated on them, which is why governed and sovereign mode
/// refuse an opaque body in the first place.
#[must_use]
pub fn admit_opaque_for_project_with_binding(
    bytes: usize,
    target: &EgressTarget<'_>,
    project_fingerprint: &[u8],
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    admit_opaque_inner(bytes, target, Some(project_fingerprint), binding)
}

fn admit_opaque_inner(
    bytes: usize,
    target: &EgressTarget<'_>,
    project_fingerprint: Option<&[u8]>,
    binding: TrustedDestinationBinding<'_>,
) -> EgressOutcome {
    let destination = destination_with_binding(target, binding);
    let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    let gateway_fingerprint = policy.fingerprint();
    let policy_fingerprint = project_fingerprint.map_or_else(
        || gateway_fingerprint.clone(),
        |project| combined_policy_fingerprint(&gateway_fingerprint, project),
    );
    let destination_policy_active = policy.gateway.has_destination_policy();
    let gateway_enabled = policy.enabled_effective();
    // This path resolves no policy and asks no verifier. It may record the
    // organization decision of the source view its caller already holds for
    // this project — and only that: nothing is refreshed or read for it.
    let held = match project_fingerprint {
        Some(_) => crate::core::policy::runtime::held_observation(),
        None => crate::core::policy::runtime::HeldObservation::NotHeld,
    };
    let org_observed = matches!(
        held,
        crate::core::policy::runtime::HeldObservation::Held(Some(_))
    );
    if (bytes == 0 || !gateway_enabled) && !destination_policy_active && !org_observed {
        return EgressOutcome::passthrough(destination, policy_fingerprint);
    }
    let capture = AdmissionCapture::new();
    capture.bind_policy(policy.gateway.mode, fingerprint_digest(&policy_fingerprint));
    match held {
        crate::core::policy::runtime::HeldObservation::Held(observation) => {
            capture.note_org_policy(observation);
        }
        // No view to take it from: the record says the organization decision
        // is not available here, rather than implying there is none.
        crate::core::policy::runtime::HeldObservation::NotHeld => {
            capture.note_org_policy_unavailable();
        }
    }
    let opaque_object = || fingerprint_digest(format!("opaque-egress:{bytes}").as_bytes());
    if destination_policy_active {
        let allowed = policy.gateway.allows_destination(&destination);
        let object = opaque_object();
        capture.record(
            &ContextDecisionV1 {
                object,
                disposition: if allowed {
                    ContextDispositionV1::Allow
                } else {
                    ContextDispositionV1::Deny
                },
                reason_codes: vec![reason(if allowed {
                    "destination.constraint_satisfied"
                } else {
                    "destination.constraint_unsatisfied"
                })],
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            AdmissionCounts::default(),
        );
        if !allowed {
            return EgressOutcome {
                body: EgressBody::Refused(
                    "request withheld by the context gateway (destination.constraint_unsatisfied)"
                        .to_owned(),
                ),
                classification: None,
                capture: Some(capture),
                destination,
                policy_fingerprint,
                receipt_required: true,
                refs: KeyedRefs::default(),
            };
        }
    }
    if !gateway_enabled || bytes == 0 {
        // Recorded only because of the held organization decision: the body
        // left uninspected, and the receipt says no more than that.
        if org_observed && capture.is_empty() {
            capture.record_unsourced(
                opaque_object(),
                policy.gateway.mode,
                fingerprint_digest(&policy_fingerprint),
            );
        }
        return EgressOutcome {
            body: EgressBody::Unchanged,
            classification: None,
            capture: Some(capture),
            destination,
            policy_fingerprint,
            receipt_required: true,
            refs: KeyedRefs::default(),
        };
    }
    let mut admission =
        admit_uninspectable(Some(Path::new("egress.opaque")), bytes as u64, &policy);
    admission
        .decision
        .reason_codes
        .push(reason("egress.opaque_payload"));
    capture.record(&admission.decision, admission.counts);
    let body = if admission.text.is_some() {
        EgressBody::Unchanged
    } else {
        EgressBody::Refused(
            "request withheld by the context gateway (egress.opaque_payload)".to_owned(),
        )
    };
    EgressOutcome {
        body,
        // Opaque bytes were never inspected: nothing is claimed about them.
        classification: None,
        capture: Some(capture),
        destination,
        policy_fingerprint,
        // The one "receipt store is mandatory" predicate MCP and CLI use,
        // under this admission's policy snapshot, plus the proxy's own
        // destination-policy condition. A held organization decision is
        // named in the sealed trace, so its receipt is owed as well.
        receipt_required: super::receipt_store::mandatory_under(&policy, destination_policy_active)
            || org_observed,
        refs: KeyedRefs::default(),
    }
}

/// Build and persist the legacy decision receipt. Its V1 `Delivered` outcome
/// predates dispatch and is not evidence of network acknowledgement. Where
/// the receipt is required it is sealed (`receipt_store::seal`): sidecar and
/// audit anchor, or the request is not dispatched. `caller` names who
/// dispatches: the receipt's index, the tool of its audit entries and the
/// transport and endpoint of its sidecar are the caller's.
fn finish_with_persistence(
    outcome: &EgressOutcome,
    forwarded: Option<&[u8]>,
    original_bytes: usize,
    agent_id: Option<&str>,
    caller: EgressCaller<'_>,
) -> Result<Option<(ContextDecisionReceiptV1, Option<Sha256Digest>)>, &'static str> {
    let Some(capture) = outcome.capture.as_ref() else {
        return Ok(None);
    };
    if capture.is_empty() {
        return if outcome.receipt_required {
            Err(RECEIPT_UNAVAILABLE)
        } else {
            Ok(None)
        };
    }
    capture.set_original_tokens((original_bytes / 4) as u64);
    let text = forwarded.map(String::from_utf8_lossy).unwrap_or_default();
    let delivered = Delivered {
        text: &text,
        // A refused request delivered nothing to the destination.
        is_error: forwarded.is_none(),
        tokens: (forwarded.map_or(0, <[u8]>::len) / 4) as u64,
    };
    let identity = CallIdentity {
        agent_id,
        destination: Some(outcome.destination.clone()),
    };
    // A request that showed a live presenter capability is recorded as that
    // presenter entry (content-free reference), never as an agent.
    let presenter_ref = crate::core::archive::presented::shown_presenter_reference();
    let Some(mut receipt) =
        capture::finish_for_presenter(capture, &identity, presenter_ref.as_deref(), &delivered)
    else {
        return if outcome.receipt_required {
            Err(RECEIPT_UNAVAILABLE)
        } else {
            Ok(None)
        };
    };
    // Opaque payloads may not be UTF-8. Bind the actual wire bytes, never the
    // replacement characters used only for the generic capture projection.
    if receipt.final_context.is_some() {
        receipt.final_context = forwarded.map(fingerprint_digest);
    }
    if receipt.validate().is_err() {
        return if outcome.receipt_required {
            Err(RECEIPT_UNAVAILABLE)
        } else {
            Ok(None)
        };
    }
    let tool = caller.tool();
    let audit_agent = agent_id.unwrap_or(tool);
    // Every turn re-sends the unchanged history: a flagged object already
    // recorded by this process is not recorded again.
    let tally = capture.record_security(tool, audit_agent);
    let persisted_digest = match super::receipt_store::persist(
        &receipt,
        caller.receipt_index(),
        capture.task_scope().as_deref(),
    )
    .and_then(|digest| {
        if outcome.receipt_required {
            // G08: a mandatory egress receipt is sealed like an MCP or CLI
            // receipt — read back verified, a trace sidecar (the caller's
            // transport and endpoint, the wire bytes PREPARED for the
            // upstream channel, the recovery references they carry), and
            // the pair anchored in one signed audit entry. What is then
            // observed of the upstream request — by the proxy, or reported
            // by the host that dispatched it — is the egress lifecycle's
            // transport observation, never this record.
            use super::receipt_store::trace::Delivery;
            let wire = forwarded.map(fingerprint_digest);
            let delivery = match caller {
                EgressCaller::Proxy => {
                    Delivery::proxy(outcome.destination.provider.as_str(), wire, capture.pack())
                }
                EgressCaller::Host { operation, .. } => {
                    Delivery::host_egress(operation, wire, capture.pack())
                }
            }
            // What the admission noted from its own snapshot; no policy,
            // marker or verifier is consulted for the record.
            .under_org_policy(capture.org_policy())
            .with_refs(outcome.refs.clone());
            let sealing = super::receipt_store::Sealing {
                capture,
                delivery: &delivery,
                delivered: &delivered,
                tool,
                audit_agent,
            };
            super::receipt_store::seal(&digest, &receipt, &sealing)?;
        } else if !tally.is_empty() {
            crate::core::security_events::anchor_receipt(tool, audit_agent, digest.hex());
        }
        Ok(digest)
    }) {
        Ok(digest) => Some(digest),
        Err(error) => {
            tracing::warn!("proxy gateway receipt not persisted: {error}");
            if outcome.receipt_required {
                return Err(RECEIPT_UNAVAILABLE);
            }
            None
        }
    };
    Ok(Some((receipt, persisted_digest)))
}

/// Persist a receipt for the exact bytes prepared for dispatch and link it to
/// the legacy decision receipt. The lifecycle record never claims a send.
#[cfg(test)]
pub fn finish(
    outcome: &EgressOutcome,
    forwarded: Option<&[u8]>,
    original_bytes: usize,
    agent_id: Option<&str>,
) -> Result<Option<ContextDecisionReceiptV1>, &'static str> {
    finish_with_persistence(
        outcome,
        forwarded,
        original_bytes,
        agent_id,
        EgressCaller::Proxy,
    )
    .map(|evidence| evidence.map(|(receipt, _)| receipt))
}

/// Finish the admission of a request the PROXY dispatches: its receipts are
/// indexed under [`PROXY_RECEIPT_KEY`]. A host that dispatches itself uses
/// [`finish_admission_for`].
#[cfg(feature = "http-server")]
pub(crate) fn finish_admission(
    outcome: &EgressOutcome,
    forwarded: Option<&[u8]>,
    original_bytes: usize,
    agent_id: Option<&str>,
) -> Result<Option<EgressEvidence>, &'static str> {
    finish_admission_for(
        outcome,
        forwarded,
        original_bytes,
        agent_id,
        EgressCaller::Proxy,
    )
}

/// [`finish_admission`] for the caller that dispatches the request: the
/// decision receipt is indexed under the caller's project and its trace
/// sidecar names the caller's transport and operation. Everything else —
/// the receipt, the admission record, the fail-closed rule — is the same.
pub(crate) fn finish_admission_for(
    outcome: &EgressOutcome,
    forwarded: Option<&[u8]>,
    original_bytes: usize,
    agent_id: Option<&str>,
    caller: EgressCaller<'_>,
) -> Result<Option<EgressEvidence>, &'static str> {
    let receipt_required = outcome.receipt_required;
    let dispatchable = !matches!(outcome.body, EgressBody::Refused(_));
    let effective_forwarded = dispatchable.then_some(forwarded).flatten();
    let Some((decision_receipt, decision_digest)) = finish_with_persistence(
        outcome,
        effective_forwarded,
        original_bytes,
        agent_id,
        caller,
    )?
    else {
        return Ok(None);
    };
    let mut evidence = EgressEvidence {
        decision_receipt: Some(decision_receipt.clone()),
        admission: None,
        admission_digest: None,
    };
    let Some(decision_digest) = decision_digest else {
        // Developer mode can still forward when its optional audit store is
        // unavailable, but it must not claim a durable admission reference.
        return Ok(Some(evidence));
    };

    let (admission_outcome, prepared_bytes_digest) = match (&outcome.body, effective_forwarded) {
        (EgressBody::Refused(_), _) => (EgressAdmissionOutcomeV1::Withheld, None),
        (_, Some(bytes)) => (
            EgressAdmissionOutcomeV1::Prepared,
            Some(fingerprint_digest(bytes)),
        ),
        (_, None) => (EgressAdmissionOutcomeV1::Failed, None),
    };
    let receipt_id =
        ProtocolReference::new(format!("egr-{}", decision_receipt.receipt_id.as_str()))
            .map_err(|_| RECEIPT_UNAVAILABLE)?;
    let admission = EgressAdmissionReceiptV1 {
        schema_version: lean_ctx_protocol::V1_SCHEMA_VERSION,
        receipt_id,
        decision_receipt_digest: Some(decision_digest),
        outcome: admission_outcome,
        prepared_bytes_digest,
    };
    match super::receipt_store::persist_admission(&admission) {
        Ok(digest) => {
            evidence.admission = Some(admission);
            evidence.admission_digest = Some(digest);
        }
        Err(error) => {
            tracing::warn!("proxy gateway admission receipt not persisted: {error}");
            if receipt_required {
                return Err(RECEIPT_UNAVAILABLE);
            }
        }
    }
    Ok(Some(evidence))
}

/// Content-free reason safe for HTTP, WebSocket and SDK callers.
pub const RECEIPT_UNAVAILABLE: &str = "required context gateway receipt unavailable";

#[cfg(feature = "http-server")]
fn receipt_unavailable(required: bool) -> Result<Option<ContextDecisionReceiptV1>, &'static str> {
    if required {
        Err(RECEIPT_UNAVAILABLE)
    } else {
        Ok(None)
    }
}

/// Lifecycle-aware asynchronous variant for transports that will record a
/// separate observation after each actual dispatch attempt.
#[cfg(feature = "http-server")]
pub(crate) async fn finish_admission_off_runtime(
    outcome: EgressOutcome,
    forwarded: Option<Vec<u8>>,
    original_bytes: usize,
    agent_id: Option<String>,
) -> Result<Option<EgressEvidence>, &'static str> {
    let receipt_required = outcome.receipt_required;
    match tokio::task::spawn_blocking(move || {
        finish_admission(
            &outcome,
            forwarded.as_deref(),
            original_bytes,
            agent_id.as_deref(),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!("proxy gateway admission task failed: {error}");
            receipt_unavailable(receipt_required).map(|_| None)
        }
    }
}

#[cfg(feature = "http-server")]
pub(crate) fn append_engine_observation(
    observation: &EgressTransportObservationV1,
) -> Result<(), &'static str> {
    super::receipt_store::append_engine_observation(observation)
}

pub(crate) fn record_host_observation(
    request: &lean_ctx_protocol::EngineEgressHostReportRequestV1,
) -> Result<lean_ctx_protocol::EngineEgressHostReportResponseV1, &'static str> {
    request
        .validate()
        .map_err(|_| "host egress report is invalid")?;
    let (observation_digest, observation) = super::receipt_store::append_host_observation(
        &request.admission_receipt_digest,
        request.attempt,
        request.transport,
        request.result,
        request.http_status,
        request.failure,
    )?;
    Ok(lean_ctx_protocol::EngineEgressHostReportResponseV1 {
        schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
        observation_digest,
        observation,
    })
}

pub(crate) fn load_egress_for_decision(
    decision_digest: &Sha256Digest,
) -> Result<Option<super::receipt_store::EgressLifecycleView>, &'static str> {
    super::receipt_store::load_egress_for_decision(decision_digest)
}

/// The admitted form of a body that is sent somewhere else as well (the
/// counterfactual token probe): masked, never with withheld content. `None`
/// when nothing may be sent.
#[must_use]
pub fn admitted_copy(doc: &Value, target: &EgressTarget<'_>) -> Option<Value> {
    match admit_request(doc, target).body {
        EgressBody::Unchanged => Some(doc.clone()),
        EgressBody::Rewritten(admitted) => Some(admitted),
        EgressBody::Refused(_) => None,
    }
}

/// Test barrier between taking the admission snapshot and deciding with it:
/// lets a test change the live environment at exactly that point.
#[cfg(test)]
pub(super) mod failpoint {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static AFTER_SNAPSHOT: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Run `hook` once, right after the next admission on this thread has
    /// taken its snapshot.
    pub(in crate::core::context_admission) fn arm(hook: impl FnMut() + 'static) {
        AFTER_SNAPSHOT.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn after_snapshot() {
        let hook = AFTER_SNAPSHOT.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }

    thread_local! {
        static AFTER_FIRST_STRING: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Run `hook` once, right after the next walk on this thread has decided
    /// on its first string — between two strings of one admission.
    pub(in crate::core::context_admission) fn arm_after_first_string(hook: impl FnMut() + 'static) {
        AFTER_FIRST_STRING.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn after_first_string() {
        let hook = AFTER_FIRST_STRING.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = hook {
            hook();
        }
    }
}
