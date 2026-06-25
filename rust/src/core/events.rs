use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const RING_CAPACITY: usize = 1000;
const JSONL_MAX_LINES: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeanCtxEvent {
    pub id: u64,
    pub timestamp: String,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventKind {
    ToolCall {
        tool: String,
        tokens_original: u64,
        tokens_saved: u64,
        mode: Option<String>,
        duration_ms: u64,
        path: Option<String>,
    },
    CacheHit {
        path: String,
        saved_tokens: u64,
    },
    Compression {
        path: String,
        before_lines: u32,
        after_lines: u32,
        strategy: String,
        kept_line_count: u32,
        removed_line_count: u32,
    },
    AgentAction {
        agent_id: String,
        action: String,
        tool: Option<String>,
    },
    KnowledgeUpdate {
        category: String,
        key: String,
        action: String,
    },
    ThresholdShift {
        language: String,
        old_entropy: f64,
        new_entropy: f64,
        old_jaccard: f64,
        new_jaccard: f64,
    },
    BudgetWarning {
        role: String,
        dimension: String,
        used: String,
        limit: String,
        percent: u8,
    },
    BudgetExhausted {
        role: String,
        dimension: String,
        used: String,
        limit: String,
    },
    PolicyViolation {
        role: String,
        tool: String,
        reason: String,
    },
    RoleChanged {
        from: String,
        to: String,
    },
    ProfileChanged {
        from: String,
        to: String,
    },
    SloViolation {
        slo_name: String,
        metric: String,
        threshold: f64,
        actual: f64,
        action: String,
    },
    Anomaly {
        metric: String,
        expected: f64,
        actual: f64,
        deviation_factor: f64,
    },
    VerificationWarning {
        warning_kind: String,
        detail: String,
        severity: String,
    },
    ThresholdAdapted {
        language: String,
        arm: String,
        old_threshold: f64,
        new_threshold: f64,
    },
}

struct EventBus {
    seq: AtomicU64,
    ring: Mutex<VecDeque<LeanCtxEvent>>,
}

impl EventBus {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
        }
    }

    fn emit(&self, kind: EventKind) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let event = LeanCtxEvent {
            id,
            timestamp: chrono::Local::now()
                .format("%Y-%m-%dT%H:%M:%S%.3f")
                .to_string(),
            kind,
        };

        {
            let mut ring = self
                .ring
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if ring.len() >= RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }

        append_jsonl(&event);
        id
    }

    fn events_since(&self, after_id: u64) -> Vec<LeanCtxEvent> {
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ring.iter().filter(|e| e.id > after_id).cloned().collect()
    }

    fn latest_events(&self, n: usize) -> Vec<LeanCtxEvent> {
        let ring = self
            .ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let len = ring.len();
        let start = len.saturating_sub(n);
        ring.iter().skip(start).cloned().collect()
    }
}

fn bus() -> &'static EventBus {
    static INSTANCE: OnceLock<EventBus> = OnceLock::new();
    INSTANCE.get_or_init(EventBus::new)
}

fn jsonl_path() -> Option<std::path::PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|d| d.join("events.jsonl"))
}

fn is_test_environment() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        if cfg!(test) {
            return true;
        }
        if std::env::var_os("__LEAN_CTX_SKIP_EVENTS").is_some() {
            return true;
        }
        std::env::current_exe().is_ok_and(|p| {
            let s = p.to_string_lossy();
            s.contains("/deps/") || s.contains("\\deps\\")
        })
    })
}

fn append_jsonl(event: &LeanCtxEvent) {
    if is_test_environment() {
        return;
    }
    let Some(path) = jsonl_path() else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(content) = std::fs::read_to_string(&path) {
        let lines = content.lines().count();
        if lines >= JSONL_MAX_LINES {
            let old = path.with_extension("jsonl.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
    }

    if let Ok(json) = serde_json::to_string(event) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{json}");
        }
    }
}

// --- Public API ---

#[must_use]
pub fn emit(kind: EventKind) -> u64 {
    bus().emit(kind)
}

#[must_use]
pub fn events_since(after_id: u64) -> Vec<LeanCtxEvent> {
    bus().events_since(after_id)
}

#[must_use]
pub fn latest_events(n: usize) -> Vec<LeanCtxEvent> {
    bus().latest_events(n)
}

#[derive(Default)]
struct FileEventCache {
    path: Option<std::path::PathBuf>,
    mtime: Option<std::time::SystemTime>,
    len: u64,
    events: Vec<LeanCtxEvent>,
}

/// File-backed event load with a process-local cache keyed on (path, mtime, len).
/// The dashboard polls this every 3 s; without the cache each poll re-read
/// and re-parsed the entire JSONL (up to 10k lines) even when nothing changed.
pub fn load_events_from_file(n: usize) -> Vec<LeanCtxEvent> {
    static CACHE: OnceLock<Mutex<FileEventCache>> = OnceLock::new();
    let Some(path) = jsonl_path() else {
        return Vec::new();
    };
    let (mtime, len) = match std::fs::metadata(&path) {
        Ok(m) => (m.modified().ok(), m.len()),
        Err(_) => return Vec::new(),
    };

    let cache = CACHE.get_or_init(|| Mutex::new(FileEventCache::default()));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    let fresh =
        guard.path.as_deref() == Some(path.as_path()) && guard.mtime == mtime && guard.len == len;
    if !fresh {
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        guard.events = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        guard.path = Some(path);
        guard.mtime = mtime;
        guard.len = len;
    }

    let start = guard.events.len().saturating_sub(n);
    guard.events[start..].to_vec()
}

pub fn emit_tool_call(
    tool: &str,
    tokens_original: u64,
    tokens_saved: u64,
    mode: Option<String>,
    duration_ms: u64,
    path: Option<String>,
) {
    let _ = emit(EventKind::ToolCall {
        tool: tool.to_string(),
        tokens_original,
        tokens_saved,
        mode,
        duration_ms,
        path,
    });
}

pub fn emit_cache_hit(path: &str, saved_tokens: u64) {
    let _ = emit(EventKind::CacheHit {
        path: path.to_string(),
        saved_tokens,
    });
}

pub fn emit_agent_action(agent_id: &str, action: &str, tool: Option<&str>) {
    let _ = emit(EventKind::AgentAction {
        agent_id: agent_id.to_string(),
        action: action.to_string(),
        tool: tool.map(std::string::ToString::to_string),
    });
}

pub fn emit_budget_warning(role: &str, dimension: &str, used: &str, limit: &str, percent: u8) {
    let _ = emit(EventKind::BudgetWarning {
        role: role.to_string(),
        dimension: dimension.to_string(),
        used: used.to_string(),
        limit: limit.to_string(),
        percent,
    });
}

pub fn emit_budget_exhausted(role: &str, dimension: &str, used: &str, limit: &str) {
    let _ = emit(EventKind::BudgetExhausted {
        role: role.to_string(),
        dimension: dimension.to_string(),
        used: used.to_string(),
        limit: limit.to_string(),
    });
}

pub fn emit_policy_violation(role: &str, tool: &str, reason: &str) {
    let _ = emit(EventKind::PolicyViolation {
        role: role.to_string(),
        tool: tool.to_string(),
        reason: reason.to_string(),
    });
}

pub fn emit_role_changed(from: &str, to: &str) {
    let _ = emit(EventKind::RoleChanged {
        from: from.to_string(),
        to: to.to_string(),
    });
}

pub fn emit_profile_changed(from: &str, to: &str) {
    let _ = emit(EventKind::ProfileChanged {
        from: from.to_string(),
        to: to.to_string(),
    });
}

pub fn emit_slo_violation(slo_name: &str, metric: &str, threshold: f64, actual: f64, action: &str) {
    let _ = emit(EventKind::SloViolation {
        slo_name: slo_name.to_string(),
        metric: metric.to_string(),
        threshold,
        actual,
        action: action.to_string(),
    });
}

pub fn emit_anomaly(metric: &str, expected: f64, actual: f64, deviation_factor: f64) {
    let _ = emit(EventKind::Anomaly {
        metric: metric.to_string(),
        expected,
        actual,
        deviation_factor,
    });
}

pub fn emit_verification_warning(warning_kind: &str, detail: &str, severity: &str) {
    let _ = emit(EventKind::VerificationWarning {
        warning_kind: warning_kind.to_string(),
        detail: detail.to_string(),
        severity: severity.to_string(),
    });
}

pub fn emit_threshold_adapted(language: &str, arm: &str, old_threshold: f64, new_threshold: f64) {
    let _ = emit(EventKind::ThresholdAdapted {
        language: language.to_string(),
        arm: arm.to_string(),
        old_threshold,
        new_threshold,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_returns_positive_id() {
        let id = emit(EventKind::ToolCall {
            tool: "ctx_read".to_string(),
            tokens_original: 1000,
            tokens_saved: 800,
            mode: Some("map".to_string()),
            duration_ms: 5,
            path: Some("src/main.rs".to_string()),
        });
        assert!(id > 0);
        let events = latest_events(100);
        assert!(events.iter().any(|e| e.id == id));
    }

    #[test]
    fn events_since_filters_correctly() {
        let id1 = emit(EventKind::CacheHit {
            path: "filter_test_a.rs".to_string(),
            saved_tokens: 100,
        });
        let id2 = emit(EventKind::CacheHit {
            path: "filter_test_b.rs".to_string(),
            saved_tokens: 200,
        });

        let after = events_since(id1);
        assert!(after.iter().any(|e| e.id == id2));
        assert!(after.iter().all(|e| e.id > id1));
    }

    /// The (path, mtime, len) cache must never serve stale events: appending a
    /// line changes the file length, which has nanosecond-independent
    /// granularity (unlike mtime), so new events show up on the next poll.
    #[test]
    fn load_events_from_file_sees_appended_events() {
        let path = jsonl_path().expect("test sandbox data dir");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create data dir");
        }

        let line_a = r#"{"id":900001,"timestamp":"2026-06-12T08:00:00.000","kind":{"type":"CacheHit","path":"cached_a.rs","saved_tokens":42}}"#;
        std::fs::write(&path, format!("{line_a}\n")).expect("write events.jsonl");

        let first = load_events_from_file(50);
        assert!(
            first.iter().any(|e| e.id == 900_001),
            "initial load should parse the seeded event"
        );

        // Second call with unchanged file exercises the cached branch.
        let cached = load_events_from_file(50);
        assert_eq!(cached.len(), first.len());

        let line_b = r#"{"id":900002,"timestamp":"2026-06-12T08:00:01.000","kind":{"type":"CacheHit","path":"cached_b.rs","saved_tokens":7}}"#;
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("append events.jsonl");
            writeln!(f, "{line_b}").expect("append line");
        }

        let second = load_events_from_file(50);
        assert!(
            second.iter().any(|e| e.id == 900_002),
            "append must invalidate the cache and surface the new event"
        );
    }
}
