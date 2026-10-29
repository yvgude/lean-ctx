// SPDX-License-Identifier: Apache-2.0
//! Admission for carriers that are not a rooted file read (G01, G05).
//!
//! Three shapes share this module so that every carrier applies the same rule
//! and renders the same typed, content-free refusal:
//!
//! - **carrier text** ([`admit_carrier_text`], [`admit_for_store`]): text that
//!   arrived from an agent, a hook payload or a foreign tool and is about to be
//!   derived from, echoed or persisted;
//! - **downstream arguments** ([`admit_downstream_arguments`]): tool arguments
//!   sent to a foreign MCP server are egress to a third party and are never
//!   rewritten — a finding refuses the call before anything connects;
//! - **release** ([`ReleaseGate`]): the last check on a finished tool result.
//!   It is idempotent on admitted text, so output that was admitted at its
//!   source keeps its bytes (#498).
//!
//! Stored and derived copies (binding, lineage, re-authorization on read) live
//! in the sibling module `stored`.

use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDispositionV1, DestinationLocalityV1, ReasonCodeV1,
};
use serde_json::{Map, Value};

use super::egress::{EgressTarget, TrustedDestinationBinding, destination_with_binding};
use super::{
    AdmissionPolicy, OutputCompleteness, SourceAdmission, admit, admit_source_typed,
    admit_uninspectable, capture, clean, fingerprint_digest,
};

/// The wording the originless tee store uses (`proxy/ccr/storage.rs`).
pub(crate) const NO_SOURCE_AUTHORITY: &str =
    "stored output has no source authority; repeat the original authorized operation";

pub(super) const NO_SOURCE_AUTHORITY_CODE: &str = "store.no_source_authority";
pub(super) const RESTRICTED_CODE: &str = "store.restricted_content";
pub(super) const SOURCE_GONE_CODE: &str = "store.source_unavailable";

/// A typed refusal. Carries reason codes only, never the inspected value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withheld {
    codes: Vec<String>,
}

impl Withheld {
    pub(crate) fn new(codes: &[&str]) -> Self {
        Self {
            codes: codes.iter().map(|code| (*code).to_owned()).collect(),
        }
    }

    pub(super) fn from_reasons(reasons: &[ReasonCodeV1]) -> Self {
        let mut codes: Vec<String> = reasons
            .iter()
            .map(|code| code.as_str().to_owned())
            .collect();
        if codes.is_empty() {
            codes.push("policy.content_withheld".to_owned());
        }
        Self { codes }
    }

    /// The copy has no source to authorize it (legacy record, lost lineage).
    pub(crate) fn no_source_authority() -> Self {
        Self::new(&[NO_SOURCE_AUTHORITY_CODE])
    }

    /// The source a copy was derived from is gone, unreadable or no longer
    /// permitted for the current project and role.
    pub(crate) fn source_unavailable() -> Self {
        Self::new(&[SOURCE_GONE_CODE])
    }

    /// The reason codes, for a caller that composes its own typed outcome.
    pub(crate) fn codes(&self) -> &[String] {
        &self.codes
    }

    #[cfg(test)]
    pub(crate) fn has(&self, code: &str) -> bool {
        self.codes.iter().any(|candidate| candidate == code)
    }

    /// `<subject> withheld by the context gateway (<codes>)` — the sentence
    /// form `admit_source` and the egress gate already use. A copy without
    /// current source authority gets the tee store's wording instead.
    pub(crate) fn message(&self, subject: &str) -> String {
        if self
            .codes
            .iter()
            .any(|code| code.starts_with("store.") && code != RESTRICTED_CODE)
        {
            return format!("{NO_SOURCE_AUTHORITY} ({})", self.codes.join(", "));
        }
        format!(
            "{subject} withheld by the context gateway ({})",
            self.codes.join(", ")
        )
    }

    /// `[lean-ctx gateway: content withheld — <codes>]` — the in-band notice
    /// form of `Admission::hud_line`.
    pub(crate) fn notice(&self) -> String {
        format!(
            "[lean-ctx gateway: content withheld — {}]",
            self.codes.join(", ")
        )
    }
}

impl std::fmt::Display for Withheld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message("content"))
    }
}

/// The gateway rules that apply here: inside a call whose capture carries the
/// call's policy snapshot (`capture::CallSnapshot`, G08), the rules of that
/// snapshot — one call decides under the rules of its entry; anywhere else
/// the rules as they are configured at this moment.
pub(super) fn current_policy() -> AdmissionPolicy {
    match capture::current()
        .as_deref()
        .and_then(capture::AdmissionCapture::snapshot)
    {
        Some(snapshot) => snapshot.admission().admission_policy().clone(),
        None => AdmissionPolicy::from_config(&crate::core::config::Config::load_arc()),
    }
}

/// The gateway rules that apply here ([`current_policy`]), as ONE snapshot
/// for everything a multi-part source read admits (the units of a transcript,
/// the values of a history database). Unobserved: taking it touches no
/// resident store. Its `digest()` is the identity a caller compares before
/// it takes over what was read.
pub(crate) fn gateway_snapshot() -> super::stores::StoreAdmission {
    super::stores::StoreAdmission::with_policy(current_policy())
}

/// Admit text that reached lean-ctx through a carrier other than a file read
/// (agent message, hook payload, foreign tool text): project policy on the
/// original bytes, then the gateway detectors, composed like `admit_source`.
/// The decision lands in the current call's receipt when one is open.
pub(crate) fn admit_carrier_text(text: &str) -> Result<SourceAdmission, Withheld> {
    admit_carrier_text_under(text, None)
}

/// [`admit_carrier_text`] under one caller-captured policy snapshot, so the
/// policy a copy is bound to is the one it was admitted under.
pub(crate) fn admit_carrier_text_under(
    text: &str,
    policy: Option<&AdmissionPolicy>,
) -> Result<SourceAdmission, Withheld> {
    admit_source_typed(text, None, false, OutputCompleteness::complete(), policy)
        .map_err(|reasons| Withheld::from_reasons(&reasons))
}

/// [`admit_carrier_text`] for text entering a derived store: restricted
/// content is delivered to its reader but never persisted (E3).
pub(crate) fn admit_for_store(text: &str) -> Result<String, Withheld> {
    admit_for_store_under(text, None)
}

/// [`admit_for_store`] under one caller-captured policy snapshot (`None`: the
/// rules configured at this moment).
pub(crate) fn admit_for_store_under(
    text: &str,
    policy: Option<&AdmissionPolicy>,
) -> Result<String, Withheld> {
    let admission = admit_carrier_text_under(text, policy)?;
    if admission.storable {
        Ok(admission.text)
    } else {
        Err(Withheld::new(&[RESTRICTED_CODE]))
    }
}

fn record(policy: &AdmissionPolicy, admission: &super::Admission) {
    let decision = &admission.decision;
    if decision.disposition == ContextDispositionV1::Deny {
        // K12: a withheld object counts under the rules its reasons name.
        crate::core::hive::metrics_v2::signals::admission_refused(
            decision.reason_codes.iter().map(ReasonCodeV1::as_str),
        );
    }
    if let Some(capture) = capture::current() {
        capture.bind_policy(
            policy.gateway.mode,
            fingerprint_digest(&policy.fingerprint()),
        );
        capture.record(&admission.decision, admission.counts);
    }
}

/// A foreign MCP server reached through the tool-catalog gateway.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Downstream<'a> {
    /// Configured server name; the destination provider is `mcp:<server>`.
    pub server: &'a str,
    /// The HTTP endpoint, or `None` for a child process on this machine.
    pub endpoint: Option<&'a str>,
    /// The downstream tool name; caller-chosen text that is sent as well.
    pub tool: &'a str,
}

const MAX_ARGUMENT_NODES: usize = 10_000;
const MAX_ARGUMENT_DEPTH: usize = 64;

/// Admit tool arguments before they are sent to `downstream`, in place.
///
/// Order: unloadable configuration, destination rule for `mcp:<server>`,
/// project policy on every key and string (original bytes), gateway detectors.
/// The configured detector actions apply exactly as for any other egress:
///
/// - a string VALUE the gateway or the project policy **redacts** is replaced
///   by its admitted form and the call proceeds — the downstream server never
///   receives the original value;
/// - a value that is **withheld** (a `block` action, mandatory detectors that
///   could not inspect it, rules that do not compose) refuses the whole call,
///   as does **restricted** content (the server is a third party whatever its
///   transport);
/// - an object KEY or the tool name cannot be rewritten without changing the
///   call: any finding there refuses (`egress.immutable_position`).
///
/// A refusal is typed and content-free and nothing is sent. A structure too
/// deep or too large to walk is refused in governed and sovereign mode and
/// under an active project policy; Community developer mode sends the part it
/// could not inspect, like other partial coverage.
pub(crate) fn admit_downstream_arguments(
    downstream: &Downstream<'_>,
    arguments: &mut Map<String, Value>,
) -> Result<(), Withheld> {
    let policy = current_policy();
    if let Some(codes) = policy.config_refusal_codes() {
        return Err(Withheld::new(&codes));
    }
    let provider = format!("mcp:{}", downstream.server);
    let destination = destination_with_binding(
        &EgressTarget {
            provider: &provider,
            model: None,
            upstream_base: downstream.endpoint.unwrap_or_default(),
        },
        TrustedDestinationBinding {
            provider_id: None,
            local: downstream.endpoint.is_none().then_some(true),
            account_ref: None,
            region: None,
        },
    );
    if policy.gateway.has_destination_policy() && !policy.gateway.allows_destination(&destination) {
        return Err(Withheld::new(&["destination.constraint_unsatisfied"]));
    }
    let mut walk = ArgumentWalk {
        policy: &policy,
        project_active: crate::core::policy::runtime::is_active(),
        gateway_active: policy.enabled_effective(),
        remote: destination.locality != DestinationLocalityV1::Local,
        nodes: 0,
    };
    walk.immutable(downstream.tool)?;
    for (key, value) in arguments.iter_mut() {
        walk.immutable(key)?;
        walk.value(value, 0)?;
    }
    Ok(())
}

struct ArgumentWalk<'a> {
    policy: &'a AdmissionPolicy,
    project_active: bool,
    gateway_active: bool,
    remote: bool,
    nodes: usize,
}

impl ArgumentWalk<'_> {
    fn bounded(&mut self, depth: usize) -> Result<bool, Withheld> {
        self.nodes += 1;
        if self.nodes <= MAX_ARGUMENT_NODES && depth <= MAX_ARGUMENT_DEPTH {
            return Ok(true);
        }
        if self.policy.detectors_mandatory() || self.project_active {
            return Err(Withheld::new(&["coverage.partial"]));
        }
        Ok(false)
    }

    fn value(&mut self, value: &mut Value, depth: usize) -> Result<(), Withheld> {
        if !self.bounded(depth)? {
            return Ok(());
        }
        match value {
            Value::String(text) => {
                if let Some(admitted) = self.admitted(text)? {
                    *text = admitted;
                }
                Ok(())
            }
            Value::Array(items) => items
                .iter_mut()
                .try_for_each(|item| self.value(item, depth + 1)),
            Value::Object(map) => map.iter_mut().try_for_each(|(key, child)| {
                self.immutable(key)?;
                self.value(child, depth + 1)
            }),
            Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        }
    }

    /// A string that cannot be rewritten (object key, tool name): admitted
    /// only when admission leaves it exactly as it is.
    fn immutable(&mut self, text: &str) -> Result<(), Withheld> {
        match self.admitted(text)? {
            None => Ok(()),
            Some(_) => Err(Withheld::new(&["egress.immutable_position"])),
        }
    }

    /// Admit one string. `Ok(None)`: send it as it is. `Ok(Some(text))`: send
    /// this admitted (redacted) form instead. `Err`: withheld or restricted.
    /// Project policy and gateway both decide on the original and their
    /// transformations are composed as for every other source.
    fn admitted(&mut self, text: &str) -> Result<Option<String>, Withheld> {
        if text.is_empty() || (!self.project_active && !self.gateway_active) {
            return Ok(None);
        }
        let project_original = if self.project_active {
            crate::core::policy::content::protect_active(text)
                .map_err(|_| Withheld::new(&["policy.content_withheld"]))?
                .into_owned()
        } else {
            text.to_owned()
        };
        if !self.gateway_active {
            return Ok((project_original != text).then_some(project_original));
        }
        let admission = admit(text, None, self.policy);
        record(self.policy, &admission);
        let Some(gateway_output) = admission.text.as_deref() else {
            return Err(Withheld::from_reasons(&admission.decision.reason_codes));
        };
        if admission.classification >= ClassificationV1::Restricted {
            return Err(Withheld::new(&[if self.remote {
                "destination.remote_restricted"
            } else {
                "egress.restricted_content"
            }]));
        }
        let delivered = if self.project_active {
            super::compose_active_gateway_output(
                &project_original,
                gateway_output,
                self.policy,
                OutputCompleteness::complete(),
            )
            .ok_or_else(|| Withheld::new(&["policy.content_withheld"]))?
        } else {
            gateway_output.to_owned()
        };
        Ok((delivered != text).then_some(delivered))
    }
}

/// Outcome of the final release check for one string of a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Release {
    /// Admitted text: deliver the bytes as they are.
    Unchanged,
    /// The text still carried a value the gateway redacts. `notice` is the
    /// content-free HUD line naming what was done.
    Redacted {
        text: String,
        notice: String,
    },
    Withheld(Withheld),
}

/// One policy snapshot for releasing every block of one tool result.
pub(crate) struct ReleaseGate {
    policy: AdmissionPolicy,
    fingerprint: Vec<u8>,
}

impl ReleaseGate {
    /// The gate under the rules that apply here ([`current_policy`]): inside
    /// a call, the call's policy snapshot.
    #[must_use]
    pub(crate) fn current() -> Self {
        Self::with(current_policy())
    }

    /// The gate under the rules of `snapshot`, for a check that runs where
    /// the call's capture is no longer open (its receipt and trace sidecar).
    #[must_use]
    pub(crate) fn under(snapshot: &super::stores::StoreAdmission) -> Self {
        Self::with(snapshot.admission_policy().clone())
    }

    fn with(mut policy: AdmissionPolicy) -> Self {
        // The release point re-checks text that was admitted where it was
        // read; that only holds for detectors that leave their own output
        // alone. A model is not idempotent on text it redacted (it finds the
        // rest of a masked address, a name beside a marker), so the advisory
        // semantic stage runs at admission only and the release point keeps
        // the mandatory built-in floor. Before, every semantic redaction of a
        // `ctx_read` was withheld here as `release.unadmitted_content`, and
        // each read paid the model twice.
        policy.semantic = None;
        let fingerprint = policy.fingerprint();
        Self {
            policy,
            fingerprint,
        }
    }

    /// Nothing to check: the gateway is off (developer mode only).
    pub(crate) fn inactive(&self) -> bool {
        !self.policy.enabled_effective()
    }

    /// Release one string. Text that was admitted where it was read comes back
    /// [`Release::Unchanged`]: masked values are markers no detector matches,
    /// and flagged-but-delivered findings (`warn`) do not change the text.
    /// Only a string that bypassed admission can be redacted or withheld here.
    pub(crate) fn text(&self, text: &str) -> Release {
        if text.is_empty() || self.inactive() {
            return Release::Unchanged;
        }
        // Same memo as the store path: clean text under this exact policy.
        let key = self
            .policy
            .semantic
            .is_none()
            .then(|| clean::key(&self.fingerprint, "release", text));
        if let Some(key) = &key
            && clean::get(key).is_some()
        {
            return Release::Unchanged;
        }
        let admission = admit(text, None, &self.policy);
        match admission.text.as_deref() {
            Some(delivered) if delivered == text => {
                if let Some(key) = key
                    && admission.decision.reason_codes.is_empty()
                    && admission.disposition() == ContextDispositionV1::Allow
                {
                    clean::insert(key, admission.decision.clone());
                }
                Release::Unchanged
            }
            Some(delivered) => {
                record(&self.policy, &admission);
                Release::Redacted {
                    text: delivered.to_owned(),
                    notice: admission
                        .hud_line()
                        .unwrap_or_else(|| "[lean-ctx gateway: content redacted]".to_owned()),
                }
            }
            None => {
                record(&self.policy, &admission);
                Release::Withheld(Withheld::from_reasons(&admission.decision.reason_codes))
            }
        }
    }

    /// Release a block no detector can read (image, audio, binary resource).
    pub(crate) fn media(&self, bytes: u64) -> Result<(), Withheld> {
        if self.inactive() {
            return Ok(());
        }
        let admission = admit_uninspectable(None, bytes, &self.policy);
        if admission.text.is_some() {
            return Ok(());
        }
        record(&self.policy, &admission);
        Err(Withheld::from_reasons(&admission.decision.reason_codes))
    }
}

#[cfg(test)]
mod tests;
