use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

struct RefEntry {
    content: String,
    created_at: Instant,
    access_count: u32,
}

const MAX_ENTRIES: usize = 200;
const TTL: Duration = Duration::from_mins(5);

fn store_lock() -> &'static Mutex<HashMap<String, RefEntry>> {
    static STORE: OnceLock<Mutex<HashMap<String, RefEntry>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn store(content: String) -> String {
    let id = format!("ref_{}", generate_id());
    let mut map = store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    map.retain(|_, v| v.created_at.elapsed() < TTL);

    while map.len() >= MAX_ENTRIES {
        if let Some(oldest_key) = map
            .iter()
            .min_by_key(|(_, v)| v.created_at)
            .map(|(k, _)| k.clone())
        {
            map.remove(&oldest_key);
        } else {
            break;
        }
    }

    map.insert(
        id.clone(),
        RefEntry {
            content,
            created_at: Instant::now(),
            access_count: 0,
        },
    );

    id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReferenceResolveError {
    Malformed,
    Missing,
    Expired,
}

/// Detailed form shared by the MCP resolver and the recovery verifier.
pub(crate) fn resolve_checked(id: &str) -> Result<String, ReferenceResolveError> {
    let Some(digest) = id.strip_prefix("ref_") else {
        return Err(ReferenceResolveError::Malformed);
    };
    if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ReferenceResolveError::Malformed);
    }

    let mut map = store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = map.get_mut(id) {
        if entry.created_at.elapsed() < TTL {
            entry.access_count += 1;
            return Ok(entry.content.clone());
        }
        map.remove(id);
        return Err(ReferenceResolveError::Expired);
    }
    Err(ReferenceResolveError::Missing)
}

/// Back-compatible resolver used by `ctx_expand`.
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
    let map = store_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let total = map.len();
    let total_bytes: usize = map.values().map(|v| v.content.len()).sum();
    (total, total_bytes)
}

fn generate_id() -> String {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{ts:x}")
}
