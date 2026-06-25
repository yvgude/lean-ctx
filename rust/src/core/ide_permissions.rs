//! Read the active IDE's tool-permission configuration and resolve an effective
//! action for a lean-ctx tool, so lean-ctx can *mirror* ("inherit") the user's
//! IDE permission rules instead of forming a second, ungoverned execution path.
//!
//! Motivation (community request): when lean-ctx is mounted as an MCP server,
//! its tools (e.g. `ctx_shell`) run inside the lean-ctx process and therefore
//! bypass the host IDE's own permission engine — a user who set `bash`/`rm *`
//! to `ask`/`deny` in their IDE would have that guard silently skipped whenever
//! the agent reaches for `ctx_shell` instead of the native tool. This module
//! parses the IDE permission config and lets the server gate apply an
//! equivalent decision.
//!
//! v1 supports **`OpenCode`** (`opencode.json` / `opencode.jsonc`, global +
//! project). The mapping is intentionally pure and side-effect-free; the server
//! wiring (client detection, tool→key mapping, messaging, caching) lives in
//! `server::permission_inheritance`.
//!
//! lean-ctx never *writes* the IDE's `permission` block — inheritance is
//! read-only and runtime-only.

use std::path::Path;

use serde_json::{Map, Value};

use crate::core::jsonc::parse_jsonc;

/// An IDE permission decision for a single action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermAction {
    Allow,
    Ask,
    Deny,
}

impl PermAction {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    /// Restrictiveness rank used to break ties between equally specific rules
    /// (the safer, more restrictive action wins).
    const fn rank(self) -> u8 {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Deny => 2,
        }
    }
}

/// A resolved decision together with the human-readable rule that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermDecision {
    pub action: PermAction,
    /// e.g. `bash`, `bash:rm *`, `read:*`, `*`.
    pub rule: String,
}

/// `OpenCode`'s documented permission keys. Used to distinguish a real tool key
/// from a free-form bash command pattern placed at the top level.
const OPENCODE_TOOL_KEYS: &[&str] = &[
    "read",
    "edit",
    "write",
    "patch",
    "glob",
    "grep",
    "bash",
    "task",
    "skill",
    "lsp",
    "question",
    "webfetch",
    "websearch",
    "external_directory",
    "doom_loop",
    "*",
];

/// Specificity score for the global `*` rule (lowest priority).
const GLOBAL_SPEC: i64 = -1;
/// Specificity score for a blanket tool rule (`bash: "allow"` or `bash: { "*": … }`).
const BLANKET_SPEC: i64 = 0;

/// Normalized IDE permission policy: the merged `permission` object from the IDE
/// config (project entries override global ones per top-level key).
#[derive(Debug, Clone, Default)]
pub struct IdePermissionPolicy {
    rules: Map<String, Value>,
}

struct Candidate {
    spec: i64,
    action: PermAction,
    rule: String,
}

impl IdePermissionPolicy {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Number of top-level permission rules in the merged policy.
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Construct directly from a raw `permission` object (test/utility hook).
    #[must_use]
    pub fn from_rules(rules: Map<String, Value>) -> Self {
        Self { rules }
    }

    /// Resolve the effective action for an `OpenCode` tool key (e.g. `bash`,
    /// `read`) given the relevant tool input (command / path / pattern).
    ///
    /// Returns `None` when no rule matches — the caller treats that as the IDE
    /// default (`allow` for the tools we mirror), so inheritance never *adds*
    /// friction that the IDE itself would not impose.
    ///
    /// Resolution is order-independent (`serde_json` maps are not insertion-ordered
    /// without `preserve_order`): the **most specific** rule wins (longest
    /// pattern by non-wildcard character count; a named tool beats the global
    /// `*`), ties broken by the **most restrictive** action.
    #[must_use]
    pub fn resolve(&self, tool_key: &str, input: Option<&str>) -> Option<PermDecision> {
        let mut best: Option<Candidate> = None;

        if let Some(value) = self.rules.get(tool_key) {
            collect_from_value(value, input, tool_key, &mut best);
        }

        // For shell: also honor top-level command-like patterns. OpenCode
        // documents these nested under `bash`, but users frequently write them
        // at the top level (e.g. `"rm *": "ask"`); we accept both so the guard
        // is never silently ineffective when proxied through `ctx_shell`. A
        // command pattern is more specific than a blanket `bash: "allow"`.
        if tool_key == "bash"
            && let Some(cmd) = input
        {
            for (key, value) in &self.rules {
                if OPENCODE_TOOL_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if !key.contains(' ') && !key.contains('*') {
                    continue;
                }
                if let Some(action) = value.as_str().and_then(PermAction::parse)
                    && wildcard_match(key, cmd)
                {
                    consider(&mut best, specificity(key), action, format!("bash:{key}"));
                }
            }
        }

        if let Some(action) = self
            .rules
            .get("*")
            .and_then(Value::as_str)
            .and_then(PermAction::parse)
        {
            consider(&mut best, GLOBAL_SPEC, action, "*".to_string());
        }

        best.map(|c| PermDecision {
            action: c.action,
            rule: c.rule,
        })
    }
}

fn collect_from_value(value: &Value, input: Option<&str>, key: &str, best: &mut Option<Candidate>) {
    if let Some(raw) = value.as_str() {
        if let Some(action) = PermAction::parse(raw) {
            consider(best, BLANKET_SPEC, action, key.to_string());
        }
        return;
    }
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(inp) = input {
        for (pat, av) in obj {
            if pat == "*" {
                continue;
            }
            if let Some(action) = av.as_str().and_then(PermAction::parse)
                && wildcard_match(pat, inp)
            {
                consider(best, specificity(pat), action, format!("{key}:{pat}"));
            }
        }
    }
    if let Some(action) = obj
        .get("*")
        .and_then(Value::as_str)
        .and_then(PermAction::parse)
    {
        consider(best, BLANKET_SPEC, action, format!("{key}:*"));
    }
}

fn consider(best: &mut Option<Candidate>, spec: i64, action: PermAction, rule: String) {
    let better = match best {
        None => true,
        Some(b) => spec > b.spec || (spec == b.spec && action.rank() > b.action.rank()),
    };
    if better {
        *best = Some(Candidate { spec, action, rule });
    }
}

/// Specificity of a glob pattern = count of non-`*` characters (more literal
/// characters → more specific).
fn specificity(pattern: &str) -> i64 {
    pattern.chars().filter(|c| *c != '*').count() as i64
}

/// Minimal glob matcher supporting `*` (matches any run of characters, including
/// empty); `**` is treated as `*`. No `?` or character classes — this mirrors
/// the simple command/path globs `OpenCode` permission rules use (`git *`,
/// `rm *`, `src/*`). Matching is case-sensitive.
#[must_use]
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;

    while t < txt.len() {
        if p < pat.len() && pat[p] == '*' {
            while p + 1 < pat.len() && pat[p + 1] == '*' {
                p += 1;
            }
            star = Some(p);
            star_t = t;
            p += 1;
        } else if p < pat.len() && pat[p] == txt[t] {
            p += 1;
            t += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Read and merge the `OpenCode` `permission` object: global config first, then
/// the project config (project keys override global). Missing/invalid files are
/// skipped silently — inheritance must never break a tool call by erroring.
#[must_use]
pub fn load_opencode(home: &Path, project_root: Option<&Path>) -> IdePermissionPolicy {
    let mut rules = Map::new();
    let opencode = home.join(".config").join("opencode");
    merge_permission_file(&opencode.join("opencode.json"), &mut rules);
    merge_permission_file(&opencode.join("opencode.jsonc"), &mut rules);
    if let Some(root) = project_root {
        merge_permission_file(&root.join("opencode.json"), &mut rules);
        merge_permission_file(&root.join("opencode.jsonc"), &mut rules);
    }
    IdePermissionPolicy { rules }
}

fn merge_permission_file(path: &Path, rules: &mut Map<String, Value>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = parse_jsonc(&text) else {
        return;
    };
    if let Some(perm) = value.get("permission").and_then(Value::as_object) {
        for (key, val) in perm {
            rules.insert(key.clone(), val.clone());
        }
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
    fn wildcard_basic() {
        assert!(wildcard_match("rm *", "rm -rf foo"));
        assert!(wildcard_match("git *", "git status"));
        assert!(!wildcard_match("git *", "gitk"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("src/*", "src/main.rs"));
        assert!(!wildcard_match("rm *", "sudo rm -rf /"));
        assert!(wildcard_match("**", ""));
        assert!(wildcard_match("a*c", "abbbc"));
        assert!(!wildcard_match("a*c", "abbb"));
    }

    #[test]
    fn string_rule_resolves() {
        let p = policy(json!({ "bash": "deny" }));
        let d = p.resolve("bash", Some("ls")).unwrap();
        assert_eq!(d.action, PermAction::Deny);
        assert_eq!(d.rule, "bash");
    }

    #[test]
    fn nested_bash_pattern_specific_wins() {
        let p = policy(json!({
            "bash": { "*": "ask", "git *": "allow", "rm *": "deny" }
        }));
        assert_eq!(
            p.resolve("bash", Some("git push")).unwrap().action,
            PermAction::Allow
        );
        assert_eq!(
            p.resolve("bash", Some("rm -rf x")).unwrap().action,
            PermAction::Deny
        );
        // unmatched falls back to the object's "*"
        assert_eq!(
            p.resolve("bash", Some("ls")).unwrap().action,
            PermAction::Ask
        );
    }

    #[test]
    fn top_level_command_pattern_overrides_blanket_bash() {
        // The exact shape from the user's screenshot: top-level "rm *": "ask"
        // alongside a blanket bash=allow. The command pattern is more specific.
        let p = policy(json!({ "bash": "allow", "rm *": "ask" }));
        let d = p.resolve("bash", Some("rm -rf /tmp/x")).unwrap();
        assert_eq!(d.action, PermAction::Ask);
        assert_eq!(d.rule, "bash:rm *");
        // a non-rm command falls through to the plain bash=allow
        assert_eq!(
            p.resolve("bash", Some("ls")).unwrap().action,
            PermAction::Allow
        );
    }

    #[test]
    fn most_specific_wins_regardless_of_map_order() {
        let p = policy(json!({ "bash": { "git *": "allow", "git push *": "ask" } }));
        assert_eq!(
            p.resolve("bash", Some("git push origin")).unwrap().action,
            PermAction::Ask
        );
    }

    #[test]
    fn read_path_pattern() {
        let p = policy(json!({ "read": { "*": "allow", "*.env": "deny" } }));
        assert_eq!(
            p.resolve("read", Some("src/main.rs")).unwrap().action,
            PermAction::Allow
        );
        assert_eq!(
            p.resolve("read", Some("prod.env")).unwrap().action,
            PermAction::Deny
        );
        assert_eq!(
            p.resolve("read", Some("config/.env")).unwrap().action,
            PermAction::Deny
        );
    }

    #[test]
    fn named_tool_beats_global_wildcard() {
        let p = policy(json!({ "*": "ask", "bash": "allow" }));
        assert_eq!(
            p.resolve("bash", Some("ls")).unwrap().action,
            PermAction::Allow
        );
        assert_eq!(
            p.resolve("read", Some("x")).unwrap().action,
            PermAction::Ask
        );
    }

    #[test]
    fn no_rule_returns_none() {
        let p = policy(json!({ "bash": "allow" }));
        assert!(p.resolve("read", Some("x")).is_none());
    }

    #[test]
    fn empty_policy_is_empty() {
        assert!(IdePermissionPolicy::default().is_empty());
    }

    #[test]
    fn load_opencode_merges_global_and_project() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(home.join(".config").join("opencode")).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            home.join(".config").join("opencode").join("opencode.json"),
            r#"{ "permission": { "bash": "ask", "read": "allow" } }"#,
        )
        .unwrap();
        std::fs::write(
            proj.join("opencode.jsonc"),
            "{ // project\n \"permission\": { \"bash\": \"deny\" } }",
        )
        .unwrap();
        let p = load_opencode(&home, Some(&proj));
        assert_eq!(
            p.resolve("bash", Some("ls")).unwrap().action,
            PermAction::Deny
        );
        assert_eq!(
            p.resolve("read", Some("x")).unwrap().action,
            PermAction::Allow
        );
    }

    #[test]
    fn load_opencode_missing_files_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = load_opencode(dir.path(), None);
        assert!(p.is_empty());
    }
}
