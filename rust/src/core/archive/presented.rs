// SPDX-License-Identifier: Apache-2.0
//! Presentation register: the authority of caller-supplied context sources.
//!
//! A source the caller hands to the context plan is not a project file. The
//! Engine cannot re-read it and does not learn when the caller's or a
//! provider's copy changes. What it can assert, and all a [`PresentedBinding`]
//! asserts, is: *this exact content (full digest) under this object reference
//! and source id was presented for this project and admitted under this
//! policy.*
//!
//! Every time the plan admits such a source for delivery (the revalidation
//! that closes a materialization, after the delivery policy has passed) it
//! records the presentation here. A binding holds only while a presentation
//! of exactly that binding exists that was admitted under the CURRENT policy
//! identity and is at most [`VALIDITY_SECONDS`] old — a caller that
//! materializes its plan each turn always passes; one that stopped
//! presenting the source does not.
//!
//! # Presenters
//!
//! A presentation belongs to a PRESENTER, and holds only for a request that
//! shows that presenter's capability ([`PresenterCapability`]): 32 bytes from
//! the system CSPRNG. Two kinds exist.
//!
//! - An ISSUED capability (`context-materialize-sources-v2`): the Engine
//!   mints it, returns it once to the caller and stores nothing it could be
//!   rebuilt from. The caller shows it again on later requests — to this
//!   Engine, the proxy, the MCP server, the CLI — and so reuses what it
//!   presented, across processes, until the capability expires or is revoked.
//! - The PROCESS capability: drawn once per process, never written, logged or
//!   returned. It is the presenter of every request that shows no issued
//!   capability, so such a presentation holds inside the presenting process
//!   only. This is the default for a caller that does not negotiate.
//!
//! What a capability proves is exactly this: *the request comes from a holder
//! of the capability under which the source was presented.* It is not an
//! agent name, not an attested identity and not an authorization. Project,
//! OS user, role, policy identity and freshness are checked as before, after
//! it; file sources of the same lineage are re-admitted as before.
//!
//! Per source the register keeps a bounded SET of presenter entries. Each has
//! its own verifier, presentations and expiry; one presenter's presentation,
//! lapse or revocation never touches another's entry.
//!
//! The register holds hashes, policy identities and times. No content, no
//! object reference, no source id, and nothing a capability can be derived
//! from: a reader of the store learns verifiers and presenter references,
//! never a capability.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::register::{self, Register};

/// How long one admitted presentation keeps a binding valid: ten minutes.
pub(crate) const VALIDITY_SECONDS: i64 = 10 * 60;

/// Longest lifetime of an issued capability: one day.
pub(crate) const MAX_CAPABILITY_TTL_SECONDS: i64 = 24 * 60 * 60;

/// Wire prefix of a capability; 64 lowercase hex characters follow.
const CAPABILITY_PREFIX: &str = "lcp1_";
/// Wire prefix of a presenter reference; 32 lowercase hex characters follow.
const REFERENCE_PREFIX: &str = "presenter:";
/// A capability file is this long at most (the capability and a line end).
const MAX_CAPABILITY_FILE_BYTES: u64 = 128;

/// Issued presenters one source can have at a time. A further one is not
/// recorded; no live entry is ever removed to make room.
const MAX_PRESENTERS_PER_SOURCE: usize = 16;
/// Process presenters one source can have at a time. Their entries are of
/// use to their own process only, so the oldest of THEM makes room.
const MAX_PROCESS_PRESENTERS_PER_SOURCE: usize = 16;
/// Policy identities kept per presenter entry (a policy change adds one).
const MAX_IDENTITIES_PER_BINDING: usize = 8;

/// Principal where the platform offers no verified per-caller fact (non-Unix
/// builds): one local operator.
#[cfg(not(unix))]
const LOCAL_OPERATOR: &str = "local-operator";

/// The caller principal of a plan request, as far as the Engine can VERIFY
/// it without anything the caller holds: the OS user its own process runs as,
/// `os-user:<euid>`, stable and not secret. A name the client supplies is
/// never a principal. Callers of one OS user are told apart by the presenter
/// capability (module documentation, "Presenters"), not by this value.
#[must_use]
pub(crate) fn verified_principal() -> String {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        format!("os-user:{uid}")
    }
    #[cfg(not(unix))]
    {
        LOCAL_OPERATOR.to_owned()
    }
}

/// A presenter capability: a bearer secret. Whoever shows it is treated as
/// the holder of the capability its presentations were recorded under — no
/// more than that.
///
/// The secret is overwritten when the value is dropped, has no `Display`, is
/// not serializable and formats as `<redacted>`. It leaves this type in one
/// place only: [`Self::expose`], for the response that hands a freshly issued
/// capability to its caller.
#[derive(Clone)]
pub(crate) struct PresenterCapability(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for PresenterCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PresenterCapability(<redacted>)")
    }
}

fn hex_nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl PresenterCapability {
    fn mint() -> Option<Self> {
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *secret).ok()?;
        Some(Self(secret))
    }

    /// The capability a host presented, from its wire form. Form only: an
    /// unknown capability parses and then matches no presenter entry.
    #[must_use]
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix(CAPABILITY_PREFIX)?.as_bytes();
        if digits.len() != 64 {
            return None;
        }
        let mut secret = Zeroizing::new([0u8; 32]);
        for (index, pair) in digits.as_chunks::<2>().0.iter().enumerate() {
            secret[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
        }
        Some(Self(secret))
    }

    /// The wire form. Only for the response that issues the capability.
    #[must_use]
    pub(crate) fn expose(&self) -> Zeroizing<String> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = Zeroizing::new(String::with_capacity(CAPABILITY_PREFIX.len() + 64));
        text.push_str(CAPABILITY_PREFIX);
        for byte in self.0.iter() {
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// One-way root verifier of the capability. Everything public about a
    /// presenter is derived from this, never from the secret directly.
    fn root_verifier(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(
            "leanctx 2026-10 presenter root verifier v1",
            &*self.0,
        ))
    }

    /// Register key of the presenter: a digest of the root verifier.
    fn id(&self) -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-presenter-id-v1\0");
        hash.update(&*self.root_verifier());
        hash.finalize().to_hex().to_string()
    }

    /// Content-free reference of the presenter entry, for records: a digest
    /// of a verifier, not of the capability. Naming it reveals no secret and
    /// proves nothing by itself.
    #[must_use]
    pub(crate) fn reference(&self) -> String {
        format!("{REFERENCE_PREFIX}{}", &self.id()[..32])
    }

    /// What the register stores for this presenter on one source: a keyed
    /// hash, under the capability, of the source fields and the presenter
    /// id. It verifies for no other source, project, caller or role.
    fn source_verifier(&self, binding: &PresentedBinding, id: &str) -> blake3::Hash {
        let mut hash = blake3::Hasher::new_keyed(&self.0);
        hash.update(b"leanctx-presenter-source-v1\0");
        binding.absorb_source(&mut hash);
        hash.update(id.as_bytes());
        hash.finalize()
    }
}

/// The capability of this process: drawn once, held here only. `None`: the
/// CSPRNG failed — the process then presents nothing and re-authorizes
/// nothing without an issued capability.
static PROCESS_SESSION: OnceLock<Option<PresenterCapability>> = OnceLock::new();

fn process_session() -> Option<&'static PresenterCapability> {
    PROCESS_SESSION
        .get_or_init(PresenterCapability::mint)
        .as_ref()
}

fn is_process_session(id: &str) -> bool {
    PROCESS_SESSION
        .get()
        .and_then(Option::as_ref)
        .is_some_and(|own| own.id() == id)
}

tokio::task_local! {
    /// The capability the request in scope showed, if any. Request state,
    /// like `policy::runtime::REQUEST_PROJECT`; never a global.
    static REQUEST_PRESENTER: Option<PresenterCapability>;
}

/// Run `operation` as the request of a host that showed `capability`
/// (`None`: it showed none — the process capability presents).
pub(crate) fn with_presenter<T>(
    capability: Option<PresenterCapability>,
    operation: impl FnOnce() -> T,
) -> T {
    REQUEST_PRESENTER.sync_scope(capability, operation)
}

/// [`with_presenter`] around a future: the whole request task.
pub(crate) async fn presenter_scope<F: std::future::Future>(
    capability: Option<PresenterCapability>,
    request: F,
) -> F::Output {
    REQUEST_PRESENTER.scope(capability, request).await
}

/// The capability the request in scope showed, to carry it onto a thread the
/// request task hands work to: task-locals do not follow `spawn_blocking`,
/// and a handler that ran without it would be served as this process's own
/// presenter. The value goes back into [`with_presenter`] and nowhere else.
#[must_use]
pub(crate) fn request_presenter() -> Option<PresenterCapability> {
    REQUEST_PRESENTER.try_with(Clone::clone).ok().flatten()
}

/// Run `use_capability` with the presenter of the request in scope: the
/// capability it showed, else this process's own.
fn with_current<T>(use_capability: impl FnOnce(&PresenterCapability) -> T) -> Option<T> {
    let shown = REQUEST_PRESENTER.try_with(Clone::clone).ok().flatten();
    match shown {
        Some(capability) => Some(use_capability(&capability)),
        None => process_session().map(use_capability),
    }
}

/// Content-free reference of the presenter of the request in scope, for
/// lineage, recovery and decision records.
#[must_use]
pub(crate) fn current_presenter_reference() -> Option<String> {
    with_current(PresenterCapability::reference)
}

/// The reference of the presenter entry of an ISSUED capability the request
/// in scope showed, while that capability may still present (known, not
/// expired, not revoked). `None` for a request that showed nothing or
/// something that is not such a capability: decision receipts name a
/// presenter only then, and never the process capability.
#[must_use]
pub(crate) fn shown_presenter_reference() -> Option<String> {
    let shown = REQUEST_PRESENTER.try_with(Clone::clone).ok().flatten()?;
    let id = shown.id();
    (!is_process_session(&id) && lifetime_end(&id, now()).is_some()).then(|| shown.reference())
}

/// What the store knows about an issued capability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenterRecord {
    /// End of the lifetime fixed at issue (register clock, seconds).
    expires_at: i64,
    revoked: bool,
}

const PRESENTERS: Register = Register {
    name: "presenters",
    shard_entries: 128,
    max_age_seconds: MAX_CAPABILITY_TTL_SECONDS,
};

/// Directory of the presenter sets, under the archive store. NOT the
/// `presented` directory of binaries before presenters existed: those keep
/// their own register (other value type), and two binaries writing one
/// directory would each start the other's shards afresh. This binary neither
/// reads nor writes nor removes `archives/presented`; what an older binary
/// left there lapses by that binary's own ten-minute rule and is dead data
/// afterwards.
pub(crate) const REGISTER_DIRECTORY: &str = "presented-v2";

const REGISTER: Register = Register {
    name: REGISTER_DIRECTORY,
    shard_entries: 128,
    max_age_seconds: MAX_CAPABILITY_TTL_SECONDS,
};

/// End of the lifetime of the presenter `id` at `now`, or `None` when it may
/// not present or reuse: unknown, expired, revoked. The process capability
/// has no record and lives as long as its process.
fn lifetime_end(id: &str, now: i64) -> Option<i64> {
    match PRESENTERS.read::<PresenterRecord>(id, now) {
        Some(record) if record.revoked || record.expires_at < now => None,
        Some(record) => Some(record.expires_at),
        None => is_process_session(id).then(|| now.saturating_add(MAX_CAPABILITY_TTL_SECONDS)),
    }
}

/// A freshly issued capability with the end of its lifetime.
pub(crate) struct IssuedCapability {
    pub(crate) capability: PresenterCapability,
    /// Register clock, seconds.
    pub(crate) expires_at: i64,
}

/// Mint a capability that lives `ttl_seconds` (1 to
/// [`MAX_CAPABILITY_TTL_SECONDS`]) and put its presenter on record. `None`:
/// no randomness, a lifetime out of range, or the record could not be
/// written — then no capability exists.
#[must_use]
pub(crate) fn issue(ttl_seconds: u32) -> Option<IssuedCapability> {
    let ttl = i64::from(ttl_seconds);
    if !(1..=MAX_CAPABILITY_TTL_SECONDS).contains(&ttl) {
        return None;
    }
    let capability = PresenterCapability::mint()?;
    let now = now();
    let expires_at = now.checked_add(ttl)?;
    PRESENTERS
        .update::<PresenterRecord>(&capability.id(), now, |known| {
            known.is_none().then_some(PresenterRecord {
                expires_at,
                revoked: false,
            })
        })
        .then_some(IssuedCapability {
            capability,
            expires_at,
        })
}

/// End of the lifetime of an ISSUED capability that may still present, or
/// `None` (unknown, expired, revoked, or not an issued capability).
#[must_use]
pub(crate) fn issued_lifetime_end(capability: &PresenterCapability) -> Option<i64> {
    let id = capability.id();
    if is_process_session(&id) {
        return None;
    }
    lifetime_end(&id, now())
}

/// Revoke an issued capability: from now on it presents nothing and reuses
/// nothing. `false`: no such presenter on record (nothing to revoke), or the
/// record could not be written.
pub(crate) fn revoke(capability: &PresenterCapability) -> bool {
    let now = now();
    PRESENTERS.update::<PresenterRecord>(&capability.id(), now, |known| {
        known.map(|record| PresenterRecord {
            revoked: true,
            ..record
        })
    })
}

/// The capability in a host's capability file, or `None`. On every platform
/// the path must name a regular file and not a symlink (checked on the path
/// itself, and again on the opened file), at most
/// [`MAX_CAPABILITY_FILE_BYTES`] long. On Unix the open itself refuses a
/// symlink (`O_NOFOLLOW`) and the file must be owned by this user and closed
/// to group and others. On Windows ownership and access rights are NOT
/// checked, and the two checks of the path are not atomic. Nothing about a
/// refused file is reported.
#[must_use]
pub(crate) fn read_capability_file(path: &Path) -> Option<PresenterCapability> {
    use std::io::Read as _;

    if !std::fs::symlink_metadata(path).ok()?.file_type().is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CAPABILITY_FILE_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let own = unsafe { libc::geteuid() };
        if metadata.uid() != own || metadata.mode() & 0o077 != 0 {
            return None;
        }
    }
    let mut text = Zeroizing::new(String::new());
    let mut bounded = file.take(MAX_CAPABILITY_FILE_BYTES);
    bounded.read_to_string(&mut text).ok()?;
    PresenterCapability::parse(text.trim_end_matches(['\r', '\n']))
}

/// One caller-supplied source as the plan admission resolved it. Content-free.
/// It says WHICH source under WHICH policy; who may reuse it is the presenter
/// set of the register, not a field here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresentedBinding {
    /// Canonical project root the plan ran for.
    project: String,
    /// Caller principal ([`verified_principal`]).
    caller: String,
    /// Role active at admission.
    role: String,
    object_ref: String,
    source_id: String,
    /// The full content digest the plan binds (`sha256:…` of the caller's
    /// original bytes).
    content_digest: String,
    /// Gateway policy, project policy pack and kernel context policy at
    /// admission ([`policy_identity`]).
    policy_identity: String,
}

/// What the register knows about a binding now, for the presenter of the
/// request in scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Presentation {
    /// Presented and admitted under the current policy identity, in time.
    Current,
    /// Presented in time, but only under another policy identity: under the
    /// current one it has not been admitted.
    OtherPolicy,
    /// No presentation within the validity by the presenter that asks: none
    /// at all, only by another presenter, or its capability is unknown,
    /// expired or revoked. One answer for all of these.
    None,
}

/// All presenter entries of one source.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePresenters {
    /// Presenter id → entry.
    presenters: BTreeMap<String, PresenterEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenterEntry {
    /// `PresenterCapability::source_verifier`, hex.
    verifier: String,
    /// Policy identity → last admitted presentation.
    admitted: BTreeMap<String, i64>,
    /// End of the presenter's lifetime as known when it last presented.
    expires_at: i64,
    /// Recorded under a process capability.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    process_only: bool,
}

fn in_time(presented_at: i64, now: i64) -> bool {
    now.checked_sub(presented_at)
        .is_some_and(|age| (0..=VALIDITY_SECONDS).contains(&age))
}

impl PresenterEntry {
    /// Still able to authorize something: not expired, and presented within
    /// the validity. Only an entry that is NOT live is ever removed.
    fn live(&self, now: i64) -> bool {
        self.expires_at >= now && self.admitted.values().any(|at| in_time(*at, now))
    }

    fn last_presented(&self) -> i64 {
        self.admitted.values().copied().max().unwrap_or(i64::MIN)
    }
}

/// Identity of everything that decides a plan admission for `project`: the
/// gateway admission policy, the project policy pack and the kernel context
/// policy (sensitivity ceiling, budget cap, retention). `None`: one of them
/// cannot be loaded — nothing is then bound or re-authorized.
pub(crate) fn policy_identity(project: &Path) -> Option<String> {
    let gateway = crate::core::context_admission::stores::StoreAdmission::current();
    let pack = crate::core::policy::runtime::for_project(project).ok()?;
    let pack = serde_json::to_vec(&pack.as_ref().map(|active| &active.resolved)).ok()?;
    let kernel =
        crate::core::engine_interface::context_plan::sources::kernel_policy_identity(project)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"leanctx-presented-policy-v1\0");
    hash.update(gateway.digest().as_bytes());
    hash.update(b"\0");
    hash.update(&pack);
    hash.update(b"\0");
    hash.update(kernel.as_bytes());
    Some(hash.finalize().to_hex().to_string())
}

fn canonical_project(project: &Path) -> Option<String> {
    crate::core::pathutil::canonicalize_secure(project)
        .ok()?
        .to_str()
        .map(str::to_owned)
}

impl PresentedBinding {
    /// The binding of a source the plan has admitted in this very step, with
    /// the caller, role and policy identity that hold now.
    pub(crate) fn admitted_now(
        project: &Path,
        object_ref: &str,
        source_id: &str,
        content_digest: &str,
    ) -> Option<Self> {
        Self::admitted_by(
            &verified_principal(),
            project,
            object_ref,
            source_id,
            content_digest,
        )
    }

    /// [`Self::admitted_now`] for an explicit, already verified principal.
    fn admitted_by(
        principal: &str,
        project: &Path,
        object_ref: &str,
        source_id: &str,
        content_digest: &str,
    ) -> Option<Self> {
        Some(Self {
            project: canonical_project(project)?,
            caller: principal.to_owned(),
            role: crate::core::roles::active_role_name(),
            object_ref: object_ref.to_owned(),
            source_id: source_id.to_owned(),
            content_digest: content_digest.to_owned(),
            policy_identity: policy_identity(project)?,
        })
    }

    pub(super) fn project(&self) -> &str {
        &self.project
    }

    /// The caller's id of the source. Content-free; it authorizes nothing.
    pub(super) fn source_id(&self) -> &str {
        &self.source_id
    }

    /// Same project, caller, role, object reference, source id and content
    /// digest. The policy identity is not part of what a binding IS, and
    /// neither is a presenter: two presenters of one source share the
    /// binding and differ in the register only.
    pub(super) fn same_source(&self, other: &Self) -> bool {
        self.key() == other.key()
    }

    /// A stable, content-free identity of this binding INCLUDING the policy
    /// it was admitted under: what a stored copy bound to it is addressed by.
    pub(super) fn identity(&self) -> String {
        format!("{}:{}", self.key(), self.policy_identity)
    }

    /// Feed `hash` the six fields that say WHICH source this is,
    /// length-prefixed.
    fn absorb_source(&self, hash: &mut blake3::Hasher) {
        for field in [
            &self.project,
            &self.caller,
            &self.role,
            &self.object_ref,
            &self.source_id,
            &self.content_digest,
        ] {
            hash.update(&(field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
    }

    /// Register key: a hash over every identifying field, length-prefixed.
    fn key(&self) -> String {
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-presented-binding-v2\0");
        self.absorb_source(&mut hash);
        hash.finalize().to_hex().to_string()
    }

    /// Whether project, caller and role of this binding are the ones of the
    /// request in scope.
    pub(super) fn matches_request(&self) -> bool {
        let project = crate::core::policy::runtime::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten()
            .and_then(|path| canonical_project(&path));
        project.as_deref() == Some(self.project.as_str())
            && self.caller == verified_principal()
            && self.role == crate::core::roles::active_role_name()
    }

    /// The entry of the presenter of the request in scope, if that presenter
    /// may reuse this source at `now`: its capability matches the stored
    /// verifier (compared in constant time, `blake3::Hash`), the entry has
    /// not expired, and the presenter is neither expired nor revoked.
    fn own_entry(&self, now: i64) -> Option<PresenterEntry> {
        with_current(|capability| {
            let id = capability.id();
            let mut set = REGISTER.read::<SourcePresenters>(&self.key(), now)?;
            let entry = set.presenters.remove(&id)?;
            let stored = blake3::Hash::from_hex(entry.verifier.as_bytes()).ok()?;
            (stored == capability.source_verifier(self, &id)
                && entry.expires_at >= now
                && lifetime_end(&id, now).is_some())
            .then_some(entry)
        })
        .flatten()
    }

    /// The state of this binding under the policy that is current NOW, for
    /// the presenter of the request in scope.
    ///
    /// Asked first: does the request hold a capability this source was
    /// presented under? Without one nothing is presented, whatever the
    /// register holds — the request learns neither the policy state nor the
    /// age of another presenter's entry.
    ///
    /// Then, as before: a text delivered under one policy is not what another
    /// policy would deliver from the same source (other redaction, other
    /// ceiling). So a binding admitted under a policy identity that is no
    /// longer the current one never holds, whatever is presented since: the
    /// text has to be delivered — and recorded — again under the current
    /// policy.
    pub(super) fn presentation(&self) -> Presentation {
        let now = now();
        if self.own_entry(now).is_none() {
            return Presentation::None;
        }
        let Some(current) = policy_identity(Path::new(&self.project)) else {
            return Presentation::OtherPolicy;
        };
        if self.policy_identity != current {
            return Presentation::OtherPolicy;
        }
        self.presentation_at(&current, now)
    }

    fn presentation_at(&self, current_identity: &str, now: i64) -> Presentation {
        let Some(entry) = self.own_entry(now) else {
            return Presentation::None;
        };
        if entry
            .admitted
            .get(current_identity)
            .is_some_and(|at| in_time(*at, now))
        {
            Presentation::Current
        } else if entry.admitted.values().any(|at| in_time(*at, now)) {
            Presentation::OtherPolicy
        } else {
            Presentation::None
        }
    }
}

/// Record that `binding` was presented and admitted now under its policy
/// identity, by the presenter of the request in scope. Call only from the
/// plan admission, after it admitted the source. A restricted source is
/// never passed here (E3). `false`: not recorded — no usable presenter, the
/// source already has its full set of live issued presenters, a bound, the
/// lock, the file system.
pub(crate) fn record_presentation(binding: &PresentedBinding) -> bool {
    record_presentation_at(binding, now())
}

/// "Now" of the validity check and of a recorded presentation: the
/// register's clock. Tests can move it ahead with [`TestClock`].
#[cfg(not(test))]
fn now() -> i64 {
    register::now()
}

#[cfg(test)]
fn now() -> i64 {
    register::now().saturating_add(TEST_CLOCK_AHEAD.load(std::sync::atomic::Ordering::SeqCst))
}

/// Seconds the presentation clock runs ahead of the system clock in tests.
#[cfg(test)]
static TEST_CLOCK_AHEAD: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Moves the presentation clock ahead until the guard drops. The offset is
/// process-wide: hold `core::data_dir::isolated_data_dir()` (which serializes
/// every test that has a register) for as long as a guard lives.
#[cfg(test)]
pub(crate) struct TestClock;

#[cfg(test)]
impl TestClock {
    /// One second past the validity of every presentation recorded so far.
    #[must_use]
    pub(crate) fn past_validity() -> Self {
        TEST_CLOCK_AHEAD.fetch_add(VALIDITY_SECONDS + 1, std::sync::atomic::Ordering::SeqCst);
        Self
    }

    /// One second past the longest lifetime of every capability issued so
    /// far.
    #[must_use]
    pub(crate) fn past_capability_lifetime() -> Self {
        TEST_CLOCK_AHEAD.fetch_add(
            MAX_CAPABILITY_TTL_SECONDS + 1,
            std::sync::atomic::Ordering::SeqCst,
        );
        Self
    }
}

#[cfg(test)]
impl Drop for TestClock {
    fn drop(&mut self) {
        TEST_CLOCK_AHEAD.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

fn record_presentation_at(binding: &PresentedBinding, now: i64) -> bool {
    let Some((id, verifier, expires_at, process_only)) = with_current(|capability| {
        let id = capability.id();
        let expires_at = lifetime_end(&id, now)?;
        let verifier = capability
            .source_verifier(binding, &id)
            .to_hex()
            .to_string();
        let process_only = is_process_session(&id);
        Some((id, verifier, expires_at, process_only))
    })
    .flatten() else {
        return false;
    };
    REGISTER.update::<SourcePresenters>(&binding.key(), now, |known| {
        let mut set = known.unwrap_or_default();
        // Only entries that authorize nothing any more go.
        set.presenters.retain(|_, entry| entry.live(now));
        let mut entry = set
            .presenters
            .remove(&id)
            .filter(|known| known.verifier == verifier)
            .unwrap_or_else(|| PresenterEntry {
                verifier: verifier.clone(),
                admitted: BTreeMap::new(),
                expires_at,
                process_only,
            });
        entry.expires_at = expires_at;
        entry.admitted.retain(|_, at| in_time(*at, now));
        entry.admitted.insert(binding.policy_identity.clone(), now);
        while entry.admitted.len() > MAX_IDENTITIES_PER_BINDING {
            let oldest = entry
                .admitted
                .iter()
                .min_by_key(|(identity, presented_at)| (**presented_at, (*identity).clone()))
                .map(|(identity, _)| identity.clone())?;
            entry.admitted.remove(&oldest);
        }
        let of_kind = |set: &SourcePresenters| {
            set.presenters
                .values()
                .filter(|known| known.process_only == process_only)
                .count()
        };
        if process_only {
            // A process entry serves its own process only: the oldest of
            // them makes room, an issued presenter's entry never does.
            while of_kind(&set) >= MAX_PROCESS_PRESENTERS_PER_SOURCE {
                let oldest = set
                    .presenters
                    .iter()
                    .filter(|(_, known)| known.process_only)
                    .min_by_key(|(known_id, known)| (known.last_presented(), (*known_id).clone()))
                    .map(|(known_id, _)| known_id.clone())?;
                set.presenters.remove(&oldest);
            }
        } else if of_kind(&set) >= MAX_PRESENTERS_PER_SOURCE {
            // Full of live issued presenters: this one is not recorded, and
            // none of them is displaced.
            return None;
        }
        set.presenters.insert(id.clone(), entry);
        Some(set)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(project: &Path, object_ref: &str, digest: &str, identity: &str) -> PresentedBinding {
        PresentedBinding {
            project: canonical_project(project).expect("project"),
            caller: verified_principal(),
            role: crate::core::roles::active_role_name(),
            object_ref: object_ref.to_owned(),
            source_id: "source-one".to_owned(),
            content_digest: digest.to_owned(),
            policy_identity: identity.to_owned(),
        }
    }

    /// Two verified principals are two bindings: a presentation by one does
    /// not satisfy the other's, and a binding of another principal is not the
    /// request's own. Before, every binding carried the constant
    /// `local-operator`, so the caller separated nothing.
    #[test]
    fn a_presentation_by_one_principal_does_not_satisfy_another() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        let by = |principal: &str| {
            PresentedBinding::admitted_by(principal, &root, "ref-quartz", "source-one", "sha256:aa")
                .expect("policy identity available")
        };
        let mine = PresentedBinding::admitted_now(&root, "ref-quartz", "source-one", "sha256:aa")
            .expect("policy identity available");
        assert_eq!(mine, by(&verified_principal()));
        assert_ne!(verified_principal(), "another-verified-principal");
        let theirs = by("another-verified-principal");
        assert!(!mine.same_source(&theirs));

        assert!(record_presentation(&theirs));
        assert_eq!(theirs.presentation(), Presentation::Current);
        assert_eq!(
            mine.presentation(),
            Presentation::None,
            "their presentation is not mine"
        );
        crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(root.clone())),
            || {
                assert!(mine.matches_request());
                assert!(
                    !theirs.matches_request(),
                    "a binding of another principal is refused for this request"
                );
            },
        );

        // On disk the principal is part of the keyed hash only, and a
        // capability is in no form: not the secret, not its wire text.
        let issued = issue(60).expect("issued");
        with_presenter(Some(issued.capability.clone()), || {
            assert!(record_presentation(&theirs));
        });
        let raw = std::fs::read_to_string(REGISTER.shard_path(&theirs.key())).expect("shard");
        assert!(!raw.contains("another-verified-principal"), "{raw}");
        let wire = issued.capability.expose();
        let records = std::fs::read_to_string(PRESENTERS.shard_path(&issued.capability.id()))
            .expect("presenter record");
        for stored in [&raw, &records] {
            assert!(!stored.contains(&wire[CAPABILITY_PREFIX.len()..]));
        }
        assert_eq!(
            format!("{:?}", issued.capability),
            "PresenterCapability(<redacted>)"
        );
        assert!(PresenterCapability::parse(&wire).is_some());
        let upper = wire.to_uppercase();
        for malformed in ["", "lcp1_", &wire[..wire.len() - 1], upper.as_str()] {
            assert!(PresenterCapability::parse(malformed).is_none());
        }
    }

    /// The register by itself, as a second process reads it: validity,
    /// policy identity, exact binding, bounds, and no identifying text.
    #[test]
    fn a_presentation_holds_for_its_exact_binding_policy_and_validity_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        let start = register::now();
        let one = binding(&root, "ref-quartz", "sha256:aa", "policy-one");

        assert_eq!(one.presentation_at("policy-one", start), Presentation::None);
        assert!(record_presentation_at(&one, start));
        assert_eq!(
            one.presentation_at("policy-one", start),
            Presentation::Current
        );
        assert_eq!(
            one.presentation_at("policy-one", start + VALIDITY_SECONDS),
            Presentation::Current
        );
        assert_eq!(
            one.presentation_at("policy-one", start + VALIDITY_SECONDS + 1),
            Presentation::None,
            "not presented again within the validity"
        );
        assert_eq!(
            one.presentation_at("policy-two", start),
            Presentation::OtherPolicy,
            "presented, but not admitted under the current policy"
        );

        // Not the same binding: other content, other reference, other
        // caller, other role, other project.
        let other_content = binding(&root, "ref-quartz", "sha256:bb", "policy-one");
        let other_ref = binding(&root, "ref-basalt", "sha256:aa", "policy-one");
        let mut other_caller = one.clone();
        other_caller.caller = "another-principal".to_owned();
        let mut other_role = one.clone();
        other_role.role = "another-role".to_owned();
        let elsewhere = tempfile::tempdir().expect("project");
        let other_project = binding(elsewhere.path(), "ref-quartz", "sha256:aa", "policy-one");
        for different in [
            &other_content,
            &other_ref,
            &other_caller,
            &other_role,
            &other_project,
        ] {
            assert!(!one.same_source(different));
            assert_eq!(
                different.presentation_at("policy-one", start),
                Presentation::None
            );
        }

        // Presented again under the new policy: current again.
        let again = binding(&root, "ref-quartz", "sha256:aa", "policy-two");
        assert!(one.same_source(&again));
        assert!(record_presentation_at(&again, start + 5));
        assert_eq!(
            one.presentation_at("policy-two", start + 5),
            Presentation::Current
        );

        // Persisted, owner-only, and free of identifying text.
        let shard = REGISTER.shard_path(&one.key());
        let raw = std::fs::read_to_string(&shard).expect("persisted shard");
        let principal = verified_principal();
        for identifying in ["ref-quartz", "source-one", "sha256:aa", principal.as_str()] {
            assert!(!raw.contains(identifying), "{identifying}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&shard)
                .expect("shard")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only");
        }

        // Bound: policy identities per presenter entry.
        for index in 0..MAX_IDENTITIES_PER_BINDING + 2 {
            let identity = format!("policy-bound-{index}");
            let presented = binding(&root, "ref-quartz", "sha256:aa", &identity);
            assert!(record_presentation_at(&presented, start + 10));
        }
        let kept = REGISTER
            .read::<SourcePresenters>(&one.key(), start + 10)
            .expect("entry");
        assert_eq!(kept.presenters.len(), 1, "one presenter: this process");
        let own = kept.presenters.values().next().expect("own entry");
        assert_eq!(own.admitted.len(), MAX_IDENTITIES_PER_BINDING);

        // Bound: issued presenters per source. The set fills; a further one
        // is not recorded and displaces nobody.
        let issued: Vec<IssuedCapability> = (0..=MAX_PRESENTERS_PER_SOURCE)
            .map(|_| issue(60).expect("issued"))
            .collect();
        let recorded: Vec<bool> = issued
            .iter()
            .map(|presenter| {
                with_presenter(Some(presenter.capability.clone()), || {
                    record_presentation_at(&one, start + 10)
                })
            })
            .collect();
        assert_eq!(
            recorded.iter().filter(|recorded| **recorded).count(),
            MAX_PRESENTERS_PER_SOURCE
        );
        assert_eq!(recorded.last(), Some(&false), "the one too many");
        with_presenter(Some(issued[0].capability.clone()), || {
            assert_eq!(
                one.presentation_at("policy-one", start + 10),
                Presentation::Current,
                "the first presenter kept its entry"
            );
        });
    }
}
