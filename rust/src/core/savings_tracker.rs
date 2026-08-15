//! Per-session compression accounting, persisted when the tracker is dropped.
//!
//! A debounced snapshot file (`compression_session.json`) is written every
//! 5 seconds so separate processes (e.g. the dashboard server) can read the
//! current session's compression state without sharing memory.

use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const SNAPSHOT_DEBOUNCE_MS: u64 = 5_000;

fn tracker() -> &'static std::sync::Mutex<SessionSavingsTracker> {
    static TRACKER: std::sync::OnceLock<std::sync::Mutex<SessionSavingsTracker>> =
        std::sync::OnceLock::new();
    TRACKER.get_or_init(|| std::sync::Mutex::new(SessionSavingsTracker::default()))
}

static LAST_SNAPSHOT_MS: AtomicU64 = AtomicU64::new(0);

pub fn record_compression(raw_tokens: u64, compressed_tokens: u64, tool: &str) {
    record_best_effort(tracker(), raw_tokens, compressed_tokens, tool);
    write_snapshot_debounced();
}

pub fn session_summary() -> SessionSavings {
    tracker().try_lock().map_or_else(
        |_| SessionSavings::default(),
        |tracker| tracker.session_summary(),
    )
}

/// Write a snapshot of current session savings to a well-known file so the
/// dashboard (separate process) can read it. Debounced to avoid excessive I/O.
fn write_snapshot_debounced() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let prev = LAST_SNAPSHOT_MS.load(Ordering::Relaxed);
    if now_ms.saturating_sub(prev) < SNAPSHOT_DEBOUNCE_MS {
        return;
    }
    if LAST_SNAPSHOT_MS
        .compare_exchange(prev, now_ms, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    if let Ok(tracker) = tracker().try_lock() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            write_snapshot_file(&tracker.session_summary());
        }));
    }
}

fn snapshot_path() -> Option<PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|d| d.join("compression_session.json"))
}

fn write_snapshot_file(summary: &SessionSavings) {
    let Some(path) = snapshot_path() else { return };
    let Ok(json) = serde_json::to_string(summary) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Load the latest compression session snapshot written by the MCP server.
/// Used by the dashboard (separate process) to display session savings.
pub fn load_snapshot() -> SessionSavings {
    let Some(path) = snapshot_path() else {
        return SessionSavings::default();
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist the process-global tracker at a known session boundary.
///
/// Accounting is best-effort: lock, serialization, and filesystem errors must
/// never delay or prevent the server from shutting down.
pub fn persist_session_summary() {
    if let Ok(tracker) = tracker().try_lock() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            tracker.persist();
            write_snapshot_file(&tracker.session_summary());
        }));
    }
}

fn record_best_effort(
    tracker: &std::sync::Mutex<SessionSavingsTracker>,
    raw_tokens: u64,
    compressed_tokens: u64,
    tool: &str,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(mut tracker) = tracker.try_lock() {
            tracker.record_compression(raw_tokens, compressed_tokens, tool);
        }
    }));
}

#[rustfmt::skip]
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
/// Summarizes compression savings accumulated during a session.
pub struct SessionSavings {
    pub total_raw: u64,
    pub total_compressed: u64,
    pub savings_tokens: u64,
    pub savings_percent: f64,
    pub tool_breakdown: Vec<(String, u64)>,
}

#[rustfmt::skip]
#[derive(Debug, Default)]
/// Accumulates and persists per-session compression savings.
pub struct SessionSavingsTracker {
    raw: u64,
    compressed: u64,
    tools: BTreeMap<String, u64>,
}

#[rustfmt::skip]
impl SessionSavingsTracker {
    pub fn record_compression(&mut self, raw_tokens: u64, compressed_tokens: u64, tool: &str) {
        let saved = raw_tokens.saturating_sub(compressed_tokens);
        self.raw = self.raw.saturating_add(raw_tokens);
        self.compressed = self.compressed.saturating_add(compressed_tokens);
        let entry = self.tools.entry(tool.to_owned()).or_default();
        *entry = entry.saturating_add(saved);
    }

    pub fn session_summary(&self) -> SessionSavings {
        let savings_tokens = self.raw.saturating_sub(self.compressed);
        SessionSavings {
            total_raw: self.raw,
            total_compressed: self.compressed,
            savings_tokens,
            savings_percent: percent(savings_tokens, self.raw),
            tool_breakdown: self
                .tools
                .iter()
                .map(|(tool, saved)| (tool.clone(), *saved))
                .collect(),
        }
    }

    fn persist(&self) {
        if self.raw == 0 {
            return;
        }
        let Ok(dir) = crate::core::paths::state_dir() else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("session_savings.jsonl"))
        else {
            return;
        };
        let Ok(summary) = serde_json::to_string(&self.session_summary()) else {
            return;
        };
        let _ = writeln!(file, "{summary}");
    }
}

#[rustfmt::skip]
impl Drop for SessionSavingsTracker {
    fn drop(&mut self) {
        let _ = catch_unwind(AssertUnwindSafe(|| self.persist()));
    }
}

#[rustfmt::skip] pub fn percent(numerator: u64, denominator: u64) -> f64 { if denominator == 0 { 0.0 } else { numerator as f64 * 100.0 / denominator as f64 } }

#[cfg(test)] #[rustfmt::skip] mod tests { use super::*;

    #[test] fn test_session_tracker_accumulates() {
        let mut tracker = SessionSavingsTracker::default();
        tracker.record_compression(1_000, 600, "ctx_read");
        tracker.record_compression(500, 400, "ctx_read");
        tracker.record_compression(100, 50, "ctx_search");
        let savings = tracker.session_summary();
        assert_eq!((savings.total_raw, savings.total_compressed, savings.savings_tokens), (1_600, 1_050, 550));
        assert_eq!(savings.tool_breakdown, vec![("ctx_read".into(), 500), ("ctx_search".into(), 50)]);
    }

    #[test]
    fn test_tracker_error_ignored() {
        let tracker = std::sync::Mutex::new(SessionSavingsTracker::default());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = tracker.lock().expect("fresh test mutex");
            panic!("poison tracker");
        }));

        let result = catch_unwind(AssertUnwindSafe(|| {
            record_best_effort(&tracker, 100, 25, "ctx_read");
        }));

        assert!(result.is_ok());
    }
}
