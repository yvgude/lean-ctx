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

use super::{AdmissionPolicy, admit, clean, fingerprint_digest};

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

    fn admit_object(&self, text: &str, path: Option<&Path>) -> Option<String> {
        if !self.policy.gateway.enabled_effective() {
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
        let admission = admit(text, path, &self.policy);
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

    /// Read `path` as text, admitted.
    #[must_use]
    pub fn read(&self, path: &Path) -> Option<String> {
        let text = crate::core::text_decode::read_text(path).ok()?;
        self.admit(&text, path)
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
