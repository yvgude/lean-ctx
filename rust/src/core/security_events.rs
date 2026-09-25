//! Provable security events for the value surface.
//!
//! lean-ctx's default-on protections (secret redaction, the shell allowlist,
//! the path jail, prompt-injection screening) used to act silently: nothing a
//! user could later point to. This module turns each firing into
//!
//! 1. a hash-chained, signed [`crate::core::audit_trail`] entry — the proof, and
//! 2. a session counter ([`SecurityCounts`]) — what the status line shows.
//!
//! Counting is **once per tool result**. Deep code (redaction, allowlist) calls
//! [`note`], which only lands in the collector that
//! [`collect`] opens around one MCP tool handler; the dispatch layer then
//! records the tally for that single result. `note` outside a collector (CLI,
//! hooks, config loading) is deliberately dropped, so a secret redacted twice
//! on its way through nested passes, or a re-rendered cache entry outside a
//! tool call, never inflates the count. Undercounting is acceptable; claiming
//! more than happened is not.
//!
//! The audit trail's `event_type` enum is **not** extended: an older binary
//! verifying a trail that contains an unknown variant would report it as
//! tampered. The kind is committed in the (hashed) `action` field instead.

use std::cell::RefCell;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// What a protection did. The wording each kind renders with is part of the
/// honesty contract: injections are *flagged*, never "neutralized".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityKind {
    /// A secret was replaced by `[REDACTED:…]` before the model saw it.
    SecretRedacted,
    /// A shell command was refused by the allowlist / dangerous-pattern gate.
    ShellBlocked,
    /// A path outside the project jail was refused.
    PathBlocked,
    /// Tool output matched prompt-injection patterns and was flagged.
    InjectionFlagged,
}

impl SecurityKind {
    /// Stable identifier committed into the audit trail's `action` field.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SecretRedacted => "secret_redacted",
            Self::ShellBlocked => "shell_blocked",
            Self::PathBlocked => "path_blocked",
            Self::InjectionFlagged => "injection_flagged",
        }
    }

    fn audit_event_type(self) -> crate::core::audit_trail::AuditEventType {
        use crate::core::audit_trail::AuditEventType;
        match self {
            Self::SecretRedacted => AuditEventType::SecretDetected,
            Self::ShellBlocked => AuditEventType::ToolDenied,
            Self::PathBlocked => AuditEventType::PathJailViolation,
            Self::InjectionFlagged => AuditEventType::SecurityViolation,
        }
    }

    /// Parses the `action` prefix written by [`record`] back into a kind.
    pub fn from_action(action: &str) -> Option<Self> {
        let kind = action.split(':').next()?;
        [
            Self::SecretRedacted,
            Self::ShellBlocked,
            Self::PathBlocked,
            Self::InjectionFlagged,
        ]
        .into_iter()
        .find(|k| k.as_str() == kind)
    }
}

/// Per-kind event counts. Persisted inside `SessionStats`, so every field is
/// `serde(default)` and old session files load unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityCounts {
    pub secrets_redacted: u64,
    pub shell_blocked: u64,
    pub path_blocked: u64,
    pub injection_flagged: u64,
}

impl SecurityCounts {
    pub fn add(&mut self, kind: SecurityKind, n: u64) {
        let slot = match kind {
            SecurityKind::SecretRedacted => &mut self.secrets_redacted,
            SecurityKind::ShellBlocked => &mut self.shell_blocked,
            SecurityKind::PathBlocked => &mut self.path_blocked,
            SecurityKind::InjectionFlagged => &mut self.injection_flagged,
        };
        *slot = slot.saturating_add(n);
    }

    pub fn merge(&mut self, other: &Self) {
        self.secrets_redacted = self.secrets_redacted.saturating_add(other.secrets_redacted);
        self.shell_blocked = self.shell_blocked.saturating_add(other.shell_blocked);
        self.path_blocked = self.path_blocked.saturating_add(other.path_blocked);
        self.injection_flagged = self
            .injection_flagged
            .saturating_add(other.injection_flagged);
    }

    /// Events since `base` (a counter never goes below zero).
    #[must_use]
    pub fn since(&self, base: &Self) -> Self {
        Self {
            secrets_redacted: self.secrets_redacted.saturating_sub(base.secrets_redacted),
            shell_blocked: self.shell_blocked.saturating_sub(base.shell_blocked),
            path_blocked: self.path_blocked.saturating_sub(base.path_blocked),
            injection_flagged: self
                .injection_flagged
                .saturating_sub(base.injection_flagged),
        }
    }

    pub fn total(&self) -> u64 {
        self.secrets_redacted
            .saturating_add(self.shell_blocked)
            .saturating_add(self.path_blocked)
            .saturating_add(self.injection_flagged)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    fn iter(&self) -> impl Iterator<Item = (SecurityKind, u64)> {
        [
            (SecurityKind::SecretRedacted, self.secrets_redacted),
            (SecurityKind::ShellBlocked, self.shell_blocked),
            (SecurityKind::PathBlocked, self.path_blocked),
            (SecurityKind::InjectionFlagged, self.injection_flagged),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
    }
}

thread_local! {
    static COLLECTOR: RefCell<Option<SecurityCounts>> = const { RefCell::new(None) };
}

/// Notes `n` firings of `kind` into the collector of the tool call running on
/// this thread. A no-op outside [`collect`] — see the module docs for why.
pub fn note(kind: SecurityKind, n: usize) {
    if n == 0 {
        return;
    }
    COLLECTOR.with(|c| {
        if let Some(counts) = c.borrow_mut().as_mut() {
            counts.add(kind, n as u64);
        }
    });
}

/// Runs `f` (one synchronous tool handler) with a fresh collector and returns
/// what it noted. Nested calls keep the outer collector's tally intact.
pub fn collect<T>(f: impl FnOnce() -> T) -> (T, SecurityCounts) {
    let outer = COLLECTOR.with(|c| c.borrow_mut().replace(SecurityCounts::default()));
    let value = f();
    let tally = COLLECTOR
        .with(|c| std::mem::replace(&mut *c.borrow_mut(), outer))
        .unwrap_or_default();
    if let Some(mut outer) = outer {
        outer.merge(&tally);
        COLLECTOR.with(|c| *c.borrow_mut() = Some(outer));
    }
    (value, tally)
}

/// Counts recorded since the MCP server last drained them into its session.
static PENDING: Mutex<SecurityCounts> = Mutex::new(SecurityCounts {
    secrets_redacted: 0,
    shell_blocked: 0,
    path_blocked: 0,
    injection_flagged: 0,
});

/// Records one tool result's security tally: one audit-trail entry per kind
/// (the proof) plus the pending session counters (the display).
pub fn record(tool: &str, agent_id: &str, counts: &SecurityCounts) {
    if counts.is_empty() {
        return;
    }
    let role = crate::core::roles::active_role_name();
    // The session id rides in the hashed `action` field (the trail's timestamp
    // is not hashed), so per-session security claims are chain-backed.
    let session = crate::core::value::current_session()
        .map(|id| format!("|session={id}"))
        .unwrap_or_default();
    for (kind, n) in counts.iter() {
        crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
            agent_id: agent_id.to_string(),
            tool: tool.to_string(),
            action: Some(format!("{}:{n}{session}", kind.as_str())),
            input_hash: String::new(),
            output_tokens: 0,
            role: role.clone(),
            event_type: kind.audit_event_type(),
        });
    }
    if let Ok(mut pending) = PENDING.lock() {
        pending.merge(counts);
    }
}

/// Convenience for a single event recorded outside a handler (dispatch layer).
pub fn record_one(tool: &str, agent_id: &str, kind: SecurityKind) {
    let mut counts = SecurityCounts::default();
    counts.add(kind, 1);
    record(tool, agent_id, &counts);
}

/// Takes the counts recorded since the last drain (the MCP server folds them
/// into the current session's stats).
pub fn drain_pending() -> SecurityCounts {
    PENDING
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default()
}

/// Splits a recorded `action` (`<kind>:<n>[|session=<id>]`) into its parts.
pub fn parse_action(action: &str) -> Option<(SecurityKind, u64, Option<&str>)> {
    let kind = SecurityKind::from_action(action)?;
    let rest = action.split_once(':').map_or("", |(_, rest)| rest);
    let (n, session) = match rest.split_once("|session=") {
        Some((n, session)) => (n, Some(session)),
        None => (rest, None),
    };
    Some((kind, n.parse().unwrap_or(1), session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_outside_collector_is_dropped() {
        note(SecurityKind::SecretRedacted, 3);
        let ((), tally) = collect(|| {});
        assert!(tally.is_empty());
    }

    #[test]
    fn collect_counts_only_its_own_handler() {
        let ((), tally) = collect(|| {
            note(SecurityKind::SecretRedacted, 2);
            note(SecurityKind::ShellBlocked, 1);
        });
        assert_eq!(tally.secrets_redacted, 2);
        assert_eq!(tally.shell_blocked, 1);
        assert_eq!(tally.total(), 3);

        let ((), next) = collect(|| {});
        assert!(
            next.is_empty(),
            "a collector never leaks into the next call"
        );
    }

    #[test]
    fn nested_collect_keeps_outer_total() {
        let ((), outer) = collect(|| {
            note(SecurityKind::PathBlocked, 1);
            let ((), inner) = collect(|| note(SecurityKind::PathBlocked, 1));
            assert_eq!(inner.path_blocked, 1);
        });
        assert_eq!(outer.path_blocked, 2);
    }

    #[test]
    fn action_roundtrips_kind() {
        for kind in [
            SecurityKind::SecretRedacted,
            SecurityKind::ShellBlocked,
            SecurityKind::PathBlocked,
            SecurityKind::InjectionFlagged,
        ] {
            let action = format!("{}:4", kind.as_str());
            assert_eq!(SecurityKind::from_action(&action), Some(kind));
        }
        assert_eq!(SecurityKind::from_action("tool_call"), None);
    }

    #[test]
    fn parse_action_reads_count_and_session() {
        assert_eq!(
            parse_action("secret_redacted:3|session=abc"),
            Some((SecurityKind::SecretRedacted, 3, Some("abc")))
        );
        assert_eq!(
            parse_action("shell_blocked:1"),
            Some((SecurityKind::ShellBlocked, 1, None))
        );
    }

    #[test]
    fn old_session_json_loads_with_zero_counts() {
        let counts: SecurityCounts = serde_json::from_str("{}").unwrap();
        assert!(counts.is_empty());
    }
}
