use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::core::context_radar::RadarEvent;

static LAST_LCTX_CALL_TS: AtomicU64 = AtomicU64::new(0);
static HINT_COOLDOWN: AtomicU32 = AtomicU32::new(0);
static SESSION_ID: Mutex<Option<String>> = Mutex::new(None);

const COOLDOWN_CALLS: u32 = 5;

const NATIVE_READ_TOOLS: &[&str] = &[
    "Read",
    "read",
    "read_file",
    "ReadFile",
    "Grep",
    "grep",
    "search",
    "ripgrep",
];

pub fn record_lctx_call() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    LAST_LCTX_CALL_TS.store(now, Ordering::Relaxed);
}

pub fn set_session_id(id: &str) {
    if let Ok(mut guard) = SESSION_ID.lock() {
        let changed = guard.as_deref() != Some(id);
        *guard = Some(id.to_string());
        if changed {
            LAST_LCTX_CALL_TS.store(0, Ordering::Relaxed);
            HINT_COOLDOWN.store(0, Ordering::Relaxed);
        }
    }
}

pub fn check(data_dir: &Path) -> Option<String> {
    let mode = effective_mode();
    if mode == "off" {
        return None;
    }

    let cfg = crate::core::config::Config::load();
    let shadow = cfg.shadow_mode;
    let aggressive = mode == "aggressive" || shadow;

    if !aggressive {
        let counter = HINT_COOLDOWN.fetch_add(1, Ordering::Relaxed);
        if !counter.is_multiple_of(COOLDOWN_CALLS) {
            return None;
        }
    }

    let last_ts = LAST_LCTX_CALL_TS.load(Ordering::Relaxed);
    if last_ts == 0 {
        return None;
    }

    let session_id = SESSION_ID.lock().ok().and_then(|g| g.clone());
    let native_count = count_native_since(data_dir, last_ts, session_id.as_deref());
    if native_count == 0 {
        return None;
    }

    if shadow {
        Some(format!(
            "\n[SHADOW MODE: This native Read/Grep call was intercepted ({native_count}x). \
             Use ctx_read/ctx_search directly — faster, cached, saves ~87% tokens.]"
        ))
    } else {
        Some(format!(
            "\n[HINT: You used native Read/Grep {native_count}x since your last ctx_read call. \
             Use ctx_read/ctx_search instead — cached, re-reads ~13 tok, saves ~87% tokens.]"
        ))
    }
}

fn count_native_since(data_dir: &Path, since_ts: u64, session_id: Option<&str>) -> usize {
    let radar_path = radar_jsonl_path(data_dir);
    if !radar_path.exists() {
        return 0;
    }

    let Ok(content) = std::fs::read_to_string(&radar_path) else {
        return 0;
    };

    let mut count = 0;
    for line in content.lines().rev() {
        if line.is_empty() {
            continue;
        }
        let event: RadarEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let event_ts_ms = event.ts * 1000;
        if event_ts_ms < since_ts {
            break;
        }

        // Only count events from the same session (avoids subagent and
        // parallel-tab false positives). Events without a conversation_id
        // are excluded when session filtering is active — they come from
        // IDE-internal hooks or background processes, not agent tool calls.
        if let Some(sid) = session_id {
            match event.conversation_id.as_deref() {
                Some(event_sid) if event_sid == sid => {}
                _ => continue,
            }
        }

        if event.event_type == "native_tool" {
            if !is_read_grep_tool(event.tool_name.as_ref()) {
                continue;
            }
            if let Some(ref name) = event.tool_name
                && (name.starts_with("ctx_") || name.starts_with("mcp__lean-ctx__"))
            {
                continue;
            }
            count += 1;
        }
        if event.event_type == "file_read" && is_read_grep_tool(event.tool_name.as_ref()) {
            count += 1;
        }
    }
    count
}

fn is_read_grep_tool(tool_name: Option<&String>) -> bool {
    tool_name.is_some_and(|name| NATIVE_READ_TOOLS.iter().any(|t| name == *t))
}

fn effective_mode() -> String {
    if let Ok(v) = std::env::var("LEAN_CTX_BYPASS_HINTS") {
        let v = v.trim().to_lowercase();
        if matches!(v.as_str(), "off" | "on" | "aggressive") {
            return v;
        }
    }
    let cfg = crate::core::config::Config::load();
    cfg.bypass_hints.as_deref().unwrap_or("on").to_lowercase()
}

fn radar_jsonl_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("context_radar.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn no_hint_when_no_native_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        std::fs::write(&path, "").unwrap();
        LAST_LCTX_CALL_TS.store(1_000_000, Ordering::Relaxed);
        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 0);
    }

    #[test]
    fn only_counts_read_grep_not_edit_write() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ts":1100,"event_type":"native_tool","tokens":200,"tool_name":"Read"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ts":1200,"event_type":"native_tool","tokens":150,"tool_name":"Grep"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ts":1300,"event_type":"native_tool","tokens":100,"tool_name":"Edit"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ts":1400,"event_type":"native_tool","tokens":100,"tool_name":"Write"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"ts":1500,"event_type":"native_tool","tokens":100,"tool_name":"Shell"}}"#
        )
        .unwrap();
        drop(f);

        // Only Read + Grep count (2), not Edit/Write/Shell
        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 2);
    }

    #[test]
    fn file_read_without_tool_name_not_counted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"ts":1100,"event_type":"file_read","tokens":100}}"#).unwrap();
        writeln!(
            f,
            r#"{{"ts":1200,"event_type":"file_read","tokens":100,"tool_name":"Read"}}"#
        )
        .unwrap();
        // file_read with non-Read tool_name should NOT count
        writeln!(
            f,
            r#"{{"ts":1300,"event_type":"file_read","tokens":100,"tool_name":"SomePlugin"}}"#
        )
        .unwrap();
        drop(f);

        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 1);
    }

    #[test]
    fn session_filter_excludes_events_without_conversation_id() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // Event with matching session
        writeln!(f, r#"{{"ts":1100,"event_type":"native_tool","tokens":200,"tool_name":"Read","conversation_id":"sess-1"}}"#).unwrap();
        // Event WITHOUT conversation_id (IDE background, hooks, etc.)
        writeln!(
            f,
            r#"{{"ts":1200,"event_type":"native_tool","tokens":150,"tool_name":"Read"}}"#
        )
        .unwrap();
        drop(f);

        // With session filter: only the matching event counts, not the one without ID
        assert_eq!(count_native_since(dir.path(), 1_000_000, Some("sess-1")), 1);
        // Without session filter: both count
        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 2);
    }

    #[test]
    fn session_filter_excludes_other_sessions() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"ts":1100,"event_type":"native_tool","tokens":200,"tool_name":"Read","conversation_id":"session-A"}}"#).unwrap();
        writeln!(f, r#"{{"ts":1200,"event_type":"native_tool","tokens":150,"tool_name":"Grep","conversation_id":"session-B"}}"#).unwrap();
        writeln!(f, r#"{{"ts":1300,"event_type":"native_tool","tokens":100,"tool_name":"Read","conversation_id":"session-A"}}"#).unwrap();
        drop(f);

        // Filter for session-A: only 2 events
        assert_eq!(
            count_native_since(dir.path(), 1_000_000, Some("session-A")),
            2
        );
        // Filter for session-B: only 1 event
        assert_eq!(
            count_native_since(dir.path(), 1_000_000, Some("session-B")),
            1
        );
    }

    #[test]
    fn no_session_filter_counts_all() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"ts":1100,"event_type":"native_tool","tokens":200,"tool_name":"Read","conversation_id":"session-A"}}"#).unwrap();
        writeln!(f, r#"{{"ts":1200,"event_type":"native_tool","tokens":150,"tool_name":"Read","conversation_id":"session-B"}}"#).unwrap();
        drop(f);

        // No session filter → counts all
        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 2);
    }

    #[test]
    fn ignores_ctx_tools_in_native_events() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ts":1100,"event_type":"native_tool","tokens":200,"tool_name":"ctx_read"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"ts":1200,"event_type":"native_tool","tokens":150,"tool_name":"mcp__lean-ctx__ctx_search"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"ts":1300,"event_type":"native_tool","tokens":100,"tool_name":"Read"}}"#
        )
        .unwrap();
        drop(f);

        assert_eq!(count_native_since(dir.path(), 1_000_000, None), 1);
    }

    #[test]
    fn millis_timestamp_precision() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"ts":5,"event_type":"native_tool","tokens":100,"tool_name":"Read"}}"#
        )
        .unwrap();
        drop(f);

        assert_eq!(count_native_since(dir.path(), 5500, None), 0);
        assert_eq!(count_native_since(dir.path(), 4999, None), 1);
    }
}
