// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const RING_CAPACITY: usize = 1000;
#[cfg(test)]
const JSONL_MAX_LINES: usize = 10_000;
const EVENT_ID_BLOCK_SIZE: u64 = 1_024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LeanCtxEvent {
    pub id: u64,
    pub timestamp: String,
    pub kind: EventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
    ring: Mutex<VecDeque<LeanCtxEvent>>,
}

impl EventBus {
    fn new() -> Self {
        Self {
            ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
        }
    }

    fn record(&self, event: LeanCtxEvent) {
        let Ok(mut ring) = crate::core::context_os::observation::lock_observation(&self.ring)
        else {
            tracing::warn!(
                "legacy projection unavailable; committed observation retained in ContextBus"
            );
            return;
        };
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(event);
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

fn event_sequence_path() -> Option<std::path::PathBuf> {
    crate::core::paths::state_dir()
        .ok()
        .map(|d| d.join("events.seq"))
}

fn max_event_id_in_journal(path: &std::path::Path) -> u64 {
    let Ok(content) = std::fs::read_to_string(path) else {
        return 0;
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<LeanCtxEvent>(line).ok())
        .map(|event| event.id)
        .max()
        .unwrap_or(0)
}

fn parse_sequence_record(record: &str) -> Option<u64> {
    let (value, checksum) = record.trim().split_once(':')?;
    let value = value.parse::<u64>().ok()?;
    let checksum = checksum.parse::<u64>().ok()?;
    (checksum == !value).then_some(value)
}

fn read_persisted_sequence(path: &std::path::Path) -> Option<u64> {
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().rev().find_map(parse_sequence_record)
}

fn sequence_record(value: u64) -> String {
    format!("{value}:{}", !value)
}

fn reserve_event_id_block_at(
    sequence_path: &std::path::Path,
    journal_path: &std::path::Path,
    block_size: u64,
) -> std::io::Result<(u64, u64)> {
    use fs2::FileExt;

    if block_size == 0 {
        return Err(std::io::Error::other("event id block must not be empty"));
    }
    if let Some(parent) = sequence_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Lock a stable companion inode. The append-only sequence journal can be
    // repaired without allowing another process to bypass the lock.
    let lock_path = sequence_path.with_extension("seq.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let result = (|| {
        let persisted = read_persisted_sequence(sequence_path);
        let baseline = persisted.unwrap_or_else(|| {
            max_event_id_in_journal(journal_path).max(max_event_id_in_journal(
                &journal_path.with_extension("jsonl.old"),
            ))
        });
        let first = baseline
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("event id space exhausted"))?;
        let last = baseline
            .checked_add(block_size)
            .ok_or_else(|| std::io::Error::other("event id space exhausted"))?;

        // A prior writer's record (or corruption) may leave the file without
        // a trailing newline; appending directly would fuse onto that line
        // and make both records unparsable. Start a fresh line first.
        let needs_leading_newline = std::fs::read(sequence_path)
            .is_ok_and(|bytes| !bytes.is_empty() && bytes.last() != Some(&b'\n'));

        let mut sequence = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(sequence_path)?;
        let mut record = Vec::new();
        if needs_leading_newline {
            record.push(b'\n');
        }
        record.extend_from_slice(sequence_record(last).as_bytes());
        record.push(b'\n');
        sequence.write_all(&record)?;
        sequence.sync_data()?;
        Ok((first, last))
    })();

    let _ = FileExt::unlock(&lock);
    result
}

#[cfg(test)]
fn next_event_id_at(
    sequence_path: &std::path::Path,
    journal_path: &std::path::Path,
) -> std::io::Result<u64> {
    reserve_event_id_block_at(sequence_path, journal_path, 1).map(|(first, _)| first)
}

fn next_event_id() -> u64 {
    #[derive(Default)]
    struct LocalBlock {
        next: u64,
        last: u64,
    }

    static BLOCK: OnceLock<Mutex<LocalBlock>> = OnceLock::new();
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    if !is_test_environment()
        && let Some((sequence, journal)) = event_sequence_path().zip(jsonl_path())
    {
        let mut block = BLOCK
            .get_or_init(|| Mutex::new(LocalBlock::default()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if block.next <= block.last && block.next != 0 {
            let id = block.next;
            block.next = block.next.saturating_add(1);
            return id;
        }
        if let Ok((first, last)) =
            reserve_event_id_block_at(&sequence, &journal, EVENT_ID_BLOCK_SIZE)
        {
            block.next = first.saturating_add(1);
            block.last = last;
            return first;
        }
    }

    let base = u64::try_from(chrono::Utc::now().timestamp_millis())
        .unwrap_or_default()
        .saturating_mul(1_000);
    base.saturating_add(FALLBACK.fetch_add(1, Ordering::Relaxed))
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

#[cfg(test)]
fn append_jsonl_at(path: &std::path::Path, event: &LeanCtxEvent) -> std::io::Result<()> {
    use fs2::FileExt;
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Lock a stable companion inode rather than events.jsonl itself: rotation
    // renames the data file, so locking that inode would let another process
    // open the replacement and bypass the lock.
    let lock_path = path.with_extension("jsonl.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock_exclusive()?;

    let result = (|| {
        if let Ok(content) = std::fs::read_to_string(path)
            && content.lines().count() >= JSONL_MAX_LINES
        {
            let old = path.with_extension("jsonl.old");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(path, old)?;
        }

        let json = serde_json::to_string(event).map_err(std::io::Error::other)?;
        let mut line = json.into_bytes();
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        // One logical write while holding the interprocess lock prevents JSON
        // objects and their trailing newlines from interleaving.
        file.write_all(&line)
    })();

    let _ = FileExt::unlock(&lock);
    result
}

// --- Public API ---

/// Persist a legacy event as a canonical local observation.
///
/// This compatibility adapter deliberately never appends to `events.jsonl`;
/// that journal remains an immutable historical read source.
pub fn emit_compatibility(kind: EventKind) -> u64 {
    let id = next_event_id();
    let event = LeanCtxEvent {
        id,
        timestamp: chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.3f")
            .to_string(),
        kind,
    };
    match crate::core::context_os::try_runtime().and_then(|runtime| {
        runtime.bus.append_legacy_observation(
            crate::core::context_os::observation::producer_instance(),
            &event,
        )
    }) {
        Ok(_) => {
            bus().record(event);
            id
        }
        Err(error) => {
            tracing::warn!("canonical legacy observation persistence failed: {error}");
            0
        }
    }
}

/// Backward-compatible spelling for the explicit legacy compatibility adapter.
pub fn emit(kind: EventKind) -> u64 {
    emit_compatibility(kind)
}

pub fn events_since(after_id: u64) -> Vec<LeanCtxEvent> {
    bus().events_since(after_id)
}

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

fn load_historical_events(
    path: &std::path::Path,
) -> Result<Vec<LeanCtxEvent>, crate::core::context_os::ObservationPersistenceError> {
    static CACHE: OnceLock<Mutex<FileEventCache>> = OnceLock::new();
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let (mtime, len) = (metadata.modified().ok(), metadata.len());
    let cache = CACHE.get_or_init(|| Mutex::new(FileEventCache::default()));
    let mut guard = crate::core::context_os::observation::lock_observation(cache)?;
    let fresh = guard.path.as_deref() == Some(path) && guard.mtime == mtime && guard.len == len;
    if !fresh {
        let content = std::fs::read_to_string(path)?;
        let events = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()?;
        // Replace the cache only after every nonempty record validates.
        guard.events = events;
        guard.path = Some(path.to_path_buf());
        guard.mtime = mtime;
        guard.len = len;
    }
    Ok(guard.events.clone())
}

/// Compatibility reader: an unavailable canonical store returns no events,
/// never apparently current history. Use the fallible API to inspect the error.
pub fn load_events_from_file(n: usize) -> Vec<LeanCtxEvent> {
    match try_load_events_from_file(n) {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!("canonical event history unavailable: {error}");
            Vec::new()
        }
    }
}

/// Merge immutable historical JSONL with validated canonical observations.
pub fn try_load_events_from_file(
    n: usize,
) -> Result<Vec<LeanCtxEvent>, crate::core::context_os::ObservationPersistenceError> {
    let mut events = jsonl_path()
        .map(|path| load_historical_events(&path))
        .transpose()?
        .unwrap_or_default();

    let compatibility = crate::core::context_os::try_runtime()?
        .bus
        .read_legacy_observations(n.max(1))?;
    events.extend(compatibility);
    // Historical file order predates the canonical cursor. Block-allocated
    // IDs from concurrent producers do not encode append order.
    // Never discard distinct records just because historical/fallback IDs
    // collide. The legacy projection omits producer identity, so even equal
    // projected payloads cannot establish that two observations are duplicates.
    let start = events.len().saturating_sub(n);
    Ok(events[start..].to_vec())
}

pub fn emit_tool_call(
    tool: &str,
    tokens_original: u64,
    tokens_saved: u64,
    mode: Option<String>,
    duration_ms: u64,
    path: Option<String>,
) {
    emit(EventKind::ToolCall {
        tool: tool.to_string(),
        tokens_original,
        tokens_saved,
        mode,
        duration_ms,
        path,
    });
}

pub fn emit_cache_hit(path: &str, saved_tokens: u64) {
    emit(EventKind::CacheHit {
        path: path.to_string(),
        saved_tokens,
    });
}

pub fn emit_agent_action(agent_id: &str, action: &str, tool: Option<&str>) {
    emit(EventKind::AgentAction {
        agent_id: agent_id.to_string(),
        action: action.to_string(),
        tool: tool.map(std::string::ToString::to_string),
    });
}

pub fn emit_budget_warning(role: &str, dimension: &str, used: &str, limit: &str, percent: u8) {
    // #1282: the post-dispatch budget guard calls this on EVERY tool call once
    // a dimension is in Warning — measured 103 of 200 ring-buffer slots in
    // 5 minutes, drowning the ToolCall events the Live view exists to show.
    // Rate-limit per (role, dimension): emit only when the 10%-bucket moves
    // or the last emission is older than 5 minutes.
    if !budget_warning_due(role, dimension, percent) {
        return;
    }
    emit(EventKind::BudgetWarning {
        role: role.to_string(),
        dimension: dimension.to_string(),
        used: used.to_string(),
        limit: limit.to_string(),
        percent,
    });
}

/// Per-(role, dimension) rate limiter for budget warnings (#1282). In-process
/// state is sufficient: the emitter lives in the long-running server, and a
/// short-lived CLI process at worst emits one warning per dimension.
fn budget_warning_due(role: &str, dimension: &str, percent: u8) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const REPEAT_AFTER: Duration = Duration::from_mins(5);

    type WarnState = HashMap<(String, String), (u8, Instant)>;
    static LAST: Mutex<Option<WarnState>> = Mutex::new(None);

    let mut guard = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = guard.get_or_insert_with(HashMap::new);
    let key = (role.to_string(), dimension.to_string());
    let bucket = percent / 10;
    match map.get(&key) {
        Some((last_bucket, at)) if *last_bucket == bucket && at.elapsed() < REPEAT_AFTER => false,
        _ => {
            map.insert(key, (bucket, Instant::now()));
            true
        }
    }
}

pub fn emit_budget_exhausted(role: &str, dimension: &str, used: &str, limit: &str) {
    emit(EventKind::BudgetExhausted {
        role: role.to_string(),
        dimension: dimension.to_string(),
        used: used.to_string(),
        limit: limit.to_string(),
    });
}

pub fn emit_policy_violation(role: &str, tool: &str, reason: &str) {
    emit(EventKind::PolicyViolation {
        role: role.to_string(),
        tool: tool.to_string(),
        reason: reason.to_string(),
    });
}

pub fn emit_role_changed(from: &str, to: &str) {
    emit(EventKind::RoleChanged {
        from: from.to_string(),
        to: to.to_string(),
    });
}

pub fn emit_profile_changed(from: &str, to: &str) {
    emit(EventKind::ProfileChanged {
        from: from.to_string(),
        to: to.to_string(),
    });
}

pub fn emit_slo_violation(slo_name: &str, metric: &str, threshold: f64, actual: f64, action: &str) {
    emit(EventKind::SloViolation {
        slo_name: slo_name.to_string(),
        metric: metric.to_string(),
        threshold,
        actual,
        action: action.to_string(),
    });
}

pub fn emit_anomaly(metric: &str, expected: f64, actual: f64, deviation_factor: f64) {
    emit(EventKind::Anomaly {
        metric: metric.to_string(),
        expected,
        actual,
        deviation_factor,
    });
}

pub fn emit_verification_warning(warning_kind: &str, detail: &str, severity: &str) {
    emit(EventKind::VerificationWarning {
        warning_kind: warning_kind.to_string(),
        detail: detail.to_string(),
        severity: severity.to_string(),
    });
}

pub fn emit_threshold_adapted(language: &str, arm: &str, old_threshold: f64, new_threshold: f64) {
    emit(EventKind::ThresholdAdapted {
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

    /// #1282: repeated warnings for an unchanged (role, dimension, bucket) are
    /// suppressed; a bucket move re-arms immediately. Uses a unique role so
    /// parallel tests never share limiter state.
    #[test]
    fn budget_warning_rate_limit_suppresses_repeats() {
        let role = format!("test-role-{}", std::process::id());
        assert!(budget_warning_due(&role, "tokens", 85));
        assert!(!budget_warning_due(&role, "tokens", 85));
        assert!(!budget_warning_due(&role, "tokens", 87)); // same 10%-bucket
        assert!(budget_warning_due(&role, "tokens", 95)); // bucket moved
        assert!(!budget_warning_due(&role, "tokens", 95));
        assert!(budget_warning_due(&role, "cost", 85)); // other dimension independent
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
    /// An isolated journal keeps unrelated canonical emissions from evicting
    /// these historical fixtures from the merged public reader's result window.
    #[test]
    fn load_events_from_file_sees_appended_events() {
        let directory = tempfile::tempdir().expect("isolated historical journal");
        let path = directory.path().join("events.jsonl");

        let line_a = r#"{"id":900001,"timestamp":"2026-06-12T08:00:00.000","kind":{"type":"CacheHit","path":"cached_a.rs","saved_tokens":42}}"#;
        std::fs::write(&path, format!("{line_a}\n")).expect("write events.jsonl");

        let first = load_historical_events(&path).expect("read initial historical events");
        assert!(
            first.iter().any(|e| e.id == 900_001),
            "initial load should parse the seeded event"
        );

        // Second call with unchanged file exercises the cached branch.
        let cached = load_historical_events(&path).expect("read cached historical events");
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

        let second = load_historical_events(&path).expect("read appended historical events");
        assert!(
            second.iter().any(|e| e.id == 900_002),
            "append must invalidate the cache and surface the new event"
        );
    }

    #[test]
    fn events_jsonl_writer_child() {
        let Some(path) = std::env::var_os("__LEAN_CTX_EVENTS_TEST_PATH") else {
            return;
        };
        let writer = std::env::var("__LEAN_CTX_EVENTS_TEST_WRITER")
            .expect("writer id")
            .parse::<u64>()
            .expect("numeric writer id");
        for sequence in 0..100 {
            let event = LeanCtxEvent {
                id: writer * 100 + sequence,
                timestamp: "2026-07-15T12:00:00.000".to_string(),
                kind: EventKind::CacheHit {
                    path: format!("writer-{writer}.rs"),
                    saved_tokens: sequence,
                },
            };
            append_jsonl_at(std::path::Path::new(&path), &event).expect("append event");
        }
    }

    fn test_event(id: u64) -> LeanCtxEvent {
        LeanCtxEvent {
            id,
            timestamp: "2026-07-15T12:00:00.000".to_string(),
            kind: EventKind::CacheHit {
                path: "event-id-test.rs".to_string(),
                saved_tokens: 1,
            },
        }
    }

    #[test]
    fn concurrent_processes_append_complete_json_lines() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        let executable = std::env::current_exe().expect("test executable");

        let mut children = Vec::new();
        for writer in 0..4 {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "core::events::tests::events_jsonl_writer_child",
                        "--nocapture",
                    ])
                    .env("__LEAN_CTX_EVENTS_TEST_PATH", &path)
                    .env("__LEAN_CTX_EVENTS_TEST_WRITER", writer.to_string())
                    .spawn()
                    .expect("spawn writer"),
            );
        }
        for mut child in children {
            assert!(child.wait().expect("wait for writer").success());
        }

        let content = std::fs::read_to_string(&path).expect("read events");
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 400, "every append must produce one line");
        for line in lines {
            serde_json::from_str::<LeanCtxEvent>(line)
                .unwrap_or_else(|error| panic!("invalid JSONL line: {error}: {line}"));
        }
    }

    #[test]
    fn concurrent_processes_serialize_rotation_and_append() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        let seed = serde_json::to_string(&LeanCtxEvent {
            id: 1,
            timestamp: "2026-07-15T12:00:00.000".to_string(),
            kind: EventKind::CacheHit {
                path: "seed.rs".to_string(),
                saved_tokens: 1,
            },
        })
        .expect("serialize seed");
        std::fs::write(&path, format!("{seed}\n").repeat(JSONL_MAX_LINES))
            .expect("seed rotation threshold");

        let executable = std::env::current_exe().expect("test executable");
        let mut children = Vec::new();
        for writer in 0..4 {
            children.push(
                std::process::Command::new(&executable)
                    .args(["--exact", "core::events::tests::events_jsonl_writer_child"])
                    .env("__LEAN_CTX_EVENTS_TEST_PATH", &path)
                    .env("__LEAN_CTX_EVENTS_TEST_WRITER", writer.to_string())
                    .spawn()
                    .expect("spawn writer"),
            );
        }
        for mut child in children {
            assert!(child.wait().expect("wait for writer").success());
        }

        let old =
            std::fs::read_to_string(path.with_extension("jsonl.old")).expect("rotated journal");
        assert_eq!(old.lines().count(), JSONL_MAX_LINES);
        assert!(
            old.lines()
                .all(|line| serde_json::from_str::<LeanCtxEvent>(line).is_ok()),
            "rotated journal must contain complete JSON lines"
        );

        let current = std::fs::read_to_string(&path).expect("current journal");
        assert_eq!(current.lines().count(), 400);
        assert!(
            current
                .lines()
                .all(|line| serde_json::from_str::<LeanCtxEvent>(line).is_ok()),
            "replacement journal must contain complete JSON lines"
        );
    }

    #[test]
    fn persistent_event_id_writer_child() {
        let Some(dir) = std::env::var_os("__LEAN_CTX_EVENT_ID_TEST_DIR") else {
            return;
        };
        let writer = std::env::var("__LEAN_CTX_EVENT_ID_TEST_WRITER").expect("writer id");
        let dir = std::path::PathBuf::from(dir);
        let sequence = dir.join("events.seq");
        let journal = dir.join("events.jsonl");
        let (first, last) =
            reserve_event_id_block_at(&sequence, &journal, 100).expect("reserve event id block");
        let ids: Vec<String> = (first..=last).map(|id| id.to_string()).collect();
        std::fs::write(dir.join(format!("writer-{writer}.ids")), ids.join("\n"))
            .expect("write allocated ids");
    }

    #[test]
    fn persistent_event_id_bootstraps_above_existing_journals() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sequence = dir.path().join("events.seq");
        let journal = dir.path().join("events.jsonl");
        let old = journal.with_extension("jsonl.old");
        std::fs::write(
            &journal,
            format!("{}\n", serde_json::to_string(&test_event(41)).unwrap()),
        )
        .unwrap();
        std::fs::write(
            &old,
            format!("{}\n", serde_json::to_string(&test_event(73)).unwrap()),
        )
        .unwrap();

        assert_eq!(next_event_id_at(&sequence, &journal).unwrap(), 74);
        assert_eq!(next_event_id_at(&sequence, &journal).unwrap(), 75);
    }

    #[test]
    fn corrupt_sequence_recovers_above_existing_journal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sequence = dir.path().join("events.seq");
        let journal = dir.path().join("events.jsonl");
        std::fs::write(&sequence, "76:broken").unwrap();
        std::fs::write(
            &journal,
            format!("{}\n", serde_json::to_string(&test_event(80)).unwrap()),
        )
        .unwrap();

        assert_eq!(next_event_id_at(&sequence, &journal).unwrap(), 81);
        assert_eq!(read_persisted_sequence(&sequence), Some(81));
    }

    #[test]
    fn concurrent_processes_allocate_unique_event_ids() {
        let dir = tempfile::tempdir().expect("temp dir");
        let executable = std::env::current_exe().expect("test executable");
        let mut children = Vec::new();
        for writer in 0..4 {
            children.push(
                std::process::Command::new(&executable)
                    .args([
                        "--exact",
                        "core::events::tests::persistent_event_id_writer_child",
                    ])
                    .env("__LEAN_CTX_EVENT_ID_TEST_DIR", dir.path())
                    .env("__LEAN_CTX_EVENT_ID_TEST_WRITER", writer.to_string())
                    .spawn()
                    .expect("spawn event id writer"),
            );
        }
        for mut child in children {
            assert!(child.wait().expect("wait for event id writer").success());
        }

        let mut ids = Vec::new();
        for writer in 0..4 {
            let content = std::fs::read_to_string(dir.path().join(format!("writer-{writer}.ids")))
                .expect("read allocated ids");
            ids.extend(content.lines().map(|line| line.parse::<u64>().unwrap()));
        }
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 400, "every process must receive unique ids");
        assert_eq!(ids.first(), Some(&1));
        assert_eq!(ids.last(), Some(&400));
        let sequence_path = dir.path().join("events.seq");
        assert_eq!(read_persisted_sequence(&sequence_path), Some(400));
        assert_eq!(
            std::fs::read_to_string(sequence_path)
                .expect("read sequence journal")
                .lines()
                .count(),
            4,
            "four writers should persist four block reservations, not 400 ids"
        );
    }
}
