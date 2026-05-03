use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_TTL_SECS: u64 = 300;
const MAX_ENTRIES: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliCacheEntry {
    pub path: String,
    pub hash: String,
    pub line_count: usize,
    pub original_tokens: usize,
    pub timestamp: u64,
    pub read_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliCacheStore {
    pub entries: HashMap<String, CliCacheEntry>,
    pub total_hits: u64,
    pub total_reads: u64,
}

pub enum CacheResult {
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

fn compute_md5(content: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize_key(path: &str) -> String {
    crate::hooks::normalize_tool_path(path)
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

pub fn check_and_read(path: &str) -> CacheResult {
    let Ok(content) = crate::tools::ctx_read::read_file_lossy(path) else {
        return CacheResult::Miss {
            content: String::new(),
        };
    };

    let key = normalize_key(path);
    let hash = compute_md5(&content);
    let now = now_secs();
    let mut store = load_store();

    store.total_reads += 1;

    if let Some(entry) = store.entries.get_mut(&key) {
        if entry.hash == hash && (now - entry.timestamp) < CACHE_TTL_SECS {
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
    };
    store.entries.insert(key, entry);

    evict_stale(&mut store, now);

    save_store(&store);
    CacheResult::Miss { content }
}

pub fn invalidate(path: &str) {
    let key = normalize_key(path);
    let mut store = load_store();
    store.entries.remove(&key);
    save_store(&store);
}

pub fn clear() -> usize {
    let mut store = load_store();
    let count = store.entries.len();
    store.entries.clear();
    save_store(&store);
    count
}

pub fn clear_project(project_root: &str) -> usize {
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

pub fn stats() -> (u64, u64, usize) {
    let store = load_store();
    (store.total_hits, store.total_reads, store.entries.len())
}

fn evict_stale(store: &mut CliCacheStore, now: u64) {
    store
        .entries
        .retain(|_, e| (now - e.timestamp) < CACHE_TTL_SECS);

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

pub fn format_hit(entry: &CliCacheEntry, file_ref: &str, short_path: &str) -> String {
    format!(
        "{file_ref} cached {short_path} [{}L {}t] (read #{})",
        entry.line_count, entry.original_tokens, entry.read_count
    )
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
                },
            );
        }
        evict_stale(&mut store, now);
        assert!(store.entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn format_hit_output() {
        let entry = CliCacheEntry {
            path: "/test.rs".into(),
            hash: "abc".into(),
            line_count: 42,
            original_tokens: 500,
            timestamp: now_secs(),
            read_count: 3,
        };
        let output = format_hit(&entry, "F1", "test.rs");
        assert!(output.contains("F1 cached"));
        assert!(output.contains("42L"));
        assert!(output.contains("500t"));
        assert!(output.contains("read #3"));
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
        std::env::set_var("LEAN_CTX_DATA_DIR", &test_data_dir);

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

        std::env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&test_data_dir);
    }
}
