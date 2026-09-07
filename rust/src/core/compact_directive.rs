//! Store for the agent-driven conversation-compaction directive (#1570 P1).
//!
//! The MCP server cannot rewrite the client's conversation — only the proxy
//! sees the messages. `ctx_session(action="compact")` therefore records a
//! *directive*; the proxy applies it to every subsequent request (cache-safe,
//! live-suffix only). Restore is lossless by construction: the client resends
//! the full original history each turn, so clearing the directive is enough.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A directive older than this is ignored and cleaned up.
const TTL_SECS: u64 = 1800;

/// Anti-obsession guard (#1570 P6): a fresh compact may only be created this
/// long after the previous one.
const MIN_INTERVAL_SECS: u64 = 300;

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

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn store_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("compact_directive.json"))
}

/// Create a new directive. Fails when one was created less than
/// `MIN_INTERVAL_SECS` ago (rate limit — nothing is ever blocked, the agent
/// simply keeps its current, already-compacted context).
pub fn create(keep_recent_turns: usize, summary: String) -> Result<(), String> {
    if let Some(existing) = load_active()
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
    save(&directive)
}

pub fn save(directive: &CompactDirective) -> Result<(), String> {
    let path = store_path().ok_or_else(|| "no data dir".to_string())?;
    let json = serde_json::to_string(directive).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Load the directive when it exists and is younger than `TTL_SECS`.
pub fn load_active() -> Option<CompactDirective> {
    let path = store_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let directive: CompactDirective = serde_json::from_str(&raw).ok()?;
    if now_unix().saturating_sub(directive.created_unix) > TTL_SECS {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(directive)
}

/// Drop the directive — the client's next request carries the full original
/// history again, so this alone is a complete, lossless restore.
pub fn clear() -> bool {
    store_path().is_some_and(|path| std::fs::remove_file(path).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_roundtrip_rate_limit_and_clear() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _dir = crate::core::data_dir::isolated_data_dir();
        assert!(load_active().is_none());

        create(6, "summary one".into()).expect("first create succeeds");
        let d = load_active().expect("directive active");
        assert_eq!(d.keep_recent_turns, 6);
        assert!(d.boundary_fingerprint.is_none());

        // Rate limit: a second compact right away is refused with guidance.
        let err = create(4, "summary two".into()).expect_err("rate-limited");
        assert!(err.contains("anti-obsession"), "{err}");

        assert!(clear());
        assert!(load_active().is_none());
        // After clear, creating again is allowed immediately.
        create(4, "summary three".into()).expect("create after clear");
        clear();
    }

    #[test]
    fn expired_directive_is_ignored_and_removed() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _dir = crate::core::data_dir::isolated_data_dir();
        let stale = CompactDirective {
            created_unix: now_unix() - TTL_SECS - 10,
            keep_recent_turns: 6,
            summary: "old".into(),
            boundary_fingerprint: None,
            original_handle: None,
        };
        save(&stale).unwrap();
        assert!(load_active().is_none(), "expired directive must not apply");
        assert!(load_active().is_none());
    }
}
