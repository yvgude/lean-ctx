// SPDX-License-Identifier: Apache-2.0
//! Request-scoped collection of admission decisions and the one Decision
//! Receipt per tool call built from them (G4).
//!
//! The MCP dispatcher opens a capture around each call; the read choke point
//! appends one content-free decision per admitted object, including from the
//! detached read workers (`TaskSpine::spawn_thread` carries the capture). When
//! the call's final result exists, `finish` turns the decisions into a
//! validated `ContextDecisionReceiptV1` whose digest binds exactly the bytes
//! that were returned. Nothing here holds or persists content.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDecisionV1, ContextDispositionV1, DeliveryOutcomeV1,
    DestinationLocalityV1, DestinationV1, GatewayModeV1, MAX_DECISIONS, PolicyRefV1,
    PrincipalKindV1, PrincipalV1, RECEIPT_V2_SCHEMA_VERSION, ReasonCodeV1, SecurityCountsV1,
    SourceCountsV1, TokenAccountV1,
};
use lean_ctx_protocol::{PolicyId, ProtocolReference, Sha256Digest, TaskId};

use super::AdmissionCounts;

tokio::task_local! {
    pub static ADMISSIONS: Option<Arc<AdmissionCapture>>;
}

/// The capture of the call running on this task or worker, if any.
pub fn current() -> Option<Arc<AdmissionCapture>> {
    ADMISSIONS.try_with(Clone::clone).ok().flatten()
}

#[derive(Debug, Clone)]
struct Recorded {
    decision: ContextDecisionV1,
    counts: AdmissionCounts,
    /// The object's class has an external or unknown origin floor: it is
    /// never clean for displacement, so it stays itemized (pz21 §1.5 item 2).
    external: bool,
}

/// The reason a receipt itemizes an object of external or unknown origin
/// with (pz21 §1.5 item 2). Content-free.
pub const UNTRUSTED_ORIGIN: &str = "origin.untrusted";

/// Most distinct origins one call's ledger records; one more sets overflow.
pub const MAX_LEDGER_REFS: usize = 256;

/// The origin ledger of one call (pz21 §1.5): every admission whose class is
/// external or unknown, independent of the capped decision records, so the
/// `MAX_DECISIONS` cap can never displace it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginLedger {
    /// Every external or unknown admission, saturating.
    pub count: u64,
    /// The distinct origins, bounded by [`MAX_LEDGER_REFS`].
    pub refs: std::collections::BTreeSet<super::origin::InputRef>,
    /// A ref too long to record, or the set full.
    pub overflow: bool,
}

impl OriginLedger {
    fn note(&mut self, input: Option<super::origin::InputRef>) {
        self.count = self.count.saturating_add(1);
        match input {
            Some(input) if self.refs.contains(&input) => {}
            Some(input) if self.refs.len() < MAX_LEDGER_REFS => {
                self.refs.insert(input);
            }
            _ => self.overflow = true,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.count = self.count.saturating_add(other.count);
        for input in &other.refs {
            if !self.refs.contains(input) {
                if self.refs.len() < MAX_LEDGER_REFS {
                    self.refs.insert(input.clone());
                } else {
                    self.overflow = true;
                }
            }
        }
        self.overflow |= other.overflow;
    }

    /// Nothing external or unknown was admitted.
    pub fn is_empty(&self) -> bool {
        self.count == 0 && !self.overflow
    }
}

/// Process-wide count of external or unknown admissions that found neither a
/// capture nor an `outside_call` scope (pz21 §1.5 item 4b). Every call open
/// while it moves is released as if its ledger overflowed.
pub static ORPHAN_EXTERNAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    /// Depth of `outside_call` scopes on this thread.
    static OUTSIDE_CALL: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Depth of `external_span` scopes on this thread.
    static IN_SPAN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Depth of `egress_check` scopes on this thread.
    static EGRESS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether the calling thread runs inside [`outside_call`].
pub fn outside_call_active() -> bool {
    OUTSIDE_CALL.with(std::cell::Cell::get) > 0
}

/// Whether the calling thread runs an egress-equality check
/// ([`egress_check`]): its text is compared, never delivered or stored, so
/// its injection action is never raised (NB-2).
pub fn egress_check_active() -> bool {
    EGRESS.with(std::cell::Cell::get) > 0
}

/// Run `check` as an egress-equality check: outside the call
/// ([`outside_call`]) and with the injection action left as configured.
pub fn egress_check<T>(check: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            EGRESS.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    EGRESS.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _leave = Leave;
    outside_call(check)
}

/// Run `operation` as an admission that is outside any tool call by design
/// (egress checks, detached store writes, background index builds). Inside
/// it an admission writes no ledger entry, raises no orphan mark and never
/// taints. Its text reaches an agent only through a reader that admits it
/// again at serve time, inside the call.
pub fn outside_call<T>(operation: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            OUTSIDE_CALL.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    OUTSIDE_CALL.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _leave = Leave;
    operation()
}

/// Note that an admission of `class` happened (pz21 §1.5 item 1). Called
/// first by every entry point that takes or derives a class, ahead of the
/// config refusal, the gateway-off return, the clean memo and the
/// detectors. A `Project` class notes nothing.
pub fn note_origin(class: &super::origin::SourceClass) {
    if !class.is_external_or_unknown() || outside_call_active() {
        return;
    }
    let input = class.input_ref();
    match current() {
        Some(capture) => capture.note_origin_ref(input),
        None => {
            ORPHAN_EXTERNAL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// The capture and the `outside_call` marker of the calling thread, for a
/// worker that admits on the call's behalf (`rayon`, `std::thread`).
#[derive(Clone)]
pub struct Carried {
    capture: Option<Arc<AdmissionCapture>>,
    outside: bool,
    project: Option<std::path::PathBuf>,
}

/// Take what a worker needs to admit as part of this call (pz21 §1.5 4a).
pub fn carry() -> Carried {
    Carried {
        capture: current(),
        outside: outside_call_active(),
        project: crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten(),
    }
}

impl Carried {
    /// Run `operation` on this worker as part of the carrying call.
    pub fn enter<T>(&self, operation: impl FnOnce() -> T) -> T {
        let inner = || {
            if self.outside {
                outside_call(operation)
            } else {
                operation()
            }
        };
        let with_project = || match &self.project {
            Some(project) if crate::core::policy::diagnostics::request_project().is_none() => {
                crate::core::policy::runtime::in_project(project.clone(), inner)
            }
            _ => inner(),
        };
        if self.capture.is_some() && current().is_none() {
            ADMISSIONS.sync_scope(self.capture.clone(), with_project)
        } else {
            with_project()
        }
    }
}

/// Admit text that a tool delivers as one marked part of a block (a hint
/// line quoting an issue title, pz21 §1.6). Its origins go into the call's
/// span list instead of the ledger, and the release point fences exactly
/// that part when it finds it in a text block; otherwise it fences every
/// block whole.
pub fn external_span(
    class: &super::origin::SourceClass,
    admit: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let Some(capture) =
        current().filter(|_| class.is_external_or_unknown() && !outside_call_active())
    else {
        return admit();
    };
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            IN_SPAN.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    IN_SPAN.with(|depth| depth.set(depth.get().saturating_add(1)));
    let admitted = {
        let _leave = Leave;
        admit()
    };
    if let Some(text) = &admitted
        && !text.is_empty()
    {
        capture
            .spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((class.input_ref(), text.clone()));
    }
    admitted
}

fn in_span() -> bool {
    IN_SPAN.with(std::cell::Cell::get) > 0
}

/// The policy snapshot of ONE call (G08), taken once at its entry — where the
/// MCP dispatcher opens the call's capture, where a CLI run starts — and
/// carried by the capture: the gateway rules, whether protection is active
/// for the request's project, and the active policy pack. While the capture
/// is open, `carrier::current_policy` returns these rules, so the release
/// point, the receipt duty, the trace sidecar and the stored-copy checks of
/// the call decide under the rules of its entry, whatever is configured by
/// the time they run.
#[derive(Debug, Clone)]
pub struct CallSnapshot {
    admission: super::stores::StoreAdmission,
    protected: bool,
    pack: Option<(String, String)>,
    /// The organization decision the verifier stated in the verification the
    /// pack above comes from; `None` when no verifier was asked.
    org_policy: Option<OrgObservation>,
}

/// See [`crate::core::policy::org::Observation`].
pub(crate) type OrgObservation = crate::core::policy::org::Observation;

/// The organization decision a capture's admissions were made under.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OrgEvidence {
    /// No verifier decision was observed.
    #[default]
    None,
    /// Every admission of the capture rests on this one observation.
    One(OrgObservation),
    /// Admissions that were merged into one capture rest on different
    /// observations (or one of them on none). No single organization decision
    /// can be named for the delivery, and none is: the record says so instead
    /// of naming the first or the last.
    Diverged,
    /// The admission ran where no verified view was held to take an
    /// observation from (an opaque body admitted outside a source view). It
    /// did not ask a verifier of its own; the record says that no
    /// organization decision is available for it — not that there is none.
    Unavailable,
}

impl OrgEvidence {
    fn of(observation: Option<OrgObservation>) -> Self {
        observation.map_or(Self::None, Self::One)
    }
}

impl CallSnapshot {
    /// Take the snapshot now. Call inside the request scope: protection and
    /// the pack are those of the request's project. Inside an open capture
    /// that carries a snapshot (a run that is part of a call) the gateway
    /// rules are that call's.
    pub fn take() -> Self {
        let admission = super::carrier::gateway_snapshot();
        let protected =
            super::stored::source_authority_required_under(admission.admission_policy());
        // One read: the pack and the organization observation are those of
        // the same verified policy.
        let (active, org_policy) = crate::core::policy::runtime::active_observed();
        let pack = active.map(|active| {
            (
                active.resolved.name.clone(),
                active.resolved.version.clone(),
            )
        });
        Self {
            admission,
            protected,
            pack,
            org_policy,
        }
    }

    /// The gateway rules of the call.
    pub fn admission(&self) -> &super::stores::StoreAdmission {
        &self.admission
    }

    /// Protection was active at the call's entry (a project policy pack, the
    /// gateway in governed or sovereign mode, an unloadable configuration).
    pub(crate) fn protected(&self) -> bool {
        self.protected
    }

    /// The organization decision observed with the pack at the call's entry.
    pub(crate) fn org_policy(&self) -> OrgEvidence {
        OrgEvidence::of(self.org_policy.clone())
    }

    /// Name and version of the policy pack active at the call's entry.
    pub(crate) fn pack(&self) -> Option<&(String, String)> {
        self.pack.as_ref()
    }
}

/// Source references one capture holds for its trace sidecar.
pub(crate) const MAX_SOURCE_REFERENCES: usize = 64;
/// Longest source reference recorded; a longer one is counted, not recorded.
pub(crate) const MAX_SOURCE_REFERENCE_BYTES: usize = 256;

/// The name a reader showed for a source it delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceName {
    /// A project file: its path relative to the project.
    Path(String),
    /// Not a project file: the id it has at its origin (the path in a
    /// remote repository, a provider's source id).
    Id(String),
}

/// What the admitted readers named of the sources this call admitted.
#[derive(Debug, Default)]
struct SourceNotes {
    /// Object digest of an itemized decision → the name its reader
    /// delivered it under.
    named: Vec<(Sha256Digest, SourceName)>,
    /// References that were not recorded (bound reached, not expressible).
    omitted: u32,
}

thread_local! {
    /// The delivering source decision this thread recorded last and no reader
    /// has named yet: the capture it belongs to and the decision's object.
    /// An admitted reader admits a source and publishes it on one thread, in
    /// that order; [`note_source`] takes the entry, so one decision is named
    /// at most once and a publication without a decision names nothing.
    static UNNAMED: std::cell::RefCell<Option<(usize, Sha256Digest)>> =
        const { std::cell::RefCell::new(None) };
}

/// Name the source the calling thread admitted last (G08): the reference the
/// reader shows for it — its path relative to `project`. Called by the
/// admitted readers' publication (`core::archive::authority::publish_read`),
/// i.e. only for a source that was delivered: a withheld source is never
/// named here, and the sidecar names it as withheld, as the reader does. A
/// path outside the project, not UTF-8 or longer than the bound is counted,
/// not recorded. No capture: nothing happens.
pub(crate) fn note_source(project: &std::path::Path, canonical_path: &str) {
    let Some((capture, object)) = unnamed_decision() else {
        return;
    };
    let path = std::path::Path::new(canonical_path);
    let relative = path
        .strip_prefix(project)
        .ok()
        .map(std::path::Path::to_path_buf)
        .or_else(|| {
            let project = crate::core::pathutil::canonicalize_secure(project).ok()?;
            path.strip_prefix(&project)
                .ok()
                .map(std::path::Path::to_path_buf)
        });
    let reference = relative
        .as_deref()
        .and_then(std::path::Path::to_str)
        .filter(|reference| !reference.is_empty() && reference.len() <= MAX_SOURCE_REFERENCE_BYTES)
        .map(|reference| SourceName::Path(reference.replace('\\', "/")));
    capture.name_source(object, reference);
}

/// `note_source` for a source that is not a project file (G08): `id` is
/// what its reader shows for it — the path it has at its origin, a
/// provider's source id. Called by the reader right after it admitted the
/// source, on the same thread, and only for a source it delivers. The same
/// bounds apply; an empty or over-long id is counted, not recorded. The
/// sidecar keeps it only if it passes the release check as a word.
pub fn note_source_id(id: &str) {
    let Some((capture, object)) = unnamed_decision() else {
        return;
    };
    let reference = (!id.is_empty() && id.len() <= MAX_SOURCE_REFERENCE_BYTES)
        .then(|| SourceName::Id(id.to_owned()));
    capture.name_source(object, reference);
}

/// The open capture and the object of the delivering decision the calling
/// thread recorded into it last and that no reader has named yet. Taking it
/// spends it.
fn unnamed_decision() -> Option<(Arc<AdmissionCapture>, Sha256Digest)> {
    let capture = current()?;
    let owner = std::ptr::from_ref::<AdmissionCapture>(&capture).addr();
    let (recorded_by, object) = UNNAMED.with(|slot| slot.borrow_mut().take())?;
    (recorded_by == owner).then_some((capture, object))
}

/// Everything one call contributes to its receipt.
#[derive(Debug)]
pub struct AdmissionCapture {
    started: Instant,
    records: Mutex<Vec<Recorded>>,
    /// Decisions beyond `MAX_DECISIONS` are counted in full, never dropped.
    overflow: Mutex<Overflow>,
    /// Original tokens the tool measured for its sources (pre-optimization).
    original_tokens: Mutex<Option<u64>>,
    policy: Mutex<Option<(GatewayModeV1, Sha256Digest)>>,
    /// The task the handler ran under and its tenant/project scope, bound
    /// where admission happens.
    task: Mutex<Option<(String, String)>>,
    /// The call's policy snapshot, when the capture was opened for a call
    /// ([`Self::for_call`]). A proxy egress capture has none: its admission
    /// holds its own snapshot.
    snapshot: Option<CallSnapshot>,
    /// An admission of this call was decided under other gateway rules than
    /// the call's snapshot.
    diverged: std::sync::atomic::AtomicBool,
    sources: Mutex<SourceNotes>,
    /// Name and version of the policy pack an egress admission resolved.
    pack: Mutex<Option<(String, String)>>,
    /// The organization decision an egress admission's own snapshot observed
    /// (`None`: nothing noted yet).
    org_policy: Mutex<Option<OrgEvidence>>,
    /// The origin ledger: never displaced by the record cap (pz21 §1.5).
    origin: Mutex<OriginLedger>,
    /// Marked parts of a block: external text a tool placed itself.
    spans: Mutex<Vec<(Option<super::origin::InputRef>, String)>>,
    /// [`ORPHAN_EXTERNAL`] when this capture was opened.
    orphan_mark: u64,
    /// The class of this call's command output, when the shell tool
    /// classified it as `tool_output` (pz21 §1.8).
    output_class: Mutex<Option<super::origin::SourceClass>>,
    /// Digests of the text blocks the release point's origin step delivered
    /// for this call: a block counts as already fenced only when it is one
    /// of these, never because its text looks like a fence (outside text can
    /// spell a fence header and closing line itself).
    released: Mutex<std::collections::BTreeSet<[u8; 32]>>,
}

/// Most released blocks one capture remembers; past it a block released
/// again is fenced again (fails closed: a double fence, never none).
const MAX_RELEASED_BLOCKS: usize = 4096;

impl Default for AdmissionCapture {
    fn default() -> Self {
        Self {
            released: Mutex::new(std::collections::BTreeSet::new()),
            origin: Mutex::new(OriginLedger::default()),
            spans: Mutex::new(Vec::new()),
            orphan_mark: ORPHAN_EXTERNAL.load(std::sync::atomic::Ordering::SeqCst),
            output_class: Mutex::new(None),
            started: Instant::now(),
            records: Mutex::new(Vec::new()),
            overflow: Mutex::new(Overflow::default()),
            original_tokens: Mutex::new(None),
            policy: Mutex::new(None),
            task: Mutex::new(None),
            snapshot: None,
            diverged: std::sync::atomic::AtomicBool::new(false),
            sources: Mutex::new(SourceNotes::default()),
            pack: Mutex::new(None),
            org_policy: Mutex::new(None),
        }
    }
}

impl AdmissionCapture {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The capture of one call, carrying the policy snapshot taken at the
    /// call's entry.
    pub fn for_call(snapshot: CallSnapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Some(snapshot),
            ..Self::default()
        })
    }

    /// The call's policy snapshot, when this capture was opened for a call.
    pub fn snapshot(&self) -> Option<&CallSnapshot> {
        self.snapshot.as_ref()
    }

    /// The shell tool classified this call's command output (pz21 §1.8):
    /// every command output it admits takes this class.
    pub fn set_output_class(&self, class: super::origin::SourceClass) {
        *self
            .output_class
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(class);
    }

    /// The class of this call's command output; `None`: the project's own.
    pub fn output_class(&self) -> Option<super::origin::SourceClass> {
        self.output_class
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn note_origin_ref(&self, input: Option<super::origin::InputRef>) {
        if in_span() {
            return;
        }
        self.origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .note(input);
    }

    /// The call's origin ledger. An orphan mark raised since the capture
    /// opened reads as overflow (pz21 §1.5 item 4b).
    pub fn origin_ledger(&self) -> OriginLedger {
        let mut ledger = self
            .origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if self.orphan_external() {
            ledger.overflow = true;
        }
        ledger
    }

    /// An external or unknown admission without capture or `outside_call`
    /// happened somewhere in the process while this call was open.
    pub fn orphan_external(&self) -> bool {
        ORPHAN_EXTERNAL.load(std::sync::atomic::Ordering::SeqCst) != self.orphan_mark
    }

    /// The marked external parts tools placed in their blocks.
    pub fn external_spans(&self) -> Vec<(Option<super::origin::InputRef>, String)> {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Record that the origin step delivered `text` as a block of this call.
    pub fn mark_released(&self, text: &str) {
        let mut released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
        if released.len() < MAX_RELEASED_BLOCKS {
            released.insert(*blake3::hash(text.as_bytes()).as_bytes());
        }
    }

    /// Whether the origin step already delivered exactly `text` for this
    /// call (a result released twice).
    pub fn was_released(&self, text: &str) -> bool {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(blake3::hash(text.as_bytes()).as_bytes())
    }

    /// Every outside origin this call's output carries so far: the ledger
    /// with the marked parts folded in. What a reused output (idempotent
    /// replay) must bring into the call that serves it again.
    pub fn output_origins(&self) -> OriginLedger {
        let mut ledger = self.origin_ledger();
        for (input, _) in self.external_spans() {
            ledger.note(input);
        }
        ledger
    }

    /// Note the origins of an output produced by an earlier call and served
    /// again by this one (idempotent replay): its release fences and taints
    /// as the first release did.
    pub fn renote_origins(&self, origins: &OriginLedger) {
        self.origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(origins);
    }

    /// Fold the marked parts into the ledger: the tool's placement did not
    /// survive to the release point, so every block is fenced whole.
    pub fn spans_into_ledger(&self) {
        let spans = std::mem::take(&mut *self.spans.lock().unwrap_or_else(PoisonError::into_inner));
        let mut ledger = self.origin.lock().unwrap_or_else(PoisonError::into_inner);
        for (input, _) in spans {
            ledger.note(input);
        }
    }

    /// Whether an admission recorded here was decided under other gateway
    /// rules than the call's snapshot. Always `false` without a snapshot.
    pub(crate) fn policy_diverged(&self) -> bool {
        self.diverged.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The references the admitted readers named, by the object digest of
    /// the decision each belongs to, and how many were not recorded.
    pub fn source_references(&self) -> (Vec<(Sha256Digest, SourceName)>, u32) {
        let sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        (sources.named.clone(), sources.omitted)
    }

    /// Record the name of the decision about `object`, or count it as not
    /// recorded (`None`, or the bound is reached).
    fn name_source(&self, object: Sha256Digest, name: Option<SourceName>) {
        let mut sources = self.sources.lock().unwrap_or_else(PoisonError::into_inner);
        match name {
            Some(name) if sources.named.len() < MAX_SOURCE_REFERENCES => {
                sources.named.push((object, name));
            }
            _ => sources.omitted = sources.omitted.saturating_add(1),
        }
    }

    /// The name of the policy pack the egress admission of this capture
    /// resolved (proxy), for its trace sidecar.
    pub(super) fn note_pack(&self, name: &str, version: &str) {
        *self.pack.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((name.to_owned(), version.to_owned()));
    }

    /// The organization decision the egress admission of this capture
    /// observed in its own snapshot — noted once per admission, also when it
    /// observed none. A second note that differs makes the evidence
    /// [`OrgEvidence::Diverged`]; it never replaces the first.
    pub(super) fn note_org_policy(&self, observation: Option<OrgObservation>) {
        self.merge_org_policy(OrgEvidence::of(observation));
    }

    /// The egress admission of this capture held no verified view to take an
    /// observation from, and asked for none.
    pub(super) fn note_org_policy_unavailable(&self) {
        self.merge_org_policy(OrgEvidence::Unavailable);
    }

    fn merge_org_policy(&self, incoming: OrgEvidence) {
        let mut noted = self
            .org_policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *noted = Some(match noted.take() {
            None => incoming,
            Some(present) if present == incoming => present,
            Some(_) => OrgEvidence::Diverged,
        });
    }

    /// The organization decision of the capture: the call snapshot's, or
    /// what its egress admissions noted.
    pub(crate) fn org_policy(&self) -> OrgEvidence {
        match &self.snapshot {
            Some(snapshot) => OrgEvidence::of(snapshot.org_policy.clone()),
            None => self
                .org_policy
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .unwrap_or_default(),
        }
    }

    /// The pack of the capture: the call snapshot's, or the one an egress
    /// admission noted.
    pub(crate) fn pack(&self) -> Option<(String, String)> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.pack.clone())
            .or_else(|| {
                self.pack
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
    }

    pub(super) fn record(&self, decision: &ContextDecisionV1, counts: AdmissionCounts) {
        self.record_as(decision, counts, false);
    }

    /// [`Self::record`] for an object whose class is `external` or unknown:
    /// such a decision is never clean for displacement (pz21 §1.5 item 2).
    pub(super) fn record_as(
        &self,
        decision: &ContextDecisionV1,
        counts: AdmissionCounts,
        external: bool,
    ) {
        if let Some(envelope) = crate::core::task_spine::TaskSpine::current() {
            let mut task = self.task.lock().unwrap_or_else(PoisonError::into_inner);
            if task.is_none() {
                *task = Some((
                    envelope.task_id.as_str().to_owned(),
                    crate::core::context_store::task_scope(
                        envelope.tenant_id.as_ref(),
                        &envelope.project_id,
                    ),
                ));
            }
        }
        // The reader that admitted this source may name it next, on this
        // thread ([`note_source`]); a note or a withheld source is not named.
        let nameable = decision.object != note_object() && decision.disposition.delivers_content();
        UNNAMED.with(|slot| {
            *slot.borrow_mut() =
                nameable.then(|| (std::ptr::from_ref(self).addr(), decision.object.clone()));
        });
        let mut decision = decision.clone();
        // The receipt itemizes an external or unknown object as not clean,
        // with a content-free reason (also on a clean-memo hit).
        if external {
            let code =
                ReasonCodeV1::new(UNTRUSTED_ORIGIN).expect("built-in reason codes are valid");
            if !decision.reason_codes.contains(&code) {
                decision.reason_codes.push(code);
            }
        }
        let incoming = Recorded {
            decision,
            counts,
            external,
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        if records.len() < MAX_DECISIONS {
            records.push(incoming);
            return;
        }
        // The receipt holds at most MAX_DECISIONS decisions. A notable one
        // (changed, withheld, flagged) displaces a clean one, so every
        // decision the receipt is about stays itemized; whatever is not
        // itemized still counts in full.
        let displaced = if incoming.is_clean() {
            incoming
        } else if let Some(position) = records.iter().position(Recorded::is_clean) {
            std::mem::replace(&mut records[position], incoming)
        } else if incoming.is_note() {
            // A note adds to no count, so one that finds no slot would be
            // gone without a trace. The receipt says so instead: one fixed
            // note stands for every note it does not itemize.
            let Some(displaced) = mark_notes_not_itemized(&mut records) else {
                return;
            };
            displaced
        } else {
            incoming
        };
        self.overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .add(&displaced);
    }

    pub(super) fn bind_policy(&self, mode: GatewayModeV1, digest: Sha256Digest) {
        if let Some(snapshot) = &self.snapshot
            && snapshot.admission.digest() != digest.hex()
        {
            self.diverged
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        *self.policy.lock().unwrap_or_else(PoisonError::into_inner) = Some((mode, digest));
    }

    /// The tenant/project scope of the call's task, for its receipt index.
    pub(crate) fn task_scope(&self) -> Option<String> {
        self.task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(_, scope)| scope.clone())
    }

    /// The measured original token count reported by the tool for this call.
    pub fn set_original_tokens(&self, tokens: u64) {
        *self
            .original_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(tokens);
    }

    pub fn is_empty(&self) -> bool {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

    /// Whether the capture holds a decision about a source, itemized or only
    /// counted: then the delivery owes a receipt. Notes alone describe no
    /// source — the rule by which [`finish_for_presenter`] returns `None`.
    pub(crate) fn describes_sources(&self) -> bool {
        let note = note_object();
        let itemized = self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|record| record.decision.object != note);
        itemized
            || self
                .overflow
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .objects
                > 0
    }

    /// Make a delivery that carries bytes but no admitted source decision a
    /// receipted one (G08): the delivered output itself becomes the one
    /// itemized object, permitted, with the fixed reason
    /// [`NO_SOURCE_DECISION`]. The receipt then says exactly that — bytes
    /// left without a source decision — instead of there being no receipt.
    /// A capture no admission bound is bound to `mode` and `policy`.
    pub(super) fn record_unsourced(
        &self,
        output: Sha256Digest,
        mode: GatewayModeV1,
        policy: Sha256Digest,
    ) {
        if self.bound_policy().is_none() {
            self.bind_policy(mode, policy);
        }
        let decision = ContextDecisionV1 {
            object: output,
            disposition: ContextDispositionV1::Allow,
            reason_codes: vec![
                ReasonCodeV1::new(NO_SOURCE_DECISION).expect("built-in reason codes are valid"),
            ],
            signals: Vec::new(),
            required_transformations: Vec::new(),
        };
        self.record(&decision, AdmissionCounts::default());
    }

    /// The itemized decisions with the counts admission measured for each,
    /// for the trace sidecar: what a V1 receipt holds only as totals.
    pub fn itemized(&self) -> Vec<(ContextDecisionV1, AdmissionCounts)> {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|record| (record.decision.clone(), record.counts))
            .collect()
    }

    /// Sources the receipt counts without itemizing them.
    pub(crate) fn counted_only(&self) -> u32 {
        self.overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .objects
    }

    /// The mode and policy digest this capture is bound to, if any.
    pub(super) fn bound_policy(&self) -> Option<(GatewayModeV1, Sha256Digest)> {
        self.policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Add every decision of `earlier` to this capture, as if it had been
    /// recorded here: itemized ones through [`Self::record`] (so the
    /// `MAX_DECISIONS` rule holds for the union), counted-only ones into the
    /// overflow totals. Policy binding and task of `self` are kept.
    pub(super) fn absorb(&self, earlier: &Self) {
        if let Some((name, version)) = earlier.pack()
            && self.pack().is_none()
        {
            self.note_pack(&name, &version);
        }
        // Unlike the pack word, the organization decision is evidence: what
        // an earlier admission noted is merged, and a difference is recorded
        // as one. A capture that noted nothing contributes nothing.
        let theirs = earlier
            .org_policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(theirs) = theirs {
            self.merge_org_policy(theirs);
        }
        let records = earlier
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for record in &records {
            self.record_as(&record.decision, record.counts, record.external);
        }
        let ledger = earlier
            .origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.origin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .merge(&ledger);
        let spans = earlier.external_spans();
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(spans);
        let released = earlier
            .released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        {
            let mut ours = self.released.lock().unwrap_or_else(PoisonError::into_inner);
            for digest in released {
                if ours.len() >= MAX_RELEASED_BLOCKS {
                    break;
                }
                ours.insert(digest);
            }
        }
        let theirs = earlier
            .overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut ours = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
        ours.objects = ours.objects.saturating_add(theirs.objects);
        ours.permitted = ours.permitted.saturating_add(theirs.permitted);
        ours.blocked = ours.blocked.saturating_add(theirs.blocked);
        ours.redactions = ours.redactions.saturating_add(theirs.redactions);
        ours.injection = ours.injection.saturating_add(theirs.injection);
        ours.incomplete = ours.incomplete.saturating_add(theirs.incomplete);
        ours.security.merge(&theirs.security);
        ours.injection_flags
            .extend(theirs.injection_flags.iter().cloned());
    }

    /// Reason codes of every itemized deny decision, in order, without
    /// duplicates: what a refusal is made of.
    pub(super) fn deny_reason_codes(
        &self,
    ) -> Vec<lean_ctx_protocol::context_gateway::ReasonCodeV1> {
        let mut codes = Vec::new();
        for record in self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|record| record.decision.disposition == ContextDispositionV1::Deny)
        {
            for code in &record.decision.reason_codes {
                if !codes.contains(code) {
                    codes.push(code.clone());
                }
            }
        }
        codes
    }

    /// Whether an admitted source carried prompt-injection signals; the
    /// dispatcher's own output scan must then not count the same text again.
    pub fn flagged_injection(&self) -> bool {
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|record| record.counts.injection > 0)
    }

    /// The call's security actions, in the event model the status line and
    /// `lean-ctx value` prove from the audit trail. Only what actually
    /// happened counts: a detected-but-delivered secret is not "kept out".
    pub(crate) fn security_tally(&self) -> crate::core::security_events::SecurityCounts {
        let mut tally = self
            .overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .security;
        for record in self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            tally.merge(&record.security());
        }
        tally
    }

    /// Every object this call flagged for prompt injection, itemized or not.
    fn injection_flags(&self) -> Vec<crate::core::security_events::InjectionFlag> {
        let mut flags = self
            .overflow
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .injection_flags
            .clone();
        flags.extend(
            self.records
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter_map(Recorded::injection_flag),
        );
        flags
    }

    /// Records the call's security actions once: the audit trail and the
    /// session counters. A flagged object counts only the first time this
    /// process delivers it (the receipt still carries every delivery's
    /// signal). Returns what was recorded.
    pub(crate) fn record_security(
        &self,
        tool: &str,
        agent_id: &str,
    ) -> crate::core::security_events::SecurityCounts {
        use crate::core::security_events::{self, SecurityKind};
        let mut tally = self.security_tally();
        tally.injection_flagged = 0;
        security_events::record(tool, agent_id, &tally);
        let flagged = security_events::record_injections(tool, agent_id, &self.injection_flags());
        tally.add(SecurityKind::InjectionFlagged, flagged);
        tally
    }
}

/// The code of the one note that stands for every note a full receipt could
/// not itemize. Fixed and content-free; it carries no number — how many
/// blocks stayed whole is then stated by the response header and the log
/// line of the request, not by the receipt.
pub(super) const NOTES_NOT_ITEMIZED: &str = "recovery.notes_not_itemized";

/// The reason of the one decision a receipt holds for a delivery whose bytes
/// no admission decided on (an empty or source-less capture under
/// protection, e.g. output with unknown lineage). Fixed and content-free.
pub(crate) const NO_SOURCE_DECISION: &str = "delivery.no_source_decision";

/// A note found every slot of a full receipt taken by decisions the receipt
/// is about. Make the receipt say that notes are missing: unless it already
/// does, the fixed [`NOTES_NOT_ITEMIZED`] note takes the slot of an itemized
/// note — which it then stands for as well — or, where no note is itemized,
/// the slot of a source decision (one that delivered content, if there is
/// one). Returns that source decision: the caller counts it in full, as it
/// counts every decision that is not itemized. `None`: no count moves.
fn mark_notes_not_itemized(records: &mut [Recorded]) -> Option<Recorded> {
    if records.iter().any(Recorded::states_notes_not_itemized) {
        return None;
    }
    let marker = Recorded {
        decision: ContextDecisionV1 {
            object: note_object(),
            disposition: ContextDispositionV1::Allow,
            reason_codes: vec![
                ReasonCodeV1::new(NOTES_NOT_ITEMIZED).expect("built-in reason codes are valid"),
            ],
            signals: Vec::new(),
            required_transformations: Vec::new(),
        },
        counts: AdmissionCounts::default(),
        external: false,
    };
    let slot = records
        .iter()
        .rposition(Recorded::is_note)
        .or_else(|| {
            records
                .iter()
                .rposition(|record| record.decision.disposition.delivers_content())
        })
        .unwrap_or(records.len().saturating_sub(1));
    let displaced = std::mem::replace(records.get_mut(slot)?, marker);
    (!displaced.is_note()).then_some(displaced)
}

impl Recorded {
    fn injection_flag(&self) -> Option<crate::core::security_events::InjectionFlag> {
        (self.counts.injection > 0).then(|| crate::core::security_events::InjectionFlag {
            object_sha256: self.decision.object.hex().to_owned(),
            patterns: self.counts.injection_patterns,
        })
    }

    /// Delivered unchanged, nothing flagged, fully inspected.
    fn is_clean(&self) -> bool {
        self.decision.disposition == ContextDispositionV1::Allow
            && self.decision.reason_codes.is_empty()
            && !self.external
    }

    /// A statement about a source, not a source ([`note_object`]).
    fn is_note(&self) -> bool {
        self.decision.object == note_object()
    }

    fn states_notes_not_itemized(&self) -> bool {
        self.is_note()
            && self
                .decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == NOTES_NOT_ITEMIZED)
    }

    /// This decision's security actions. Only what actually happened counts:
    /// a detected-but-delivered secret is not "kept out".
    fn security(&self) -> crate::core::security_events::SecurityCounts {
        use crate::core::security_events::{SecurityCounts, SecurityKind};
        let mut tally = SecurityCounts::default();
        let has = |code: &str| {
            self.decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == code)
        };
        if has("secret.redacted") {
            tally.add(SecurityKind::SecretRedacted, u64::from(self.counts.secrets));
        }
        if has("pii.redacted") {
            tally.add(SecurityKind::PiiRedacted, u64::from(self.counts.pii));
        }
        if self.counts.injection > 0 {
            tally.add(
                SecurityKind::InjectionFlagged,
                u64::from(self.counts.injection),
            );
        }
        if self.decision.disposition == ContextDispositionV1::Deny {
            tally.add(SecurityKind::ContentWithheld, 1);
        } else if self.counts.incomplete > 0 {
            tally.add(SecurityKind::CoverageIncomplete, 1);
        }
        tally
    }

    fn redactions(&self) -> u32 {
        if self.decision.disposition == ContextDispositionV1::AllowRedacted {
            self.counts.secrets.saturating_add(self.counts.pii)
        } else {
            0
        }
    }
}

/// Everything about the decisions a receipt counts but does not itemize.
#[derive(Debug, Default)]
struct Overflow {
    objects: u32,
    permitted: u32,
    blocked: u32,
    redactions: u32,
    injection: u32,
    incomplete: u32,
    security: crate::core::security_events::SecurityCounts,
    injection_flags: Vec<crate::core::security_events::InjectionFlag>,
}

impl Overflow {
    fn add(&mut self, record: &Recorded) {
        // A note is a statement about a source, not a source: where it cannot
        // be itemized it adds to no source count.
        if record.decision.object == note_object() {
            return;
        }
        let disposition = record.decision.disposition;
        self.objects = self.objects.saturating_add(1);
        if disposition.delivers_content() {
            self.permitted = self.permitted.saturating_add(1);
        }
        if disposition == ContextDispositionV1::Deny {
            self.blocked = self.blocked.saturating_add(1);
        }
        self.redactions = self.redactions.saturating_add(record.redactions());
        self.injection = self.injection.saturating_add(record.counts.injection);
        if record.counts.incomplete > 0 {
            self.incomplete = self.incomplete.saturating_add(1);
        }
        self.security.merge(&record.security());
        self.injection_flags.extend(record.injection_flag());
    }
}

/// Identity of the call the receipt describes.
pub struct CallIdentity<'a> {
    pub agent_id: Option<&'a str>,
    /// Where the context goes, when the caller knows it (the proxy does).
    pub destination: Option<DestinationV1>,
}

/// The delivered result as the host receives it.
pub struct Delivered<'a> {
    pub text: &'a str,
    pub is_error: bool,
    pub tokens: u64,
}

fn reference(prefix: &str, raw: &str) -> Option<ProtocolReference> {
    // Opaque references must stay bounded and printable; anything else is
    // reduced to a digest rather than guessed at.
    ProtocolReference::new(format!("{prefix}{raw}"))
        .ok()
        .or_else(|| {
            let digest = blake3::hash(raw.as_bytes()).to_hex();
            ProtocolReference::new(format!("{prefix}{}", &digest[..32])).ok()
        })
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    use sha2::{Digest, Sha256};
    let hex = crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    Sha256Digest::new(format!("sha256:{hex}")).expect("SHA-256 digest is canonical")
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The fixed object of a note decision: an itemized, content-free statement
/// about a source the same receipt already counts (the proxy's "this block
/// stayed whole, and why"). A constant, so a note names no content, and the
/// one mark by which [`finish`] keeps notes out of the source counts — the
/// block a note is about was inspected and permitted as its own decision.
pub(super) fn note_object() -> Sha256Digest {
    sha256(b"proxy-block-kept-whole")
}

/// Whether `decision` is a note — a statement about a source the receipt
/// already counts — and not a source of its own. For readers of receipts.
pub fn is_note(decision: &ContextDecisionV1) -> bool {
    let note = note_object();
    // A version 2 receipt names the note keyed under this installation's key.
    decision.object == note
        || super::receipt_store::keyed::object_key()
            .is_ok_and(|key| decision.object == key.object(&note))
}

/// Build the call's receipt, or `None` when no governed source was admitted.
/// The receipt is validated before it is returned; an inconsistent receipt is
/// never produced. Product callers use `finish_for_presenter`; this form
/// without a presenter is what the tests call.
#[cfg(any(test, feature = "test-support"))]
pub fn finish(
    capture: &AdmissionCapture,
    identity: &CallIdentity<'_>,
    delivered: &Delivered<'_>,
) -> Option<ContextDecisionReceiptV1> {
    finish_for_presenter(capture, identity, None, delivered)
}

/// [`finish`] for a call that showed a live presenter capability
/// (`core::archive::presented`, "Presenters"). `presenter_ref` is the
/// content-free reference of its presenter entry — a digest of a stored
/// verifier, never of the capability — or `None`.
///
/// With a reference the receipt's principal is of kind `Session` with that
/// reference as its id: it says "the holder of this session capability", so
/// that receipts of one presenter entry can be tied together. It is never
/// kind `Agent`: a capability names no agent and attests no identity. A
/// host-named `agent_id` is then not recorded as the principal.
pub(crate) fn finish_for_presenter(
    capture: &AdmissionCapture,
    identity: &CallIdentity<'_>,
    presenter_ref: Option<&str>,
    delivered: &Delivered<'_>,
) -> Option<ContextDecisionReceiptV1> {
    let records = capture
        .records
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    if records.is_empty() {
        return None;
    }
    let overflow = capture
        .overflow
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (mode, policy_digest) = capture
        .policy
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;

    let decisions: Vec<ContextDecisionV1> = records
        .iter()
        .map(|record| record.decision.clone())
        .collect();
    // Notes are itemized but are no sources: they count neither as inspected
    // nor as permitted or selected. A capture that holds nothing but notes
    // describes no admitted source, exactly like an empty one.
    let note = note_object();
    let notes = decisions
        .iter()
        .filter(|decision| decision.object == note)
        .count();
    if notes == decisions.len() && overflow.objects == 0 {
        return None;
    }
    let count = |disposition: ContextDispositionV1| {
        decisions
            .iter()
            .filter(|decision| decision.disposition == disposition)
            .count()
    };
    // `blocked_objects`/`quarantined_objects` must equal the itemized
    // decisions (contract); every other count includes what was not
    // itemized, so a large request is described in full.
    let blocked = count(ContextDispositionV1::Deny);
    let quarantined = count(ContextDispositionV1::Quarantine);
    let permitted = saturating_u32(
        decisions
            .iter()
            .filter(|decision| decision.object != note && decision.disposition.delivers_content())
            .count(),
    )
    .saturating_add(overflow.permitted);
    let redactions: u32 = records
        .iter()
        .map(Recorded::redactions)
        .fold(overflow.redactions, u32::saturating_add);
    let injection: u32 = records
        .iter()
        .map(|record| record.counts.injection)
        .fold(overflow.injection, u32::saturating_add);
    let incomplete = saturating_u32(
        records
            .iter()
            .filter(|record| record.counts.incomplete > 0)
            .count(),
    )
    .saturating_add(overflow.incomplete);

    // AP10-C4: a receipt is version 2 — its objects keyed under the
    // installation's receipt key. No key, no receipt: a plain digest of a
    // single string confirms a guess of it, so none is ever emitted again.
    let object_key = super::receipt_store::keyed::object_key().ok()?;
    let decisions: Vec<ContextDecisionV1> = decisions
        .into_iter()
        .map(|mut decision| {
            decision.object = object_key.object(&decision.object);
            decision
        })
        .collect();
    let outcome = if permitted == 0 {
        DeliveryOutcomeV1::Withheld
    } else if delivered.is_error {
        DeliveryOutcomeV1::Failed
    } else {
        DeliveryOutcomeV1::Delivered
    };
    let final_context =
        (outcome == DeliveryOutcomeV1::Delivered).then(|| sha256(delivered.text.as_bytes()));
    let principal = presenter_ref
        .and_then(|presenter| ProtocolReference::new(presenter).ok())
        .and_then(|id| PrincipalV1::new(PrincipalKindV1::Session, id).ok())
        .or_else(|| {
            identity
                .agent_id
                .and_then(|agent| reference("agent:", agent))
                .and_then(|id| PrincipalV1::new(PrincipalKindV1::Agent, id).ok())
        })
        .unwrap_or_else(PrincipalV1::unknown);
    let receipt_seed = format!(
        "{}|{}|{}|{:?}",
        policy_digest.as_str(),
        decisions
            .iter()
            .map(|decision| decision.object.as_str())
            .collect::<Vec<_>>()
            .join(","),
        final_context.as_ref().map_or("", Sha256Digest::as_str),
        capture.started
    );
    let receipt = ContextDecisionReceiptV1 {
        schema_version: RECEIPT_V2_SCHEMA_VERSION,
        object_digest: Some(object_key.descriptor()),
        receipt_id: reference(
            "rcpt-",
            &blake3::hash(receipt_seed.as_bytes()).to_hex()[..32],
        )?,
        mode,
        principal,
        task: capture
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|(task, _)| TaskId::new(task.as_str()).ok()),
        // An MCP call serves the host; where the host sends the context is
        // not known there, so locality is honestly unknown. The proxy names
        // the real model destination (G6).
        destination: identity
            .destination
            .clone()
            .unwrap_or_else(|| DestinationV1 {
                provider: ProtocolReference::new("mcp-host").expect("valid provider ref"),
                model: None,
                locality: DestinationLocalityV1::Unknown,
                organization_managed: false,
                account_ref: None,
                region: None,
            }),
        policy: Some(PolicyRefV1 {
            id: PolicyId::new("context-gateway").expect("valid policy id"),
            version: None,
            digest: policy_digest,
        }),
        sources: SourceCountsV1 {
            inspected: saturating_u32(decisions.len() - notes).saturating_add(overflow.objects),
            permitted,
            selected: permitted,
            blocked: saturating_u32(blocked).saturating_add(overflow.blocked),
        },
        decisions,
        security: SecurityCountsV1 {
            redactions,
            blocked_objects: saturating_u32(blocked),
            quarantined_objects: saturating_u32(quarantined),
            injection_signals: injection,
            incomplete_coverage: incomplete,
        },
        tokens: TokenAccountV1 {
            // The tool's own tokenizer measurement of its sources; a tool that
            // reports none delivered its sources as they were.
            original: capture
                .original_tokens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .filter(|tokens| *tokens > 0)
                .unwrap_or(delivered.tokens),
            delivered: delivered.tokens,
        },
        final_context,
        outcome,
        duration_us: u64::try_from(capture.started.elapsed().as_micros()).unwrap_or(u64::MAX),
        quality: None,
    };
    receipt.validate().ok()?;
    Some(receipt)
}
