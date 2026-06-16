use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::cache::SessionCache;
use crate::core::context_radar::RadarEvent;

pub static LAST_COMPACTION_TS: AtomicU64 = AtomicU64::new(0);

/// Effective cache policy: "aggressive" (default), "safe", or "off".
pub fn effective_cache_policy() -> &'static str {
    static POLICY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    POLICY.get_or_init(|| {
        if let Ok(v) = std::env::var("LEAN_CTX_CACHE_POLICY") {
            let v = v.trim().to_lowercase();
            if matches!(v.as_str(), "aggressive" | "safe" | "off") {
                return v;
            }
        }
        let cfg = crate::core::config::Config::load();
        cfg.cache_policy
            .as_deref()
            .unwrap_or("aggressive")
            .to_lowercase()
    })
}

/// Check if a host compaction event occurred since our last check.
/// If so, reset all `full_content_delivered` flags so the next read
/// delivers full content instead of a stub.
pub fn sync_if_compacted(cache: &mut SessionCache, data_dir: &Path) -> bool {
    let last_seen = LAST_COMPACTION_TS.load(Ordering::Relaxed);
    let radar_path = data_dir.join("context_radar.jsonl");

    if !radar_path.exists() {
        return false;
    }

    let Some(latest_compaction_ts) = find_latest_compaction(&radar_path, last_seen) else {
        return false;
    };

    LAST_COMPACTION_TS.store(latest_compaction_ts, Ordering::Relaxed);
    crate::core::search_delta::reset();
    let reset_count = cache.reset_delivery_flags();
    if reset_count > 0 {
        eprintln!(
            "[lean-ctx] compaction detected — reset {reset_count} delivery flags for re-read"
        );
    }

    std::thread::spawn(|| {
        if let Some(session) = crate::core::session::SessionState::load_latest()
            && let Some(ref root) = session.project_root
            && (!session.findings.is_empty() || !session.decisions.is_empty())
        {
            crate::tools::startup::auto_consolidate_knowledge(root);
        }
    });

    true
}

/// Scan only the tail of radar JSONL for a compaction event newer than `since_ts`.
/// Reads at most 4KB from the end to avoid unbounded I/O on large radar files.
fn find_latest_compaction(radar_path: &Path, since_ts: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(radar_path).ok()?;
    let file_len = file.metadata().ok()?.len();

    const TAIL_BYTES: u64 = 4096;
    let content = if file_len <= TAIL_BYTES {
        let mut s = String::new();
        file.read_to_string(&mut s).ok()?;
        s
    } else {
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).ok()?;
        let mut buf = vec![0u8; TAIL_BYTES as usize];
        let n = file.read(&mut buf).ok()?;
        let s = String::from_utf8_lossy(&buf[..n]).into_owned();
        // Skip first partial line (we seeked into the middle of it)
        if let Some(idx) = s.find('\n') {
            s[idx + 1..].to_string()
        } else {
            s
        }
    };

    for line in content.lines().rev() {
        if line.is_empty() {
            continue;
        }
        let event: RadarEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if event.ts <= since_ts {
            break;
        }
        if event.event_type == "compaction" {
            return Some(event.ts);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_cache_with_delivered(paths: &[&str]) -> SessionCache {
        let mut cache = SessionCache::default();
        for p in paths {
            cache.store(p, "hello world");
            cache.mark_full_delivered(p);
        }
        cache
    }

    #[test]
    #[serial]
    fn no_reset_without_compaction_event() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();
        writeln!(f, r#"{{"ts":1000,"event_type":"mcp_call","tokens":50}}"#).unwrap();
        drop(f);

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/a.rs"]);
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/a.rs"));
    }

    #[test]
    #[serial]
    fn resets_after_compaction() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();
        writeln!(f, r#"{{"ts":1000,"event_type":"mcp_call","tokens":50}}"#).unwrap();
        writeln!(f, r#"{{"ts":2000,"event_type":"compaction","tokens":0}}"#).unwrap();
        drop(f);

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/a.rs", "/tmp/b.rs"]);

        assert!(cache.is_full_delivered("/tmp/a.rs"));
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/a.rs"));
        assert!(!cache.is_full_delivered("/tmp/b.rs"));
    }

    #[test]
    #[serial]
    fn does_not_double_reset() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();
        writeln!(f, r#"{{"ts":2000,"event_type":"compaction","tokens":0}}"#).unwrap();
        drop(f);

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/a.rs"]);
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/a.rs"));

        cache.mark_full_delivered("/tmp/a.rs");
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/a.rs"));
    }
}
