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

use lean_ctx_protocol::context_gateway::ContextDecisionV1;

const CAPACITY: usize = 4_096;

type Key = [u8; 32];

struct Memo {
    order: VecDeque<Key>,
    decisions: HashMap<Key, Arc<ContextDecisionV1>>,
}

fn memo() -> &'static Mutex<Memo> {
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    MEMO.get_or_init(|| {
        Mutex::new(Memo {
            order: VecDeque::with_capacity(CAPACITY),
            decisions: HashMap::with_capacity(CAPACITY),
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
    memo()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .decisions
        .get(key)
        .cloned()
}

#[cfg(test)]
pub(super) fn clear() {
    let mut memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    memo.order.clear();
    memo.decisions.clear();
}

pub(super) fn insert(key: Key, decision: ContextDecisionV1) {
    let mut memo = memo().lock().unwrap_or_else(PoisonError::into_inner);
    if memo.decisions.insert(key, Arc::new(decision)).is_some() {
        return;
    }
    memo.order.push_back(key);
    if memo.order.len() > CAPACITY
        && let Some(oldest) = memo.order.pop_front()
    {
        memo.decisions.remove(&oldest);
    }
}
