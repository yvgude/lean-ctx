// SPDX-License-Identifier: Apache-2.0
//! Trace sidecar of a Decision Receipt (G08).
//!
//! A V1 receipt is a wire contract and stays byte for byte what it is. What
//! it cannot say about a delivery is recorded here, in a separate, internal,
//! content-free record with its own schema id and version:
//! `<data>/gateway/traces/<receipt sha256>.json`. A binary that does not know
//! the directory never reads or writes it.
//!
//! Binding: the sidecar names the canonical digest of the receipt it belongs
//! to and is filed under it; the SHA-256 of the sidecar's stored bytes is
//! anchored together with the receipt digest in one entry of the signed audit
//! trail. A sidecar that names another receipt, whose bytes no longer match
//! the anchored digest, or that is missing although an anchor names one, is a
//! finding the inspector shows.
//!
//! Content rule: every field is a digest, a number, a closed vocabulary, a
//! validated reason code, a recovery id, or a word — the tool id or provider
//! label, the name and version of the active policy pack, and the source
//! references. Every word is checked with the gateway's own release gate
//! (`admissible_word`): a sidecar whose endpoint or pack words would not
//! pass is not built, and the delivery is then treated like one whose
//! receipt failed; a source reference that would not pass is left out and
//! counted.
//!
//! PREPARED, not handed over: the sidecar is stored BEFORE a byte leaves.
//! `before_release` is the digest of the bytes before the release point
//! (when the caller has them); `release_prepared` names the channel the
//! bytes are intended for and the digest of exactly the bytes prepared for
//! it. Neither proves a hand-over. What the Engine observes afterwards is a
//! separate record ([`super::observed`]); the proxy's is the egress
//! lifecycle. That a host received or a terminal displayed the bytes is not
//! observable and is recorded nowhere.
//!
//! Source and recovery references: an itemized object carries the reference
//! the reader showed for it — the path relative to the project — when the
//! admitted reader named one, and the word `withheld`, never a path, for a
//! source that was not delivered (the reader shows a refusal in its place).
//! `recovery` lists the recovery ids issued by this process that the
//! prepared bytes carry ([`super::references`]): ids, never content.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDispositionV1, GatewayModeV1,
};
use serde::{Deserialize, Serialize};

use crate::core::context_admission::capture::{
    self, AdmissionCapture, CallSnapshot, Delivered, OrgEvidence, OrgObservation,
};
use crate::core::context_admission::carrier::{Release, ReleaseGate};

pub(crate) const SCHEMA: &str = "leanctx.gateway.trace-sidecar";
pub(crate) const VERSION: u32 = 1;
/// Objects a sidecar itemizes; the rest is counted in `objects_omitted`.
const MAX_OBJECTS: usize = 64;
const MAX_SIDECAR_BYTES: usize = 256 * 1024;
const MAX_WORD_BYTES: usize = 128;

/// The transport the delivery actually used. V1 says `mcp-host` for MCP and
/// CLI alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Transport {
    Mcp,
    Cli,
    Proxy,
    /// The Engine's tool session (`lean-ctx engine tool-session`): framed
    /// tool calls of an SDK host over the session's streams.
    ToolSession,
    /// The Engine interface (`lean-ctx engine <operation>`): one wire
    /// document per invocation on the process's stdout.
    EngineInterface,
    /// Egress a HOST dispatches itself (`lean-ctx engine egress-admit` /
    /// `egress-prepare`, a lean-ctx client command such as `lean-ctx team`):
    /// the Engine admitted and prepared the request bytes; the request to
    /// the destination is the host's, not the proxy's.
    HostEgress,
}

/// The channel the released bytes are intended for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Channel {
    /// The MCP result, returned by the handler to the transport layer.
    McpTransport,
    Stdout,
    Stderr,
    /// The proxy's request to the upstream provider.
    Upstream,
    /// A file the CLI command writes the output to instead of a stream. The
    /// channel says only that; the path is not recorded.
    File,
    /// The system clipboard the CLI command hands the output to.
    Clipboard,
    /// A response frame of the tool session, written to the session's
    /// transport.
    SessionFrame,
}

/// What the caller knows about a delivery beyond its receipt.
pub(crate) struct Delivery {
    transport: Transport,
    endpoint: String,
    channel: Channel,
    before_release: Option<Sha256Digest>,
    /// The digest of the prepared bytes where it is not that of the
    /// delivered text (the proxy's wire bytes need not be UTF-8).
    released: Option<Sha256Digest>,
    pack: Option<(String, String)>,
    /// The organization decision the delivery's admissions were made under,
    /// from their own snapshot.
    org_policy: OrgEvidence,
    /// Hex digest of the call's policy snapshot.
    policy_snapshot: Option<String>,
    /// The keyed references of the caller the proxy told apart and of the
    /// request head ([`super::keyed`]).
    refs: super::keyed::KeyedRefs,
    gate: ReleaseGate,
}

/// The organization decision in force for a delivery, as the verifier stated
/// it in the verification the delivery's policy snapshot comes from.
/// Content-free: a closed state, a digest, a number and an instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgPolicyTrace {
    /// `required`: a signed policy was enforced. `unpinned`: the requirement
    /// was lifted by a signed unpin — an organization decision, recorded
    /// although it leaves no floor.
    pub(crate) state: OrgPolicyState,
    /// Digest of the accepted statement (the policy, or the unpin).
    pub(crate) statement: Sha256Digest,
    /// The signed sequence of that statement: the policy version enforced.
    /// Not `pack_version`, which is the version word of the merged pack.
    pub(crate) policy_version: u64,
    /// The signed end of an enforced policy, as parsed and checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) valid_until: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrgPolicyState {
    Required,
    Unpinned,
}

impl OrgPolicyTrace {
    /// Whether the record is one an observation can have produced: a signed
    /// sequence in the verifier's own range (positive, a safe integer), and
    /// a horizon exactly where one exists — an enforced policy has one,
    /// written as canonical UTC with nanoseconds; an unpin has none.
    ///
    /// The horizon is checked for its form only. A stored record is history:
    /// a horizon that has passed since is what it should be, never a reason
    /// to reject the record.
    fn well_formed(&self) -> bool {
        if self.policy_version == 0 || self.policy_version > lean_ctx_protocol::MAX_SAFE_INTEGER {
            return false;
        }
        match (self.state, self.valid_until.as_deref()) {
            (OrgPolicyState::Unpinned, None) => true,
            (OrgPolicyState::Required, Some(until)) => chrono::DateTime::parse_from_rfc3339(until)
                .is_ok_and(|parsed| {
                    parsed
                        .with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
                        == until
                }),
            _ => false,
        }
    }

    /// `None` when the observation cannot be written as a well-formed record
    /// (its statement is no SHA-256 digest, its sequence is out of range):
    /// then none is made of it rather than one that was not checked.
    fn of(observation: &OrgObservation) -> Option<Self> {
        use crate::core::policy::org::ObservedState;
        Some(Self {
            state: match observation.state() {
                ObservedState::Required => OrgPolicyState::Required,
                ObservedState::Unpinned => OrgPolicyState::Unpinned,
            },
            statement: Sha256Digest::new(observation.statement()).ok()?,
            policy_version: observation.policy_version(),
            valid_until: observation
                .valid_until()
                .map(|until| until.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        })
        .filter(Self::well_formed)
    }
}

impl TraceSidecarV1 {
    /// The organization part of a sidecar says one thing: no decision, one
    /// well-formed decision, or "diverged" — never a decision and the mark
    /// that none could be named. Checked before a sidecar is stored and when
    /// one is read; a sidecar without these fields (every earlier one, every
    /// one without an organization) passes unchanged.
    fn org_policy_consistent(&self) -> bool {
        match &self.org_policy {
            None => !(self.org_policy_diverged && self.org_policy_unavailable),
            Some(observed) => {
                !self.org_policy_diverged && !self.org_policy_unavailable && observed.well_formed()
            }
        }
    }
}

impl Delivery {
    /// The result of the MCP tool `tool`, to be returned to the MCP
    /// transport, under the call's policy snapshot.
    pub(crate) fn mcp(tool: &str, snapshot: &CallSnapshot) -> Self {
        Self::of_call(Transport::Mcp, tool, Channel::McpTransport, snapshot)
    }

    /// The response frame of the tool-session call of `tool`, to be written
    /// to the session transport, under the call's policy snapshot.
    pub(crate) fn tool_session(tool: &str, snapshot: &CallSnapshot) -> Self {
        Self::of_call(
            Transport::ToolSession,
            tool,
            Channel::SessionFrame,
            snapshot,
        )
    }

    /// The wire document of the Engine-interface operation `operation`, to
    /// be written to stdout, under the invocation's policy snapshot.
    pub(crate) fn engine_interface(operation: &str, snapshot: &CallSnapshot) -> Self {
        Self::of_call(
            Transport::EngineInterface,
            operation,
            Channel::Stdout,
            snapshot,
        )
    }

    /// Output of the CLI run of `tool`, to be written to `channel`, under
    /// the run's policy snapshot.
    pub(crate) fn cli(tool: &str, channel: Channel, snapshot: &CallSnapshot) -> Self {
        Self::of_call(Transport::Cli, tool, channel, snapshot)
    }

    /// A proxy egress request to `provider` (the destination's provider
    /// label), with the digest of the wire bytes prepared for dispatch. The
    /// egress admission holds its own policy snapshot and the receipt names
    /// its fingerprint (gateway and project policy); `pack` is the policy
    /// pack that admission resolved, as its capture noted it. The word check
    /// runs under the rules configured now.
    pub(crate) fn proxy(
        provider: &str,
        wire_bytes: Option<Sha256Digest>,
        pack: Option<(String, String)>,
    ) -> Self {
        Self {
            transport: Transport::Proxy,
            endpoint: provider.to_owned(),
            channel: Channel::Upstream,
            before_release: None,
            released: wire_bytes,
            pack,
            org_policy: OrgEvidence::None,
            policy_snapshot: None,
            refs: super::keyed::KeyedRefs::default(),
            gate: ReleaseGate::current(),
        }
    }

    /// The keyed references the egress admission was recorded with: the
    /// caller it was decided for (AP10-C6) and the request head the bytes
    /// are sent with (AP10-C2).
    pub(crate) fn with_refs(mut self, refs: super::keyed::KeyedRefs) -> Self {
        self.refs = refs;
        self
    }

    /// The organization decision the egress admission observed in its own
    /// snapshot, as its capture holds it. Nothing is read here.
    pub(crate) fn under_org_policy(mut self, evidence: OrgEvidence) -> Self {
        self.org_policy = evidence;
        self
    }

    /// An egress request the host `operation` (`egress-prepare`, `team`)
    /// dispatches itself: as [`Self::proxy`], but the transport is
    /// `host_egress` and the endpoint names the operation that asked — the
    /// destination's provider is in the receipt.
    pub(crate) fn host_egress(
        operation: &str,
        wire_bytes: Option<Sha256Digest>,
        pack: Option<(String, String)>,
    ) -> Self {
        Self {
            transport: Transport::HostEgress,
            ..Self::proxy(operation, wire_bytes, pack)
        }
    }

    fn of_call(
        transport: Transport,
        endpoint: &str,
        channel: Channel,
        snapshot: &CallSnapshot,
    ) -> Self {
        Self {
            transport,
            endpoint: endpoint.to_owned(),
            channel,
            before_release: None,
            released: None,
            pack: snapshot.pack().cloned(),
            org_policy: snapshot.org_policy(),
            policy_snapshot: Some(snapshot.admission().digest().to_owned()),
            refs: super::keyed::KeyedRefs::default(),
            gate: ReleaseGate::under(snapshot.admission()),
        }
    }

    /// The digest of the prepared bytes where they are more than the
    /// delivered text: the whole MCP result (AP10-C3).
    pub(crate) fn prepared_as(mut self, digest: Sha256Digest) -> Self {
        self.released = Some(digest);
        self
    }

    /// The digest of the bytes as they were before the release point.
    pub(crate) fn before_release(mut self, digest: Option<Sha256Digest>) -> Self {
        self.before_release = digest;
        self
    }

    /// The channel the bytes are intended for.
    pub(crate) fn channel(&self) -> Channel {
        self.channel
    }

    /// The digest of exactly the bytes prepared for the channel.
    pub(crate) fn prepared_bytes(&self, delivered: &Delivered<'_>) -> Sha256Digest {
        self.released
            .clone()
            .unwrap_or_else(|| super::output_digest(delivered.text.as_bytes()))
    }

    /// Endpoint and pack words pass the content rule.
    fn admissible(&self) -> bool {
        admissible_word(&self.gate, &self.endpoint, MAX_WORD_BYTES)
            && self.pack.as_ref().is_none_or(|(name, version)| {
                admissible_word(&self.gate, name, MAX_WORD_BYTES)
                    && admissible_word(&self.gate, version, MAX_WORD_BYTES)
            })
    }
}

/// A word may enter a sidecar when it is short, printable and passes the
/// release gate unchanged — the admission a released text gets.
fn admissible_word(gate: &ReleaseGate, word: &str, max_bytes: usize) -> bool {
    !word.is_empty()
        && word.len() <= max_bytes
        && !word.chars().any(char::is_control)
        && matches!(gate.text(word), Release::Unchanged)
}

/// The reference of one itemized object, never more than its reader showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SourceRef {
    /// Delivered by an admitted reader under this path, relative to the
    /// project.
    Path(String),
    /// Delivered by a reader of sources that are not project files, under
    /// this id (the path at its origin, a provider's source id).
    Id(String),
    /// Not delivered: the reader showed a refusal in its place and no name.
    Withheld,
}

/// Transformation and refusal counts of the whole delivery, by class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Totals {
    pub(crate) secrets_masked: u32,
    pub(crate) pii_masked: u32,
    pub(crate) injection_signals: u32,
    pub(crate) withheld: u32,
    pub(crate) not_fully_inspected: u32,
    /// Notes: statements about a source that stayed whole, with their code.
    pub(crate) notes: u32,
    /// Itemized decisions per typed reason code.
    pub(crate) reasons: BTreeMap<String, u32>,
    /// Itemized decisions per required transformation kind.
    pub(crate) transformations: BTreeMap<String, u32>,
}

/// One itemized object with the counts V1 holds only as receipt totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ObjectTrace {
    pub(crate) object: Sha256Digest,
    pub(crate) disposition: ContextDispositionV1,
    pub(crate) secrets_masked: u32,
    pub(crate) pii_masked: u32,
    pub(crate) injection_signals: u32,
    pub(crate) not_fully_inspected: bool,
    /// What the reader showed for this object; absent when no reader named
    /// it (not a project file, or the reference was not recorded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<SourceRef>,
}

/// What was prepared for a channel BEFORE any byte left: the channel the
/// bytes are intended for and the digest of exactly those bytes. Not a
/// hand-over; see [`super::observed`] for what was observed afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReleasePrepared {
    pub(crate) channel: Channel,
    pub(crate) bytes: Sha256Digest,
    pub(crate) is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TraceSidecarV1 {
    pub(crate) schema: String,
    pub(crate) version: u32,
    /// Canonical digest of the V1 receipt this record belongs to.
    pub(crate) receipt: Sha256Digest,
    pub(crate) transport: Transport,
    /// The MCP tool, the tool id of the CLI command, the Engine operation,
    /// the proxy's provider label, or the host operation that dispatches an
    /// egress request itself.
    pub(crate) endpoint: String,
    pub(crate) gateway_mode: GatewayModeV1,
    /// Hex digest of the policy snapshot taken at the call's entry (MCP,
    /// CLI). The receipt's own policy digest is that of the admissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) policy_snapshot: Option<String>,
    /// An admission of the call was decided under other gateway rules than
    /// that snapshot.
    #[serde(default)]
    pub(crate) policy_diverged: bool,
    /// Keyed reference of the caller the proxy told apart by its presented
    /// credential or gateway person ([`super::keyed`]); absent where
    /// the route tells no callers apart. The receipt's principal is the
    /// agent id the caller claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) principal_ref: Option<String>,
    /// Keyed reference of the method, final URL and header set the request
    /// is bound to be sent with ([`super::keyed`]); proxy egress only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) request_head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pack_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pack_version: Option<String>,
    /// The organization decision the delivery was made under. Absent when no
    /// verifier decision was observed — and when `org_policy_diverged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) org_policy: Option<OrgPolicyTrace>,
    /// The delivery's admissions rest on different organization decisions,
    /// or the observed one could not be recorded as a digest: none is named.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) org_policy_diverged: bool,
    /// The delivery was admitted where no verified view was held to take an
    /// observation from, and no verifier was asked for it: no organization
    /// decision is available for this record. Not a statement that none was
    /// in force.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) org_policy_unavailable: bool,
    /// The delivery carried bytes no admission decided on.
    pub(crate) unsourced: bool,
    pub(crate) totals: Totals,
    pub(crate) objects: Vec<ObjectTrace>,
    pub(crate) objects_omitted: u32,
    /// Source references that are not shown: not recorded by the capture
    /// (bound, not expressible) or not admissible as a sidecar word.
    #[serde(default)]
    pub(crate) source_references_omitted: u32,
    /// Recovery ids issued by this process that the prepared bytes carry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) recovery: Vec<String>,
    #[serde(default)]
    pub(crate) recovery_omitted: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) before_release: Option<Sha256Digest>,
    pub(crate) release_prepared: ReleasePrepared,
}

fn serde_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The sidecar of the receipt stored under `receipt_digest`, from the same
/// capture the receipt was built from and the bytes prepared for the
/// channel.
pub(crate) fn build(
    receipt_digest: &Sha256Digest,
    receipt: &ContextDecisionReceiptV1,
    capture: &AdmissionCapture,
    delivery: &Delivery,
    delivered: &Delivered<'_>,
) -> Result<TraceSidecarV1, String> {
    if !delivery.admissible() {
        return Err(
            "the trace sidecar would carry a value the gateway does not release".to_owned(),
        );
    }
    // Objects are named as the receipt names them: keyed in a version 2
    // receipt (AP10-C4), so the sidecar confirms no guess the receipt refuses.
    let object_key = match receipt.object_digest {
        Some(_) => Some(super::keyed::object_key().map_err(str::to_owned)?),
        None => None,
    };
    let mut totals = Totals::default();
    let mut objects = Vec::new();
    let mut unsourced = false;
    let itemized = capture.itemized();
    let (mut named, mut source_references_omitted) = capture.source_references();
    for (decision, counts) in &itemized {
        let has = |code: &str| {
            decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == code)
        };
        for reason in &decision.reason_codes {
            *totals
                .reasons
                .entry(reason.as_str().to_owned())
                .or_default() += 1;
        }
        for kind in &decision.required_transformations {
            *totals.transformations.entry(serde_name(kind)).or_default() += 1;
        }
        if capture::is_note(decision) {
            totals.notes += 1;
            continue;
        }
        unsourced |= has(capture::NO_SOURCE_DECISION);
        let secrets_masked = if has("secret.redacted") {
            counts.secrets
        } else {
            0
        };
        let pii_masked = if has("pii.redacted") { counts.pii } else { 0 };
        totals.secrets_masked = totals.secrets_masked.saturating_add(secrets_masked);
        totals.pii_masked = totals.pii_masked.saturating_add(pii_masked);
        totals.injection_signals = totals.injection_signals.saturating_add(counts.injection);
        if decision.disposition == ContextDispositionV1::Deny {
            totals.withheld += 1;
        }
        if counts.incomplete > 0 {
            totals.not_fully_inspected += 1;
        }
        // A source that was not delivered is named as withheld and nothing
        // else, whatever a reader noted. A delivered one carries the
        // reference its reader noted, once, if the word is admissible.
        let source = if decision.disposition.delivers_content() {
            named
                .iter()
                .position(|(object, _)| *object == decision.object)
                .map(|position| named.swap_remove(position).1)
                .and_then(|name| {
                    let (word, reference) = match name {
                        capture::SourceName::Path(path) => (path.clone(), SourceRef::Path(path)),
                        capture::SourceName::Id(id) => (id.clone(), SourceRef::Id(id)),
                    };
                    let admissible =
                        admissible_word(&delivery.gate, &word, capture::MAX_SOURCE_REFERENCE_BYTES);
                    if !admissible {
                        source_references_omitted = source_references_omitted.saturating_add(1);
                    }
                    admissible.then_some(reference)
                })
        } else {
            Some(SourceRef::Withheld)
        };
        if objects.len() < MAX_OBJECTS {
            objects.push(ObjectTrace {
                object: object_key.as_ref().map_or_else(
                    || decision.object.clone(),
                    |key| key.object(&decision.object),
                ),
                disposition: decision.disposition,
                secrets_masked,
                pii_masked,
                injection_signals: counts.injection,
                not_fully_inspected: counts.incomplete > 0,
                source,
            });
        } else if matches!(source, Some(SourceRef::Path(_) | SourceRef::Id(_))) {
            source_references_omitted = source_references_omitted.saturating_add(1);
        }
    }
    // References whose decision is not itemized (displaced at the receipt's
    // bound) are counted, never shown apart from their object.
    source_references_omitted =
        source_references_omitted.saturating_add(u32::try_from(named.len()).unwrap_or(u32::MAX));
    let prepared_bytes = delivery.prepared_bytes(delivered);
    let (recovery, recovery_omitted) = super::references::recovery_in(delivered.text.as_bytes());
    let sources = itemized.len() - totals.notes as usize;
    let objects_omitted = u32::try_from(sources - objects.len())
        .unwrap_or(u32::MAX)
        .saturating_add(capture.counted_only());
    // One observation is named; anything else is not turned into one.
    let (org_policy, org_policy_diverged, org_policy_unavailable) = match &delivery.org_policy {
        OrgEvidence::None => (None, false, false),
        OrgEvidence::One(observation) => match OrgPolicyTrace::of(observation) {
            Some(trace) => (Some(trace), false, false),
            None => (None, true, false),
        },
        OrgEvidence::Diverged => (None, true, false),
        OrgEvidence::Unavailable => (None, false, true),
    };
    Ok(TraceSidecarV1 {
        schema: SCHEMA.to_owned(),
        version: VERSION,
        receipt: receipt_digest.clone(),
        transport: delivery.transport,
        endpoint: delivery.endpoint.clone(),
        gateway_mode: receipt.mode,
        policy_snapshot: delivery.policy_snapshot.clone(),
        policy_diverged: capture.policy_diverged(),
        principal_ref: delivery.refs.principal.clone(),
        request_head_ref: delivery.refs.request_head.clone(),
        pack_name: delivery.pack.as_ref().map(|(name, _)| name.clone()),
        pack_version: delivery.pack.as_ref().map(|(_, version)| version.clone()),
        org_policy,
        org_policy_diverged,
        org_policy_unavailable,
        unsourced,
        totals,
        objects,
        objects_omitted,
        source_references_omitted,
        recovery,
        recovery_omitted,
        before_release: delivery.before_release.clone(),
        release_prepared: ReleasePrepared {
            channel: delivery.channel,
            bytes: prepared_bytes,
            is_error: delivered.is_error,
        },
    })
}

fn sidecar_path(dir: &Path, receipt: &Sha256Digest) -> PathBuf {
    dir.join("traces").join(format!("{}.json", receipt.hex()))
}

/// A sidecar as it is found in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoredTrace {
    /// No record is filed under the receipt.
    Absent,
    /// A record is there but cannot be read as a sidecar of this schema.
    Unreadable,
    /// A readable record that names another receipt.
    Unbound,
    /// Names the receipt; `digest` is the SHA-256 (hex) of its stored bytes,
    /// the value the audit anchor carries.
    Bound {
        digest: String,
        sidecar: Box<TraceSidecarV1>,
    },
}

/// The sidecar filed under `receipt`.
pub(crate) fn load(receipt: &Sha256Digest) -> StoredTrace {
    match super::gateway_dir() {
        Some(dir) => load_in(&dir, receipt),
        None => StoredTrace::Absent,
    }
}

/// Stored bytes as a sidecar of this schema and version whose organization
/// part is consistent; anything else is not one.
pub(crate) fn decode(bytes: &[u8]) -> Option<TraceSidecarV1> {
    let sidecar = serde_json::from_slice::<TraceSidecarV1>(bytes).ok()?;
    (sidecar.schema == SCHEMA && sidecar.version == VERSION && sidecar.org_policy_consistent())
        .then_some(sidecar)
}

pub(crate) fn load_in(dir: &Path, receipt: &Sha256Digest) -> StoredTrace {
    let file = match std::fs::File::open(sidecar_path(dir, receipt)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return StoredTrace::Absent,
        Err(_) => return StoredTrace::Unreadable,
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_SIDECAR_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_SIDECAR_BYTES
    {
        return StoredTrace::Unreadable;
    }
    let Some(sidecar) = decode(&bytes) else {
        return StoredTrace::Unreadable;
    };
    if sidecar.receipt != *receipt {
        return StoredTrace::Unbound;
    }
    StoredTrace::Bound {
        digest: super::output_digest(&bytes).hex().to_owned(),
        sidecar: Box::new(sidecar),
    }
}

/// Store `sidecar` under its receipt and return the digest (hex) of the
/// stored bytes. It is only claimed once it reads back bound and equal.
pub(crate) fn persist(sidecar: &TraceSidecarV1) -> Result<String, String> {
    if !sidecar.org_policy_consistent() {
        return Err("trace sidecar states no consistent organization decision".to_owned());
    }
    let dir = super::gateway_dir().ok_or("no lean-ctx data directory")?;
    let bytes = serde_json::to_vec(sidecar).map_err(|e| e.to_string())?;
    super::write_atomic(&dir, &sidecar_path(&dir, &sidecar.receipt), &bytes)?;
    match load_in(&dir, &sidecar.receipt) {
        StoredTrace::Bound {
            digest,
            sidecar: stored,
        } if *stored == *sidecar => Ok(digest),
        _ => Err("stored trace sidecar does not verify".to_owned()),
    }
}
