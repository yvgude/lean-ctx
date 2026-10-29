// SPDX-License-Identifier: Apache-2.0
//! The position-aware walk of one egress admission over a request document.
//!
//! Every string a caller controls is admitted; a key is structural only at
//! the exact position where the provider protocol defines it; provider-signed
//! or -encrypted material is recognised only in the provider shape that
//! carries it — never by a member's name alone.
//!
//! One admission is one policy snapshot: the gateway policy and the project
//! policy are captured before the walk starts and used for every string of
//! it. Nothing here reads live configuration or the live project policy.

use std::borrow::Cow;
use std::sync::Arc;

use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDecisionV1, ContextDispositionV1, ReasonCodeV1, TransformationKindV1,
};
use serde_json::{Map, Value};

use super::super::capture::AdmissionCapture;
use super::super::{
    AdmissionCounts, AdmissionPolicy, admit, admit_uninspectable, clean, fingerprint_digest,
    mark_policy_withheld, mark_project_redaction, reason, withheld,
};
use crate::core::policy::runtime::ActivePolicy;

pub(super) struct Walk<'a> {
    pub(super) policy: &'a AdmissionPolicy,
    pub(super) fingerprint: Vec<u8>,
    pub(super) capture: &'a AdmissionCapture,
    pub(super) remote: bool,
    pub(super) mandatory: bool,
    /// The project policy of this admission, captured once before the walk.
    pub(super) project: Option<Arc<ActivePolicy>>,
    /// The wire family of the destination's provider.
    pub(super) wire: Wire,
    /// Claude's safeguard protocol fields, only on the trusted Anthropic route.
    pub(super) anthropic_safeguards: bool,
    pub(super) refused: Option<String>,
    pub(super) rewritten: bool,
    pub(super) classification: Option<ClassificationV1>,
    /// Values visited so far (bound of the walk).
    pub(super) nodes: usize,
    /// The walk hit a bound: the body is only partially covered.
    pub(super) partial: bool,
    /// What this walk learned for the provider-sealed items of the request:
    /// its source-bearing parts, the sealed items awaiting their verdict, and
    /// whether the request refers to state only the provider holds.
    pub(super) sealed: SealedWalk,
    /// The destination provider, as its receipt names it: a human's grant
    /// for a withheld block holds for this provider only.
    pub(super) provider: String,
    /// Where the walk is: the request member below the root, and the index
    /// in a message list. Content-free, for the explanation of a withheld
    /// block (`lean-ctx gateway why`).
    pub(super) at: Location,
    /// Classification of the blocks withheld in place for the destination:
    /// not part of what leaves, but of a request refused as a whole.
    pub(super) withheld_classification: Option<ClassificationV1>,
}

/// See [`Walk::at`].
#[derive(Default)]
pub(super) struct Location {
    member: Option<String>,
    message: Option<usize>,
}

impl Location {
    fn describe(&self) -> String {
        match (&self.member, self.message) {
            (Some(member), Some(index)) if is_message_list(member) => format!("message {index}"),
            (Some(member), _) => member.clone(),
            (None, _) => "request".to_owned(),
        }
    }
}

fn is_message_list(member: &str) -> bool {
    matches!(member, "messages" | "input" | "contents")
}

/// The content-free notice that replaces a withheld block in place.
pub(super) fn withheld_notice(codes: &[&str], id: Option<&str>) -> String {
    match id {
        Some(id) => format!(
            "[lean-ctx gateway: content withheld — {} · lean-ctx gateway why {id}]",
            codes.join(", ")
        ),
        None => format!(
            "[lean-ctx gateway: content withheld — {}]",
            codes.join(", ")
        ),
    }
}

/// See [`Walk::sealed`]. Filled only where full inspection is demanded.
#[derive(Default)]
pub(super) struct SealedWalk {
    parts: Vec<SealedPart>,
    items: Vec<String>,
    server_state: bool,
    /// Whose content the walk is in; `None` is host-authored text.
    kind: Option<PartKind>,
}

/// One admitted string.
enum Verdict {
    Keep,
    Replace(String),
}

impl Walk<'_> {
    fn record(&self, decision: &ContextDecisionV1, counts: AdmissionCounts) {
        if decision.disposition == ContextDispositionV1::Deny {
            // K12: a withheld object counts under the rules its reasons name.
            crate::core::hive::metrics_v2::signals::admission_refused(
                decision.reason_codes.iter().map(ReasonCodeV1::as_str),
            );
        }
        self.capture.record(decision, counts);
    }

    fn observe(&mut self, classification: ClassificationV1) {
        self.classification = Some(
            self.classification
                .map_or(classification, |seen| seen.join(classification)),
        );
    }

    /// The stable id of a withheld block, its explanation recorded for
    /// `lean-ctx gateway why`. `None` without a usable receipt key.
    fn note_withheld(&self, decision: &ContextDecisionV1) -> Option<String> {
        let id = withheld::block_id(&decision.object)?;
        withheld::record(&withheld::WithheldBlock {
            id: id.clone(),
            reason_codes: decision
                .reason_codes
                .iter()
                .map(|code| code.as_str().to_owned())
                .collect(),
            position: self.at.describe(),
            provider: self.provider.clone(),
            locality: if self.remote { "remote" } else { "local" }.to_owned(),
            mode: format!("{:?}", self.policy.gateway.mode).to_ascii_lowercase(),
        });
        Some(id)
    }

    /// Whether a human allowed exactly this block to go to this provider
    /// (`lean-ctx gateway allow-once`). Never where checks are mandatory.
    fn allowed_by_human(&self, decision: &ContextDecisionV1) -> bool {
        !self.mandatory
            && withheld::block_id(&decision.object)
                .is_some_and(|id| withheld::allowed(&id, &self.provider))
    }

    fn refuse(&mut self, reasons: &[ReasonCodeV1]) {
        if self.refused.is_none() {
            let codes: Vec<&str> = reasons.iter().map(ReasonCodeV1::as_str).collect();
            self.refused = Some(format!(
                "request withheld by the context gateway ({})",
                codes.join(", ")
            ));
        }
    }

    /// `text` under the captured project policy: the protected form, or `Err`
    /// when the policy blocks it. Without a project policy the text itself.
    fn project_protected<'t>(&self, text: &'t str) -> Result<Cow<'t, str>, ()> {
        let Some(active) = &self.project else {
            return Ok(Cow::Borrowed(text));
        };
        let outcome = crate::core::policy::content::evaluate_text(text, active);
        if outcome.blocked {
            Err(())
        } else {
            Ok(Cow::Owned(outcome.text))
        }
    }

    /// The project and gateway layers must commute over the original input,
    /// as for every other carrier; if their order changes the result,
    /// withholding is safer than publishing text of ambiguous provenance.
    /// Both sides use the policies captured for this admission.
    fn compose(&self, project_original: &str, gateway_output: &str) -> Option<String> {
        let project_after_gateway = self.project_protected(gateway_output).ok()?.into_owned();
        let gateway_after_project = admit(project_original, None, self.policy).text?;
        (project_after_gateway == gateway_after_project).then_some(project_after_gateway)
    }

    fn admit_string(&mut self, text: &str, sealed: bool) -> Verdict {
        let verdict = self.admit_string_inner(text, sealed);
        if let Some(kind) = self.sealed.kind
            && !text.is_empty()
        {
            self.source_part(kind, text);
        }
        #[cfg(test)]
        super::failpoint::after_first_string();
        verdict
    }

    /// Governed and sovereign mode, or an active project policy whose content
    /// rules cannot be evaluated on material nothing can read.
    fn full_inspection(&self) -> bool {
        self.mandatory || self.project.is_some()
    }

    /// Note one part that is not host-authored text, by the digest of the
    /// text exactly as the host supplied it — before this admission masks
    /// anything and before any later transformation.
    fn source_part(&mut self, kind: PartKind, supplied: &str) {
        if self.full_inspection() {
            self.sealed.parts.push(SealedPart {
                kind,
                digest: crate::core::archive::delivered::digest(supplied),
            });
        }
    }

    fn admit_string_inner(&mut self, text: &str, sealed: bool) -> Verdict {
        if text.is_empty() {
            return Verdict::Keep;
        }
        // Project policy must see the original value before a Gateway detector
        // can redact a phrase that the project policy is required to block.
        let project_original = if self.project.is_some() {
            if let Ok(admitted) = self.project_protected(text) {
                Some(admitted)
            } else {
                let mut admission = admit(text, None, self.policy);
                self.observe(admission.classification);
                mark_policy_withheld(&mut admission, "policy.content_withheld");
                self.note_withheld(&admission.decision);
                self.record(&admission.decision, admission.counts);
                self.refuse(&admission.decision.reason_codes);
                return Verdict::Keep;
            }
        } else {
            None
        };
        let project_rewrites = project_original
            .as_ref()
            .is_some_and(|admitted| admitted.as_ref() != text);
        // Clean memo: unchanged, unflagged text under the same policy is the
        // common case for every earlier turn of a conversation. The project
        // policy is checked above on every call; the combined fingerprint below
        // keeps entries separate across project policy epochs and roots.
        let key = self
            .policy
            .semantic
            .is_none()
            .then(|| clean::key(&self.fingerprint, "egress", text));
        if !project_rewrites
            && let Some(key) = &key
            && let Some(decision) = clean::get(key)
        {
            // Only clean, unflagged text is memoized; it carries the mode's
            // classification for unclassified content, as a fresh admit would.
            self.observe(self.policy.gateway.mode.unclassified());
            self.record(&decision, AdmissionCounts::default());
            return Verdict::Keep;
        }
        let mut admission = admit(text, None, self.policy);
        let mut destination_denied = false;
        // Destination is part of the authorization: restricted content
        // never leaves for a remote model. The block is withheld in place —
        // the rest of the request still goes — so a conversation that once
        // carried it is not refused on every later turn. Only a human's grant
        // for exactly these bytes and this provider lets it go.
        if self.remote
            && admission.text.is_some()
            && admission.classification >= ClassificationV1::Restricted
        {
            if self.allowed_by_human(&admission.decision) {
                admission
                    .decision
                    .reason_codes
                    .push(reason(withheld::ALLOWED_ONCE));
                // The person released exactly this block for this
                // destination: what leaves is classified as released.
                self.observe(ClassificationV1::Confidential);
            } else {
                admission.text = None;
                admission.decision.disposition = ContextDispositionV1::Deny;
                admission
                    .decision
                    .reason_codes
                    .push(reason("destination.remote_restricted"));
                destination_denied = true;
                // Withheld in place: it does not classify what leaves, but
                // a request refused as a whole still states it.
                self.withheld_classification = Some(
                    self.withheld_classification
                        .map_or(admission.classification, |seen| {
                            seen.join(admission.classification)
                        }),
                );
            }
        } else {
            self.observe(admission.classification);
        }
        if let Some(project_original) = &project_original {
            if destination_denied {
                // Keep the original Gateway classification and its refusal;
                // a project redaction cannot make restricted remote content safe.
            } else if let Some(gateway_output) = admission.text.as_deref() {
                if let Some(composed) = self.compose(project_original.as_ref(), gateway_output) {
                    if project_rewrites || composed != gateway_output {
                        mark_project_redaction(&mut admission);
                    }
                    admission.text = Some(composed);
                } else {
                    mark_policy_withheld(&mut admission, "policy.content_withheld");
                    self.refuse(&admission.decision.reason_codes);
                }
            } else if project_rewrites {
                // A project transformation cannot be reconciled with an
                // already withheld Gateway result; withhold the whole request.
                mark_policy_withheld(&mut admission, "policy.content_withheld");
                self.refuse(&admission.decision.reason_codes);
            }
        }
        if sealed && project_rewrites {
            // Signed provider blocks cannot be rewritten, and the project rule
            // cannot be bypassed by Developer-mode Gateway behavior.
            mark_policy_withheld(&mut admission, "policy.content_withheld");
            self.refuse(&admission.decision.reason_codes);
        }
        if !project_rewrites
            && sealed
            && admission
                .text
                .as_deref()
                .is_some_and(|delivered| delivered != text)
        {
            // Signed/encrypted by the provider: masking would break the
            // request. Governed modes refuse it; developer mode delivers it
            // unchanged and says so.
            admission
                .decision
                .reason_codes
                .push(reason("egress.sealed_content"));
            admission
                .decision
                .required_transformations
                .retain(|kind| *kind != TransformationKindV1::Redaction);
            if self.mandatory {
                admission.decision.disposition = ContextDispositionV1::Deny;
                admission.text = None;
            } else {
                admission.decision.disposition = ContextDispositionV1::Allow;
                admission.text = Some(text.to_owned());
            }
        }
        self.record(&admission.decision, admission.counts);
        if let Some(key) = key
            && admission.disposition() == ContextDispositionV1::Allow
            && admission.decision.reason_codes.is_empty()
            && admission.text.as_deref() == Some(text)
        {
            clean::insert(key, admission.decision.clone());
        }
        match admission.text {
            Some(delivered) if delivered == text => Verdict::Keep,
            Some(delivered) => {
                self.rewritten = true;
                Verdict::Replace(delivered)
            }
            None if sealed => {
                // Provider-signed text cannot be replaced: the request is
                // refused, and its refusal names this block's id.
                self.note_withheld(&admission.decision);
                self.refuse(&admission.decision.reason_codes);
                Verdict::Keep
            }
            None => {
                let id = self.note_withheld(&admission.decision);
                let codes: Vec<&str> = admission
                    .decision
                    .reason_codes
                    .iter()
                    .map(ReasonCodeV1::as_str)
                    .collect();
                self.rewritten = true;
                Verdict::Replace(withheld_notice(&codes, id.as_deref()))
            }
        }
    }

    fn media(&mut self, bytes: usize) {
        let admission = admit_uninspectable(None, bytes as u64, self.policy);
        self.record(&admission.decision, admission.counts);
        if admission.text.is_none() {
            self.refuse(&admission.decision.reason_codes);
        }
    }

    /// A string at a position that cannot be rewritten without corrupting the
    /// request: an object key, a tool or function name. It is inspected like
    /// any other content; a finding refuses the request in every mode, with
    /// the reason codes and the position class, never the value.
    fn verify_immutable(&mut self, text: &str, protocol_flag: bool) {
        if text.is_empty() || self.refused.is_some() {
            return;
        }
        if self.project.is_some()
            && !self
                .project_protected(text)
                .is_ok_and(|admitted| admitted.as_ref() == text)
        {
            let codes = [
                reason("policy.content_withheld"),
                reason("egress.immutable_position"),
            ];
            // The refusal receipt carries its deny decision.
            self.record(
                &ContextDecisionV1 {
                    object: fingerprint_digest(b"egress-immutable-position"),
                    disposition: ContextDispositionV1::Deny,
                    reason_codes: codes.to_vec(),
                    signals: Vec::new(),
                    required_transformations: Vec::new(),
                },
                AdmissionCounts::default(),
            );
            self.refuse(&codes);
            return;
        }
        let key = (!protocol_flag && self.policy.semantic.is_none())
            .then(|| clean::key(&self.fingerprint, "egress", text));
        if let Some(key) = &key
            && clean::get(key).is_some()
        {
            return;
        }
        // A known boolean protocol field is not a classification banner. Its
        // project policy and all other detectors still apply. Never memoize
        // this positional decision as clean text in ordinary source content.
        let protocol_policy = protocol_flag.then(|| {
            let mut policy = self.policy.clone();
            policy.gateway.classification = crate::core::input_filters::FilterAction::Off;
            policy
        });
        let mut admission = admit(text, None, protocol_policy.as_ref().unwrap_or(self.policy));
        let mut restricted =
            self.remote && admission.classification >= ClassificationV1::Restricted;
        if restricted && self.allowed_by_human(&admission.decision) {
            restricted = false;
            admission
                .decision
                .reason_codes
                .push(reason(withheld::ALLOWED_ONCE));
            // Released by a person for this destination (see admit_string).
            admission.classification = ClassificationV1::Confidential;
        }
        if admission.text.as_deref() == Some(text) && !restricted {
            // A key or a tool name is not a source object: a clean one adds no
            // decision and no classification, so the receipt's source counts
            // (`permitted`, and with them Withheld vs. Failed) keep describing
            // the content strings only. Only a finding is recorded.
            if admission.decision.reason_codes.is_empty() {
                if let Some(key) = key
                    && admission.disposition() == ContextDispositionV1::Allow
                {
                    clean::insert(key, admission.decision.clone());
                }
            } else {
                self.observe(admission.classification);
                self.record(&admission.decision, admission.counts);
            }
            return;
        }
        self.observe(admission.classification);
        if restricted {
            admission
                .decision
                .reason_codes
                .push(reason("destination.remote_restricted"));
        }
        admission
            .decision
            .reason_codes
            .push(reason("egress.immutable_position"));
        admission.decision.disposition = ContextDispositionV1::Deny;
        admission
            .decision
            .required_transformations
            .retain(|kind| *kind != TransformationKindV1::Redaction);
        self.note_withheld(&admission.decision);
        self.record(&admission.decision, admission.counts);
        self.refuse(&admission.decision.reason_codes);
    }

    /// Provider-signed or -encrypted material at its protocol position, in
    /// its provider shape. No detector can read it and rewriting it breaks
    /// the request, so it is recorded for what it is — opaque, not inspected,
    /// `bytes` long — never as inspected by absence.
    ///
    /// Developer mode forwards it byte-identical.
    ///
    /// Where full inspection is demanded it is not decided here: the item
    /// waits until the whole request has been walked ([`Self::settle_sealed`]),
    /// because whether it may leave depends on every other part of the
    /// request having been admitted.
    fn opaque(&mut self, member: &Value) {
        let sealed = member
            .as_str()
            .map_or_else(|| Cow::Owned(member.to_string()), Cow::Borrowed);
        if self.full_inspection() {
            self.sealed.items.push(sealed.into_owned());
        } else {
            self.record_sealed(&sealed, None);
        }
    }

    /// After the walk: decide on the provider-sealed items of the request.
    ///
    /// Such an item may leave only as what this destination itself produced
    /// for this very context, and only while every source its producing
    /// request was derived from is known and still admitted. The carrier
    /// that saw the responses answers through the resolver of this admission
    /// ([`with_resolver`]). Returned to its origin an item is not new
    /// egress; the receipt states it with its digest as provider material
    /// returned to its origin, not inspected. Without a resolver, or for
    /// anything the resolver does not vouch for, the request is refused,
    /// with a reason that tells unknown origin, unknown source authority and
    /// lapsed authority apart and never says anything about the content.
    ///
    /// The resolver is also called for a request without sealed items: the
    /// source-bearing parts of this request are what the sealed items of its
    /// response derive from.
    pub(super) fn settle_sealed(&mut self) {
        if !self.full_inspection() {
            return;
        }
        let sealed = std::mem::take(&mut self.sealed);
        let verdicts = resolve_sealed(&SealedRequest {
            items: &sealed.items,
            parts: &sealed.parts,
            server_state: sealed.server_state,
        });
        for (index, item) in sealed.items.iter().enumerate() {
            let verdict = verdicts
                .get(index)
                .copied()
                .unwrap_or(SealedVerdict::UnknownOrigin);
            self.record_sealed(item, Some(verdict));
        }
    }

    /// Record one sealed item; `verdict` is `None` in developer mode, where
    /// it is forwarded as opaque material without a verdict.
    fn record_sealed(&mut self, sealed: &str, verdict: Option<SealedVerdict>) {
        let size_only =
            || fingerprint_digest(format!("opaque-provider-material:{}", sealed.len()).as_bytes());
        let (disposition, object, verdict_code) = match verdict {
            None => (ContextDispositionV1::Allow, size_only(), None),
            Some(SealedVerdict::ReturnedToOrigin) => (
                ContextDispositionV1::Allow,
                fingerprint_digest(sealed.as_bytes()),
                Some("egress.sealed_returned_to_origin"),
            ),
            Some(SealedVerdict::UnknownOrigin) => (
                ContextDispositionV1::Deny,
                size_only(),
                Some("egress.sealed_origin_unknown"),
            ),
            Some(SealedVerdict::SourceAuthorityUnknown) => (
                ContextDispositionV1::Deny,
                size_only(),
                Some("egress.sealed_source_authority_unknown"),
            ),
            Some(SealedVerdict::SourceNotPresented) => (
                ContextDispositionV1::Deny,
                size_only(),
                Some("egress.sealed_source_not_presented"),
            ),
            Some(SealedVerdict::AuthorityLapsed) => (
                ContextDispositionV1::Deny,
                size_only(),
                Some("egress.sealed_authority_lapsed"),
            ),
        };
        let mut codes = vec![reason(OPAQUE_PROVIDER_MATERIAL)];
        if let Some(code) = verdict_code {
            codes.push(reason(code));
            codes.push(reason("coverage.uninspected"));
        }
        self.record(
            &ContextDecisionV1 {
                object,
                disposition,
                reason_codes: codes.clone(),
                signals: Vec::new(),
                required_transformations: Vec::new(),
            },
            AdmissionCounts::default(),
        );
        if disposition == ContextDispositionV1::Deny {
            self.refuse(&codes);
        }
    }

    /// Depth and node bounds of the walk. Beyond them the body is only
    /// partially covered: refused when detectors are mandatory or a project
    /// policy is active, forwarded with the recorded reason in developer mode.
    fn bounded(&mut self, depth: usize) -> bool {
        self.nodes += 1;
        if self.nodes <= MAX_WALK_NODES && depth <= MAX_WALK_DEPTH {
            return true;
        }
        if !self.partial {
            self.partial = true;
            let codes = [reason("coverage.partial")];
            let refuse = self.mandatory || self.project.is_some();
            self.record(
                &ContextDecisionV1 {
                    object: fingerprint_digest(b"egress-walk-bound"),
                    disposition: if refuse {
                        ContextDispositionV1::Deny
                    } else {
                        ContextDispositionV1::Allow
                    },
                    reason_codes: codes.to_vec(),
                    signals: Vec::new(),
                    required_transformations: Vec::new(),
                },
                AdmissionCounts::default(),
            );
            if refuse {
                self.refuse(&codes);
            }
        }
        false
    }

    pub(super) fn walk(
        &mut self,
        value: &mut Value,
        position: Position,
        sealed: bool,
        depth: usize,
    ) {
        if !self.bounded(depth) {
            return;
        }
        match value {
            Value::String(text) => {
                if let Verdict::Replace(admitted) = self.admit_string(text, sealed) {
                    *text = admitted;
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    if depth == 1 {
                        self.at.message = Some(index);
                    }
                    let next = if position == Position::Safeguard && !item.is_object() {
                        Position::Content
                    } else {
                        position
                    };
                    self.walk(item, next, sealed, depth + 1);
                }
            }
            Value::Object(map) => {
                // Provider-sealed material exists only in the provider shape
                // that carries it, at its protocol position. Any other object
                // with a member of the same name is ordinary caller data.
                let opaque_members = provider_sealed(self.wire, position, map);
                let sealed_block = position.protocol() && (sealed || !opaque_members.is_empty());
                let outer = self.sealed.kind;
                let kind = part_kind(position, map, outer);
                if position == Position::Item
                    && map.get("type").and_then(Value::as_str) == Some("item_reference")
                {
                    self.sealed.server_state = true;
                }
                for (key, child) in map.iter_mut() {
                    let key = key.as_str();
                    if depth == 0 {
                        self.at = Location {
                            member: Some(key.to_owned()),
                            message: None,
                        };
                    }
                    self.sealed.kind = kind;
                    if refers_to_provider_state(position, key, child) {
                        self.sealed.server_state = true;
                    }
                    // A known boolean flag has protocol meaning, while every
                    // caller-chosen key remains content to inspect in full.
                    let protocol_flag = self.anthropic_safeguards
                        && position == Position::ClassifierContext
                        && key == "restricted"
                        && child.is_boolean();
                    self.verify_immutable(key, protocol_flag);
                    let next = match (position, key) {
                        (Position::Root, "safeguards")
                            if self.anthropic_safeguards && child.is_array() =>
                        {
                            Child::Walk(Position::Safeguard)
                        }
                        (Position::Safeguard, "classifier_context") if child.is_object() => {
                            Child::Walk(Position::ClassifierContext)
                        }
                        _ => position.child(key, opaque_members),
                    };
                    match next {
                        Child::Identifier => {}
                        Child::Name => {
                            if let Value::String(name) = child {
                                self.verify_immutable(name, false);
                            } else {
                                self.walk(child, Position::Content, false, depth + 1);
                            }
                        }
                        Child::Opaque => self.opaque(child),
                        Child::Walk(next) => {
                            // Media payloads exist only in content blocks; a
                            // tool argument called `data` is text to inspect.
                            if position == Position::Block
                                && let Some(bytes) = media_bytes(key, child)
                            {
                                self.media(bytes);
                                // An attachment is never host-authored text.
                                if let Value::String(payload) = &*child {
                                    self.source_part(kind.unwrap_or(PartKind::Source), payload);
                                }
                                continue;
                            }
                            // The text of a sealed block is inspected but not
                            // rewritten; objects below it that are ordinary
                            // caller content are not sealed.
                            let sealed = sealed_block && (child.is_string() || next.protocol());
                            self.walk(child, next, sealed, depth + 1);
                        }
                    }
                }
                self.sealed.kind = outer;
            }
            _ => {}
        }
    }
}

const MAX_WALK_NODES: usize = 200_000;
const MAX_WALK_DEPTH: usize = 64;
const OPAQUE_PROVIDER_MATERIAL: &str = "egress.opaque_provider_material";

/// Whose content an object at `position` carries, for the lineage of what
/// the provider seals in its answer. `None` is host-authored text: system
/// and user text that is neither a tool result nor an attachment.
///
/// - items: an `assistant`/`model` message and every provider-produced item
///   (calls, reasoning, compaction) are assistant parts; a `tool`/`function`
///   message and every `*_output` item are tool results;
/// - blocks of a host message: `tool_result` shapes, documents, images, files
///   and audio, and Gemini `functionResponse`/`inlineData`/`fileData` parts
///   are tool results or attachments;
/// - everything below keeps the kind of what encloses it.
fn part_kind(
    position: Position,
    map: &Map<String, Value>,
    outer: Option<PartKind>,
) -> Option<PartKind> {
    let member = |key: &str| map.get(key).and_then(Value::as_str);
    match position {
        Position::Item => match (member("role"), member("type")) {
            (Some("assistant" | "model"), _) => Some(PartKind::Assistant),
            (Some("tool" | "function"), _) => Some(PartKind::Source),
            (Some(_), _) | (None, None | Some("message" | "mcp_approval_response")) => None,
            (None, Some(kind)) if kind.ends_with("_output") || kind == "item_reference" => {
                Some(PartKind::Source)
            }
            (None, Some(_)) => Some(PartKind::Assistant),
        },
        Position::Block if outer.is_none() => {
            let source = match member("type") {
                Some(kind) => {
                    kind.ends_with("tool_result")
                        || matches!(
                            kind,
                            "document"
                                | "image"
                                | "image_url"
                                | "input_image"
                                | "input_file"
                                | "file"
                                | "input_audio"
                                | "audio"
                                | "video"
                                | "search_result"
                                | "container_upload"
                        )
                }
                None => [
                    "functionResponse",
                    "function_response",
                    "inlineData",
                    "inline_data",
                    "fileData",
                    "file_data",
                ]
                .iter()
                .any(|shape| map.contains_key(*shape)),
            };
            source.then_some(PartKind::Source)
        }
        _ => outer,
    }
}

/// Whether this member makes the provider derive its answer from state the
/// request does not carry: a stored conversation or response, a cached
/// prefix, a stored prompt, a file or media object by id or by URL. What the
/// provider seals in such an answer has an origin no admission ever saw.
fn refers_to_provider_state(position: Position, key: &str, value: &Value) -> bool {
    if value.is_null() {
        return false;
    }
    match (position, key) {
        (
            Position::Root,
            "previous_response_id" | "conversation" | "prompt" | "cachedContent" | "cached_content",
        )
        | (Position::Block, "file_id" | "fileData" | "file_data" | "fileUri" | "file_uri") => true,
        (Position::Block, "url" | "image_url") => value
            .as_str()
            .is_some_and(|url| url.starts_with("http://") || url.starts_with("https://")),
        _ => false,
    }
}

/// What the carrier that saw a destination's responses says about one
/// provider-sealed item a request returns, where full inspection is demanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedVerdict {
    /// Exactly these bytes were produced by this destination for this
    /// context, the sources its producing request was derived from are all
    /// known and still admitted, and everything it was produced under still
    /// holds.
    ReturnedToOrigin,
    /// Never seen from this destination (or no longer remembered).
    UnknownOrigin,
    /// Produced by this destination for this context, but the sources its
    /// producing request was derived from are not all known. A known origin
    /// alone never grants.
    SourceAuthorityUnknown,
    /// Its sources are known, but one that a caller presented to the Engine
    /// is not presented now.
    SourceNotPresented,
    /// Produced by this destination, but not for this context; or the
    /// authority it was produced under is no longer current; or one of its
    /// sources is no longer admitted.
    AuthorityLapsed,
}

/// What a part of a request is, when it is not host-authored text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    /// A tool result or an attachment: content that came from a source.
    Source,
    /// What an earlier response of the provider produced.
    Assistant,
}

/// One part of a request that is not host-authored text, by the digest of
/// its text as the host supplied it: the decoded string value, digested the
/// way the Engine digests what it delivers (`archive::delivered::digest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedPart {
    pub kind: PartKind,
    pub digest: String,
}

/// What one walk hands the resolver: the sealed items it met (their exact
/// bytes, to be digested and dropped), every part that is not host-authored
/// text, and whether the request refers to state only the provider holds.
#[derive(Debug, Clone, Copy)]
pub struct SealedRequest<'a> {
    pub items: &'a [String],
    pub parts: &'a [SealedPart],
    pub server_state: bool,
}

type SealedResolver = Box<dyn Fn(&SealedRequest<'_>) -> Vec<SealedVerdict>>;

thread_local! {
    static SEALED_RESOLVER: std::cell::RefCell<Option<SealedResolver>> =
        const { std::cell::RefCell::new(None) };
}

/// Run one admission with `resolver` answering for the provider-sealed items
/// of its walk. The scope is this call on this thread; the previous resolver
/// is restored afterwards.
pub(super) fn with_resolver<T>(resolver: SealedResolver, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<SealedResolver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            SEALED_RESOLVER.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let _restore = Restore(SEALED_RESOLVER.with(|slot| slot.borrow_mut().replace(resolver)));
    operation()
}

/// One verdict per item, in order. Without a resolver nothing vouches for a
/// sealed item: unknown origin (a missing verdict means the same).
fn resolve_sealed(request: &SealedRequest<'_>) -> Vec<SealedVerdict> {
    SEALED_RESOLVER.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or_else(Vec::new, |resolver| resolver(request))
    })
}

/// The wire family a destination's provider label speaks. Provider-sealed
/// shapes are recognised for their own family only; a label this module does
/// not know cannot narrow the family and is matched by shape and position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wire {
    Anthropic,
    OpenAi,
    Gemini,
    Unknown,
}

impl Wire {
    pub(super) fn of(provider: &str) -> Self {
        match provider.to_ascii_lowercase().as_str() {
            "anthropic" | "bedrock" | "copilot" => Self::Anthropic,
            "openai" | "chatgpt" | "grok" | "commandcode" => Self::OpenAi,
            "gemini" | "google" => Self::Gemini,
            _ => Self::Unknown,
        }
    }

    fn speaks(self, family: Self) -> bool {
        self == family || self == Self::Unknown
    }
}

fn has_string(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(Value::is_string)
}

/// The members of `map` that are provider-signed or -encrypted material, by
/// provider, position and shape — empty for every other object:
///
/// - Anthropic content blocks: `signature` of a `thinking` block, `data` of a
///   `redacted_thinking` block;
/// - a Gemini Part: `thoughtSignature` beside a `functionCall` or `text`;
/// - OpenAI Responses input items: `encrypted_content` of a `reasoning` or
///   `compaction` item, `fingerprint` of a `program` item.
fn provider_sealed(
    wire: Wire,
    position: Position,
    map: &Map<String, Value>,
) -> &'static [&'static str] {
    let kind = map.get("type").and_then(Value::as_str);
    match (position, kind) {
        (Position::Block, Some("thinking"))
            if wire.speaks(Wire::Anthropic) && has_string(map, "signature") =>
        {
            &["signature"]
        }
        (Position::Block, Some("redacted_thinking"))
            if wire.speaks(Wire::Anthropic) && has_string(map, "data") =>
        {
            &["data"]
        }
        (Position::Block, None)
            if wire.speaks(Wire::Gemini)
                && (has_string(map, "thoughtSignature")
                    || has_string(map, "thought_signature"))
                && ["functionCall", "function_call", "text"]
                    .iter()
                    .any(|member| map.contains_key(*member)) =>
        {
            &["thoughtSignature", "thought_signature"]
        }
        (Position::Item, Some("reasoning" | "compaction"))
            if wire.speaks(Wire::OpenAi) && has_string(map, "encrypted_content") =>
        {
            &["encrypted_content"]
        }
        (Position::Item, Some("program"))
            if wire.speaks(Wire::OpenAi) && has_string(map, "fingerprint") =>
        {
            &["fingerprint"]
        }
        _ => &[],
    }
}

/// Where in a provider wire shape (Anthropic Messages, OpenAI Chat and
/// Responses, Gemini) a value sits. A key is structural only at the exact
/// position where the protocol defines it — never by its name anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Position {
    /// The request object.
    Root,
    /// An element of `messages`, `input` or `contents`.
    Item,
    /// A content block: an element of an item's `content`/`parts`/`summary`,
    /// of `system`, or a media holder inside a block.
    Block,
    /// A tool call: an element of OpenAI Chat `tool_calls`, its `function`,
    /// or a Gemini `functionCall`/`functionResponse`.
    Call,
    /// An immediate object element of Anthropic's root safeguard array.
    Safeguard,
    /// The object-valued classifier context of one such safeguard.
    ClassifierContext,
    /// Caller-controlled content at any depth — tool inputs and arguments,
    /// tool definitions and schemas, `stop` sequences, response formats,
    /// metadata. No key-name exemption applies below this point.
    Content,
}

/// What the walk does with one object member.
enum Child {
    /// A protocol identifier at its protocol position: forwarded unchanged.
    Identifier,
    /// A tool or function name: inspected, never rewritten.
    Name,
    /// Provider-signed or -encrypted material at its protocol position.
    Opaque,
    Walk(Position),
}

impl Position {
    /// Positions the protocol defines, as opposed to caller-controlled content.
    fn protocol(self) -> bool {
        matches!(self, Self::Root | Self::Item | Self::Block | Self::Call)
    }

    /// `opaque_members`: the provider-sealed members of the object this key
    /// belongs to (see [`provider_sealed`]); they outrank every other rule.
    fn child(self, key: &str, opaque_members: &[&str]) -> Child {
        if opaque_members.contains(&key) {
            return Child::Opaque;
        }
        match (self, key) {
            // Protocol identifiers, each only at its own position.
            (Self::Root, "model" | "previous_response_id")
            | (Self::Item, "role" | "type" | "id" | "call_id" | "tool_call_id" | "status")
            | (
                Self::Block,
                "type" | "id" | "tool_use_id" | "call_id" | "file_id" | "media_type" | "mime_type"
                | "mimeType",
            )
            | (Self::Call, "id" | "type") => Child::Identifier,
            (Self::Root, "messages" | "input" | "contents") => Child::Walk(Self::Item),
            (
                Self::Root,
                "system" | "instructions" | "systemInstruction" | "system_instruction",
            )
            | (Self::Item, "content" | "parts" | "summary" | "output")
            | (
                Self::Block,
                "content" | "parts" | "source" | "inlineData" | "inline_data" | "image_url"
                | "fileData" | "file_data" | "input_audio",
            ) => Child::Walk(Self::Block),
            (Self::Item, "tool_calls")
            | (
                Self::Block,
                "functionCall" | "functionResponse" | "function_call" | "function_response",
            )
            | (Self::Call, "function") => Child::Walk(Self::Call),
            (Self::Item | Self::Block | Self::Call, "name") => Child::Name,
            _ => Child::Walk(Self::Content),
        }
    }
}

/// Base64 media payloads: Anthropic `source.data`, Gemini `inlineData.data`,
/// OpenAI `image_url.url` / `input_image.image_url` data URIs.
fn media_bytes(key: &str, value: &Value) -> Option<usize> {
    match (key, value) {
        ("data", Value::String(data)) if data.len() > 256 && looks_base64(data) => Some(data.len()),
        ("url" | "image_url" | "file_data", Value::String(url)) if url.starts_with("data:") => {
            Some(url.len())
        }
        _ => None,
    }
}

fn looks_base64(data: &str) -> bool {
    data.bytes()
        .take(512)
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}
