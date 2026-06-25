//! Audit-trail aggregation over a date range (GL #677).
//!
//! Reads the append-only audit chain (`<data_dir>/audit/trail.jsonl`) and folds
//! the entries whose `timestamp` falls inside `[from, to]` into the
//! privacy-preserving counts a compliance report needs: how many agent actions
//! were **blocked** (`ToolDenied`) and how much content was **redacted**
//! (`SecretDetected`), plus the chain anchor/head that bind those counts to the
//! exact append-only segment that produced them.
//!
//! Mirrors the streaming, multi-object-per-line tolerant parse of
//! [`crate::core::evidence_bundle`] (concurrent appends have historically
//! produced two back-to-back JSON objects on one line), but — unlike an
//! evidence bundle — an **empty** window is a valid, healthy result (a quiet
//! period with zero violations), never an error.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};

use crate::core::audit_trail::{AuditEntry, AuditEventType};

/// Folded, privacy-preserving view of one audit-trail segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregation {
    /// Entries whose timestamp fell inside the window.
    pub entries: usize,
    /// `ToolCall` events (the denominator for an enforcement rate).
    pub tool_calls: usize,
    /// `ToolDenied` events — agent actions blocked by role/policy/egress.
    pub blocked: usize,
    /// `SecretDetected` events — outputs where redaction fired.
    pub redacted: usize,
    /// Other non-`ToolCall` security events (path-jail, budget, rate-limit, …).
    pub other_security: usize,
    /// `(event_label, count)` for every event type seen, sorted by label.
    pub by_event: Vec<(String, usize)>,
    /// `(tool, blocked_count)` for blocked actions, top rows by count.
    pub by_tool_blocked: Vec<(String, usize)>,
    /// `prev_hash` of the first in-window entry (`genesis` when empty).
    pub anchor_prev_hash: String,
    /// `entry_hash` of the last in-window entry (`""` when empty).
    pub head_hash: String,
}

/// Cap on `by_tool_blocked` rows embedded in a report (keeps it bounded).
const MAX_TOOL_ROWS: usize = 12;

/// Stable `snake_case` label for an event type (matches the on-disk encoding,
/// without depending on serde formatting on the hot path).
#[must_use]
pub fn event_label(ev: &AuditEventType) -> &'static str {
    match ev {
        AuditEventType::ToolCall => "tool_call",
        AuditEventType::ToolDenied => "tool_denied",
        AuditEventType::PathJailViolation => "path_jail_violation",
        AuditEventType::BudgetExceeded => "budget_exceeded",
        AuditEventType::CrossProjectAccess => "cross_project_access",
        AuditEventType::RateLimited => "rate_limited",
        AuditEventType::SecurityViolation => "security_violation",
        AuditEventType::RoleChanged => "role_changed",
        AuditEventType::SecretDetected => "secret_detected",
        AuditEventType::AgentRegistered => "agent_registered",
        AuditEventType::AgentSuspended => "agent_suspended",
        AuditEventType::AgentResumed => "agent_resumed",
        AuditEventType::AgentDecommissioned => "agent_decommissioned",
    }
}

/// Aggregate the on-disk audit trail over `[from, to]` (both inclusive).
///
/// A missing trail file yields an empty aggregation (no audit activity yet),
/// not an error — a fresh install can still produce a (zero-violation) report.
pub fn aggregate(
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> Result<Aggregation, String> {
    let trail_path = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| format!("data dir: {e}"))?
        .join("audit")
        .join("trail.jsonl");
    let raw = match std::fs::read_to_string(&trail_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(empty()),
        Err(e) => return Err(format!("read {}: {e}", trail_path.display())),
    };
    aggregate_str(&raw, from, to)
}

/// Pure aggregation over raw JSONL — testable without touching the data dir.
pub fn aggregate_str(
    raw: &str,
    from: DateTime<FixedOffset>,
    to: DateTime<FixedOffset>,
) -> Result<Aggregation, String> {
    let mut agg = empty();
    let mut events: BTreeMap<String, usize> = BTreeMap::new();
    let mut tools: BTreeMap<String, usize> = BTreeMap::new();
    let mut anchor: Option<String> = None;

    for line in raw.lines() {
        // Tolerate concurrent-append history (`…}{…`) with a streaming parse.
        for value in serde_json::Deserializer::from_str(line)
            .into_iter::<serde_json::Value>()
            .flatten()
        {
            let Ok(entry) = serde_json::from_value::<AuditEntry>(value) else {
                continue;
            };
            let Ok(ts) = DateTime::parse_from_rfc3339(&entry.timestamp) else {
                continue;
            };
            if ts < from || ts > to {
                continue;
            }

            if anchor.is_none() {
                anchor = Some(entry.prev_hash.clone());
            }
            agg.head_hash.clone_from(&entry.entry_hash);
            agg.entries += 1;
            *events
                .entry(event_label(&entry.event_type).to_string())
                .or_default() += 1;

            match entry.event_type {
                AuditEventType::ToolCall => agg.tool_calls += 1,
                AuditEventType::ToolDenied => {
                    agg.blocked += 1;
                    *tools.entry(entry.tool.clone()).or_default() += 1;
                }
                AuditEventType::SecretDetected => agg.redacted += 1,
                _ => agg.other_security += 1,
            }
        }
    }

    agg.anchor_prev_hash = anchor.unwrap_or_else(|| "genesis".to_string());
    agg.by_event = events.into_iter().collect();
    agg.by_tool_blocked = top_rows(tools);
    Ok(agg)
}

fn empty() -> Aggregation {
    Aggregation {
        entries: 0,
        tool_calls: 0,
        blocked: 0,
        redacted: 0,
        other_security: 0,
        by_event: Vec::new(),
        by_tool_blocked: Vec::new(),
        anchor_prev_hash: "genesis".to_string(),
        head_hash: String::new(),
    }
}

/// Sort `(tool, count)` by count desc then tool asc (stable, deterministic),
/// capped at [`MAX_TOOL_ROWS`].
fn top_rows(map: BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut rows: Vec<(String, usize)> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(MAX_TOOL_ROWS);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    /// One audit line with a fixed (already-broken-out) shape. Hashes are
    /// arbitrary here — aggregation never recomputes the chain, it only reads
    /// the recorded `prev_hash`/`entry_hash`.
    fn line(ts: &str, event: &str, tool: &str, prev: &str, hash: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","agent_id":"a","tool":"{tool}","action":null,"input_hash":"x","output_tokens":0,"role":"r","event_type":"{event}","prev_hash":"{prev}","entry_hash":"{hash}"}}"#
        )
    }

    #[test]
    fn counts_blocked_and_redacted_in_window() {
        let raw = [
            line(
                "2026-06-01T10:00:00+00:00",
                "tool_call",
                "ctx_read",
                "genesis",
                "h1",
            ),
            line(
                "2026-06-01T11:00:00+00:00",
                "tool_denied",
                "ctx_url_read",
                "h1",
                "h2",
            ),
            line(
                "2026-06-01T12:00:00+00:00",
                "secret_detected",
                "ctx_read",
                "h2",
                "h3",
            ),
            line(
                "2026-06-01T13:00:00+00:00",
                "tool_denied",
                "ctx_url_read",
                "h3",
                "h4",
            ),
        ]
        .join("\n");
        let agg = aggregate_str(
            &raw,
            ts("2026-06-01T00:00:00+00:00"),
            ts("2026-06-02T00:00:00+00:00"),
        )
        .unwrap();
        assert_eq!(agg.entries, 4);
        assert_eq!(agg.blocked, 2);
        assert_eq!(agg.redacted, 1);
        assert_eq!(agg.tool_calls, 1);
        assert_eq!(agg.anchor_prev_hash, "genesis");
        assert_eq!(agg.head_hash, "h4");
        assert_eq!(agg.by_tool_blocked, vec![("ctx_url_read".to_string(), 2)]);
    }

    #[test]
    fn excludes_entries_outside_window() {
        let raw = [
            line(
                "2026-05-01T10:00:00+00:00",
                "tool_denied",
                "ctx_url_read",
                "genesis",
                "h1",
            ),
            line(
                "2026-06-15T10:00:00+00:00",
                "tool_denied",
                "ctx_url_read",
                "h1",
                "h2",
            ),
        ]
        .join("\n");
        let agg = aggregate_str(
            &raw,
            ts("2026-06-01T00:00:00+00:00"),
            ts("2026-07-01T00:00:00+00:00"),
        )
        .unwrap();
        assert_eq!(agg.entries, 1);
        assert_eq!(agg.blocked, 1);
        assert_eq!(agg.anchor_prev_hash, "h1");
        assert_eq!(agg.head_hash, "h2");
    }

    #[test]
    fn empty_window_is_ok_not_error() {
        let raw = line(
            "2026-01-01T10:00:00+00:00",
            "tool_denied",
            "ctx_url_read",
            "genesis",
            "h1",
        );
        let agg = aggregate_str(
            &raw,
            ts("2026-06-01T00:00:00+00:00"),
            ts("2026-07-01T00:00:00+00:00"),
        )
        .unwrap();
        assert_eq!(agg, super::empty());
    }

    #[test]
    fn tolerates_two_objects_on_one_line() {
        let a = line(
            "2026-06-01T10:00:00+00:00",
            "tool_denied",
            "ctx_url_read",
            "genesis",
            "h1",
        );
        let b = line(
            "2026-06-01T10:00:01+00:00",
            "secret_detected",
            "ctx_read",
            "h1",
            "h2",
        );
        let raw = format!("{a}{b}"); // concatenated, no newline
        let agg = aggregate_str(
            &raw,
            ts("2026-06-01T00:00:00+00:00"),
            ts("2026-06-02T00:00:00+00:00"),
        )
        .unwrap();
        assert_eq!(agg.entries, 2);
        assert_eq!(agg.blocked, 1);
        assert_eq!(agg.redacted, 1);
    }
}
