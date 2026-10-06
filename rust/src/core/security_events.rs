// SPDX-License-Identifier: Apache-2.0
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
    /// A checksum-validated PII value was masked by the context gateway.
    PiiRedacted,
    /// The context gateway withheld a source entirely.
    ContentWithheld,
    /// A source was delivered although a detector did not inspect all of it.
    CoverageIncomplete,
}

impl SecurityKind {
    /// Stable identifier committed into the audit trail's `action` field.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SecretRedacted => "secret_redacted",
            Self::ShellBlocked => "shell_blocked",
            Self::PathBlocked => "path_blocked",
            Self::InjectionFlagged => "injection_flagged",
            Self::PiiRedacted => "pii_redacted",
            Self::ContentWithheld => "content_withheld",
            Self::CoverageIncomplete => "coverage_incomplete",
        }
    }

    /// Existing trail event types only: an unknown variant would read as
    /// tampering to an older verifier, so the kind lives in `action`.
    fn audit_event_type(self) -> crate::core::audit_trail::AuditEventType {
        use crate::core::audit_trail::AuditEventType;
        match self {
            Self::SecretRedacted | Self::PiiRedacted => AuditEventType::SecretDetected,
            Self::ShellBlocked | Self::ContentWithheld => AuditEventType::ToolDenied,
            Self::PathBlocked => AuditEventType::PathJailViolation,
            Self::InjectionFlagged | Self::CoverageIncomplete => AuditEventType::SecurityViolation,
        }
    }

    /// Parses the `action` prefix written by [`record`] back into a kind.
    pub fn from_action(action: &str) -> Option<Self> {
        let kind = action.split(':').next()?;
        Self::ALL.into_iter().find(|k| k.as_str() == kind)
    }

    /// Kinds a signed savings-batch tally carries; server mirrors verify the
    /// tally field by field and predate the context-gateway kinds.
    #[must_use]
    pub const fn in_signed_tally(self) -> bool {
        matches!(
            self,
            Self::SecretRedacted | Self::ShellBlocked | Self::PathBlocked | Self::InjectionFlagged
        )
    }

    const ALL: [Self; 7] = [
        Self::SecretRedacted,
        Self::ShellBlocked,
        Self::PathBlocked,
        Self::InjectionFlagged,
        Self::PiiRedacted,
        Self::ContentWithheld,
        Self::CoverageIncomplete,
    ];
}

/// Per-kind event counts. Persisted inside `SessionStats`, so every field is
/// `serde(default)` and old session files load unchanged.
///
/// The context-gateway kinds are written only when non-zero: every count
/// serialized before they existed keeps its exact bytes (signed tallies, see
/// [`Self::signed_tally_projection`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityCounts {
    pub secrets_redacted: u64,
    pub shell_blocked: u64,
    pub path_blocked: u64,
    pub injection_flagged: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub pii_redacted: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub content_withheld: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub coverage_incomplete: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl SecurityCounts {
    /// The counts a signed savings-batch tally carries. Server mirrors verify
    /// the tally field by field and do not know the gateway kinds yet, so they
    /// are left out (zero, hence not serialized) instead of breaking the
    /// signature; the audit trail still proves them locally.
    #[must_use]
    pub fn signed_tally_projection(&self) -> Self {
        Self {
            pii_redacted: 0,
            content_withheld: 0,
            coverage_incomplete: 0,
            ..*self
        }
    }

    pub const ZERO: Self = Self {
        secrets_redacted: 0,
        shell_blocked: 0,
        path_blocked: 0,
        injection_flagged: 0,
        pii_redacted: 0,
        content_withheld: 0,
        coverage_incomplete: 0,
    };

    fn slot(&mut self, kind: SecurityKind) -> &mut u64 {
        match kind {
            SecurityKind::SecretRedacted => &mut self.secrets_redacted,
            SecurityKind::ShellBlocked => &mut self.shell_blocked,
            SecurityKind::PathBlocked => &mut self.path_blocked,
            SecurityKind::InjectionFlagged => &mut self.injection_flagged,
            SecurityKind::PiiRedacted => &mut self.pii_redacted,
            SecurityKind::ContentWithheld => &mut self.content_withheld,
            SecurityKind::CoverageIncomplete => &mut self.coverage_incomplete,
        }
    }

    fn get(&self, kind: SecurityKind) -> u64 {
        match kind {
            SecurityKind::SecretRedacted => self.secrets_redacted,
            SecurityKind::ShellBlocked => self.shell_blocked,
            SecurityKind::PathBlocked => self.path_blocked,
            SecurityKind::InjectionFlagged => self.injection_flagged,
            SecurityKind::PiiRedacted => self.pii_redacted,
            SecurityKind::ContentWithheld => self.content_withheld,
            SecurityKind::CoverageIncomplete => self.coverage_incomplete,
        }
    }

    pub fn add(&mut self, kind: SecurityKind, n: u64) {
        let slot = self.slot(kind);
        *slot = slot.saturating_add(n);
    }

    pub fn merge(&mut self, other: &Self) {
        for kind in SecurityKind::ALL {
            self.add(kind, other.get(kind));
        }
    }

    /// Events since `base` (a counter never goes below zero).
    #[must_use]
    pub fn since(&self, base: &Self) -> Self {
        let mut out = Self::ZERO;
        for kind in SecurityKind::ALL {
            *out.slot(kind) = self.get(kind).saturating_sub(base.get(kind));
        }
        out
    }

    pub fn total(&self) -> u64 {
        SecurityKind::ALL
            .into_iter()
            .fold(0u64, |sum, kind| sum.saturating_add(self.get(kind)))
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    fn iter(&self) -> impl Iterator<Item = (SecurityKind, u64)> + '_ {
        SecurityKind::ALL
            .into_iter()
            .map(|kind| (kind, self.get(kind)))
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
static PENDING: Mutex<SecurityCounts> = Mutex::new(SecurityCounts::ZERO);

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

/// Anchors a Context Gateway receipt in the signed, hash-chained audit trail:
/// the digest in the hashed `action` binds the stored receipt to the chain.
/// Not a counted kind (`parse_action` ignores it), so nothing is counted twice.
pub fn anchor_receipt(tool: &str, agent_id: &str, receipt_sha256_hex: &str) {
    let session = crate::core::value::current_session()
        .map(|id| format!("|session={id}"))
        .unwrap_or_default();
    crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
        agent_id: agent_id.to_string(),
        tool: tool.to_string(),
        action: Some(format!("gateway_receipt:{receipt_sha256_hex}{session}")),
        input_hash: String::new(),
        output_tokens: 0,
        role: crate::core::roles::active_role_name(),
        event_type: crate::core::audit_trail::AuditEventType::ToolCall,
    });
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
