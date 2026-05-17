use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::core::context_radar::RadarEvent;

static LAST_LCTX_CALL_TS: AtomicU64 = AtomicU64::new(0);
static HINT_COOLDOWN: AtomicU32 = AtomicU32::new(0);

const COOLDOWN_CALLS: u32 = 5;

pub fn record_lctx_call() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    LAST_LCTX_CALL_TS.store(now, Ordering::Relaxed);
}

pub fn check(data_dir: &Path) -> Option<String> {
    let mode = effective_mode();
    if mode == "off" {
        return None;
    }

    let aggressive = mode == "aggressive";
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

    let native_count = count_native_since(data_dir, last_ts);
    if native_count == 0 {
        return None;
    }

    Some(format!(
        "\n[HINT: You used native Read/Grep {native_count}x since your last ctx_read call. \
         Use ctx_read/ctx_search instead — cached, re-reads ~13 tok, saves ~87% tokens.]"
    ))
}

fn count_native_since(data_dir: &Path, since_ts: u64) -> usize {
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

        if event.ts < since_ts {
            break;
        }

        if matches!(event.event_type.as_str(), "native_tool" | "file_read") {
            count += 1;
        }
    }
    count
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
        LAST_LCTX_CALL_TS.store(1000, Ordering::Relaxed);
        assert_eq!(count_native_since(dir.path(), 1000), 0);
    }

    #[test]
    fn counts_native_events_after_timestamp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("context_radar.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"ts":900,"event_type":"native_tool","tokens":100}}"#).unwrap();
        writeln!(
            f,
            r#"{{"ts":1100,"event_type":"native_tool","tokens":200}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"ts":1200,"event_type":"file_read","tokens":150}}"#).unwrap();
        writeln!(f, r#"{{"ts":1300,"event_type":"mcp_call","tokens":50}}"#).unwrap();
        drop(f);

        assert_eq!(count_native_since(dir.path(), 1000), 2);
    }
}
