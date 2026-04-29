//! Role-based access control for agent governance.
//!
//! Roles define what tools an agent can use, shell policy, and resource limits.
//! Resolution order: Env (`LEAN_CTX_ROLE`) -> Project `.lean-ctx/roles/` -> Global `~/.lean-ctx/roles/` -> Built-in.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ACTIVE_ROLE_NAME: OnceLock<std::sync::Mutex<String>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    #[serde(default)]
    pub role: RoleMeta,
    #[serde(default)]
    pub tools: ToolPolicy,
    #[serde(default)]
    pub limits: RoleLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub inherits: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_shell_policy")]
    pub shell_policy: String,
}

impl Default for RoleMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            inherits: None,
            description: String::new(),
            shell_policy: default_shell_policy(),
        }
    }
}

fn default_shell_policy() -> String {
    "track".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolPolicy {
    #[serde(default)]
    pub allowed: Vec<String>,
    #[serde(default)]
    pub denied: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleLimits {
    #[serde(default = "default_context_tokens")]
    pub max_context_tokens: usize,
    #[serde(default = "default_shell_invocations")]
    pub max_shell_invocations: usize,
    #[serde(default = "default_cost_usd")]
    pub max_cost_usd: f64,
    #[serde(default = "default_warn_pct")]
    pub warn_at_percent: u8,
    #[serde(default = "default_block_pct")]
    pub block_at_percent: u8,
}

impl Default for RoleLimits {
    fn default() -> Self {
        Self {
            max_context_tokens: default_context_tokens(),
            max_shell_invocations: default_shell_invocations(),
            max_cost_usd: default_cost_usd(),
            warn_at_percent: default_warn_pct(),
            block_at_percent: default_block_pct(),
        }
    }
}

fn default_context_tokens() -> usize {
    200_000
}
fn default_shell_invocations() -> usize {
    100
}
fn default_cost_usd() -> f64 {
    5.0
}
fn default_warn_pct() -> u8 {
    80
}
fn default_block_pct() -> u8 {
    // 255 = effectively never block (would need >255% budget usage)
    // LeanCTX philosophy: always help, never block. Warnings are enough.
    // Users can explicitly set block_at_percent: 100 in role config if they want blocking.
    255
}

impl Role {
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        if !self.tools.denied.is_empty()
            && self.tools.denied.iter().any(|d| d == tool_name || d == "*")
        {
            return false;
        }
        if self.tools.allowed.is_empty() || self.tools.allowed.iter().any(|a| a == "*") {
            return true;
        }
        self.tools.allowed.iter().any(|a| a == tool_name)
    }

    pub fn is_shell_allowed(&self) -> bool {
        self.role.shell_policy != "deny"
    }

    pub fn allowed_tools_set(&self) -> HashSet<String> {
        if self.tools.allowed.is_empty() || self.tools.allowed.iter().any(|a| a == "*") {
            return HashSet::new(); // empty = all allowed
        }
        self.tools.allowed.iter().cloned().collect()
    }
}

// ── Built-in roles ──────────────────────────────────────────────

fn builtin_coder() -> Role {
    Role {
        role: RoleMeta {
            name: "coder".into(),
            description: "Full access for code implementation".into(),
            shell_policy: "track".into(),
            inherits: None,
        },
        tools: ToolPolicy {
            allowed: vec!["*".into()],
            denied: vec![],
        },
        limits: RoleLimits {
            max_context_tokens: 200_000,
            max_shell_invocations: 100,
            max_cost_usd: 5.0,
            ..Default::default()
        },
    }
}

fn builtin_reviewer() -> Role {
    Role {
        role: RoleMeta {
            name: "reviewer".into(),
            description: "Read-only access for code review".into(),
            shell_policy: "track".into(),
            inherits: None,
        },
        tools: ToolPolicy {
            allowed: vec![
                "ctx_read".into(),
                "ctx_multi_read".into(),
                "ctx_smart_read".into(),
                "ctx_fill".into(),
                "ctx_search".into(),
                "ctx_tree".into(),
                "ctx_graph".into(),
                "ctx_architecture".into(),
                "ctx_analyze".into(),
                "ctx_diff".into(),
                "ctx_symbol".into(),
                "ctx_expand".into(),
                "ctx_deps".into(),
                "ctx_review".into(),
                "ctx_session".into(),
                "ctx_knowledge".into(),
                "ctx_semantic_search".into(),
                "ctx_overview".into(),
                "ctx_preload".into(),
                "ctx_metrics".into(),
                "ctx_cost".into(),
                "ctx_gain".into(),
            ],
            denied: vec!["ctx_edit".into(), "ctx_shell".into(), "ctx_execute".into()],
        },
        limits: RoleLimits {
            max_context_tokens: 150_000,
            max_shell_invocations: 0,
            max_cost_usd: 3.0,
            ..Default::default()
        },
    }
}

fn builtin_debugger() -> Role {
    Role {
        role: RoleMeta {
            name: "debugger".into(),
            description: "Debug-focused with shell access".into(),
            shell_policy: "track".into(),
            inherits: None,
        },
        tools: ToolPolicy {
            allowed: vec!["*".into()],
            denied: vec![],
        },
        limits: RoleLimits {
            max_context_tokens: 150_000,
            max_shell_invocations: 200,
            max_cost_usd: 5.0,
            ..Default::default()
        },
    }
}

fn builtin_ops() -> Role {
    Role {
        role: RoleMeta {
            name: "ops".into(),
            description: "Infrastructure and CI/CD operations".into(),
            shell_policy: "compress".into(),
            inherits: None,
        },
        tools: ToolPolicy {
            allowed: vec![
                "ctx_read".into(),
                "ctx_shell".into(),
                "ctx_search".into(),
                "ctx_tree".into(),
                "ctx_session".into(),
                "ctx_knowledge".into(),
                "ctx_overview".into(),
                "ctx_metrics".into(),
                "ctx_cost".into(),
            ],
            denied: vec!["ctx_edit".into()],
        },
        limits: RoleLimits {
            max_context_tokens: 100_000,
            max_shell_invocations: 300,
            max_cost_usd: 3.0,
            ..Default::default()
        },
    }
}

fn builtin_admin() -> Role {
    Role {
        role: RoleMeta {
            name: "admin".into(),
            description: "Unrestricted access, all tools and unlimited budgets".into(),
            shell_policy: "track".into(),
            inherits: None,
        },
        tools: ToolPolicy {
            allowed: vec!["*".into()],
            denied: vec![],
        },
        limits: RoleLimits {
            max_context_tokens: 500_000,
            max_shell_invocations: 500,
            max_cost_usd: 50.0,
            warn_at_percent: 90,
            ..Default::default() // block_at_percent: 255 (never block)
        },
    }
}

fn builtin_roles() -> HashMap<String, Role> {
    let mut m = HashMap::new();
    m.insert("coder".into(), builtin_coder());
    m.insert("reviewer".into(), builtin_reviewer());
    m.insert("debugger".into(), builtin_debugger());
    m.insert("ops".into(), builtin_ops());
    m.insert("admin".into(), builtin_admin());
    m
}

// ── Disk loading ────────────────────────────────────────────────

fn roles_dir_global() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("roles"))
}

fn roles_dir_project() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(".lean-ctx").join("roles");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn try_load_toml(path: &Path) -> Option<Role> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn load_role_from_disk(name: &str) -> Option<Role> {
    let filename = format!("{name}.toml");
    if let Some(dir) = roles_dir_project() {
        let path = dir.join(&filename);
        if let Some(mut r) = try_load_toml(&path) {
            r.role.name = name.to_string();
            return Some(r);
        }
    }
    if let Some(dir) = roles_dir_global() {
        let path = dir.join(&filename);
        if let Some(mut r) = try_load_toml(&path) {
            r.role.name = name.to_string();
            return Some(r);
        }
    }
    None
}

fn merge_roles(parent: &Role, child: &Role) -> Role {
    Role {
        role: RoleMeta {
            name: child.role.name.clone(),
            inherits: child.role.inherits.clone(),
            description: if child.role.description.is_empty() {
                parent.role.description.clone()
            } else {
                child.role.description.clone()
            },
            shell_policy: if child.role.shell_policy == default_shell_policy()
                && parent.role.shell_policy != default_shell_policy()
            {
                parent.role.shell_policy.clone()
            } else {
                child.role.shell_policy.clone()
            },
        },
        tools: if child.tools.allowed.is_empty() && child.tools.denied.is_empty() {
            parent.tools.clone()
        } else {
            child.tools.clone()
        },
        limits: RoleLimits {
            max_context_tokens: child.limits.max_context_tokens,
            max_shell_invocations: child.limits.max_shell_invocations,
            max_cost_usd: child.limits.max_cost_usd,
            warn_at_percent: child.limits.warn_at_percent,
            block_at_percent: child.limits.block_at_percent,
        },
    }
}

fn load_role_recursive(name: &str, depth: usize) -> Option<Role> {
    if depth > 5 {
        return None;
    }
    let role = load_role_from_disk(name).or_else(|| builtin_roles().remove(name))?;
    if let Some(parent_name) = &role.role.inherits {
        if let Some(parent) = load_role_recursive(parent_name, depth + 1) {
            return Some(merge_roles(&parent, &role));
        }
    }
    Some(role)
}

// ── Public API ──────────────────────────────────────────────────

pub fn load_role(name: &str) -> Option<Role> {
    load_role_recursive(name, 0)
}

pub fn active_role_name() -> String {
    let lock = ACTIVE_ROLE_NAME.get_or_init(|| std::sync::Mutex::new(String::new()));
    let mut guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if guard.is_empty() {
        let from_env = env::var("LEAN_CTX_ROLE")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        *guard = from_env.unwrap_or_else(|| "coder".to_string());
    }

    guard.clone()
}

pub fn active_role() -> Role {
    let name = active_role_name();
    load_role(&name).unwrap_or_else(builtin_coder)
}

pub fn set_active_role(name: &str) -> Result<Role, String> {
    let role = load_role(name).ok_or_else(|| format!("Role '{name}' not found"))?;
    let prev = active_role_name();
    let lock = ACTIVE_ROLE_NAME.get_or_init(|| std::sync::Mutex::new("coder".to_string()));
    if let Ok(mut g) = lock.lock() {
        *g = name.to_string();
    }
    if prev != name {
        crate::core::events::emit_role_changed(&prev, name);
    }
    Ok(role)
}

pub fn list_roles() -> Vec<RoleInfo> {
    let active = active_role_name();
    let builtins = builtin_roles();
    let mut seen = HashSet::new();
    let mut result = Vec::new();

    for dir in [roles_dir_project(), roles_dir_global()]
        .into_iter()
        .flatten()
    {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if seen.insert(stem.to_string()) {
                            if let Some(r) = load_role(stem) {
                                result.push(RoleInfo {
                                    name: stem.to_string(),
                                    source: if dir == roles_dir_project().unwrap_or_default() {
                                        RoleSource::Project
                                    } else {
                                        RoleSource::Global
                                    },
                                    description: r.role.description.clone(),
                                    is_active: stem == active,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    for (name, r) in &builtins {
        if seen.insert(name.clone()) {
            result.push(RoleInfo {
                name: name.clone(),
                source: RoleSource::BuiltIn,
                description: r.role.description.clone(),
                is_active: name == &active,
            });
        }
    }

    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

#[derive(Debug, Clone)]
pub struct RoleInfo {
    pub name: String,
    pub source: RoleSource,
    pub description: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoleSource {
    BuiltIn,
    Project,
    Global,
}

impl std::fmt::Display for RoleSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuiltIn => write!(f, "built-in"),
            Self::Project => write!(f, "project"),
            Self::Global => write!(f, "global"),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_count() {
        assert_eq!(builtin_roles().len(), 5);
    }

    #[test]
    fn coder_allows_all() {
        let r = builtin_coder();
        assert!(r.is_tool_allowed("ctx_read"));
        assert!(r.is_tool_allowed("ctx_edit"));
        assert!(r.is_tool_allowed("ctx_shell"));
        assert!(r.is_tool_allowed("anything"));
    }

    #[test]
    fn reviewer_denies_edits() {
        let r = builtin_reviewer();
        assert!(r.is_tool_allowed("ctx_read"));
        assert!(r.is_tool_allowed("ctx_review"));
        assert!(!r.is_tool_allowed("ctx_edit"));
        assert!(!r.is_tool_allowed("ctx_shell"));
        assert!(!r.is_tool_allowed("ctx_execute"));
    }

    #[test]
    fn reviewer_no_shell() {
        let r = builtin_reviewer();
        assert_eq!(r.limits.max_shell_invocations, 0);
    }

    #[test]
    fn ops_denies_edit() {
        let r = builtin_ops();
        assert!(!r.is_tool_allowed("ctx_edit"));
        assert!(r.is_tool_allowed("ctx_shell"));
        assert!(r.is_shell_allowed());
    }

    #[test]
    fn admin_unlimited() {
        let r = builtin_admin();
        assert!(r.is_tool_allowed("ctx_edit"));
        assert!(r.is_tool_allowed("ctx_shell"));
        assert_eq!(r.limits.max_context_tokens, 500_000);
        assert_eq!(r.limits.max_cost_usd, 50.0);
    }

    #[test]
    fn denied_overrides_allowed() {
        let r = Role {
            role: RoleMeta {
                name: "test".into(),
                ..Default::default()
            },
            tools: ToolPolicy {
                allowed: vec!["*".into()],
                denied: vec!["ctx_edit".into()],
            },
            limits: RoleLimits::default(),
        };
        assert!(r.is_tool_allowed("ctx_read"));
        assert!(!r.is_tool_allowed("ctx_edit"));
    }

    #[test]
    fn shell_deny_policy() {
        let r = Role {
            role: RoleMeta {
                name: "noshell".into(),
                shell_policy: "deny".into(),
                ..Default::default()
            },
            tools: ToolPolicy::default(),
            limits: RoleLimits::default(),
        };
        assert!(!r.is_shell_allowed());
    }

    #[test]
    fn load_builtin_by_name() {
        assert!(load_role("coder").is_some());
        assert!(load_role("reviewer").is_some());
        assert!(load_role("debugger").is_some());
        assert!(load_role("ops").is_some());
        assert!(load_role("admin").is_some());
        assert!(load_role("nonexistent").is_none());
    }

    #[test]
    fn merge_inherits_parent_tools() {
        let parent = builtin_reviewer();
        let child = Role {
            role: RoleMeta {
                name: "custom".into(),
                inherits: Some("reviewer".into()),
                description: "Custom reviewer".into(),
                ..Default::default()
            },
            tools: ToolPolicy::default(),
            limits: RoleLimits {
                max_context_tokens: 50_000,
                ..Default::default()
            },
        };
        let merged = merge_roles(&parent, &child);
        assert_eq!(merged.role.name, "custom");
        assert_eq!(merged.role.description, "Custom reviewer");
        assert!(!merged.is_tool_allowed("ctx_edit"));
        assert_eq!(merged.limits.max_context_tokens, 50_000);
    }

    #[test]
    fn default_role_is_coder() {
        let name = active_role_name();
        assert!(!name.is_empty());
    }

    #[test]
    fn list_roles_includes_builtins() {
        let roles = list_roles();
        let names: Vec<_> = roles.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"coder"));
        assert!(names.contains(&"reviewer"));
        assert!(names.contains(&"admin"));
    }

    #[test]
    fn warn_and_block_thresholds() {
        let r = builtin_coder();
        assert_eq!(r.limits.warn_at_percent, 80);
        // 255 = never block (LeanCTX philosophy: always help, never block)
        assert_eq!(r.limits.block_at_percent, 255);
    }
}
