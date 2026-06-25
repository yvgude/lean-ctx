use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FsRead,
    FsWrite,
    FsDelete,
    NetOutbound,
    ExecSandbox,
    ExecUnrestricted,
    KnowledgeRead,
    KnowledgeWrite,
    CrossProject,
    ConfigWrite,
    AgentManage,
}

impl Capability {
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::FsRead => "fs:read",
            Self::FsWrite => "fs:write",
            Self::FsDelete => "fs:delete",
            Self::NetOutbound => "net:outbound",
            Self::ExecSandbox => "exec:sandbox",
            Self::ExecUnrestricted => "exec:unrestricted",
            Self::KnowledgeRead => "knowledge:read",
            Self::KnowledgeWrite => "knowledge:write",
            Self::CrossProject => "cross_project",
            Self::ConfigWrite => "config:write",
            Self::AgentManage => "agent:manage",
        }
    }
}

pub struct CapabilityCheckResult {
    pub allowed: bool,
    pub missing: Vec<Capability>,
}

#[must_use]
pub fn required_capabilities(tool_name: &str) -> &'static [Capability] {
    match tool_name {
        "ctx_edit" => &[Capability::FsRead, Capability::FsWrite],
        "ctx_shell" => &[Capability::ExecUnrestricted],
        "ctx_knowledge" => &[Capability::KnowledgeRead, Capability::KnowledgeWrite],
        "ctx_handoff" => &[Capability::KnowledgeRead, Capability::AgentManage],
        "ctx_agent" | "ctx_task" => &[Capability::AgentManage],
        "ctx_session" | "ctx" => &[],
        "ctx_share" => &[Capability::KnowledgeRead, Capability::CrossProject],
        _ => &[Capability::FsRead],
    }
}

#[must_use]
pub fn role_capabilities(role_name: &str) -> HashSet<Capability> {
    match role_name {
        "admin" => HashSet::from([
            Capability::FsRead,
            Capability::FsWrite,
            Capability::FsDelete,
            Capability::NetOutbound,
            Capability::ExecSandbox,
            Capability::ExecUnrestricted,
            Capability::KnowledgeRead,
            Capability::KnowledgeWrite,
            Capability::CrossProject,
            Capability::ConfigWrite,
            Capability::AgentManage,
        ]),
        "reviewer" | "ci" => HashSet::from([
            Capability::FsRead,
            Capability::ExecSandbox,
            Capability::KnowledgeRead,
        ]),
        "minimal" => HashSet::from([Capability::FsRead, Capability::KnowledgeRead]),
        _ => capabilities_from_role(role_name),
    }
}

fn capabilities_from_role(role_name: &str) -> HashSet<Capability> {
    let Some(role) = crate::core::roles::load_role(role_name) else {
        return HashSet::from([
            Capability::FsRead,
            Capability::FsWrite,
            Capability::ExecSandbox,
            Capability::ExecUnrestricted,
            Capability::KnowledgeRead,
            Capability::KnowledgeWrite,
            Capability::AgentManage,
        ]);
    };

    let mut caps = HashSet::new();
    caps.insert(Capability::FsRead);

    let has_tool = |name: &str| {
        role.tools.allowed.iter().any(|a| a == "*" || a == name)
            && !role.tools.denied.iter().any(|d| d == name || d == "*")
    };

    if has_tool("ctx_edit") {
        caps.insert(Capability::FsWrite);
    }
    if has_tool("ctx_shell") {
        caps.insert(Capability::ExecSandbox);
        caps.insert(Capability::ExecUnrestricted);
    }
    if has_tool("ctx_knowledge") {
        caps.insert(Capability::KnowledgeRead);
        caps.insert(Capability::KnowledgeWrite);
    }
    if has_tool("ctx_agent") || has_tool("ctx_task") || has_tool("ctx_handoff") {
        caps.insert(Capability::AgentManage);
    }
    if role.io.allow_cross_project_search {
        caps.insert(Capability::CrossProject);
    }
    if role.io.allow_secret_paths {
        caps.insert(Capability::ConfigWrite);
    }

    caps
}

#[must_use]
pub fn check_capabilities(role_name: &str, tool_name: &str) -> CapabilityCheckResult {
    let required = required_capabilities(tool_name);
    if required.is_empty() {
        return CapabilityCheckResult {
            allowed: true,
            missing: Vec::new(),
        };
    }
    let granted = role_capabilities(role_name);
    let missing: Vec<Capability> = required
        .iter()
        .filter(|c| !granted.contains(c))
        .copied()
        .collect();
    CapabilityCheckResult {
        allowed: missing.is_empty(),
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_has_all_capabilities() {
        let caps = role_capabilities("admin");
        assert!(caps.contains(&Capability::FsRead));
        assert!(caps.contains(&Capability::FsWrite));
        assert!(caps.contains(&Capability::FsDelete));
        assert!(caps.contains(&Capability::NetOutbound));
        assert!(caps.contains(&Capability::ExecUnrestricted));
        assert!(caps.contains(&Capability::ConfigWrite));
        assert!(caps.contains(&Capability::AgentManage));
    }

    #[test]
    fn reviewer_cannot_write() {
        let result = check_capabilities("reviewer", "ctx_edit");
        assert!(!result.allowed);
        assert!(result.missing.contains(&Capability::FsWrite));
    }

    #[test]
    fn minimal_cannot_shell() {
        let result = check_capabilities("minimal", "ctx_shell");
        assert!(!result.allowed);
        assert!(result.missing.contains(&Capability::ExecUnrestricted));
    }

    #[test]
    fn session_always_allowed() {
        let result = check_capabilities("minimal", "ctx_session");
        assert!(result.allowed);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn developer_can_edit() {
        let result = check_capabilities("developer", "ctx_edit");
        assert!(result.allowed);
    }

    #[test]
    fn unknown_role_gets_defaults() {
        let result = check_capabilities("unknown_role", "ctx_read");
        assert!(result.allowed);
    }

    #[test]
    fn unknown_tool_requires_fs_read() {
        let required = required_capabilities("some_unknown_tool");
        assert_eq!(required, &[Capability::FsRead]);
    }

    #[test]
    fn display_names_are_colon_separated() {
        assert_eq!(Capability::FsRead.display_name(), "fs:read");
        assert_eq!(
            Capability::ExecUnrestricted.display_name(),
            "exec:unrestricted"
        );
        assert_eq!(Capability::AgentManage.display_name(), "agent:manage");
    }
}
