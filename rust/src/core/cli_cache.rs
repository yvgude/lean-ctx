use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_TTL_SECS: u64 = 300;
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CliCacheEntry {
    pub path: String,
    pub hash: String,
    pub line_count: usize,
    pub original_tokens: usize,
    pub timestamp: u64,
    pub read_count: u32,
    /// Process-lifetime nonce scoping a hit to the process that actually
    /// delivered the file's full content. A fresh process carries a different
    /// nonce, so it always misses and receives content instead of a
    /// `cached … [NL]` stub (#1459). `serde(default)` keeps older on-disk
    /// entries (written before this field existed) parseable; they simply never
    /// match and are overwritten on the next read.
    #[serde(default)]
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct CliCacheStore {
    pub entries: HashMap<String, CliCacheEntry>,
    pub total_hits: u64,
    pub total_reads: u64,
}

pub(crate) enum CacheResult {
    Hit {
        entry: CliCacheEntry,
        file_ref: String,
    },
    Miss {
        content: String,
    },
}

fn cache_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("cli-cache"))
}

fn cache_file() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("cache.json"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A value unique to this process for its whole lifetime, generated once and
/// reused. Scopes cache hits to the process that first delivered the content:
/// a one-shot `lean-ctx read` invocation is its own process, so a subsequent
/// invocation (new nonce) can never be served a stub from a previous run (#1459).
fn process_nonce() -> &'static str {
    static NONCE: OnceLock<String> = OnceLock::new();
    NONCE
        .get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}-{}", std::process::id(), nanos)
        })
        .as_str()
}

fn compute_md5(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    crate::core::agent_identity::hex_encode(&hasher.finalize())
}

fn normalize_key(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

/// Serializes every load → modify → save of the store within this process.
/// Without it, two concurrent callers (parallel tool calls in the daemon, or
/// tests) each load the file, change their own key and write it back — and the
/// later write silently drops the earlier caller's entry.
fn store_guard() -> std::sync::MutexGuard<'static, ()> {
    static STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn load_store() -> CliCacheStore {
    let Some(path) = cache_file() else {
        return CliCacheStore::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => CliCacheStore::default(),
    }
}

fn save_store(store: &CliCacheStore) {
    let Some(dir) = cache_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("cache.json");
    if let Ok(data) = serde_json::to_string(store) {
        let _ = std::fs::write(path, data);
    }
}

fn file_ref(key: &str, store: &CliCacheStore) -> String {
    let keys: Vec<&String> = store.entries.keys().collect();
    let idx = keys
        .iter()
        .position(|k| k.as_str() == key)
        .unwrap_or(store.entries.len());
    format!("F{}", idx + 1)
}

pub(crate) fn check_and_read(path: &str) -> CacheResult {
    let Ok(content) = crate::core::io_boundary::read_file_lossy(path) else {
        return CacheResult::Miss {
            content: String::new(),
        };
    };

    let key = normalize_key(path);
    let hash = compute_md5(&content);
    let now = now_secs();
    let _guard = store_guard();
    let mut store = load_store();

    store.total_reads += 1;

    if let Some(entry) = store.entries.get_mut(&key)
        && entry.hash == hash
        && entry.nonce == process_nonce()
        && now.saturating_sub(entry.timestamp) < CACHE_TTL_SECS
    {
        entry.read_count += 1;
        entry.timestamp = now;
        store.total_hits += 1;
        let result = CacheResult::Hit {
            entry: entry.clone(),
            file_ref: file_ref(&key, &store),
        };
        save_store(&store);
        return result;
    }

    let line_count = content.lines().count();
    let original_tokens = crate::core::tokens::count_tokens(&content);

    let entry = CliCacheEntry {
        path: key.clone(),
        hash,
        line_count,
        original_tokens,
        timestamp: now,
        read_count: 1,
        nonce: process_nonce().to_string(),
    };
    store.entries.insert(key, entry);

    evict_stale(&mut store, now);

    save_store(&store);
    CacheResult::Miss { content }
}

pub(crate) fn invalidate(path: &str) {
    let key = normalize_key(path);
    let _guard = store_guard();
    let mut store = load_store();
    store.entries.remove(&key);
    save_store(&store);
}

pub(crate) fn clear() -> usize {
    let _guard = store_guard();
    let mut store = load_store();
    let count = store.entries.len();
    store.entries.clear();
    save_store(&store);
    count
}

pub(crate) fn clear_project(project_root: &str) -> usize {
    let _guard = store_guard();
    let mut store = load_store();
    let prefix = normalize_key(project_root);
    let before = store.entries.len();
    store
        .entries
        .retain(|key, entry| !key.starts_with(&prefix) && !entry.path.starts_with(&prefix));
    let removed = before - store.entries.len();
    save_store(&store);
    removed
}

pub(crate) fn stats() -> (u64, u64, usize) {
    let store = load_store();
    (store.total_hits, store.total_reads, store.entries.len())
}

fn evict_stale(store: &mut CliCacheStore, now: u64) {
    // A concurrent writer can store an entry stamped after this caller read
    // `now`. Plain `now - timestamp` then underflowed: a panic in debug builds,
    // and in release a wrapped, huge age that evicted the other writer's fresh
    // entry. An entry from the future is simply not stale.
    store
        .entries
        .retain(|_, e| now.saturating_sub(e.timestamp) < CACHE_TTL_SECS);

    if store.entries.len() > MAX_ENTRIES {
        let mut entries: Vec<(String, u64)> = store
            .entries
            .iter()
            .map(|(k, e)| (k.clone(), e.timestamp))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);
        let to_remove = store.entries.len() - MAX_ENTRIES;
        for (key, _) in entries.into_iter().take(to_remove) {
            store.entries.remove(&key);
        }
    }
}

pub(crate) fn format_hit(entry: &CliCacheEntry, file_ref: &str, short_path: &str) -> String {
    if crate::core::protocol::savings_footer_visible() {
        format!(
            "{file_ref} cached {short_path} [{}L {}t] (read #{})",
            entry.line_count, entry.original_tokens, entry.read_count
        )
    } else {
        format!("cached {short_path} [{}L]", entry.line_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_md5_deterministic() {
        let h1 = compute_md5("test content");
        let h2 = compute_md5("test content");
        assert_eq!(h1, h2);
        assert_ne!(h1, compute_md5("different"));
    }

    #[test]
    fn evict_stale_removes_old_entries() {
        let mut store = CliCacheStore::default();
        store.entries.insert(
            "/old.rs".to_string(),
            CliCacheEntry {
                path: "/old.rs".to_string(),
                hash: "h1".into(),
                line_count: 10,
                original_tokens: 50,
                timestamp: 1000,
                read_count: 1,
                nonce: "n1".into(),
            },
        );
        store.entries.insert(
            "/new.rs".to_string(),
            CliCacheEntry {
                path: "/new.rs".to_string(),
                hash: "h2".into(),
                line_count: 20,
                original_tokens: 100,
                timestamp: now_secs(),
                read_count: 1,
                nonce: "n2".into(),
            },
        );

        evict_stale(&mut store, now_secs());
        assert!(!store.entries.contains_key("/old.rs"));
        assert!(store.entries.contains_key("/new.rs"));
    }

    #[test]
    fn evict_respects_max_entries() {
        let mut store = CliCacheStore::default();
        let now = now_secs();
        for i in 0..MAX_ENTRIES + 10 {
            store.entries.insert(
                format!("/file_{i}.rs"),
                CliCacheEntry {
                    path: format!("/file_{i}.rs"),
                    hash: format!("h{i}"),
                    line_count: 1,
                    original_tokens: 10,
                    timestamp: now - i as u64,
                    read_count: 1,
                    nonce: format!("n{i}"),
                },
            );
        }
        evict_stale(&mut store, now);
        assert!(store.entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn format_hit_output() {
        let _lock = crate::core::data_dir::test_env_lock();
        let entry = CliCacheEntry {
            path: "/test.rs".into(),
            hash: "abc".into(),
            line_count: 42,
            original_tokens: 500,
            timestamp: now_secs(),
            read_count: 3,
            nonce: "n".into(),
        };
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        let output = format_hit(&entry, "F1", "test.rs");
        assert!(output.contains("cached test.rs"));
        assert!(output.contains("42L"));
        assert!(!output.contains("F1"));
        assert!(!output.contains("500t"));
        assert!(!output.contains("read #3"));
        crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    }

    #[test]
    fn stats_returns_defaults_on_empty() {
        let s = CliCacheStore::default();
        assert_eq!(s.total_hits, 0);
        assert_eq!(s.total_reads, 0);
        assert!(s.entries.is_empty());
    }

    #[test]
    fn cache_result_integration() {
        let _lock = crate::core::data_dir::test_env_lock();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_data_dir = std::env::temp_dir().join(format!("lean_ctx_cache_iso_{nanos}"));
        std::fs::create_dir_all(&test_data_dir).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", &test_data_dir);

        let tmp = test_data_dir.join("test_file.txt");
        std::fs::write(&tmp, "fn main() {}\n").unwrap();
        let path_str = tmp.to_str().unwrap();

        invalidate(path_str);

        let result = check_and_read(path_str);
        assert!(matches!(result, CacheResult::Miss { .. }));

        let result2 = check_and_read(path_str);
        assert!(matches!(result2, CacheResult::Hit { .. }));
        if let CacheResult::Hit { entry, .. } = result2 {
            assert_eq!(entry.line_count, 1);
            assert!(entry.read_count >= 2);
        }

        invalidate(path_str);
        let result3 = check_and_read(path_str);
        assert!(matches!(result3, CacheResult::Miss { .. }));

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&test_data_dir);
    }

    #[test]
    fn cross_process_entry_is_not_served_as_hit() {
        // #1459: a persistent entry written by a *different* process (a foreign
        // nonce) must not be served as a `cached … [NL]` stub — the reader never
        // received the content, so it must miss and get the real bytes.
        let _lock = crate::core::data_dir::test_env_lock();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let test_data_dir = std::env::temp_dir().join(format!("lean_ctx_cache_xproc_{nanos}"));
        std::fs::create_dir_all(&test_data_dir).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", &test_data_dir);

        let tmp = test_data_dir.join("test_file.txt");
        let content = "fn main() {}\n";
        std::fs::write(&tmp, content).unwrap();
        let path_str = tmp.to_str().unwrap();
        let key = normalize_key(path_str);

        // Simulate a prior process that cached this path under its own nonce.
        let mut store = load_store();
        store.entries.insert(
            key.clone(),
            CliCacheEntry {
                path: key,
                hash: compute_md5(content),
                line_count: 1,
                original_tokens: 4,
                timestamp: now_secs(),
                read_count: 1,
                nonce: "a-different-process".into(),
            },
        );
        save_store(&store);

        // A fresh process (different nonce) must miss and receive full content.
        let result = check_and_read(path_str);
        assert!(
            matches!(&result, CacheResult::Miss { content } if content == "fn main() {}\n"),
            "foreign-process cache entry must not be served as a hit"
        );

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&test_data_dir);
    }

    #[test]
    fn concurrent_writers_never_drop_each_others_entries() {
        // Each writer loads the whole store, changes its own key and writes the
        // file back. Unserialized, a later write dropped entries that a
        // concurrent writer had just added (flaked ctx_refactor's
        // `reformat_jetbrains_scope_invalidates_all_changed_paths`).
        let data = crate::core::data_dir::isolated_data_dir();
        let files: Vec<String> = (0..64)
            .map(|i| {
                let path = data.path().join(format!("f{i}.rs"));
                std::fs::write(&path, format!("fn f{i}() {{}}\n")).unwrap();
                path.to_str().unwrap().to_string()
            })
            .collect();

        std::thread::scope(|s| {
            for chunk in files.chunks(8) {
                s.spawn(move || {
                    for path in chunk {
                        let _ = check_and_read(path);
                    }
                });
            }
        });

        let store = load_store();
        for path in &files {
            assert!(
                store.entries.contains_key(&normalize_key(path)),
                "entry for {path} was lost to a concurrent write"
            );
        }
    }
}
