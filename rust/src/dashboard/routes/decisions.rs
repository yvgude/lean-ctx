// SPDX-License-Identifier: Apache-2.0
//! `GET /api/hive/decisions?range=24h|7d|30d`: the gateway's decision log
//! without content (owner checklist K11), read from the signed audit trail.
//!
//! One row per decision the gateway made: when, what (blocked, redacted,
//! withheld, flagged), the rule id, the tool, the agent host family and the
//! path class. Never the entry's `action` text, input hash, paths, commands or
//! any other value: tool names outside the registered pattern read `other`,
//! hosts outside the known families read `other`. The trail records no
//! policy version per decision and no monitor-mode ("would block") outcome
//! yet; both are reported as not recorded instead of being guessed.

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::core::audit_trail::{AuditEntry, AuditEventType};
use crate::core::security_events::{SecurityKind, parse_action};

use super::helpers::extract_query_param;

/// Rows returned at most, newest first.
const MAX_ROWS: usize = 2000;
const HOSTS: &[&str] = &[
    "claude", "cursor", "codex", "copilot", "windsurf", "gemini", "opencode", "zed", "cline",
    "aider", "kiro", "amp", "pi",
];

pub(super) fn handle(
    path: &str,
    query: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/hive/decisions" || !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    let range = extract_query_param(query, "range").unwrap_or_else(|| "7d".into());
    let Some(days) = days_of(&range) else {
        return Some((
            "400 Bad Request",
            "application/json",
            json!({ "error": { "code": "invalid_field", "field": "range" } }).to_string(),
        ));
    };
    let now = Utc::now();
    let since = now - Duration::days(days);
    let text = crate::core::audit_trail::default_trail_path()
        .and_then(|trail| crate::core::audit_trail::read_trail_since(&trail, Some(since)).ok())
        .flatten()
        .unwrap_or_default();
    Some((
        "200 OK",
        "application/json",
        log(&text, since, &range).to_string(),
    ))
}

fn days_of(range: &str) -> Option<i64> {
    match range {
        "24h" => Some(1),
        "7d" => Some(7),
        "30d" => Some(30),
        _ => None,
    }
}

/// One decision row of an audit entry, or `None` for an entry that records
/// no gateway decision.
fn decision(entry: &AuditEntry) -> Option<Value> {
    let (decision, rule, path_class) = match &entry.event_type {
        AuditEventType::ToolDenied => ("blocked", "gateway.tool_denied", None),
        AuditEventType::PathJailViolation => {
            ("blocked", "gateway.path_jail", Some("outside_project"))
        }
        AuditEventType::BudgetExceeded => ("blocked", "gateway.budget_exceeded", None),
        AuditEventType::CrossProjectAccess => {
            ("blocked", "gateway.cross_project", Some("other_project"))
        }
        AuditEventType::RateLimited => ("blocked", "gateway.rate_limited", None),
        AuditEventType::SecurityViolation => ("blocked", "gateway.security_violation", None),
        AuditEventType::SecretDetected => ("redacted", "guard.secret_detected", None),
        AuditEventType::ToolCall => {
            let (kind, _, _) = parse_action(entry.action.as_deref()?)?;
            match kind {
                SecurityKind::SecretRedacted => ("redacted", "guard.secret_redacted", None),
                SecurityKind::PiiRedacted => ("redacted", "guard.pii_redacted", None),
                SecurityKind::ContentWithheld => ("withheld", "guard.content_withheld", None),
                SecurityKind::InjectionFlagged => ("flagged", "guard.injection_flagged", None),
                SecurityKind::PathBlocked => {
                    ("blocked", "guard.path_blocked", Some("outside_project"))
                }
                SecurityKind::ShellBlocked => ("blocked", "guard.shell_blocked", None),
                SecurityKind::CoverageIncomplete => ("flagged", "guard.coverage_incomplete", None),
            }
        }
        _ => return None,
    };
    Some(json!({
        "at": entry.timestamp,
        "decision": decision,
        "rule": rule,
        "tool": tool_name(&entry.tool),
        "host": host_family(&entry.agent_id),
        "path_class": path_class,
        "policy_version": Value::Null,
    }))
}

/// A registered-looking tool name, else `other`: never a command or path.
fn tool_name(raw: &str) -> &str {
    let ok = (1..=48).contains(&raw.len())
        && raw.as_bytes()[0].is_ascii_lowercase()
        && raw
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok { raw } else { "other" }
}

/// The agent host family of an agent id, else `other`.
fn host_family(agent_id: &str) -> &'static str {
    let lower = agent_id.to_ascii_lowercase();
    HOSTS
        .iter()
        .find(|host| {
            lower == **host
                || lower
                    .strip_prefix(**host)
                    .is_some_and(|rest| rest.starts_with(['-', '_', ':', ' ']))
        })
        .copied()
        .unwrap_or("other")
}

fn log(text: &str, since: DateTime<Utc>, range: &str) -> Value {
    let mut rows: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<AuditEntry>(line) else {
            continue;
        };
        let at = DateTime::parse_from_rfc3339(&entry.timestamp).map(|t| t.to_utc());
        if !at.is_ok_and(|at| at >= since) {
            continue;
        }
        if let Some(row) = decision(&entry) {
            total += 1;
            rows.push(row);
        }
    }
    rows.reverse();
    let truncated = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    json!({
        "range": range,
        "total": total,
        "truncated": truncated,
        "items": rows,
        "not_recorded": ["policy_version", "would_block", "program"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(event: &str, tool: &str, agent: &str, action: Option<&str>, at: &str) -> String {
        json!({
            "timestamp": at, "agent_id": agent, "tool": tool, "action": action,
            "input_hash": "abc", "output_tokens": 0, "role": "default", "event_type": event,
            "prev_hash": "0", "entry_hash": "1"
        })
        .to_string()
    }

    #[test]
    fn decisions_carry_rule_tool_and_host_never_content() {
        let now = Utc::now();
        let at = (now - Duration::hours(1)).to_rfc3339();
        let old = (now - Duration::days(9)).to_rfc3339();
        let text = [
            line(
                "path_jail_violation",
                "ctx_read",
                "claude-code-4711",
                Some("/etc/passwd"),
                &at,
            ),
            line("tool_denied", "rm -rf /", "mystery-agent", None, &at),
            line(
                "tool_call",
                "ctx_read",
                "cursor",
                Some("secret_redacted:2|session=s1"),
                &at,
            ),
            line(
                "tool_call",
                "ctx_read",
                "cursor",
                Some("ordinary read"),
                &at,
            ),
            line("tool_denied", "ctx_shell", "codex", None, &old),
            "not json".to_string(),
        ]
        .join("\n");
        let shown = log(&text, now - Duration::days(7), "7d");
        assert_eq!(shown["total"], 3);
        let items = shown["items"].as_array().unwrap();
        // Newest first; the old denial and the plain tool call are not decisions here.
        assert_eq!(items.len(), 3);
        let jail = items
            .iter()
            .find(|r| r["rule"] == "gateway.path_jail")
            .unwrap();
        assert_eq!(jail["path_class"], "outside_project");
        assert_eq!(jail["host"], "claude");
        let denied = items
            .iter()
            .find(|r| r["rule"] == "gateway.tool_denied")
            .unwrap();
        assert_eq!(denied["tool"], "other");
        assert_eq!(denied["host"], "other");
        let redacted = items.iter().find(|r| r["decision"] == "redacted").unwrap();
        assert_eq!(redacted["rule"], "guard.secret_redacted");
        let text = shown.to_string();
        for leak in ["/etc/passwd", "rm -rf", "session=s1", "abc", "4711"] {
            assert!(!text.contains(leak), "{leak} leaked");
        }
    }

    #[test]
    fn only_closed_ranges() {
        assert_eq!(days_of("24h"), Some(1));
        assert_eq!(days_of("30d"), Some(30));
        assert_eq!(days_of("365d"), None);
        assert!(
            handle("/api/hive/decisions", "range=1y", "GET", "")
                .is_some_and(|r| r.0.starts_with("400"))
        );
        assert!(handle("/api/hive/decisions", "", "POST", "").is_none());
    }
}
