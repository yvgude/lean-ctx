use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::cache::SessionCache;
use crate::core::context_radar::RadarEvent;

pub static LAST_COMPACTION_TS: AtomicU64 = AtomicU64::new(0);

/// Effective cache policy: "aggressive" (default), "safe", or "off".
pub fn effective_cache_policy() -> &'static str {
    // Resolve on each call: tests and embedded callers may change the
    // process override between engine instances; a process-wide OnceLock
    // would permanently retain the first caller's policy.
    let value = std::env::var("LEAN_CTX_CACHE_POLICY")
        .ok()
        .map(|v| v.trim().to_lowercase())
        .filter(|v| matches!(v.as_str(), "aggressive" | "safe" | "off"))
        .or_else(|| {
            crate::core::config::Config::load()
                .cache_policy
                .map(|v| v.to_lowercase())
        })
        .unwrap_or_else(|| "aggressive".to_string());
    match value.as_str() {
        "safe" => "safe",
        "off" => "off",
        _ => "aggressive",
    }
}

/// Check if a host compaction event occurred since our last check.
/// If so, reset all `full_content_delivered` flags so the next read
/// delivers full content instead of a stub.
///
/// Detection strategy (#808):
/// 1. Check `last_compaction.json` marker file first (written atomically by
///    the observe hook). This is only a few bytes, immune to being displaced
///    by large events.
/// 2. Fall back to scanning the tail of `context_radar.jsonl` (256 KB window)
///    for older hook binaries that don't write the marker.
pub fn sync_if_compacted(cache: &mut SessionCache, data_dir: &Path) -> bool {
    let last_seen = LAST_COMPACTION_TS.load(Ordering::Relaxed);

    let latest_ts = check_compaction_marker(data_dir, last_seen)
        .or_else(|| scan_radar_tail(data_dir, last_seen));

    let Some(latest_compaction_ts) = latest_ts else {
        return false;
    };

    LAST_COMPACTION_TS.store(latest_compaction_ts, Ordering::Relaxed);
    crate::core::search_delta::reset();
    let reset_count = cache.reset_delivery_flags();
    crate::core::cache_telemetry::record_compaction(reset_count as u64);
    // Drop the persistent stub index too (#955): the conversation's context was
    // summarised away, so neither a warm nor a cold stub may claim "you already
    // have this". Writes the emptied index synchronously so a restart in the
    // crash window can't resurrect a pre-compaction stub.
    crate::core::read_stub_index::reset_in_dir(data_dir);
    if reset_count > 0 {
        // #1781: `eprintln!` panics when the write fails, and a host closing the
        // pipe is routine for an MCP server (restart, aborted call, IDE reload).
        // On Windows that surfaced as `failed printing to stderr: The pipe is
        // being closed. (os error 232)`. `tracing` returns the error instead of
        // panicking, and the subscriber already targets stderr.
        tracing::info!(
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

// ---------------------------------------------------------------------------
// Detection: marker file (primary, #808)
// ---------------------------------------------------------------------------

/// Read `last_compaction.json` — a few-byte file written atomically by the
/// observe hook. Returns `Some(ts)` if the marker exists and its timestamp
/// is newer than `since_ts`.
fn check_compaction_marker(data_dir: &Path, since_ts: u64) -> Option<u64> {
    let marker_path = data_dir.join("last_compaction.json");
    let content = std::fs::read_to_string(&marker_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(content.trim()).ok()?;
    let ts = parsed.get("ts")?.as_u64()?;
    if ts > since_ts { Some(ts) } else { None }
}

// ---------------------------------------------------------------------------
// Detection: radar tail scan (fallback for older hook binaries)
// ---------------------------------------------------------------------------

/// Scan only the tail of radar JSONL for a compaction event newer than `since_ts`.
/// #808: window widened from 4 KB to 256 KB so large `agent_response` /
/// `thinking` events (up to 50 000 chars each) cannot bury the compaction entry.
fn scan_radar_tail(data_dir: &Path, since_ts: u64) -> Option<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let radar_path = data_dir.join("context_radar.jsonl");
    let mut file = std::fs::File::open(&radar_path).ok()?;
    let file_len = file.metadata().ok()?.len();

    const TAIL_BYTES: u64 = 256 * 1024; // #808: was 4096
    let content = if file_len <= TAIL_BYTES {
        let mut s = String::new();
        file.read_to_string(&mut s).ok()?;
        s
    } else {
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).ok()?;
        let mut buf = vec![0u8; TAIL_BYTES as usize];
        let n = file.read(&mut buf).ok()?;
        let s = String::from_utf8_lossy(&buf[..n]).into_owned();
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

    // --- #808 new tests ---

    /// The marker file alone (without a radar entry) triggers a reset,
    /// and the same marker timestamp does not trigger it a second time.
    #[test]
    #[serial]
    fn marker_alone_triggers_reset() {
        let dir = TempDir::new().unwrap();
        // No context_radar.jsonl — only the marker
        let marker = dir.path().join("last_compaction.json");
        std::fs::write(&marker, r#"{"ts":5000}"#).unwrap();
        // Need a radar file to exist (sync_if_compacted checks marker OR radar)
        let radar = dir.path().join("context_radar.jsonl");
        std::fs::write(&radar, "").unwrap();

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/x.rs"]);
        assert!(cache.is_full_delivered("/tmp/x.rs"));

        // First call: marker is newer than last_seen (0) → reset
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/x.rs"));
        assert_eq!(LAST_COMPACTION_TS.load(Ordering::Relaxed), 5000);

        // Re-deliver, then call again: same marker ts → no reset
        cache.mark_full_delivered("/tmp/x.rs");
        assert!(!sync_if_compacted(&mut cache, dir.path()));
        assert!(cache.is_full_delivered("/tmp/x.rs"));
    }

    /// A compaction event buried under a large (10 KB+) event line is still
    /// detected via the widened 256 KB fallback window. This reproduces the
    /// original bug — the old 4 KB window would miss it.
    #[test]
    #[serial]
    fn compaction_buried_under_large_event_line_is_still_detected() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();

        // Write the compaction event
        writeln!(f, r#"{{"ts":3000,"event_type":"compaction","tokens":0}}"#).unwrap();

        // Bury it under a large agent_response event (10 KB content)
        let large_content = "x".repeat(10_000);
        writeln!(
            f,
            r#"{{"ts":3001,"event_type":"agent_response","tokens":2500,"content":"{large_content}"}}"#
        )
        .unwrap();
        drop(f);

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/buried.rs"]);
        assert!(cache.is_full_delivered("/tmp/buried.rs"));

        // Must find the compaction despite the large event after it
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/buried.rs"));
    }

    /// Marker file takes priority over the radar scan: even if the radar
    /// has no compaction event, the marker triggers a reset.
    #[test]
    #[serial]
    fn marker_takes_priority_over_empty_radar() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();
        writeln!(f, r#"{{"ts":1000,"event_type":"mcp_call","tokens":50}}"#).unwrap();
        drop(f);

        let marker = dir.path().join("last_compaction.json");
        std::fs::write(&marker, r#"{"ts":4000}"#).unwrap();

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/priority.rs"]);
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/priority.rs"));
        assert_eq!(LAST_COMPACTION_TS.load(Ordering::Relaxed), 4000);
    }

    /// A corrupt or empty marker file is ignored — falls back to radar scan.
    #[test]
    #[serial]
    fn corrupt_marker_falls_back_to_radar() {
        let dir = TempDir::new().unwrap();
        let radar = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&radar).unwrap();
        writeln!(f, r#"{{"ts":6000,"event_type":"compaction","tokens":0}}"#).unwrap();
        drop(f);

        let marker = dir.path().join("last_compaction.json");
        std::fs::write(&marker, "not valid json").unwrap();

        LAST_COMPACTION_TS.store(0, Ordering::Relaxed);
        let mut cache = make_cache_with_delivered(&["/tmp/fallback.rs"]);
        // Corrupt marker ignored → radar scan finds the compaction
        assert!(sync_if_compacted(&mut cache, dir.path()));
        assert!(!cache.is_full_delivered("/tmp/fallback.rs"));
    }
}
