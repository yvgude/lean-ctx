//! Pre-dispatch permission-inheritance gate.
//!
//! When `permission_inheritance = on`, lean-ctx mirrors the host IDE's
//! tool-permission rules onto its own MCP tools so that, e.g., `ctx_shell`
//! honors the user's `bash` / `rm *` rules instead of forming a parallel,
//! ungoverned execution path. Shaped like [`super::role_guard`]: returns a
//! blocking [`CallToolResult`] message, or `None` to proceed.
//!
//! The decision is split into a pure `decide` (policy in, decision out — fully
//! unit-tested) and a thin [`check`] that loads/caches the IDE policy from disk.
//! lean-ctx never *writes* the IDE's `permission` block; this is read-only.

use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value};

use crate::core::config::{Config, PermissionInheritance};
use crate::core::ide_permissions::{self, IdePermissionPolicy, PermAction, PermDecision};

/// Result of a permission-inheritance check.
pub struct PermissionCheck {
    pub blocked: bool,
    pub message: Option<String>,
}

impl PermissionCheck {
    fn allow() -> Self {
        Self {
            blocked: false,
            message: None,
        }
    }

    fn blocked(message: String) -> Self {
        Self {
            blocked: true,
            message: Some(message),
        }
    }
}

const CACHE_TTL: Duration = Duration::from_secs(5);

struct CacheEntry {
    key: String,
    at: Instant,
    policy: IdePermissionPolicy,
}

static POLICY_CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();

/// Map an MCP client name (from the `initialize` handshake) to a known IDE id we
/// can read a permission config for. `None` → no reader → never gated.
fn client_id(client_name: &str) -> Option<&'static str> {
    let n = client_name.to_ascii_lowercase();
    if n.contains("opencode") {
        Some("opencode")
    } else {
        None
    }
}

/// Map a lean-ctx tool + its args to the IDE permission key and the relevant
/// input (command / path / pattern). `None` → tool not mirrored → allowed.
fn map_tool(
    tool: &str,
    args: Option<&Map<String, Value>>,
) -> Option<(&'static str, Option<String>)> {
    let get = |k: &str| crate::server::helpers::get_str(args, k);
    match tool {
        "ctx_shell" | "ctx_execute" => Some(("bash", get("command"))),
        "ctx_read" | "ctx_multi_read" | "ctx_smart_read" => Some(("read", get("path"))),
        "ctx_edit" | "ctx_patch" => Some(("edit", get("path"))),
        "ctx_search" => Some(("grep", get("pattern").or_else(|| get("query")))),
        _ => None,
    }
}

/// Public entry point used by the dispatch path. Honors config + env, detects
/// the IDE, loads (and caches) its permission policy, then defers to `decide`.
#[must_use]
pub fn check(
    client_name: &str,
    tool: &str,
    args: Option<&Map<String, Value>>,
    project_root: Option<&str>,
    config: &Config,
) -> PermissionCheck {
    if config.permission_inheritance_effective() != PermissionInheritance::On {
        return PermissionCheck::allow();
    }
    // shadow_mode writes permission denies to the same opencode.json `permission`
    // object that inheritance reads from. If both are active, native tools are
    // denied (shadow mode) AND ctx_* tools are denied (inheritance mirroring the
    // shadow denies back), leaving the agent with no working tools. Since shadow
    // mode already handles its own permission controls, disable inheritance.
    if config.shadow_mode {
        return PermissionCheck::allow();
    }
    let Some(cid) = client_id(client_name) else {
        return PermissionCheck::allow();
    };
    let Some((key, input)) = map_tool(tool, args) else {
        return PermissionCheck::allow();
    };
    let policy = policy_for(cid, project_root);
    if policy.is_empty() {
        return PermissionCheck::allow();
    }
    decide(display_name(cid), &policy, tool, key, input.as_deref())
}

/// Pure decision: given a loaded policy, resolve the action for `tool` (mapped to
/// IDE `key` + `input`) and turn it into a [`PermissionCheck`].
fn decide(
    ide: &str,
    policy: &IdePermissionPolicy,
    tool: &str,
    key: &str,
    input: Option<&str>,
) -> PermissionCheck {
    let Some(decision) = policy.resolve(key, input) else {
        return PermissionCheck::allow();
    };
    match decision.action {
        PermAction::Allow => PermissionCheck::allow(),
        PermAction::Ask => PermissionCheck::blocked(ask_message(ide, &decision, key, input)),
        PermAction::Deny => {
            PermissionCheck::blocked(deny_message(ide, tool, &decision, key, input))
        }
    }
}

fn ask_message(ide: &str, decision: &PermDecision, key: &str, input: Option<&str>) -> String {
    format!(
        "[IDE PERMISSION] {ide} gates this with `{rule}` = ask. lean-ctx mirrors your IDE \
         permissions (permission_inheritance=on) and cannot show an interactive prompt for MCP \
         tools, so the call is held back to honor your rule.{suffix}\n\
         Approve it via {ide}'s native tool, set the rule to `allow`, or disable inheritance with \
         `lean-ctx config set permission_inheritance off`.",
        ide = ide,
        rule = decision.rule,
        suffix = input_suffix(key, input),
    )
}

fn deny_message(
    ide: &str,
    tool: &str,
    decision: &PermDecision,
    key: &str,
    input: Option<&str>,
) -> String {
    format!(
        "[IDE PERMISSION] {ide} blocks this via `{rule}` = deny. lean-ctx mirrors your IDE \
         permissions (permission_inheritance=on), so `{tool}` is blocked too.{suffix}",
        ide = ide,
        rule = decision.rule,
        tool = tool,
        suffix = input_suffix(key, input),
    )
}

fn input_suffix(key: &str, input: Option<&str>) -> String {
    let Some(value) = input else {
        return String::new();
    };
    let label = match key {
        "bash" => "Command",
        "grep" => "Pattern",
        _ => "Path",
    };
    format!(" {label}: `{}`", truncate(value, 200))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn display_name(client_id: &str) -> &'static str {
    crate::core::client_constraints::by_client_id(client_id).map_or("your IDE", |c| c.display_name)
}

fn policy_for(client_id: &str, project_root: Option<&str>) -> IdePermissionPolicy {
    let key = format!("{client_id}|{}", project_root.unwrap_or(""));
    let cache = POLICY_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = guard.as_ref()
        && entry.key == key
        && entry.at.elapsed() < CACHE_TTL
    {
        return entry.policy.clone();
    }
    let policy = load_policy(client_id, project_root);
    *guard = Some(CacheEntry {
        key,
        at: Instant::now(),
        policy: policy.clone(),
    });
    policy
}

fn load_policy(client_id: &str, project_root: Option<&str>) -> IdePermissionPolicy {
    let Some(home) = dirs::home_dir() else {
        return IdePermissionPolicy::default();
    };
    match client_id {
        "opencode" => ide_permissions::load_opencode(&home, project_root.map(Path::new)),
        _ => IdePermissionPolicy::default(),
    }
}

/// Convert a check into a blocking tool result (like `role_guard`): a successful
/// result carrying the explanation, so the agent reads *why* it was held back.
#[must_use]
pub fn into_call_tool_result(check: &PermissionCheck) -> Option<CallToolResult> {
    if check.blocked {
        Some(CallToolResult::success(vec![ContentBlock::text(
            check
                .message
                .clone()
                .unwrap_or_else(|| "Blocked by IDE permission inheritance".to_string()),
        )]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(v: Value) -> IdePermissionPolicy {
        match v {
            Value::Object(map) => IdePermissionPolicy::from_rules(map),
            _ => IdePermissionPolicy::default(),
        }
    }

    #[test]
    fn client_id_detects_opencode() {
        assert_eq!(client_id("opencode"), Some("opencode"));
        assert_eq!(client_id("OpenCode 1.2"), Some("opencode"));
        assert_eq!(client_id("cursor"), None);
        assert_eq!(client_id(""), None);
    }

    #[test]
    fn map_tool_covers_mirrored_tools() {
        let args = json!({ "command": "rm -rf x", "path": "a.rs", "pattern": "foo" });
        let map = args.as_object().unwrap();
        assert_eq!(map_tool("ctx_shell", Some(map)).unwrap().0, "bash");
        assert_eq!(map_tool("ctx_execute", Some(map)).unwrap().0, "bash");
        assert_eq!(map_tool("ctx_read", Some(map)).unwrap().0, "read");
        assert_eq!(map_tool("ctx_edit", Some(map)).unwrap().0, "edit");
        // ctx_patch (anchored editing) inherits the same "edit" permission key.
        assert_eq!(map_tool("ctx_patch", Some(map)).unwrap().0, "edit");
        assert_eq!(map_tool("ctx_search", Some(map)).unwrap().0, "grep");
        assert!(map_tool("ctx_knowledge", Some(map)).is_none());
    }

    #[test]
    fn decide_allow_passes() {
        let p = policy(json!({ "bash": "allow" }));
        let c = decide("OpenCode", &p, "ctx_shell", "bash", Some("ls"));
        assert!(!c.blocked);
    }

    #[test]
    fn decide_deny_blocks_with_message() {
        let p = policy(json!({ "bash": "deny" }));
        let c = decide("OpenCode", &p, "ctx_shell", "bash", Some("ls"));
        assert!(c.blocked);
        let msg = c.message.unwrap();
        assert!(msg.contains("deny"));
        assert!(msg.contains("ctx_shell"));
        assert!(msg.contains("Command: `ls`"));
    }

    #[test]
    fn decide_ask_holds_back_rm() {
        // The user's screenshot scenario: bash=allow but rm *=ask.
        let p = policy(json!({ "bash": "allow", "rm *": "ask" }));
        let c = decide("OpenCode", &p, "ctx_shell", "bash", Some("rm -rf /tmp/x"));
        assert!(c.blocked);
        let msg = c.message.unwrap();
        assert!(msg.contains("ask"));
        assert!(msg.contains("bash:rm *"));
        assert!(msg.contains("permission_inheritance off"));
    }

    #[test]
    fn decide_unmatched_tool_input_allows() {
        let p = policy(json!({ "read": "deny" }));
        // bash has no rule here → allowed.
        let c = decide("OpenCode", &p, "ctx_shell", "bash", Some("ls"));
        assert!(!c.blocked);
    }

    #[test]
    fn into_result_only_when_blocked() {
        assert!(into_call_tool_result(&PermissionCheck::allow()).is_none());
        assert!(into_call_tool_result(&PermissionCheck::blocked("x".into())).is_some());
    }

    #[test]
    fn check_off_by_default_allows_everything() {
        // Env var takes precedence over config; skip if a stray one is set.
        if std::env::var("LEAN_CTX_PERMISSION_INHERITANCE").is_ok() {
            return;
        }
        let cfg = Config {
            permission_inheritance: Some("off".to_string()),
            ..Default::default()
        };
        let args = json!({ "command": "rm -rf /" });
        let c = check(
            "opencode",
            "ctx_shell",
            Some(args.as_object().unwrap()),
            None,
            &cfg,
        );
        assert!(!c.blocked);
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("short", 200), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }
}
