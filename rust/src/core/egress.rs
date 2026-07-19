//! Egress / output DLP for agent writes & actions (GL #676) — the *output*
//! side of the Great Filter.
//!
//! Where [`crate::core::input_filters`] governs what reaches the agent, this
//! module governs what the agent *emits*: file writes (`ctx_edit`) and shell
//! actions (`ctx_shell`). It runs **before** the tool executes, so a blocked
//! write never touches disk and a blocked command never runs.
//!
//! Driven by the active pack's `[egress]` section ([`crate::core::policy`]):
//! - `forbidden_patterns` — regexes that, if matched, block the write/action
//!   (e.g. a direct prod-DB DSN);
//! - `block_secrets` — refuse content carrying detected secrets/PII (reusing the
//!   pack redaction patterns + [`crate::core::input_filters::pii`]);
//! - `max_writes_per_min` — a per-process sliding-window rate limit on actions.
//!
//! **Local-Free:** only the agent's tool-driven egress is gated; a human's
//! manual edits never pass through this path.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use regex::Regex;
use serde_json::{Map, Value};

/// Map a write/action tool call to the outbound payload the egress DLP must
/// inspect, plus its audit kind ("Write"/"Action"). Single source of truth for
/// the MCP dispatch gate (`server::call_tool`) AND the CLI `policy enforce`
/// mirror, so the two gates can never drift. `None` → the tool carries no
/// governed egress payload.
///
/// `ctx_patch` (#1008) can carry several write bodies in one call — top-level
/// `new_text` (anchored ops / create / replace_symbol) and each `ops[].new_text`
/// of a batch — all are concatenated for inspection.
#[must_use]
pub fn write_payload(
    tool: &str,
    args: Option<&Map<String, Value>>,
) -> Option<(String, &'static str)> {
    let get = |k: &str| args?.get(k)?.as_str().map(String::from);
    match tool {
        "ctx_edit" => get("new_string").map(|s| (s, "Write")),
        "ctx_patch" => patch_payload(args).map(|s| (s, "Write")),
        "ctx_shell" | "ctx_execute" => get("command").map(|s| (s, "Action")),
        _ => None,
    }
}

/// Every write body a `ctx_patch` call may carry, joined for one inspection.
fn patch_payload(args: Option<&Map<String, Value>>) -> Option<String> {
    let map = args?;
    let mut parts: Vec<&str> = Vec::new();
    if let Some(s) = map.get("new_text").and_then(Value::as_str) {
        parts.push(s);
    }
    if let Some(ops) = map.get("ops").and_then(Value::as_array) {
        for op in ops {
            if let Some(s) = op.get("new_text").and_then(Value::as_str) {
                parts.push(s);
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Resolved, ready-to-run egress configuration. Forbidden-pattern regexes are
/// compiled once at policy load, off the hot path.
pub struct EgressConfig {
    /// `(source, compiled)` — source kept for the (non-sensitive) audit reason.
    forbidden: Vec<(String, Regex)>,
    block_secrets: bool,
    /// Max agent write/action tool calls per 60 s; `None` = unlimited.
    pub max_writes_per_min: Option<u32>,
}

impl Default for EgressConfig {
    fn default() -> Self {
        Self::off()
    }
}

impl EgressConfig {
    /// A no-op config.
    #[must_use]
    pub fn off() -> Self {
        Self {
            forbidden: Vec::new(),
            block_secrets: false,
            max_writes_per_min: None,
        }
    }

    /// Build from resolved policy. Invalid regexes are skipped (validation
    /// already rejects them at load — defense in depth).
    #[must_use]
    pub fn new(
        forbidden_patterns: &[String],
        block_secrets: bool,
        max_writes_per_min: Option<u32>,
    ) -> Self {
        let forbidden = forbidden_patterns
            .iter()
            .filter_map(|p| Regex::new(p).ok().map(|re| (p.clone(), re)))
            .collect();
        Self {
            forbidden,
            block_secrets,
            max_writes_per_min,
        }
    }

    /// True if any egress rule is configured (cheap hot-path gate).
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.forbidden.is_empty() || self.block_secrets || self.max_writes_per_min.is_some()
    }

    /// Inspect outbound `content` (a write body or a shell command). Returns a
    /// privacy-preserving block reason (pattern source / class — never the
    /// matched value), or `None` to allow. `redaction` are the active pack's
    /// compiled secret patterns, consulted when `block_secrets` is set.
    #[must_use]
    pub fn check_content(&self, content: &str, redaction: &[(String, Regex)]) -> Option<String> {
        for (source, re) in &self.forbidden {
            if re.is_match(content) {
                return Some(format!("forbidden-pattern:{source}"));
            }
        }
        if self.block_secrets {
            let (_, hits) = crate::core::redaction::redact_with_patterns(content, redaction);
            if hits > 0 {
                return Some("secret".to_string());
            }
            if let Some((class, _)) = crate::core::input_filters::pii::detect(content).first() {
                return Some(format!("pii:{class}"));
            }
        }
        None
    }
}

/// Per-process sliding-window rate check. Records the action and returns `true`
/// when within `max_per_min`, or `false` (without recording) when the limit is
/// already reached in the trailing 60 s.
#[must_use]
pub fn check_rate(max_per_min: u32) -> bool {
    let mut q = rate_state().lock().expect("egress rate state poisoned");
    within_limit(&mut q, Instant::now(), max_per_min)
}

fn rate_state() -> &'static Mutex<VecDeque<Instant>> {
    static STATE: OnceLock<Mutex<VecDeque<Instant>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Pure sliding-window decision (testable without the global state): prune
/// entries older than 60 s, then admit + record if under `max_per_min`.
fn within_limit(q: &mut VecDeque<Instant>, now: Instant, max_per_min: u32) -> bool {
    while let Some(&front) = q.front() {
        if now.duration_since(front).as_secs() >= 60 {
            q.pop_front();
        } else {
            break;
        }
    }
    if u32::try_from(q.len()).unwrap_or(u32::MAX) >= max_per_min {
        return false;
    }
    q.push_back(now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cfg(patterns: &[&str], block_secrets: bool) -> EgressConfig {
        let pats: Vec<String> = patterns.iter().map(|s| (*s).to_string()).collect();
        EgressConfig::new(&pats, block_secrets, None)
    }

    #[test]
    fn off_config_is_inactive() {
        assert!(!EgressConfig::off().is_active());
    }

    #[test]
    fn forbidden_pattern_blocks_action() {
        let c = cfg(&[r"prod\.db\.internal"], false);
        let reason = c.check_content("psql postgres://prod.db.internal/main", &[]);
        assert_eq!(
            reason.as_deref(),
            Some("forbidden-pattern:prod\\.db\\.internal")
        );
    }

    #[test]
    fn clean_content_is_allowed() {
        let c = cfg(&[r"prod\.db\.internal"], true);
        assert!(
            c.check_content("fn main() { println!(\"hi\"); }", &[])
                .is_none()
        );
    }

    #[test]
    fn block_secrets_catches_pii() {
        let c = cfg(&[], true);
        let reason = c.check_content("email jane@example.com into config", &[]);
        assert_eq!(reason.as_deref(), Some("pii:email"));
    }

    #[test]
    fn block_secrets_catches_redaction_pattern() {
        let c = cfg(&[], true);
        let redaction = vec![("employee_id".to_string(), Regex::new(r"EMP-\d{4}").unwrap())];
        let reason = c.check_content("commit by EMP-1234", &redaction);
        assert_eq!(reason.as_deref(), Some("secret"));
    }

    #[test]
    fn rate_limit_triggers_after_max() {
        let mut q = VecDeque::new();
        let now = Instant::now();
        assert!(within_limit(&mut q, now, 2));
        assert!(within_limit(&mut q, now, 2));
        // Third within the window is refused.
        assert!(!within_limit(&mut q, now, 2));
    }

    #[test]
    fn rate_limit_window_slides() {
        let mut q = VecDeque::new();
        let base = Instant::now();
        assert!(within_limit(&mut q, base, 1));
        // Same instant: over limit.
        assert!(!within_limit(&mut q, base, 1));
        // 61 s later the old entry has aged out → admitted again.
        assert!(within_limit(&mut q, base + Duration::from_secs(61), 1));
    }

    fn args(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn write_payload_covers_edit_shell_and_execute() {
        let edit = args(serde_json::json!({"new_string": "body"}));
        assert_eq!(
            write_payload("ctx_edit", Some(&edit)),
            Some(("body".to_string(), "Write"))
        );
        let sh = args(serde_json::json!({"command": "rm -rf /tmp/x"}));
        assert_eq!(
            write_payload("ctx_shell", Some(&sh)),
            Some(("rm -rf /tmp/x".to_string(), "Action"))
        );
        assert_eq!(
            write_payload("ctx_execute", Some(&sh)),
            Some(("rm -rf /tmp/x".to_string(), "Action"))
        );
        assert_eq!(write_payload("ctx_read", Some(&sh)), None);
    }

    #[test]
    fn write_payload_collects_every_patch_body() {
        // #1008 security pass: single op, replace_symbol AND every batch op body
        // must all be inspected — a secret in ops[1] is as forbidden as one in
        // a top-level new_text.
        let single = args(serde_json::json!({"op": "set_line", "new_text": "top"}));
        assert_eq!(
            write_payload("ctx_patch", Some(&single)),
            Some(("top".to_string(), "Write"))
        );

        let symbol = args(serde_json::json!({"op": "replace_symbol", "new_text": "fn x() {}"}));
        assert_eq!(
            write_payload("ctx_patch", Some(&symbol)),
            Some(("fn x() {}".to_string(), "Write"))
        );

        let batch = args(serde_json::json!({"ops": [
            {"op": "set_line", "line": 1, "hash": "aa", "new_text": "first"},
            {"op": "insert_after", "line": 2, "hash": "bb", "new_text": "second"}
        ]}));
        let (payload, kind) = write_payload("ctx_patch", Some(&batch)).unwrap();
        assert_eq!(kind, "Write");
        assert!(payload.contains("first") && payload.contains("second"));

        // No write body (e.g. a malformed call) → nothing to inspect.
        let empty = args(serde_json::json!({"op": "delete", "line": 3, "hash": "cc"}));
        assert_eq!(write_payload("ctx_patch", Some(&empty)), None);
    }

    #[test]
    fn patch_payload_is_checkable_content() {
        // End-to-end shape: a forbidden pattern hidden in a batch op is caught.
        let c = cfg(&[r"prod\.db\.internal"], false);
        let batch = args(serde_json::json!({"ops": [
            {"op": "set_line", "line": 1, "hash": "aa", "new_text": "safe"},
            {"op": "set_line", "line": 9, "hash": "bb", "new_text": "url = prod.db.internal"}
        ]}));
        let (payload, _) = write_payload("ctx_patch", Some(&batch)).unwrap();
        assert!(c.check_content(&payload, &[]).is_some());
    }
}
