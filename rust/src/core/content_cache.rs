//! Resident, bounded file-content cache shared across the search-index build and
//! `ctx_search` (issue #148).
//!
//! Before this module the trigram [`bm25_index`](crate::core::chunk_data)
//! build read *every* file in the corpus to extract trigrams and then threw the
//! content away, after which `ctx_search` read the narrowed candidate files
//! **again** to run the regex line-by-line — the corpus was read from disk
//! twice. This cache lets the first reader (whichever it is) populate file
//! contents once, keyed by absolute path and validated by `(mtime, size)`, and
//! every subsequent reader reuse them as an in-memory hit.
//!
//! Correctness: an entry is only ever served when the file's *current*
//! `(mtime, size)` exactly matches the stored identity, so any edit (which
//! changes mtime, and usually size) is a guaranteed miss — results can never go
//! stale. A miss simply falls back to a disk read.
//!
//! Bounds & safety:
//! - Total resident bytes are capped (`LEAN_CTX_CONTENT_CACHE_MB`, default
//!   128 MB) with approximate-LRU eviction, so a large corpus cannot grow the
//!   cache without limit.
//! - Inserts are skipped while the process is under memory pressure, and the
//!   eviction orchestrator can [`clear`] the cache on `UnloadIndices` /
//!   `EmergencyDrop`.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

/// Default resident byte budget when `LEAN_CTX_CONTENT_CACHE_MB` is unset.
const DEFAULT_BUDGET_MB: usize = 128;

/// Identity of one file *version*. A changed mtime or size ⇒ stale ⇒ cache miss.
/// Mirrors the `(mtime, size)` pair the BM25 index already trusts for staleness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    pub mtime_ms: u64,
    pub size_bytes: u64,
}

impl FileState {
    /// Build from an already-`stat`ed [`Metadata`] (no extra syscall) — callers
    /// in the hot path typically have this in hand from their size/regular-file
    /// checks. Returns `None` only when the platform cannot report mtime.
    #[must_use]
    pub fn from_metadata(meta: &Metadata) -> Option<Self> {
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)?;
        Some(Self {
            mtime_ms,
            size_bytes: meta.len(),
        })
    }

    /// Convenience: `stat` the path then build the state. Costs one syscall.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        Self::from_metadata(&path.metadata().ok()?)
    }
}

struct Entry {
    state: FileState,
    content: Arc<str>,
    /// Logical clock tick of the last hit/insert — drives approximate LRU.
    last_used: u64,
}

struct Cache {
    map: HashMap<PathBuf, Entry>,
    total_bytes: usize,
    budget_bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    inserts: u64,
    evictions: u64,
}

impl Cache {
    fn new(budget_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            total_bytes: 0,
            budget_bytes,
            clock: 0,
            hits: 0,
            misses: 0,
            inserts: 0,
            evictions: 0,
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn remove_entry(&mut self, path: &Path) {
        if let Some(old) = self.map.remove(path) {
            self.total_bytes = self.total_bytes.saturating_sub(old.content.len());
        }
    }

    /// Evict approximate-LRU entries until the budget is satisfied. Eviction
    /// only runs after an over-budget insert, so the `O(n)` min-scan is rare and
    /// dwarfed by the disk reads it prevents.
    fn evict_to_budget(&mut self) {
        while self.total_bytes > self.budget_bytes && !self.map.is_empty() {
            let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.remove_entry(&victim);
            self.evictions += 1;
        }
    }
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn budget_bytes() -> usize {
    let mb = std::env::var("LEAN_CTX_CONTENT_CACHE_MB")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_BUDGET_MB);
    mb.saturating_mul(1024 * 1024)
}

fn disabled() -> bool {
    // A zero byte budget (or the explicit disable flag) turns the cache into a
    // no-op pass-through — every `get` misses and `insert` is dropped.
    std::env::var("LEAN_CTX_DISABLE_CONTENT_CACHE")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        || budget_bytes() == 0
}

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| Mutex::new(Cache::new(budget_bytes())))
}

fn lock() -> std::sync::MutexGuard<'static, Cache> {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Look up `path`; returns the cached content only when the supplied current
/// `(mtime, size)` matches the stored identity. A mismatch evicts the stale
/// entry and reports a miss. `state` is passed in (not re-`stat`ed) because hot
/// callers already hold the metadata.
#[must_use]
pub fn get(path: &Path, current: FileState) -> Option<Arc<str>> {
    if disabled() {
        return None;
    }
    let mut c = lock();
    let Some(entry) = c.map.get(path) else {
        c.misses += 1;
        return None;
    };
    let matches = entry.state == current;
    if !matches {
        // Stale version cached — drop it so we don't keep paying for it.
        c.remove_entry(path);
        c.misses += 1;
        return None;
    }
    let tick = c.tick();
    c.hits += 1;
    // The entry is present under the lock we still hold, but degrade gracefully
    // instead of panicking on the read hot path if that invariant ever changes.
    let entry = c.map.get_mut(path)?;
    entry.last_used = tick;
    Some(Arc::clone(&entry.content))
}

/// Insert (or replace) the content for `path` at version `state`. Skipped while
/// the process is under memory pressure or when the cache is disabled, so the
/// cache never *adds* to a memory problem.
pub fn insert(path: &Path, state: FileState, content: Arc<str>) {
    if disabled() || crate::core::memory_guard::is_under_pressure() {
        return;
    }
    let len = content.len();
    let mut c = lock();
    // A single file larger than the whole budget would thrash eviction — skip it.
    if len > c.budget_bytes {
        return;
    }
    c.remove_entry(path);
    let tick = c.tick();
    c.map.insert(
        path.to_path_buf(),
        Entry {
            state,
            content,
            last_used: tick,
        },
    );
    c.total_bytes += len;
    c.inserts += 1;
    if c.total_bytes > c.budget_bytes {
        c.evict_to_budget();
    }
}

/// Read a file through the cache: returns cached content on a fresh hit, else
/// reads from disk (UTF-8), populates the cache, and returns it. `None` on a
/// non-UTF-8/unreadable/unstatable file. Convenience for callers without their
/// own size/special-file gating (the search-index build and `ctx_search` use
/// the explicit [`get`]/[`insert`] pair so they keep their own skip rules).
#[must_use]
pub fn get_or_read(path: &Path) -> Option<Arc<str>> {
    let state = FileState::from_path(path)?;
    if let Some(hit) = get(path, state) {
        return Some(hit);
    }
    let content = std::fs::read_to_string(path).ok()?;
    let arc: Arc<str> = Arc::from(content);
    insert(path, state, Arc::clone(&arc));
    Some(arc)
}

/// Drop all entries, freeing the heap. Called by the eviction orchestrator under
/// memory pressure; the cache simply re-warms on subsequent reads.
pub fn clear() {
    if CACHE.get().is_none() {
        return;
    }
    let mut c = lock();
    c.map.clear();
    c.total_bytes = 0;
}

/// Approximate resident heap used by cached contents, in bytes.
pub fn memory_usage_bytes() -> usize {
    if CACHE.get().is_none() {
        return 0;
    }
    lock().total_bytes
}

/// Observability snapshot: `(hits, misses, entries, bytes, evictions)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
    pub inserts: u64,
    pub evictions: u64,
}

pub fn stats() -> CacheStats {
    if CACHE.get().is_none() {
        return CacheStats::default();
    }
    let c = lock();
    CacheStats {
        hits: c.hits,
        misses: c.misses,
        entries: c.map.len(),
        bytes: c.total_bytes,
        inserts: c.inserts,
        evictions: c.evictions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is a process-wide global and tests mutate it (and the budget
    /// env var). Serialize them so they cannot observe each other's state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fresh_cache(budget_bytes: usize) {
        crate::test_env::remove_var("LEAN_CTX_CONTENT_CACHE_MB");
        crate::test_env::remove_var("LEAN_CTX_DISABLE_CONTENT_CACHE");
        let mut c = lock();
        *c = Cache::new(budget_bytes);
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn hit_after_insert_with_matching_state() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh_cache(1024 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.rs", "fn main() {}\n");
        let state = FileState::from_path(&p).unwrap();
        assert!(get(&p, state).is_none(), "cold cache must miss");
        insert(&p, state, Arc::from("fn main() {}\n"));
        let got = get(&p, state).expect("warm cache must hit");
        assert_eq!(&*got, "fn main() {}\n");
    }

    #[test]
    fn mtime_or_size_change_invalidates() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh_cache(1024 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.rs", "v1\n");
        let s1 = FileState::from_path(&p).unwrap();
        insert(&p, s1, Arc::from("v1\n"));
        assert!(get(&p, s1).is_some());

        // Different size ⇒ different state ⇒ miss, and the stale entry is dropped.
        let s_bigger = FileState {
            size_bytes: s1.size_bytes + 10,
            ..s1
        };
        assert!(get(&p, s_bigger).is_none(), "size change must miss");
        assert!(
            get(&p, s1).is_none(),
            "stale entry must be evicted on mismatch"
        );

        // Different mtime ⇒ miss as well.
        insert(&p, s1, Arc::from("v1\n"));
        let s_newer = FileState {
            mtime_ms: s1.mtime_ms + 1,
            ..s1
        };
        assert!(get(&p, s_newer).is_none(), "mtime change must miss");
    }

    #[test]
    fn get_or_read_populates_then_serves_from_cache() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh_cache(1024 * 1024);
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.rs", "hello world\n");

        let before = stats();
        let first = get_or_read(&p).unwrap();
        assert_eq!(&*first, "hello world\n");
        let after_first = stats();
        assert_eq!(
            after_first.inserts,
            before.inserts + 1,
            "first read inserts"
        );

        let second = get_or_read(&p).unwrap();
        assert_eq!(&*second, "hello world\n");
        let after_second = stats();
        assert_eq!(
            after_second.inserts, after_first.inserts,
            "second read must NOT re-insert (served from cache)"
        );
        assert!(after_second.hits > after_first.hits, "second read is a hit");
    }

    #[test]
    fn eviction_keeps_cache_within_budget() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Budget fits ~2 small files; a third insert must evict the LRU one.
        fresh_cache(64);
        let dir = tempfile::tempdir().unwrap();
        let pa = write(dir.path(), "a", "aaaaaaaaaaaaaaaaaaaaaaaaaaaa"); // 28 bytes
        let pb = write(dir.path(), "b", "bbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let pc = write(dir.path(), "c", "cccccccccccccccccccccccccccc");
        let sa = FileState::from_path(&pa).unwrap();
        let sb = FileState::from_path(&pb).unwrap();
        let sc = FileState::from_path(&pc).unwrap();

        insert(&pa, sa, Arc::from("aaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        // Touch a so b becomes the LRU victim.
        let _ = get(&pa, sa);
        insert(&pb, sb, Arc::from("bbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
        let _ = get(&pa, sa);
        insert(&pc, sc, Arc::from("cccccccccccccccccccccccccccc"));

        let st = stats();
        assert!(st.bytes <= 64, "cache must respect byte budget: {st:?}");
        assert!(st.evictions >= 1, "an eviction must have occurred: {st:?}");
        assert!(get(&pa, sa).is_some(), "recently-used entry must survive");
    }

    #[test]
    fn disabled_via_zero_budget_is_passthrough() {
        let _g = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fresh_cache(1024 * 1024);
        crate::test_env::set_var("LEAN_CTX_CONTENT_CACHE_MB", "0");
        let dir = tempfile::tempdir().unwrap();
        let p = write(dir.path(), "a.rs", "x\n");
        let state = FileState::from_path(&p).unwrap();
        insert(&p, state, Arc::from("x\n"));
        assert!(get(&p, state).is_none(), "zero-budget cache is a no-op");
        crate::test_env::remove_var("LEAN_CTX_CONTENT_CACHE_MB");
    }
}
