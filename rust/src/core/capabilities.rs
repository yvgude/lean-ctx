use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    FsRead,
    FsWrite,
    FsDelete,
    NetOutbound,
    /// Only the organization's linked Hive (its connection, lease and
    /// opt-in decide what goes out), never another host.
    HiveNet,
    ExecSandbox,
    ExecUnrestricted,
    KnowledgeRead,
    KnowledgeWrite,
    CrossProject,
    ConfigWrite,
    AgentManage,
}

impl Capability {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::FsRead => "fs:read",
            Self::FsWrite => "fs:write",
            Self::FsDelete => "fs:delete",
            Self::NetOutbound => "net:outbound",
            Self::HiveNet => "x-hive:net",
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

pub fn required_capabilities(tool_name: &str) -> &'static [Capability] {
    match tool_name {
        "ctx_edit" => &[Capability::FsRead, Capability::FsWrite],
        "ctx_shell" => &[Capability::ExecUnrestricted],
        "ctx_knowledge" => &[Capability::KnowledgeRead, Capability::KnowledgeWrite],
        // ctx_share is agent-to-agent handover WITHIN one project: the store is
        // keyed per project root and push is jailed to it (enterprise#28), so it
        // is not a cross-project data flow. AgentManage keeps it (like
        // ctx_handoff) out of read-only roles (reviewer/ci/minimal).
        "ctx_handoff" | "ctx_share" => &[Capability::KnowledgeRead, Capability::AgentManage],
        "ctx_agent" | "ctx_work_graph" => &[Capability::AgentManage],
        "ctx_tasks" => &[Capability::AgentManage, Capability::HiveNet],
        "ctx_session" | "ctx" => &[],
        _ => &[Capability::FsRead],
    }
}

pub fn role_capabilities(role_name: &str) -> HashSet<Capability> {
    let mut caps = granted_capabilities(role_name);
    // The Hive release switch (3.12.1): off, no role holds or advertises
    // the Hive capability, as the Hive tools are withheld.
    if !crate::core::hive::available_locally() {
        caps.remove(&Capability::HiveNet);
    }
    caps
}

fn granted_capabilities(role_name: &str) -> HashSet<Capability> {
    match role_name {
        "admin" => HashSet::from([
            Capability::FsRead,
            Capability::FsWrite,
            Capability::FsDelete,
            Capability::NetOutbound,
            Capability::HiveNet,
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
            Capability::HiveNet,
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
    if has_tool("ctx_agent") || has_tool("ctx_handoff") || has_tool("ctx_work_graph") {
        caps.insert(Capability::AgentManage);
    }
    // Hive tasks reach the organization's linked Hive only; the
    // connection, its lease and the opt-in decide what goes out (C0 1.9 §4).
    if crate::core::hive::available_locally() && has_tool("ctx_tasks") {
        caps.insert(Capability::AgentManage);
        caps.insert(Capability::HiveNet);
    }
    if role.io.allow_cross_project_search {
        caps.insert(Capability::CrossProject);
    }
    if role.io.allow_secret_paths {
        caps.insert(Capability::ConfigWrite);
    }

    caps
}

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
        assert!(result.missing.is_empty(), "{:?}", result.missing);
    }

    #[test]
    fn developer_can_edit() {
        let result = check_capabilities("developer", "ctx_edit");
        assert!(result.allowed);
    }

    /// With the Hive release switch off, no role holds the Hive capability.
    #[test]
    fn no_role_holds_hive_net_while_hive_is_withheld() {
        if crate::core::hive::available_locally() {
            return;
        }
        for role in ["admin", "coder", "unknown_role", "reviewer", "minimal"] {
            assert!(!role_capabilities(role).contains(&Capability::HiveNet));
        }
        assert!(!check_capabilities("admin", "ctx_tasks").allowed);
    }

    /// ctx_tasks works out of the box for the default role (and any role
    /// that allows it), never for the read-only ones — once the Hive
    /// switch is on (here: the test preview).
    #[test]
    fn tasks_work_for_the_default_role_only_where_allowed() {
        if crate::test_env::run_in_own_process(
            "core::capabilities::tests::tasks_work_for_the_default_role_only_where_allowed",
            &[(crate::core::hive::PREVIEW_ENV, "1")],
        ) {
            return;
        }
        assert!(check_capabilities("coder", "ctx_tasks").allowed);
        assert!(check_capabilities("unknown_role", "ctx_tasks").allowed);
        for read_only in ["reviewer", "ci", "minimal"] {
            assert!(!check_capabilities(read_only, "ctx_tasks").allowed);
        }
        // Hive only: no role but admin reaches other hosts.
        for role in ["coder", "unknown_role", "reviewer", "minimal"] {
            assert!(!role_capabilities(role).contains(&Capability::NetOutbound));
        }
        assert_eq!(Capability::HiveNet.display_name(), "x-hive:net");
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
