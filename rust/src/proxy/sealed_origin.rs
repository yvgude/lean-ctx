// SPDX-License-Identifier: Apache-2.0
//! Provenance of provider-sealed items (thinking signatures, encrypted
//! reasoning, thought signatures).
//!
//! Providers require such items to be sent back unchanged. No detector can
//! read them, so where full inspection is demanded they may leave only as
//! what the SAME destination produced for the SAME context under an
//! authority that still holds. The proxy sees the responses: it records, for
//! every sealed item at its provider shape, a digest bound to
//!
//! - the destination identity of the producing request,
//! - the context that request ran in (project, role, principal),
//! - the authority that request was admitted under, and
//! - the source lineage of the producing request ([`Lineage`]),
//!
//! and answers the egress walk of a later request through [`resolver`].
//!
//! A register hit proves origin. Origin is never a grant: a sealed item was
//! derived from everything its producing request carried, so it may leave
//! again only while every source of that request is known and admitted NOW.
//! The proxy does not see source reads; the Engine does, and records at its
//! release point which sources a delivered text was derived from
//! (`core::archive::delivered`) — project files and sources a caller
//! presented to it. The lineage of a request is the join of
//!
//! - what the Engine recorded for each text block of a tool-result, context,
//!   recovery or attachment part, looked up by the digest of the block as the
//!   host supplied it — a block without a record makes the lineage unknown;
//! - the lineage recorded here for each assistant part (text, tool call,
//!   sealed item) with the response that produced it — an assistant part no
//!   recorded response of this context produced makes the lineage unknown;
//! - nothing for host-authored text (system and user text that is neither a
//!   tool result nor an attachment). That is the boundary of a raw proxy:
//!   file text a person pasted into a message is invisible to source
//!   authority. No header, field or marker of a client is ever read as a
//!   statement about lineage.
//!
//! A request that refers to state only the provider holds (a stored
//! response or conversation, a cached prefix, a file by id or URL) has an
//! unknown lineage. At return an item with an unknown lineage is refused
//! (`egress.sealed_source_authority_unknown`); one whose sources are known is
//! carried on only if the Engine's `still_admitted` holds for the returning
//! request — a caller-presented source that is not presented now refuses as
//! `egress.sealed_source_not_presented`, a source that is no longer admitted
//! as `egress.sealed_authority_lapsed`; a complete lineage without any source
//! — host-authored text only — is admissible. The proxy never records into
//! the Engine's register and never presents a source: presentation happens
//! only at the Engine.
//!
//! Content-free: digests and identities only, never a sealed byte. Bounded in
//! count and age, in memory of this proxy process. After a restart, an
//! eviction or the age limit an item is of unknown origin again and refused
//! where full inspection is demanded.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::admitted::Authority;
use crate::core::context_admission::egress::{
    self, EgressTarget, PartKind, SealedRequest, SealedVerdict, TrustedDestinationBinding,
};

/// The one seam to the Engine's delivered-content register
/// (`core::archive::delivered`): which sources a delivered text was derived
/// from, by the Engine's digest of that text, and the Engine's own type for
/// such a source set. Nothing else in the proxy names that module's lineage.
#[cfg(not(test))]
mod engine {
    pub(super) use crate::core::archive::delivered::{Lineage as Sources, LineageRefusal};

    pub(super) fn lineage_of(digest: &str) -> Option<Sources> {
        crate::core::archive::delivered::lineage_of(digest)
    }
}

#[cfg(test)]
#[path = "sealed_origin_engine_stand_in.rs"]
pub(crate) mod engine;

use engine::{LineageRefusal, Sources};

/// The sources a request was derived from, as far as they are bound.
// `Unknown` is the rare, refusing case; a lineage is built once per request.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum Lineage {
    /// Not every source is known. Never grants.
    Unknown,
    /// Every source-bearing part is bound to these sources; `None` when the
    /// request carried no source-bearing part at all.
    Complete(Option<Sources>),
}

/// Content-free on purpose: a source set names paths and references that
/// belong in no log line.
impl std::fmt::Debug for Lineage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => "Lineage::Unknown",
            Self::Complete(None) => "Lineage::Complete(no sources)",
            Self::Complete(Some(_)) => "Lineage::Complete(sources)",
        })
    }
}

impl Lineage {
    pub(crate) fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// What the Engine has on record for one delivered text: unknown without
    /// a record, and unknown when the record itself is not complete.
    fn delivered(digest: &str) -> Self {
        match engine::lineage_of(digest) {
            Some(sources) if sources.is_complete() => Self::Complete(Some(sources)),
            _ => Self::Unknown,
        }
    }

    /// Join `other` into this lineage. Unknown absorbs everything, and so
    /// does a join the Engine does not call complete.
    fn add(&mut self, other: &Self) {
        let joined = match (std::mem::replace(self, Self::Unknown), other) {
            (Self::Complete(None), Self::Complete(sources)) => Self::Complete(sources.clone()),
            (Self::Complete(Some(mine)), Self::Complete(None)) => Self::Complete(Some(mine)),
            (Self::Complete(Some(mut mine)), Self::Complete(Some(theirs))) => {
                mine.join(theirs.clone());
                if mine.is_complete() {
                    Self::Complete(Some(mine))
                } else {
                    Self::Unknown
                }
            }
            _ => Self::Unknown,
        };
        *self = joined;
    }
}

/// Where the caller-stage admission of a request leaves that request's
/// lineage for the response recorder.
pub(crate) type LineageSlot = Rc<RefCell<Option<Lineage>>>;

/// The content-free identity of the context a request runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SealedContext {
    /// Destination: provider, upstream base, account and region as bound.
    origin: String,
    /// Project fingerprint (root and policy) when a project policy is active.
    project: Option<[u8; 32]>,
    /// Role fingerprint under that project policy.
    role: Option<[u8; 32]>,
    /// Digest of what identifies the caller to the gateway and upstream.
    principal: Option<String>,
}

impl SealedContext {
    pub(crate) fn new(
        target: &EgressTarget<'_>,
        binding: TrustedDestinationBinding<'_>,
        project: Option<([u8; 32], [u8; 32])>,
        principal: Option<String>,
    ) -> Self {
        let destination = egress::destination_with_binding(target, binding);
        let mut identity = blake3::Hasher::new();
        for part in [
            destination.provider.as_str(),
            target.upstream_base.trim_end_matches('/'),
            destination
                .account_ref
                .as_ref()
                .map_or("", |account| account.as_str()),
            destination
                .region
                .as_ref()
                .map_or("", |region| region.as_str()),
        ] {
            identity.update(&(part.len() as u64).to_be_bytes());
            identity.update(part.as_bytes());
        }
        Self {
            origin: identity.finalize().to_hex().to_string(),
            project: project.map(|(project, _)| project),
            role: project.map(|(_, role)| role),
            principal,
        }
    }

    /// The keyed reference of this context's caller for the receipt
    /// (AP10-C6); the sealed principal itself is never recorded.
    /// `Ok(None)`: the route tells no callers apart. `Err`: the receipt key
    /// is unusable.
    pub(crate) fn principal_ref(&self) -> Result<Option<String>, &'static str> {
        self.principal
            .as_deref()
            .map(crate::core::context_admission::receipt_store::keyed::principal_ref)
            .transpose()
    }

    /// The register key of an assistant part with this text digest, produced
    /// for this context.
    fn part_key(&self, text_digest: &str) -> [u8; 32] {
        let mut key = blake3::Hasher::new();
        key.update(b"leanctx-assistant-part-v1\0");
        key.update(&(self.origin.len() as u64).to_be_bytes());
        key.update(self.origin.as_bytes());
        for fingerprint in [&self.project, &self.role] {
            match fingerprint {
                Some(fingerprint) => key.update(&[1]).update(fingerprint),
                None => key.update(&[0]),
            };
        }
        match &self.principal {
            Some(principal) => key
                .update(&[1])
                .update(&(principal.len() as u64).to_be_bytes())
                .update(principal.as_bytes()),
            None => key.update(&[0]),
        };
        key.update(text_digest.as_bytes());
        *key.finalize().as_bytes()
    }
}

/// The request whose response may produce sealed items: its context, the
/// authority it was admitted under and the sources it was derived from.
#[derive(Debug, Clone)]
pub(crate) struct SealedProducer {
    pub(crate) context: SealedContext,
    pub(crate) authority: Arc<Authority>,
    /// `None`: the request was not admitted under full inspection, so no
    /// lineage was derived for it. Its items never pass where one is needed.
    pub(crate) lineage: Option<Arc<Lineage>>,
}

struct Entry {
    digest: [u8; 32],
    producer: SealedProducer,
    recorded: Instant,
}

struct Register {
    entries: VecDeque<Entry>,
    max_entries: usize,
    max_age: Duration,
}

const MAX_ENTRIES: usize = 8192;
const MAX_AGE: Duration = Duration::from_hours(24);

static REGISTER: Mutex<Register> = Mutex::new(Register {
    entries: VecDeque::new(),
    max_entries: MAX_ENTRIES,
    max_age: MAX_AGE,
});

fn register() -> std::sync::MutexGuard<'static, Register> {
    REGISTER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn digest_of(sealed: &str) -> [u8; 32] {
    *blake3::hash(sealed.as_bytes()).as_bytes()
}

fn record(producer: &SealedProducer, sealed: &str) {
    if sealed.is_empty() {
        return;
    }
    let digest = digest_of(sealed);
    let mut register = register();
    let max_age = register.max_age;
    register.entries.retain(|entry| {
        entry.recorded.elapsed() < max_age
            && !(entry.digest == digest && entry.producer.context == producer.context)
    });
    register.entries.push_back(Entry {
        digest,
        producer: producer.clone(),
        recorded: Instant::now(),
    });
    while register.entries.len() > register.max_entries {
        register.entries.pop_front();
    }
}

/// The producers on record for exactly these sealed bytes at this origin.
fn producers_of(origin: &str, sealed: &str) -> Vec<SealedProducer> {
    let digest = digest_of(sealed);
    let register = register();
    register
        .entries
        .iter()
        .filter(|entry| {
            entry.digest == digest
                && entry.producer.context.origin == origin
                && entry.recorded.elapsed() < register.max_age
        })
        .map(|entry| entry.producer.clone())
        .collect()
}

/// What responses produced as ordinary assistant content — text, thinking
/// text, tool-call arguments — by context and digest, with the lineage of the
/// request each was produced for.
struct Parts {
    known: HashMap<[u8; 32], Part>,
    /// Keys in the order they were recorded, with the serial of that record.
    order: VecDeque<([u8; 32], u64)>,
    recorded: u64,
}

struct Part {
    serial: u64,
    at: Instant,
    lineage: Arc<Lineage>,
}

const MAX_PARTS: usize = 65_536;

static PARTS: LazyLock<Mutex<Parts>> = LazyLock::new(|| {
    Mutex::new(Parts {
        known: HashMap::new(),
        order: VecDeque::new(),
        recorded: 0,
    })
});

fn parts() -> std::sync::MutexGuard<'static, Parts> {
    PARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Record one assistant part. The same text produced again for the same
/// context under another lineage is bound to the join of both: a smaller
/// source set never replaces a larger one.
fn record_part(context: &SealedContext, text: &str, lineage: &Arc<Lineage>) {
    let key = context.part_key(&crate::core::archive::delivered::digest(text));
    let mut parts = parts();
    parts.recorded += 1;
    let serial = parts.recorded;
    let lineage = match parts.known.get(&key) {
        Some(known) if known.at.elapsed() < MAX_AGE && !Arc::ptr_eq(&known.lineage, lineage) => {
            let mut joined = (*known.lineage).clone();
            joined.add(lineage);
            Arc::new(joined)
        }
        _ => Arc::clone(lineage),
    };
    parts.known.insert(
        key,
        Part {
            serial,
            at: Instant::now(),
            lineage,
        },
    );
    parts.order.push_back((key, serial));
    while parts.known.len() > MAX_PARTS || parts.order.len() > 4 * MAX_PARTS {
        let Some((oldest, recorded_as)) = parts.order.pop_front() else {
            break;
        };
        if parts.known.len() > MAX_PARTS
            && parts
                .known
                .get(&oldest)
                .is_some_and(|part| part.serial == recorded_as)
        {
            parts.known.remove(&oldest);
        }
    }
}

/// The lineage on record for an assistant part of this context.
fn assistant_lineage(context: &SealedContext, text_digest: &str) -> Option<Arc<Lineage>> {
    parts()
        .known
        .get(&context.part_key(text_digest))
        .filter(|part| part.at.elapsed() < MAX_AGE)
        .map(|part| Arc::clone(&part.lineage))
}

/// The lineage of a request from its parts that are not host-authored text.
/// The first part whose source is not on record makes it unknown.
fn request_lineage(
    context: &SealedContext,
    request: &SealedRequest<'_>,
    project: Option<&Path>,
) -> Lineage {
    if request.server_state {
        return Lineage::Unknown;
    }
    let mut lineage = Lineage::Complete(None);
    let mut seen = HashSet::new();
    for part in request.parts {
        let source = part.kind == PartKind::Source;
        if !seen.insert((source, part.digest.as_str())) {
            continue;
        }
        if source {
            // Without a project this admission resolved, a source has nothing
            // it could be admitted for: unknown, and the Engine is not asked
            // (its lookup would fall back to a lineage of whatever project).
            let Some(project) = project else {
                return Lineage::Unknown;
            };
            // The digest is the Engine's own (`delivered::digest`), taken by
            // the egress walk on the decoded string value of the block.
            lineage.add(&in_project(project, || Lineage::delivered(&part.digest)));
        } else {
            match assistant_lineage(context, &part.digest) {
                Some(produced) => lineage.add(&produced),
                None => return Lineage::Unknown,
            }
        }
        if lineage.is_unknown() {
            break;
        }
    }
    lineage
}

/// The lineage one sealed item returned in `context` was produced under, or
/// the refusal when it has none that could grant: the same destination must
/// have produced exactly these bytes, for the same project, role and
/// principal, under an authority that is still the current one, and a
/// lineage must have been derived for its producing request. A known origin
/// alone never grants.
fn produced_under(context: &SealedContext, sealed: &str) -> Result<Arc<Lineage>, SealedVerdict> {
    let producers = producers_of(&context.origin, sealed);
    if producers.is_empty() {
        return Err(SealedVerdict::UnknownOrigin);
    }
    let Some(producer) = producers
        .into_iter()
        .find(|producer| producer.context == *context)
    else {
        return Err(SealedVerdict::AuthorityLapsed);
    };
    if producer.authority.verify_current().is_err() {
        return Err(SealedVerdict::AuthorityLapsed);
    }
    producer
        .lineage
        .ok_or(SealedVerdict::SourceAuthorityUnknown)
}

/// Run `operation` in the request scope of `project` — the project the
/// admission of this request resolved, never the process working directory.
fn in_project<T>(project: &Path, operation: impl FnOnce() -> T) -> T {
    crate::core::policy::runtime::REQUEST_PROJECT
        .sync_scope(RefCell::new(Some(project.to_path_buf())), operation)
}

/// Whether every source of `lineage` is known and admitted NOW, for
/// `project` — the project the admission of the returning request resolved —
/// and the current role. Without such a project a lineage that names sources
/// is not checked against anything else: its authority is unknown.
fn lineage_verdict(lineage: &Lineage, project: Option<&Path>) -> SealedVerdict {
    match lineage {
        Lineage::Unknown => SealedVerdict::SourceAuthorityUnknown,
        // Host-authored text only: complete, and nothing to re-admit.
        Lineage::Complete(None) => SealedVerdict::ReturnedToOrigin,
        Lineage::Complete(Some(sources)) => {
            let Some(project) = project else {
                return SealedVerdict::SourceAuthorityUnknown;
            };
            match in_project(project, || sources.still_admitted()) {
                Ok(()) => SealedVerdict::ReturnedToOrigin,
                Err(LineageRefusal::Unknown) => SealedVerdict::SourceAuthorityUnknown,
                Err(LineageRefusal::SourceNotPresented) => SealedVerdict::SourceNotPresented,
                Err(LineageRefusal::SourceNoLongerAdmitted) => SealedVerdict::AuthorityLapsed,
            }
        }
    }
}

/// The answer for the sealed items of a request running in `context`.
///
/// `derive`: the caller-stage admission passes a slot and receives the
/// lineage of this request — from its parts as the host supplied them and
/// from the sealed items it returns. The final-stage admission passes `None`:
/// a body the proxy transformed is not what the Engine delivered, and its
/// lineage is the one the caller stage derived.
///
/// `project`: the root the admission of this request resolved its project
/// policy for, or `None` when it resolved none. Sources are looked up and
/// re-admitted in that project's scope only.
pub(crate) fn resolver(
    context: SealedContext,
    derive: Option<LineageSlot>,
    project: Option<PathBuf>,
) -> Box<dyn Fn(&SealedRequest<'_>) -> Vec<SealedVerdict>> {
    Box::new(move |request| {
        let project = project.as_deref();
        let mut lineage = derive
            .as_ref()
            .map(|_| request_lineage(&context, request, project));
        // Items of one response share their lineage: its sources are
        // re-admitted once per call, not once per item.
        let mut decided: Vec<(Arc<Lineage>, SealedVerdict)> = Vec::new();
        let mut verdicts = Vec::with_capacity(request.items.len());
        for sealed in request.items {
            let verdict = match produced_under(&context, sealed) {
                Ok(produced) => {
                    if let Some(lineage) = &mut lineage {
                        lineage.add(&produced);
                    }
                    let known = decided
                        .iter()
                        .find(|(seen, _)| Arc::ptr_eq(seen, &produced))
                        .map(|(_, verdict)| *verdict);
                    known.unwrap_or_else(|| {
                        let verdict = lineage_verdict(&produced, project);
                        decided.push((produced, verdict));
                        verdict
                    })
                }
                Err(refused) => {
                    lineage = lineage.map(|_| Lineage::Unknown);
                    refused
                }
            };
            verdicts.push(verdict);
        }
        if let Some(slot) = &derive {
            *slot.borrow_mut() = lineage;
        }
        verdicts
    })
}

/// Restores the register's bounds when dropped.
#[cfg(test)]
pub(crate) struct Bounds;

/// Tighten the register's bounds for a test and forget what it holds.
#[cfg(test)]
pub(crate) fn bound_for_test(max_entries: usize, max_age: Duration) -> Bounds {
    let mut register = register();
    register.entries.clear();
    register.max_entries = max_entries;
    register.max_age = max_age;
    forget_parts();
    Bounds
}

/// Forget the sealed items the register holds, for a test that asks what an
/// origin never produced. An earlier test's items stay on record for the
/// address of its upstream, and the system hands that port to a later
/// test's listener: a fresh "another upstream" found the item of a closed
/// one and refused it as `sealed_authority_lapsed` (25.10.). Every test that
/// records the shared test items holds `serial(cache_telemetry)`, as the
/// caller must.
#[cfg(test)]
pub(crate) fn forget_sealed_items_for_test() {
    register().entries.clear();
}

#[cfg(test)]
fn forget_parts() {
    let mut parts = parts();
    parts.known.clear();
    parts.order.clear();
}

#[cfg(test)]
impl Drop for Bounds {
    fn drop(&mut self) {
        let mut register = register();
        register.entries.clear();
        register.max_entries = MAX_ENTRIES;
        register.max_age = MAX_AGE;
        forget_parts();
    }
}

/// Visit every string of `value`, at any depth the recorder is willing to
/// follow.
fn strings(value: &Value, depth: usize, visit: &mut dyn FnMut(&str)) {
    if depth > 64 {
        return;
    }
    match value {
        Value::String(text) => visit(text),
        Value::Array(elements) => {
            for element in elements {
                strings(element, depth + 1, visit);
            }
        }
        Value::Object(members) => {
            for member in members.values() {
                strings(member, depth + 1, visit);
            }
        }
        _ => {}
    }
}

/// Append a streamed piece; what would exceed the bound is dropped, and the
/// shortened text then matches nothing a client returns.
fn append(whole: &mut String, piece: &str) {
    if whole.len() + piece.len() <= MAX_LINE_BYTES {
        whole.push_str(piece);
    }
}

/// One Anthropic content block as a stream delivers it, piece by piece.
#[derive(Default)]
struct StreamedBlock {
    signature: String,
    text: String,
    thinking: String,
    /// The JSON text of a tool call's input.
    input: String,
}

fn kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn text_member<'a>(value: &'a Value, member: &str) -> Option<&'a str> {
    value.get(member).and_then(Value::as_str)
}

/// Visit the elements of `value` when it is an array.
fn each(value: Option<&Value>, visit: &mut dyn FnMut(&Value)) {
    for element in value.and_then(Value::as_array).into_iter().flatten() {
        visit(element);
    }
}

/// Reads a destination's response — whole or as a stream — and records the
/// sealed items it carries, each only in its provider shape:
///
/// - Anthropic: `thinking.signature` and `redacted_thinking.data` content
///   blocks; streamed, the signature arrives as `signature_delta`s and is
///   recorded when its block stops;
/// - OpenAI Responses: `encrypted_content` of `reasoning` and `compaction`
///   output items and `fingerprint` of `program` items, in the final
///   `output` array and in `response.output_item.done` events;
/// - Gemini: `thoughtSignature` of a part beside a `functionCall` or `text`.
///
/// For a request that has a lineage it also records what the response
/// produced as ordinary assistant content — every string of the same content
/// blocks, output items and parts — because a later request that carries it
/// back derives from the same sources. Streamed text is recorded as the
/// whole a client reassembles: an Anthropic block when it stops (its text,
/// its thinking, the strings of its tool input), a Gemini candidate's text
/// per chunk and joined when the response ends.
pub(crate) struct Recorder {
    producer: SealedProducer,
    /// The unfinished line or frame of a streamed response.
    line: Vec<u8>,
    /// Anthropic content blocks in progress, by index.
    blocks: HashMap<u64, StreamedBlock>,
    /// Gemini text across chunks, by candidate and by whether it is thought.
    gemini_text: HashMap<(usize, bool), String>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        for text in std::mem::take(&mut self.gemini_text).into_values() {
            self.part(&text);
        }
    }
}

/// A streamed line longer than this is not an event carrying a sealed item
/// this recorder understands; it is skipped instead of buffered without end.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

impl Recorder {
    pub(crate) fn new(producer: SealedProducer) -> Self {
        Self {
            producer,
            line: Vec::new(),
            blocks: HashMap::new(),
            gemini_text: HashMap::new(),
        }
    }

    /// One string the response produced as assistant content.
    fn part(&self, text: &str) {
        if let Some(lineage) = &self.producer.lineage
            && !text.is_empty()
        {
            record_part(&self.producer.context, text, lineage);
        }
    }

    /// Every string of one content block, output item or part.
    fn parts_of(&self, produced: &Value) {
        if self.producer.lineage.is_some() {
            strings(produced, 0, &mut |text| self.part(text));
        }
    }

    /// A complete, non-streamed response body. A body that does not parse
    /// completely — truncated, failed — records nothing.
    ///
    /// Gemini's `streamGenerateContent` without `alt=sse` answers with one
    /// JSON array of response chunks under `application/json`; the proxy
    /// reads such a response whole (it is not one of its stream content
    /// types), so the array arrives here and each element is a response.
    pub(crate) fn observe_body(&mut self, body: &[u8]) {
        match serde_json::from_slice::<Value>(body) {
            Ok(Value::Array(chunks)) => {
                for chunk in &chunks {
                    self.observe(chunk);
                }
            }
            Ok(document) => self.observe(&document),
            Err(_) => {}
        }
    }

    /// The next chunk of an AWS event-stream response (Bedrock
    /// `invoke-with-response-stream`). Only a complete frame whose prelude
    /// and message checksums hold is read; its payload is either the event
    /// itself or `{"bytes": <base64 of the event>}`. A partial or damaged
    /// frame records nothing.
    pub(crate) fn feed_eventstream(&mut self, chunk: &[u8]) {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        const PRELUDE: usize = 12;
        const OVERHEAD: usize = 16;
        self.line.extend_from_slice(chunk);
        loop {
            if self.line.len() < PRELUDE {
                return;
            }
            let word = |at: usize| {
                u32::from_be_bytes([
                    self.line[at],
                    self.line[at + 1],
                    self.line[at + 2],
                    self.line[at + 3],
                ])
            };
            let total = word(0) as usize;
            let headers = word(4) as usize;
            if !(OVERHEAD..=MAX_LINE_BYTES).contains(&total) || headers > total - OVERHEAD {
                // Not a frame boundary any more: nothing after it can be trusted.
                self.line.clear();
                return;
            }
            if self.line.len() < total {
                return;
            }
            let frame: Vec<u8> = self.line.drain(..total).collect();
            let checksum = |at: usize| {
                u32::from_be_bytes([frame[at], frame[at + 1], frame[at + 2], frame[at + 3]])
            };
            if super::bedrock::crc32(&frame[..8]) != checksum(8)
                || super::bedrock::crc32(&frame[..total - 4]) != checksum(total - 4)
            {
                continue;
            }
            let Ok(payload) = serde_json::from_slice::<Value>(&frame[PRELUDE + headers..total - 4])
            else {
                continue;
            };
            let wrapped = text_member(&payload, "bytes")
                .and_then(|encoded| STANDARD.decode(encoded).ok())
                .and_then(|event| serde_json::from_slice::<Value>(&event).ok());
            self.observe(wrapped.as_ref().unwrap_or(&payload));
        }
    }

    /// The next chunk of a streamed response (SSE `data:` lines or one JSON
    /// document per line).
    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if byte != b'\n' {
                if self.line.len() < MAX_LINE_BYTES {
                    self.line.push(byte);
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let Ok(line) = std::str::from_utf8(&line) else {
                continue;
            };
            let payload = line.strip_prefix("data:").unwrap_or(line).trim();
            if payload.starts_with('{')
                && let Ok(event) = serde_json::from_str::<Value>(payload)
            {
                self.observe(&event);
            }
        }
    }

    fn seal(&self, sealed: Option<&str>) {
        if let Some(sealed) = sealed {
            record(&self.producer, sealed);
        }
    }

    fn anthropic_block(&self, block: &Value) {
        self.parts_of(block);
        match kind(block) {
            Some("thinking") => self.seal(text_member(block, "signature")),
            Some("redacted_thinking") => self.seal(text_member(block, "data")),
            _ => {}
        }
    }

    fn openai_item(&self, item: &Value) {
        self.parts_of(item);
        match kind(item) {
            Some("reasoning" | "compaction") => self.seal(text_member(item, "encrypted_content")),
            Some("program") => self.seal(text_member(item, "fingerprint")),
            _ => {}
        }
    }

    fn gemini_part(&mut self, candidate: usize, part: &Value) {
        self.parts_of(part);
        if self.producer.lineage.is_some()
            && let Some(text) = text_member(part, "text")
        {
            let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
            append(
                self.gemini_text.entry((candidate, thought)).or_default(),
                text,
            );
        }
        if ["functionCall", "function_call", "text"]
            .iter()
            .any(|member| part.get(*member).is_some())
        {
            self.seal(
                text_member(part, "thoughtSignature")
                    .or_else(|| text_member(part, "thought_signature")),
            );
        }
    }

    fn observe(&mut self, document: &Value) {
        // Whole responses.
        each(document.get("content"), &mut |block| {
            self.anthropic_block(block);
        });
        each(document.get("output"), &mut |item| self.openai_item(item));
        let candidates = document.get("candidates").and_then(Value::as_array);
        for (index, candidate) in candidates.into_iter().flatten().enumerate() {
            let parts = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array);
            for part in parts.into_iter().flatten() {
                self.gemini_part(index, part);
            }
        }
        // OpenAI Responses stream events.
        if let Some(item) = document.get("item")
            && kind(document) == Some("response.output_item.done")
        {
            self.openai_item(item);
        }
        each(document.pointer("/response/output"), &mut |item| {
            self.openai_item(item);
        });
        // Anthropic stream events.
        let index = document.get("index").and_then(Value::as_u64);
        match (kind(document), index) {
            (Some("content_block_start"), _) => {
                if let Some(block) = document.get("content_block") {
                    self.anthropic_block(block);
                }
            }
            (Some("content_block_delta"), Some(index)) => {
                if let Some(delta) = document.get("delta") {
                    let block = self.blocks.entry(index).or_default();
                    let piece = |member: &str| text_member(delta, member).unwrap_or_default();
                    match kind(delta) {
                        Some("signature_delta") => append(&mut block.signature, piece("signature")),
                        Some("text_delta") => append(&mut block.text, piece("text")),
                        Some("thinking_delta") => append(&mut block.thinking, piece("thinking")),
                        Some("input_json_delta") => append(&mut block.input, piece("partial_json")),
                        _ => {}
                    }
                }
            }
            (Some("content_block_stop"), Some(index)) => {
                if let Some(block) = self.blocks.remove(&index) {
                    if !block.signature.is_empty() {
                        self.seal(Some(&block.signature));
                    }
                    self.part(&block.text);
                    self.part(&block.thinking);
                    if let Ok(input) = serde_json::from_str::<Value>(&block.input) {
                        self.parts_of(&input);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_admission::egress::SealedPart;

    fn producer(origin: &str) -> SealedProducer {
        let target = EgressTarget {
            provider: "Anthropic",
            model: None,
            upstream_base: origin,
        };
        let binding = TrustedDestinationBinding::default();
        let admission = crate::proxy::admitted::admit(
            crate::proxy::admitted::Payload::Opaque(0),
            &target,
            binding,
        )
        .expect("an empty payload is admitted");
        SealedProducer {
            context: SealedContext::new(&target, binding, None, None),
            authority: Arc::new(admission.into_authority()),
            // Produced for a request of host-authored text only.
            lineage: Some(Arc::new(Lineage::Complete(None))),
        }
    }

    /// The verdict on one sealed item returned in `context`.
    fn asks(context: &SealedContext) -> impl Fn(&str) -> SealedVerdict {
        let resolve = resolver(context.clone(), None, None);
        move |sealed| {
            resolve(&SealedRequest {
                items: &[sealed.to_owned()],
                parts: &[],
                server_state: false,
            })[0]
        }
    }

    /// The lineage the caller stage derives for a request with these parts.
    fn derived(context: &SealedContext, parts: &[(PartKind, &str)], server_state: bool) -> Lineage {
        derived_in(context, parts, server_state, None)
    }

    fn derived_in(
        context: &SealedContext,
        parts: &[(PartKind, &str)],
        server_state: bool,
        project: Option<&Path>,
    ) -> Lineage {
        let parts: Vec<SealedPart> = parts
            .iter()
            .map(|(kind, text)| SealedPart {
                kind: *kind,
                digest: crate::core::archive::delivered::digest(text),
            })
            .collect();
        let slot: LineageSlot = Rc::new(RefCell::new(None));
        let project = project.map(Path::to_path_buf);
        let resolve = resolver(context.clone(), Some(Rc::clone(&slot)), project);
        resolve(&SealedRequest {
            items: &[],
            parts: &parts,
            server_state,
        });
        slot.take().expect("the caller stage derives a lineage")
    }

    /// A known origin is not a grant: an item whose producing request has no
    /// lineage, or an unknown one, is refused as "source authority unknown";
    /// only a complete lineage lets it return — here one without any source.
    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn known_origin_without_a_complete_lineage_never_grants() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _bounds = bound_for_test(64, Duration::from_mins(1));
        let complete = producer("http://127.0.0.1:9");
        let verdict = asks(&complete.context);
        for (lineage, sealed, expected) in [
            (None, "no-lineage", SealedVerdict::SourceAuthorityUnknown),
            (
                Some(Lineage::Unknown),
                "unknown-lineage",
                SealedVerdict::SourceAuthorityUnknown,
            ),
            (
                Some(Lineage::Complete(None)),
                "host-text-only",
                SealedVerdict::ReturnedToOrigin,
            ),
        ] {
            let producer = SealedProducer {
                lineage: lineage.map(Arc::new),
                ..complete.clone()
            };
            record(&producer, sealed);
            assert_eq!(verdict(sealed), expected, "{sealed}");
        }
        assert_eq!(verdict("never-recorded"), SealedVerdict::UnknownOrigin);
    }

    /// The lineage of a request: host-authored text contributes nothing; an
    /// assistant part counts only when a recorded response of this context
    /// produced it, and then with that response's lineage; a tool result the
    /// Engine has no record of, and a reference to provider-held state, make
    /// it unknown. A sealed item returned with it joins its own lineage.
    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn request_lineage_is_complete_only_when_every_part_is_bound() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _bounds = bound_for_test(64, Duration::from_mins(1));
        let producer = producer("http://127.0.0.1:9");
        let context = &producer.context;
        engine::reset();
        let no_sources = |lineage: Lineage| matches!(lineage, Lineage::Complete(None));
        assert!(no_sources(derived(context, &[], false)));
        assert!(derived(context, &[], true).is_unknown());
        assert!(
            derived(context, &[(PartKind::Assistant, "the answer")], false).is_unknown(),
            "no recorded response produced this text"
        );
        assert!(
            derived(context, &[(PartKind::Source, "tool output")], false).is_unknown(),
            "the Engine has no record of this text"
        );
        // [stand-in] A text the Engine delivered binds its sources — but only
        // for an admission that resolved a project; without one the Engine
        // is not even asked.
        engine::deliver("tool output", engine::Sources::presented("object://unit"));
        let root = tempfile::tempdir().expect("project");
        let bound = [(PartKind::Source, "tool output")];
        assert!(matches!(
            derived_in(context, &bound, false, Some(root.path())),
            Lineage::Complete(Some(_))
        ));
        assert!(derived(context, &bound, false).is_unknown());
        engine::reset();

        // A whole response and a streamed one produce assistant parts.
        let mut whole = Recorder::new(producer.clone());
        whole.observe_body(
            br#"{"content":[{"type":"text","text":"the answer"},
                {"type":"tool_use","id":"t1","name":"read","input":{"path":"src/lib.rs"}}]}"#,
        );
        let mut streamed = Recorder::new(producer.clone());
        for event in [
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"streamed "}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"src/"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"main.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
        ] {
            streamed.feed(format!("data: {event}\n\n").as_bytes());
        }
        for produced in ["the answer", "src/lib.rs", "streamed answer", "src/main.rs"] {
            let lineage = derived(context, &[(PartKind::Assistant, produced)], false);
            assert!(no_sources(lineage), "{produced}");
        }
        assert!(
            derived(context, &[(PartKind::Assistant, "streamed ")], false).is_unknown(),
            "a piece of a streamed text is not what a client returns"
        );
        let elsewhere = self::producer("http://127.0.0.1:10");
        let foreign = derived(
            &elsewhere.context,
            &[(PartKind::Assistant, "the answer")],
            false,
        );
        assert!(foreign.is_unknown(), "produced for another context");

        // The same text produced again under an unknown lineage is bound to
        // the join of both: the weaker record never wins.
        let unbound = SealedProducer {
            lineage: Some(Arc::new(Lineage::Unknown)),
            ..producer.clone()
        };
        Recorder::new(unbound.clone())
            .observe_body(br#"{"content":[{"type":"text","text":"the answer"}]}"#);
        assert!(derived(context, &[(PartKind::Assistant, "the answer")], false).is_unknown());

        // A returned sealed item contributes the lineage it was produced under.
        record(&unbound, "sealed-under-unknown");
        let slot: LineageSlot = Rc::new(RefCell::new(None));
        let verdicts = resolver(context.clone(), Some(Rc::clone(&slot)), None)(&SealedRequest {
            items: &["sealed-under-unknown".to_owned()],
            parts: &[],
            server_state: false,
        });
        assert_eq!(verdicts, [SealedVerdict::SourceAuthorityUnknown]);
        assert!(matches!(slot.take(), Some(Lineage::Unknown)));
    }

    /// The recorder takes sealed items only from their provider shapes, also
    /// when a stream delivers them in pieces, and a namesake elsewhere in a
    /// response is not recorded.
    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn sealed_items_are_recorded_from_provider_shapes_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _bounds = bound_for_test(64, Duration::from_mins(1));
        let producer = producer("http://127.0.0.1:9");
        let verdict = asks(&producer.context);

        let mut whole = Recorder::new(producer.clone());
        whole.observe_body(
            br#"{"content":[{"type":"thinking","thinking":"t","signature":"anthropic-sig"},
                {"type":"redacted_thinking","data":"anthropic-data"},
                {"type":"text","text":"x","signature":"namesake"}],
                "output":[{"type":"reasoning","encrypted_content":"openai-reasoning"},
                {"type":"compaction","encrypted_content":"openai-compaction"},
                {"type":"program","fingerprint":"openai-program"},
                {"type":"message","encrypted_content":"namesake-item"}],
                "candidates":[{"content":{"parts":[
                {"functionCall":{"name":"f","args":{}},"thoughtSignature":"gemini-sig"},
                {"inlineData":{},"thoughtSignature":"namesake-part"}]}}]}"#,
        );
        let mut streamed = Recorder::new(producer.clone());
        for chunk in [
            &b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,"[..],
            b"\"delta\":{\"type\":\"signature_delta\",\"signature\":\"stream-\"}}\n\n",
            b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n",
            b"data: {\"type\":\"content_block_stop\",\"index\":0}\n",
            b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"encrypted_content\":\"stream-reasoning\"}}\n",
        ] {
            streamed.feed(chunk);
        }

        for recorded in [
            "anthropic-sig",
            "anthropic-data",
            "openai-reasoning",
            "openai-compaction",
            "openai-program",
            "gemini-sig",
            "stream-sig",
            "stream-reasoning",
        ] {
            assert_eq!(
                verdict(recorded),
                SealedVerdict::ReturnedToOrigin,
                "{recorded}"
            );
        }
        for unknown in [
            "namesake",
            "namesake-item",
            "namesake-part",
            "stream-",
            "sig",
        ] {
            assert_eq!(verdict(unknown), SealedVerdict::UnknownOrigin, "{unknown}");
        }
    }

    /// One AWS event-stream frame without headers around `payload`.
    fn frame(payload: &[u8]) -> Vec<u8> {
        let total = u32::try_from(payload.len() + 16).unwrap();
        let mut frame = total.to_be_bytes().to_vec();
        frame.extend_from_slice(&0_u32.to_be_bytes());
        let prelude = crate::proxy::bedrock::crc32(&frame);
        frame.extend_from_slice(&prelude.to_be_bytes());
        frame.extend_from_slice(payload);
        let message = crate::proxy::bedrock::crc32(&frame);
        frame.extend_from_slice(&message.to_be_bytes());
        frame
    }

    fn bedrock_event(event: &Value) -> Vec<u8> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let wrapped = serde_json::json!({ "bytes": STANDARD.encode(event.to_string()) });
        frame(wrapped.to_string().as_bytes())
    }

    /// Bedrock streams Anthropic events inside AWS event-stream frames: the
    /// signature of a thinking block is recorded once its block stopped, for
    /// this destination only. A frame that is incomplete or fails its
    /// checksum records nothing; a changed byte is another item.
    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn bedrock_eventstream_frames_record_sealed_items_only_when_complete() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _bounds = bound_for_test(64, Duration::from_mins(1));
        let producer = producer("http://127.0.0.1:9");
        let verdict = asks(&producer.context);
        let other = self::producer("http://127.0.0.1:10");
        let elsewhere = asks(&other.context);
        let delta = |signature: &str| {
            bedrock_event(&serde_json::json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "signature_delta", "signature": signature}
            }))
        };
        let stop = bedrock_event(&serde_json::json!({"type": "content_block_stop", "index": 0}));

        let mut stream = delta("bedrock-sig");
        stream.extend_from_slice(&stop);
        let mut recorder = Recorder::new(producer.clone());
        // Delivered in two pieces that split a frame.
        let (first, second) = stream.split_at(stream.len() / 3);
        recorder.feed_eventstream(first);
        assert_eq!(verdict("bedrock-sig"), SealedVerdict::UnknownOrigin);
        recorder.feed_eventstream(second);
        assert_eq!(verdict("bedrock-sig"), SealedVerdict::ReturnedToOrigin);
        assert_eq!(verdict("bedrock-sih"), SealedVerdict::UnknownOrigin);
        assert_eq!(elsewhere("bedrock-sig"), SealedVerdict::UnknownOrigin);

        // The stream ends inside the frame that would complete the item.
        let mut truncated = Recorder::new(producer.clone());
        truncated.feed_eventstream(&delta("cut-short"));
        truncated.feed_eventstream(&stop[..stop.len() - 3]);
        assert_eq!(verdict("cut-short"), SealedVerdict::UnknownOrigin);

        // A frame whose checksum does not hold is not read.
        let mut damaged_frame = delta("damaged");
        let middle = damaged_frame.len() / 2;
        damaged_frame[middle] ^= 0x01;
        let mut damaged = Recorder::new(producer.clone());
        damaged.feed_eventstream(&damaged_frame);
        damaged.feed_eventstream(&stop);
        assert_eq!(verdict("damaged"), SealedVerdict::UnknownOrigin);
    }

    /// Gemini's array-shaped stream response is one JSON array of chunks; a
    /// truncated array records nothing.
    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn gemini_array_response_records_sealed_items_only_when_complete() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let _bounds = bound_for_test(64, Duration::from_mins(1));
        let producer = producer("http://127.0.0.1:9");
        let verdict = asks(&producer.context);
        let chunks = |signature: &str| {
            serde_json::json!([
                {"candidates": [{"content": {"role": "model", "parts": [{"text": "thinking"}]}}]},
                {"candidates": [{"content": {"role": "model", "parts": [
                    {"functionCall": {"name": "lookup", "args": {}}, "thoughtSignature": signature}
                ]}}]}
            ])
            .to_string()
        };
        let mut recorder = Recorder::new(producer.clone());
        let whole = chunks("gemini-array-sig");
        let cut = chunks("gemini-cut-sig");
        recorder.observe_body(&cut.as_bytes()[..cut.len() - 4]);
        recorder.observe_body(whole.as_bytes());
        assert_eq!(verdict("gemini-array-sig"), SealedVerdict::ReturnedToOrigin);
        assert_eq!(verdict("gemini-cut-sig"), SealedVerdict::UnknownOrigin);
    }
}
