use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::core::archive::authority::ArchiveAuthority;
use crate::core::policy::{content, runtime};

#[derive(Clone)]
struct RefEntry {
    content: Arc<str>,
    authority: Option<ArchiveAuthority>,
    created_at: Instant,
}

const MAX_ENTRIES: usize = 200;
const MAX_STORE_BYTES: usize = 64 * 1024 * 1024;
const TTL: Duration = Duration::from_mins(5);

fn store_lock() -> &'static Mutex<HashMap<String, RefEntry>> {
    static STORE: OnceLock<Mutex<HashMap<String, RefEntry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

const RESOLVE_LOCK_WAIT: Duration = Duration::from_millis(50);

fn lock_briefly() -> Option<std::sync::MutexGuard<'static, HashMap<String, RefEntry>>> {
    let deadline = Instant::now() + RESOLVE_LOCK_WAIT;
    loop {
        match store_lock().try_lock() {
            Ok(map) => return Some(map),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_micros(200));
            }
            Err(std::sync::TryLockError::WouldBlock) => return None,
        }
    }
}

/// Store unbound Community output. Mandatory policy requires observed origin.
pub fn store(content: &str) -> Option<String> {
    store_with_authority(content, None)
}

pub(crate) fn store_with_authority(
    text: &str,
    authority: Option<&ArchiveAuthority>,
) -> Option<String> {
    runtime::with_source_view(|| {
        if text.len() > entry_budget()
            || (runtime::is_active() && authority.is_none())
            || authority.is_some_and(|origin| !origin.admitted())
        {
            return None;
        }
        let text = content::protect_active(text).ok()?;
        // A recovery store holds admitted text only, never withheld or
        // restricted content (G5, E3).
        let text = crate::core::context_admission::recovery::admit_for_storage(&text)?;
        if text.len() > entry_budget() {
            return None;
        }
        let id = reference_id(&text, authority)?;
        // Optional compression must never park a request behind a shared mutex.
        let mut map = store_lock().try_lock().ok()?;
        map.retain(|_, entry| entry.created_at.elapsed() < TTL);
        let mut bytes: usize = map.values().map(|entry| entry.content.len()).sum();
        if let Some(previous) = map.remove(&id) {
            bytes -= previous.content.len();
        }
        while map.len() >= MAX_ENTRIES || bytes.saturating_add(text.len()) > MAX_STORE_BYTES {
            let oldest = map
                .iter()
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(key, _)| key.clone())?;
            bytes -= map.remove(&oldest)?.content.len();
        }
        map.insert(
            id.clone(),
            RefEntry {
                content: Arc::from(text.as_str()),
                authority: authority.cloned(),
                created_at: Instant::now(),
            },
        );
        Some(id)
    })
    .ok()
    .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReferenceResolveError {
    Malformed,
    Missing,
    Expired,
    /// Present, but its source authority, project binding or the current
    /// content rules do not allow it to be handed back. Content-free.
    Refused(&'static str),
}

/// Detailed form shared by the MCP resolver and the recovery verifier. The
/// lookup is cheap and lock-scoped; every source, policy and role check runs
/// after the store lock is released.
pub(crate) fn resolve_checked(id: &str) -> Result<String, ReferenceResolveError> {
    let Some(digest) = id.strip_prefix("ref_") else {
        return Err(ReferenceResolveError::Malformed);
    };
    if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ReferenceResolveError::Malformed);
    }
    let entry = {
        // An explicit resolve waits out a concurrent store (microseconds),
        // but a store held longer degrades to "missing" instead of parking.
        let Some(mut map) = lock_briefly() else {
            return Err(ReferenceResolveError::Missing);
        };
        let Some(entry) = map.get(id) else {
            return Err(ReferenceResolveError::Missing);
        };
        if entry.created_at.elapsed() >= TTL {
            map.remove(id);
            return Err(ReferenceResolveError::Expired);
        }
        entry.clone()
    };
    // Do not infer a protected HTTP request's identity from the proxy's cwd.
    if entry.authority.is_some()
        && !runtime::REQUEST_PROJECT
            .try_with(|project| project.borrow().is_some())
            .unwrap_or(false)
    {
        return Err(ReferenceResolveError::Refused(
            "reference is bound to a project this request does not name",
        ));
    }
    runtime::with_source_view(|| {
        if runtime::is_active() && entry.authority.is_none() {
            return Err(ReferenceResolveError::Refused(
                "stored output has no source authority under the active policy",
            ));
        }
        if entry
            .authority
            .as_ref()
            .is_some_and(|origin| !origin.admitted())
            || reference_id(&entry.content, entry.authority.as_ref()).as_deref() != Some(id)
        {
            return Err(ReferenceResolveError::Refused(
                "the reference's source is no longer admitted",
            ));
        }
        content::protect_active(&entry.content)
            .map(std::borrow::Cow::into_owned)
            .map_err(|_| {
                ReferenceResolveError::Refused("stored output is withheld by current content rules")
            })
    })
    .unwrap_or(Err(ReferenceResolveError::Refused(
        "current policy cannot be verified",
    )))
}

/// Back-compatible resolver used by `ctx_expand` and the proxy.
pub fn resolve(id: &str) -> Option<String> {
    resolve_checked(id).ok()
}

#[cfg(test)]
pub(crate) fn expire_for_test(id: &str) {
    if let Some(entry) = store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_mut(id)
    {
        entry.created_at = Instant::now()
            .checked_sub(TTL + Duration::from_secs(1))
            .expect("test timestamp supports TTL subtraction");
    }
}

pub fn stats() -> (usize, usize) {
    let Ok(map) = store_lock().try_lock() else {
        return (0, 0);
    };
    let live = map
        .values()
        .filter(|entry| entry.created_at.elapsed() < TTL);
    live.fold((0, 0), |(count, bytes), entry| {
        (count + 1, bytes + entry.content.len())
    })
}

fn entry_budget() -> usize {
    crate::core::limits::max_read_bytes().min(content::MAX_PROTECTED_CONTENT_BYTES)
}

fn reference_id(text: &str, authority: Option<&ArchiveAuthority>) -> Option<String> {
    let digest = if let Some(origin) = authority {
        origin.id(text)?
    } else {
        let mut hash = blake3::Hasher::new();
        hash.update(b"leanctx-public-reference-v1\0");
        hash.update(text.as_bytes());
        hash.finalize().to_hex().to_string()
    };
    Some(format!("ref_{digest}"))
}

#[cfg(test)]
mod tests;
