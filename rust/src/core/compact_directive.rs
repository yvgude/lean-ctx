// SPDX-License-Identifier: Apache-2.0

//! Store for the agent-driven conversation-compaction directive (#1570 P1).
//!
//! The MCP server cannot rewrite the client's conversation — only the proxy
//! sees the messages. `ctx_session(action="compact")` therefore records a
//! *directive*; the proxy applies it to the requests of the app session that
//! armed it (cache-safe, live-suffix only). Restore is lossless by
//! construction: the client resends the full original history each turn, so
//! clearing the directive is enough.
//!
//! One proxy carries many app sessions (the launching app's
//! `x-leanctx-session-id` header, `LEAN_CTX_SESSION_ID` for the agent's MCP
//! server). Every directive belongs to exactly one of them: it is stored
//! under that session, applied only to requests naming it, and a request
//! that names no valid session gets none (fail closed). The proxy also notes
//! each session it carries, so `ctx_session compact` can say honestly
//! whether a proxy would apply the directive at all.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A directive older than this is ignored and cleaned up.
const TTL_SECS: u64 = 1800;

/// Anti-obsession guard (#1570 P6): a fresh compact may only be created this
/// long after the session's previous one.
const MIN_INTERVAL_SECS: u64 = 300;

/// A session counts as carried by a proxy when one of its requests reached a
/// proxy within this window: an armed directive would apply to its next turn.
const CARRIED_WINDOW_SECS: u64 = TTL_SECS;

/// The proxy refreshes a session's carried note at most this often.
const CARRIED_REFRESH_SECS: u64 = 60;

const STORE_DIR: &str = "compact_directives";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactDirective {
    pub created_unix: u64,
    /// How many trailing plain-user turns stay verbatim.
    pub keep_recent_turns: usize,
    /// The agent-authored summary that replaces the compacted span.
    pub summary: String,
    /// Fingerprint of the first message AFTER the cut, pinned on first apply
    /// so the replaced prefix stays byte-identical while the conversation
    /// grows (provider prompt caches keep hitting).
    pub boundary_fingerprint: Option<String>,
    /// CCR handle of the verbatim replaced span (defense in depth — restore
    /// itself only needs the directive cleared).
    pub original_handle: Option<String>,
}

#[cfg(not(any(test, feature = "test-support")))]
fn now_unix() -> u64 {
    system_now()
}

#[cfg(any(test, feature = "test-support"))]
fn now_unix() -> u64 {
    system_now().saturating_add(TEST_CLOCK_AHEAD.load(std::sync::atomic::Ordering::SeqCst))
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Seconds the directive clock runs ahead of the system clock in tests.
#[cfg(any(test, feature = "test-support"))]
static TEST_CLOCK_AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Moves the directive clock ahead until the guard drops. The offset is
/// process-wide: hold `core::data_dir::isolated_data_dir()` for as long as a
/// guard lives.
#[cfg(any(test, feature = "test-support"))]
pub struct TestClock;

#[cfg(any(test, feature = "test-support"))]
impl TestClock {
    /// One second past the minimum re-arm interval of every directive armed
    /// so far, still inside their TTL.
    #[must_use]
    pub fn past_min_interval() -> Self {
        TEST_CLOCK_AHEAD.fetch_add(MIN_INTERVAL_SECS + 1, std::sync::atomic::Ordering::SeqCst);
        Self
    }

    /// One second past the TTL of every directive armed so far.
    #[must_use]
    pub fn past_ttl() -> Self {
        TEST_CLOCK_AHEAD.fetch_add(TTL_SECS + 1, std::sync::atomic::Ordering::SeqCst);
        Self
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestClock {
    fn drop(&mut self) {
        TEST_CLOCK_AHEAD.store(0, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The store file of `session` with `suffix`, or `None` when `session` is
/// no valid app session id or there is no data dir. Ids are hashed into the
/// file name: they may carry characters some file systems refuse.
fn session_path(session: &str, suffix: &str) -> Option<PathBuf> {
    let session = crate::core::activity::session_id(session)?;
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join(STORE_DIR);
    let key = blake3::hash(session.as_bytes()).to_hex();
    Some(dir.join(format!("{}.{suffix}", &key[..32])))
}

fn directive_path(session: &str) -> Option<PathBuf> {
    session_path(session, "json")
}

fn carried_path(session: &str) -> Option<PathBuf> {
    session_path(session, "carried")
}

/// Write `contents` to `path` through a sibling temp file and a rename, so a
/// reader in another process never sees half a file. Every write gets its
/// own temp name (file name, pid, process-wide counter): a session's
/// directive and carried note, and concurrent requests of one session, are
/// written from the same proxy process at the same time.
///
/// Once the temp file exists, every failure removes it (a full disk would
/// otherwise leave one orphan per proxied request). The replace goes through
/// `atomic_fs::replace_file`, which retries a target Windows Defender or the
/// indexer briefly holds open (#956).
fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    use std::io::Write as _;
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| "no store dir".to_string())?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "no store file name".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{name}.tmp{}.{n}", std::process::id()));
    // `create_new`: never truncate or adopt a file this write did not create,
    // so the cleanup below only ever removes our own temp file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let written = file.write_all(contents.as_bytes()).and_then(|()| {
        drop(file);
        crate::core::atomic_fs::replace_file(&tmp, path)
    });
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Create a new directive for `session`. Fails when the session has no
/// valid id, or when its previous one was created less than
/// `MIN_INTERVAL_SECS` ago (rate limit — nothing is ever blocked, the agent
/// simply keeps its current, already-compacted context).
pub fn create(session: &str, keep_recent_turns: usize, summary: String) -> Result<(), String> {
    if let Some(existing) = load_active(session)
        && now_unix().saturating_sub(existing.created_unix) < MIN_INTERVAL_SECS
    {
        return Err(format!(
            "a compaction from {}s ago is still active — one compact per {}s \
             (anti-obsession guard); use ctx_session action=\"restore\" first if needed",
            now_unix().saturating_sub(existing.created_unix),
            MIN_INTERVAL_SECS
        ));
    }
    let directive = CompactDirective {
        created_unix: now_unix(),
        keep_recent_turns,
        summary,
        boundary_fingerprint: None,
        original_handle: None,
    };
    save(session, &directive)
}

pub fn save(session: &str, directive: &CompactDirective) -> Result<(), String> {
    let mut admitted = directive.clone();
    admitted.summary =
        crate::core::session::admission::admit_storable_tool_text(&directive.summary)?;
    write_admitted(session, &admitted)
}

fn write_admitted(session: &str, directive: &CompactDirective) -> Result<(), String> {
    let path =
        directive_path(session).ok_or_else(|| "no app session id or data dir".to_string())?;
    let json = serde_json::to_string(directive).map_err(|e| e.to_string())?;
    write_atomic(&path, &json)
}

/// The directive of `session` when it exists and is younger than `TTL_SECS`.
pub fn load_active(session: &str) -> Option<CompactDirective> {
    let path = directive_path(session)?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let directive: CompactDirective = serde_json::from_str(&raw).ok()?;
    if now_unix().saturating_sub(directive.created_unix) > TTL_SECS {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let original_summary = directive.summary.clone();
    let Ok(admitted_summary) =
        crate::core::session::admission::admit_storable_tool_text(&original_summary)
    else {
        let _ = std::fs::remove_file(path);
        return None;
    };
    let mut admitted = directive;
    admitted.summary = admitted_summary;
    if admitted.summary != original_summary {
        write_admitted(session, &admitted).ok()?;
    }
    Some(admitted)
}

/// Drop the directive of `session` — its next request carries the full
/// original history again, so this alone is a complete, lossless restore.
pub fn clear(session: &str) -> bool {
    directive_path(session).is_some_and(|path| std::fs::remove_file(path).is_ok())
}

/// The proxy carried a request of `session` now. Best effort and throttled:
/// a failed note only makes a later `compact` reply say "no proxy".
pub fn note_carried(session: &str) {
    let Some(path) = carried_path(session) else {
        return;
    };
    let now = now_unix();
    let fresh =
        read_carried(&path).is_some_and(|seen| seen <= now && now - seen < CARRIED_REFRESH_SECS);
    if !fresh {
        let _ = write_atomic(&path, &now.to_string());
    }
}

/// Whether a proxy carried a request of `session` within the carried window.
pub fn is_carried(session: &str) -> bool {
    carried_path(session)
        .and_then(|path| read_carried(&path))
        .is_some_and(|seen| now_unix().saturating_sub(seen) <= CARRIED_WINDOW_SECS)
}

fn read_carried(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_roundtrip_rate_limit_and_clear() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let a = "session-a";
        assert!(load_active(a).is_none());

        create(a, 6, "summary one".into()).expect("first create succeeds");
        let d = load_active(a).expect("directive active");
        assert_eq!(d.keep_recent_turns, 6);
        assert!(d.boundary_fingerprint.is_none());

        // Rate limit: a second compact right away is refused with guidance.
        let err = create(a, 4, "summary two".into()).expect_err("rate-limited");
        assert!(err.contains("anti-obsession"), "{err}");

        assert!(clear(a));
        assert!(load_active(a).is_none());
        // After clear, creating again is allowed immediately.
        create(a, 4, "summary three".into()).expect("create after clear");
        clear(a);
    }

    #[test]
    fn directives_belong_to_one_session_and_need_a_valid_id() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        create("session-a", 4, "for a".into()).expect("a arms");
        assert!(load_active("session-b").is_none(), "b sees nothing");
        create("session-b", 4, "for b".into()).expect("b's own guard");
        assert!(!clear("session-c"));
        assert_eq!(load_active("session-a").unwrap().summary, "for a");
        for bad in ["", "  ", "bad id", "../x"] {
            assert!(create(bad, 4, "x".into()).is_err(), "{bad:?}");
            assert!(load_active(bad).is_none());
            assert!(!clear(bad));
            note_carried(bad);
            assert!(!is_carried(bad));
        }
    }

    #[test]
    fn expired_directive_is_ignored_and_removed() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let stale = CompactDirective {
            created_unix: now_unix() - TTL_SECS - 10,
            keep_recent_turns: 6,
            summary: "old".into(),
            boundary_fingerprint: None,
            original_handle: None,
        };
        save("session-a", &stale).unwrap();
        assert!(
            load_active("session-a").is_none(),
            "expired directive must not apply"
        );
        assert!(load_active("session-a").is_none());
    }

    /// A session's directive and carried note are written concurrently from
    /// one proxy process; neither write may land in the other's file.
    #[test]
    fn concurrent_writes_of_one_session_keep_both_files_whole() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let session = "session-concurrent";
        let directive = CompactDirective {
            created_unix: now_unix(),
            keep_recent_turns: 4,
            summary: "concurrent summary".into(),
            boundary_fingerprint: Some("0123456789abcdef".into()),
            original_handle: None,
        };
        let json = serde_json::to_string(&directive).unwrap();
        let (directive_file, carried_file) = (
            directive_path(session).unwrap(),
            carried_path(session).unwrap(),
        );
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..50 {
                        write_atomic(&directive_file, &json).expect("directive write");
                        write_atomic(&carried_file, &now_unix().to_string())
                            .expect("carried write");
                        save(session, &directive).expect("save");
                        note_carried(session);
                    }
                });
            }
        });
        let stored = load_active(session).expect("the directive parses");
        assert_eq!(stored.summary, "concurrent summary");
        assert!(is_carried(session), "the carried note parses");
        let leftovers: Vec<_> = std::fs::read_dir(directive_file.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files left: {leftovers:?}");
    }

    /// A write that fails after its temp file exists removes the temp file
    /// again: a failing store (full disk, a target that cannot be replaced)
    /// is retried on every proxied request and must not leak one file each.
    #[test]
    fn failed_writes_leave_no_temp_files() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let carried = carried_path("session-fail").unwrap();
        // A non-empty directory at the target: the replace fails every time.
        std::fs::create_dir_all(carried.join("blocker")).unwrap();
        for _ in 0..3 {
            assert!(write_atomic(&carried, "1").is_err(), "the write must fail");
            note_carried("session-fail");
        }
        let leftovers: Vec<_> = std::fs::read_dir(carried.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files left: {leftovers:?}");
        assert!(!is_carried("session-fail"));
    }

    #[test]
    fn carried_note_lasts_the_window() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        assert!(!is_carried("session-a"));
        note_carried("session-a");
        assert!(is_carried("session-a"));
        assert!(!is_carried("session-b"));
        let _later = TestClock::past_ttl();
        assert!(!is_carried("session-a"), "an old note no longer counts");
    }
}
