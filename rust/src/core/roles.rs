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
    pub io: IoPolicy,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoPolicy {
    /// Boundary enforcement mode for sensitive I/O (warn|enforce).
    #[serde(default = "default_boundary_mode")]
    pub boundary_mode: String,
    /// Allow search to ignore .gitignore and scan everything.
    #[serde(default)]
    pub allow_ignore_gitignore: bool,
    /// Allow reading/indexing secret-like paths (e.g. .env, *.pem).
    #[serde(default)]
    pub allow_secret_paths: bool,
    /// Enable output redaction for tool outputs (admin can disable; non-admin always on).
    #[serde(default = "default_redact_outputs")]
    pub redact_outputs: bool,
    /// Allow cross-project knowledge search (default: false for non-admin roles).
    #[serde(default)]
    pub allow_cross_project_search: bool,
}

fn default_boundary_mode() -> String {
    "enforce".to_string()
}

fn default_redact_outputs() -> bool {
    true
}

impl Default for IoPolicy {
    fn default() -> Self {
        Self {
            boundary_mode: default_boundary_mode(),
            allow_ignore_gitignore: false,
            allow_secret_paths: false,
            redact_outputs: default_redact_outputs(),
            allow_cross_project_search: false,
        }
    }
}

impl IoPolicy {
    fn is_default(&self) -> bool {
        self.boundary_mode == default_boundary_mode()
            && !self.allow_ignore_gitignore
            && !self.allow_secret_paths
            && self.redact_outputs == default_redact_outputs()
    }
}

impl Role {
    #[must_use]
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

    #[must_use]
    pub fn is_shell_allowed(&self) -> bool {
        self.role.shell_policy != "deny"
    }

    #[must_use]
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
        io: IoPolicy::default(),
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
        io: IoPolicy {
            boundary_mode: "enforce".into(),
            ..Default::default()
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
        io: IoPolicy::default(),
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
        io: IoPolicy::default(),
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
        io: IoPolicy {
            boundary_mode: "enforce".into(),
            allow_ignore_gitignore: true,
            allow_secret_paths: true,
            redact_outputs: true,
            allow_cross_project_search: true,
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
    roles_dir_project_from(None)
}

fn roles_dir_project_from(project_root: Option<&str>) -> Option<PathBuf> {
    if let Some(root) = project_root {
        let candidate = PathBuf::from(root).join(".lean-ctx").join("roles");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
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
    if !is_valid_role_name(name) {
        tracing::warn!(
            "[SECURITY] Invalid role name rejected (path-traversal/special chars): {name}"
        );
        return None;
    }

    let filename = format!("{name}.toml");
    if let Some(dir) = roles_dir_project() {
        let path = dir.join(&filename);
        if path.exists() {
            if RESERVED_ROLE_NAMES
                .iter()
                .any(|r| r.eq_ignore_ascii_case(name))
            {
                tracing::warn!(
                    "[SECURITY] Project-level shadowing of reserved role '{name}' ignored. \
                     Use global ~/.lean-ctx/roles/ to customize built-in roles."
                );
            } else if let Some(mut r) = try_load_toml(&path) {
                r.role.name = name.to_string();
                return Some(r);
            }
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
        io: if child.io.is_default() && !parent.io.is_default() {
            parent.io.clone()
        } else {
            child.io.clone()
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

fn load_role_recursive(name: &str, visited: &mut HashSet<String>) -> Option<Role> {
    if !visited.insert(name.to_string()) {
        tracing::warn!("[SECURITY] Circular role inheritance detected at '{name}'");
        return None;
    }
    let role = load_role_from_disk(name).or_else(|| builtin_roles().remove(name))?;
    if let Some(parent_name) = &role.role.inherits {
        if is_privileged_role(parent_name) && roles_dir_project().is_some() {
            let is_from_project =
                roles_dir_project().is_some_and(|d| d.join(format!("{name}.toml")).exists());
            if is_from_project {
                tracing::warn!(
                    "[SECURITY] Project-level role '{name}' inheriting from privileged \
                     role '{parent_name}' is blocked."
                );
                return None;
            }
        }
        if let Some(parent) = load_role_recursive(parent_name, visited) {
            return Some(merge_roles(&parent, &role));
        }
    }
    Some(role)
}

// ── Public API ──────────────────────────────────────────────────

#[must_use]
pub fn load_role(name: &str) -> Option<Role> {
    let mut visited = HashSet::new();
    load_role_recursive(name, &mut visited)
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

/// Roles that grant elevated privileges and cannot be activated via MCP tool calls.
/// These roles must be set via env var (`LEAN_CTX_ROLE`) or config file.
const PRIVILEGED_ROLES: &[&str] = &["admin", "ops"];

/// Reserved role names that cannot be shadowed from project-level config.
const RESERVED_ROLE_NAMES: &[&str] = &["coder", "reviewer", "debugger", "ops", "admin"];

/// Returns true if the named role has elevated privileges that require
/// explicit configuration (env/config) rather than runtime activation.
/// Case-insensitive comparison to prevent "Admin" bypass.
#[must_use]
pub fn is_privileged_role(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    PRIVILEGED_ROLES.iter().any(|p| *p == lower)
}

/// Validate role name: alphanumeric, underscore, hyphen only. No path traversal.
fn is_valid_role_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Check if a merged role is effectively privileged (wildcard tools + no denials, or secret paths).
fn is_effectively_privileged(role: &Role) -> bool {
    let wildcard_tools =
        role.tools.allowed.iter().any(|a| a == "*") && role.tools.denied.is_empty();
    wildcard_tools || role.io.allow_secret_paths
}

pub fn set_active_role(name: &str) -> Result<Role, String> {
    set_active_role_with_source(name, false)
}

/// Set active role. `from_config` = true allows privileged roles (env/config startup).
pub fn set_active_role_with_source(name: &str, from_config: bool) -> Result<Role, String> {
    if !is_valid_role_name(name) {
        return Err(format!(
            "[SECURITY] Invalid role name '{name}'. Only alphanumeric, underscore, and hyphen allowed."
        ));
    }

    let role = load_role(name).ok_or_else(|| format!("Role '{name}' not found"))?;

    if !from_config && is_privileged_role(name) {
        return Err(format!(
            "[SECURITY] Cannot escalate to privileged role '{name}' at runtime. \
             Set LEAN_CTX_ROLE={name} in your environment or config to use this role."
        ));
    }

    let is_builtin = builtin_roles().contains_key(name);
    if !from_config && !is_builtin && is_effectively_privileged(&role) {
        return Err(format!(
            "[SECURITY] Cannot activate role '{name}' at runtime: it has effectively \
             privileged permissions (wildcard tools or secret path access). \
             Set LEAN_CTX_ROLE={name} in environment or config."
        ));
    }

    let prev = active_role_name();
    let lock = ACTIVE_ROLE_NAME.get_or_init(|| std::sync::Mutex::new("coder".to_string()));
    match lock.lock() {
        Ok(mut g) => *g = name.to_string(),
        Err(poisoned) => *poisoned.into_inner() = name.to_string(),
    }
    if prev != name {
        crate::core::events::emit_role_changed(&prev, name);
    }
    Ok(role)
}

#[must_use]
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
                if path.extension().is_some_and(|e| e == "toml")
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                    && seen.insert(stem.to_string())
                    && let Some(r) = load_role(stem)
                {
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

    result.sort_by_key(|r| r.name.clone());
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
            io: IoPolicy::default(),
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
            io: IoPolicy::default(),
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
            io: IoPolicy::default(),
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

    #[test]
    fn runtime_escalation_to_admin_blocked() {
        let result = set_active_role("admin");
        assert!(
            result.is_err(),
            "runtime escalation to admin must be blocked"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("SECURITY"),
            "error must indicate security: {err}"
        );
    }

    #[test]
    fn runtime_escalation_to_ops_blocked() {
        let result = set_active_role("ops");
        assert!(result.is_err(), "runtime escalation to ops must be blocked");
    }

    #[test]
    fn config_escalation_to_admin_allowed() {
        let result = set_active_role_with_source("admin", true);
        assert!(
            result.is_ok(),
            "config-source escalation to admin must work"
        );
    }

    #[test]
    fn runtime_switch_to_coder_allowed() {
        let result = set_active_role("coder");
        assert!(result.is_ok(), "switching to coder must always work");
    }

    #[test]
    fn runtime_switch_to_reviewer_allowed() {
        let result = set_active_role("reviewer");
        assert!(result.is_ok(), "switching to reviewer must always work");
    }

    #[test]
    fn privileged_roles_detected() {
        assert!(is_privileged_role("admin"));
        assert!(is_privileged_role("ops"));
        assert!(!is_privileged_role("coder"));
        assert!(!is_privileged_role("reviewer"));
        assert!(!is_privileged_role("debugger"));
    }

    // --- Phase 2 V2: Role name validation ---

    #[test]
    fn valid_role_names() {
        assert!(is_valid_role_name("coder"));
        assert!(is_valid_role_name("my-custom-role"));
        assert!(is_valid_role_name("Role_123"));
        assert!(is_valid_role_name("ADMIN"));
    }

    #[test]
    fn invalid_role_names() {
        assert!(!is_valid_role_name(""));
        assert!(!is_valid_role_name("../../evil"));
        assert!(!is_valid_role_name("role with spaces"));
        assert!(!is_valid_role_name("role;drop"));
        assert!(!is_valid_role_name("a".repeat(65).as_str()));
    }

    #[test]
    fn case_insensitive_privileged_check() {
        assert!(is_privileged_role("Admin"));
        assert!(is_privileged_role("ADMIN"));
        assert!(is_privileged_role("Ops"));
        assert!(is_privileged_role("OPS"));
    }

    #[test]
    fn invalid_role_name_rejected_at_set() {
        let result = set_active_role("../../evil");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid role name"));
    }

    #[test]
    fn effectively_privileged_role_blocked_at_runtime() {
        let role = Role {
            role: RoleMeta {
                name: "sneaky".into(),
                ..Default::default()
            },
            tools: ToolPolicy {
                allowed: vec!["*".into()],
                denied: vec![],
            },
            io: IoPolicy::default(),
            limits: RoleLimits::default(),
        };
        assert!(
            is_effectively_privileged(&role),
            "wildcard + no denials = effectively privileged"
        );
    }

    #[test]
    fn debugger_runtime_switch_allowed() {
        let result = set_active_role("debugger");
        assert!(
            result.is_ok(),
            "built-in debugger must be activatable at runtime"
        );
    }
}
