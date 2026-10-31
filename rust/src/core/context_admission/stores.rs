// SPDX-License-Identifier: Apache-2.0
//! The one admitted read for derived stores and search views (G5, E3).
//!
//! Indexes, caches and search paths used to read sources raw: a key in a
//! source file reached the persistent BM25 index on disk, the shared content
//! cache and search snippets, even though `ctx_read` masked it. Every such
//! builder now reads through a [`StoreAdmission`]:
//!
//! - content the gateway withholds, and `restricted` content (secret-like
//!   paths, `TOP SECRET` markings, delivered secrets), never enters a store;
//! - everything else enters exactly as it was admitted (masked);
//! - a policy change invalidates what was admitted under the old one: the
//!   shared content cache is dropped, persisted indexes record the digest of
//!   the policy they were built under and are rebuilt when it differs.
//!
//! A builder resolves one snapshot and uses it for every file, so a build
//! never mixes two policies and records exactly the one it applied. Resident
//! stores are bound to the policy *epoch*: observing a new policy advances
//! it and drops them, and an insert carrying an older epoch is refused, so a
//! build still running under the old policy cannot repopulate them.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::core::input_filters::FilterAction;
use crate::core::policy::runtime::PublicationAuthority;

use super::{
    AdmissionPolicy, OutputCompleteness, admit, admit_output_for_poll, admit_output_for_storage,
    clean, fingerprint_digest,
};

/// One resolved admission policy, applied to every object of a store build.
#[derive(Debug, Clone)]
pub struct StoreAdmission {
    policy: AdmissionPolicy,
    fingerprint: Vec<u8>,
    digest: String,
    epoch: u64,
}

/// Advances whenever the observed admission policy changes.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// The epoch of the most recently observed admission policy.
#[must_use]
pub fn current_epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

impl StoreAdmission {
    /// The current policy. Observing a changed policy advances the epoch and
    /// drops the resident stores filled under the previous one.
    #[must_use]
    pub fn current() -> Self {
        let policy = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
        let mut admission = Self::with_policy(policy);
        admission.epoch = observe_policy(&admission.digest);
        admission
    }

    /// A snapshot of an explicit policy (tests, tools with their own policy).
    /// It is not observed: it never drops resident stores, and it may only
    /// fill them while its policy is the observed one.
    #[must_use]
    pub fn with_policy(policy: AdmissionPolicy) -> Self {
        let fingerprint = policy.fingerprint();
        let digest = fingerprint_digest(&fingerprint).hex().to_owned();
        Self {
            policy,
            fingerprint,
            digest,
            epoch: u64::MAX,
        }
    }

    /// Hex digest of the policy, persisted next to admitted content.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The immutable detector snapshot used to admit a bounded source batch.
    /// Callers must still reauthorize this snapshot before publishing derived data.
    pub(crate) fn admission_policy(&self) -> &AdmissionPolicy {
        &self.policy
    }

    /// Whether text admitted by this snapshot may still enter a resident
    /// store: its policy is the most recently observed one.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.epoch == current_epoch()
    }

    /// The epoch this snapshot was observed at.
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Revalidate both the project authority and global Gateway content digest
    /// immediately before an admitted filesystem publication.
    pub(crate) fn authorize_publication(
        &self,
        authority: &PublicationAuthority,
    ) -> Result<(), String> {
        if self.epoch == u64::MAX || !self.is_current() || !self.matches_current_config() {
            return Err("session admission policy changed; retry".into());
        }
        authority.verify()
    }

    fn matches_current_config(&self) -> bool {
        // Config::load_arc reads and content-hashes its inputs on every call.
        // Do not use StoreAdmission::current here: it mutates the observed epoch
        // and resident stores while callers hold a per-project memory lock.
        let config = crate::core::config::Config::load_arc();
        let policy = AdmissionPolicy::from_config(&config);
        fingerprint_digest(&policy.fingerprint()).hex() == self.digest
    }

    /// Admit `text` read from `path`. `None`: it must not be stored or served
    /// (withheld, or restricted).
    #[must_use]
    pub fn admit(&self, text: &str, path: &Path) -> Option<String> {
        self.admit_object(text, Some(path))
    }

    /// Admit text with no source path (tool output entering an archive or a
    /// recovery store). `None`: it must not be stored.
    #[must_use]
    pub fn admit_text(&self, text: &str) -> Option<String> {
        self.admit_object(text, None)
    }

    /// The same snapshot with the advisory semantic stage left out, for
    /// strings that must stay exact (identifiers, configuration tags, paths).
    /// There a model finding could only withhold the whole record — a user
    /// name in a project path read as a person ended every session save —
    /// while semantic detection never blocks on its own. The built-in floor
    /// still inspects them; content fields keep the semantic stage.
    #[must_use]
    pub(crate) fn built_in_floor(&self) -> Self {
        let mut floor = self.clone();
        floor.policy.semantic = None;
        floor.fingerprint = floor.policy.fingerprint();
        floor
    }

    /// Admit a parsed Engine session ID as typed metadata. Only the PII
    /// detector is skipped; all other Gateway detectors still inspect it.
    #[must_use]
    pub(crate) fn admit_engine_session_id(
        &self,
        id: crate::core::session::EngineSessionId<'_>,
    ) -> Option<String> {
        let text = id.as_str();
        let mut policy = self.policy.clone();
        policy.gateway.pii = FilterAction::Off;
        let admission = admit(text, None, &policy);
        (admission.storable() && admission.text.as_deref() == Some(text)).then(|| text.to_owned())
    }

    /// Admit a shell command before writing it to persistent shell artifacts.
    #[must_use]
    pub(crate) fn admit_shell_command(&self, text: &str) -> Option<String> {
        // Project policy sees original text before Gateway redactions.
        crate::core::policy::content::protect_active(text).ok()?;
        let admitted = self.admit_text(text)?;
        crate::core::policy::content::protect_active(&admitted)
            .ok()
            .map(std::borrow::Cow::into_owned)
    }

    /// Admit captured shell output for persistent retention. Incomplete capture
    /// is never made storable, even when Developer mode permits live delivery.
    #[must_use]
    pub(crate) fn admit_shell_output(
        &self,
        text: &str,
        completeness: OutputCompleteness,
    ) -> Option<String> {
        super::semantic_deferred::for_stored_context(|| {
            admit_output_for_storage(text, completeness, &self.policy)
        })
    }

    /// Admit a finished background job's output for its job record, read
    /// back by a status poll (#2164). Unlike `admit_shell_output`, a capture
    /// whose only gap is a pipe still held by a process the command left
    /// running is kept; delivery re-admits it with its real completeness.
    #[must_use]
    pub(crate) fn admit_background_output(
        &self,
        text: &str,
        completeness: OutputCompleteness,
    ) -> Option<String> {
        super::semantic_deferred::for_stored_context(|| {
            admit_output_for_poll(text, completeness, &self.policy)
        })
    }

    fn admit_object(&self, text: &str, path: Option<&Path>) -> Option<String> {
        if !self.policy.enabled_effective() {
            return Some(text.to_owned());
        }
        // Clean memo: only built-in detectors are pure functions of the input.
        // A secret-like path is restricted whatever its content, so it never
        // takes the memo shortcut.
        let label = path.map(|path| path.to_string_lossy());
        let key = self
            .policy
            .semantic
            .is_none()
            .then(|| clean::key(&self.fingerprint, label.as_deref().unwrap_or(""), text));
        if let Some(key) = &key
            && clean::get(key).is_some()
            && path.is_none_or(|path| crate::core::io_boundary::is_secret_like(path).is_none())
        {
            return Some(text.to_owned());
        }
        let admission =
            super::semantic_deferred::for_stored_context(|| admit(text, path, &self.policy));
        if !admission.storable() {
            return None;
        }
        if let Some(key) = key
            && admission.decision.reason_codes.is_empty()
            && admission.text.as_deref() == Some(text)
        {
            clean::insert(key, admission.decision.clone());
        }
        admission.text
    }

    /// Read `path` as text, admitted. A source the decoder could not
    /// represent completely (invalid UTF-8 replaced, UTF-16 tail dropped,
    /// unpaired surrogate) is never a complete source: it does not enter a
    /// cache, an index or any other derived store, in any mode.
    #[must_use]
    pub fn read(&self, path: &Path) -> Option<String> {
        let decoded = crate::core::text_decode::read_text_checked(path).ok()?;
        if !decoded.complete() {
            tracing::debug!(
                "store admission: incompletely decoded source skipped ({})",
                decoded.encoding.label()
            );
            return None;
        }
        self.admit(&decoded.text, path)
    }
}

/// Records `digest` as the observed policy and returns its epoch. A change
/// advances the epoch *before* dropping the resident stores: an insert that
/// raced the change either sees the new epoch and is refused, or landed
/// first and is dropped with the rest.
fn observe_policy(digest: &str) -> u64 {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if last.as_deref() != Some(digest) {
        if last.is_some() {
            EPOCH.fetch_add(1, Ordering::SeqCst);
            crate::core::content_cache::clear();
            crate::core::search_index::clear_resident();
        }
        *last = Some(digest.to_owned());
    }
    current_epoch()
}

/// [`StoreAdmission::read`] under the current policy, for one-off reads.
#[must_use]
pub fn admitted_store_text(path: &Path) -> Option<String> {
    StoreAdmission::current().read(path)
}
