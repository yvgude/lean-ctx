// SPDX-License-Identifier: Apache-2.0
//! Bounded memo of content admitted unchanged and without any finding, keyed
//! by the content *and* the policy that admitted it. Admission is a pure
//! function of (text, path, policy) for built-in detectors, so a hit is exactly
//! the result a fresh scan would produce; re-reads of clean files skip it.
//! Results with any reason (findings, timeouts, partial coverage) are never
//! memoized, so a budget-limited run is always re-evaluated. The memo keeps the
//! content-free decision, so a hit still appears in the call's receipt.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use lean_ctx_protocol::context_gateway::{ClassificationV1, ContextDecisionV1};

const CAPACITY: usize = 4_096;

type Key = [u8; 32];

pub(super) struct CachedAdmission {
    pub(super) decision: ContextDecisionV1,
    pub(super) classification: ClassificationV1,
    pub(super) storable: bool,
}

enum CachedEntry {
    Decision(Arc<ContextDecisionV1>),
    Source(Arc<CachedAdmission>),
}

struct Memo {
    order: VecDeque<Key>,
    entries: HashMap<Key, Arc<CachedEntry>>,
}

fn memo() -> &'static Mutex<Memo> {
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    MEMO.get_or_init(|| {
        Mutex::new(Memo {
            order: VecDeque::with_capacity(CAPACITY),
            entries: HashMap::with_capacity(CAPACITY),
        })
    })
}

/// The key binds the policy fingerprint, the path and the exact bytes.
pub(super) fn key(policy_fingerprint: &[u8], path: &str, text: &str) -> Key {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(policy_fingerprint.len() as u64).to_le_bytes());
    hasher.update(policy_fingerprint);
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path.as_bytes());
    hasher.update(text.as_bytes());
    *hasher.finalize().as_bytes()
}

pub(super) fn get(key: &Key) -> Option<Arc<ContextDecisionV1>> {
    let memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    match memo.entries.get(key)?.as_ref() {
        CachedEntry::Decision(decision) => Some(decision.clone()),
        CachedEntry::Source(admission) => Some(Arc::new(admission.decision.clone())),
    }
}

pub(super) fn get_source(key: &Key) -> Option<Arc<CachedAdmission>> {
    let memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    match memo.entries.get(key)?.as_ref() {
        CachedEntry::Source(admission) => Some(admission.clone()),
        CachedEntry::Decision(_) => None,
    }
}

#[cfg(test)]
pub(super) fn clear() {
    let mut memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    memo.order.clear();
    memo.entries.clear();
}

pub(super) fn insert(key: Key, decision: ContextDecisionV1) {
    insert_entry(key, CachedEntry::Decision(Arc::new(decision)));
}

pub(super) fn insert_source(
    key: Key,
    decision: ContextDecisionV1,
    classification: ClassificationV1,
    storable: bool,
) {
    insert_entry(
        key,
        CachedEntry::Source(Arc::new(CachedAdmission {
            decision,
            classification,
            storable,
        })),
    );
}

fn insert_entry(key: Key, entry: CachedEntry) {
    let mut memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    if memo.entries.insert(key, Arc::new(entry)).is_some() {
        return;
    }
    memo.order.push_back(key);
    if memo.order.len() > CAPACITY
        && let Some(oldest) = memo.order.pop_front()
    {
        memo.entries.remove(&oldest);
    }
}
