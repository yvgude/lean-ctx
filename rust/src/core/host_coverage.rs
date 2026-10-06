// SPDX-License-Identifier: Apache-2.0
//! Host coverage — what the context gateway can actually see and stop per host.
//!
//! One source for `doctor`, the generated coverage matrix and the gateway
//! status line. It never claims more than the integration delivers:
//!
//! - **Enforced:** the host's model traffic flows through the lean-ctx proxy,
//!   so egress admission checks every byte before it leaves the device. If
//!   the proxy is down the request fails — fail-closed by construction.
//! - **Observed:** a hook sees every tool call but cannot stop or change it.
//! - **Partial:** lean-ctx admits its own tools and rewrites shell commands,
//!   but host-native file tools still reach the model unchecked. Rewrite
//!   hooks fail open on purpose (compression is best effort).
//! - **NotObservable:** MCP only — `ctx_*` calls are admitted, nothing else is
//!   visible to lean-ctx.
//! - **Unsupported:** no integration exists.

use crate::hooks::{HYBRID_AGENTS, HookMode, MCP_ONLY_AGENTS, REPLACE_AGENTS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageLevel {
    Unsupported,
    NotObservable,
    Partial,
    Observed,
    Enforced,
}

impl CoverageLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::Observed => "observed",
            Self::Partial => "partial",
            Self::NotObservable => "not_observable",
            Self::Unsupported => "unsupported",
        }
    }
}

/// What a host's own tooling lets lean-ctx do at the tool layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolIntegration {
    /// Native tools can be denied (`permissions.deny` / pre-tool deny hook).
    DenyNativeTools,
    /// Pre-tool hooks rewrite shell commands; native tools stay available.
    ShellRewrite,
    /// MCP server registration only.
    McpOnly,
    /// Not integrated.
    None,
}

/// Hosts whose model traffic `lean-ctx proxy enable` can route through the
/// proxy (`proxy_setup`), which is what makes egress admission complete.
/// `egress_routed_hosts` can only confirm routing for claude, codex and grok;
/// pi and commandcode are routable too but stay unconfirmed (never overclaimed).
const EGRESS_ROUTABLE: &[&str] = &[
    "claude",
    "claude-code",
    "codex",
    "grok",
    "pi",
    "commandcode",
];

#[must_use]
pub fn tool_integration(agent_key: &str) -> ToolIntegration {
    let key = agent_key.to_ascii_lowercase();
    let key = key.as_str();
    if REPLACE_AGENTS.contains(&key) {
        ToolIntegration::DenyNativeTools
    } else if HYBRID_AGENTS.contains(&key) {
        ToolIntegration::ShellRewrite
    } else if MCP_ONLY_AGENTS.contains(&key) || is_mcp_registered_host(key) {
        ToolIntegration::McpOnly
    } else {
        ToolIntegration::None
    }
}

/// Every host `lean-ctx setup` registers (the editor registry's agent keys).
/// Kept static so classification never spawns detection subprocesses; a test
/// pins it to the registry so a new host cannot go unclassified.
pub const HOSTS: &[&str] = &[
    "aider",
    "amazonq",
    "amp",
    "antigravity",
    "antigravity-cli",
    "augment",
    "claude",
    "cline",
    "codebuddy",
    "codewhale",
    "codex",
    "continue",
    "copilot",
    "crush",
    "cursor",
    "emacs",
    "gemini",
    "grok",
    "hermes",
    "jetbrains",
    "kiro",
    "neovim",
    "omp",
    "openclaw",
    "opencode",
    "qoder",
    "qodercli",
    "qoderwork",
    "qwen",
    "roo",
    "sublime",
    "trae",
    "verdent",
    "vibe",
    "vscode",
    "vscode-insiders",
    "windsurf",
    "zed",
];

fn is_mcp_registered_host(key: &str) -> bool {
    HOSTS.contains(&key)
}

#[must_use]
pub fn egress_routable(agent_key: &str) -> bool {
    EGRESS_ROUTABLE.contains(&agent_key.to_ascii_lowercase().as_str())
}

/// Coverage of one host as currently configured on this machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostCoverage {
    pub host: String,
    pub level: CoverageLevel,
    pub tools: ToolIntegration,
    pub egress_routed: bool,
    /// What is still outside the gateway at this level, in one sentence.
    pub gap: &'static str,
}

/// Classify a host from its static integration, the hook mode actually
/// installed (if any) and whether its model traffic is routed through the proxy.
#[must_use]
pub fn classify(agent_key: &str, installed: Option<HookMode>, egress_routed: bool) -> HostCoverage {
    let tools = tool_integration(agent_key);
    let routed = egress_routed && egress_routable(agent_key);
    let (level, gap) = if routed {
        let gap = match agent_key.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => {
                "needs an Anthropic API key (Pro/Max sign-in cannot be proxied); non-model calls pass through unchanged"
            }
            _ => "non-model calls of the host (sign-in, pairing) pass through unchanged",
        };
        (CoverageLevel::Enforced, gap)
    } else {
        match (tools, installed) {
            (ToolIntegration::None, _) => (
                CoverageLevel::Unsupported,
                "lean-ctx is not integrated with this host",
            ),
            (ToolIntegration::McpOnly, _) | (_, None | Some(HookMode::Mcp)) => (
                CoverageLevel::NotObservable,
                "only ctx_* calls are admitted; the host's own tools are invisible",
            ),
            (_, Some(HookMode::Hybrid | HookMode::Replace)) => (
                CoverageLevel::Partial,
                "host-native file tools reach the model without gateway admission",
            ),
        }
    };
    HostCoverage {
        host: agent_key.to_ascii_lowercase(),
        level,
        tools,
        egress_routed: routed,
        gap,
    }
}

/// The best level each integration can reach when fully configured — the
/// published matrix. `Observed` is never claimed: no installed integration
/// delivers an all-calls observer without also routing or rewriting.
#[must_use]
pub fn achievable(agent_key: &str) -> CoverageLevel {
    let best_mode = match tool_integration(agent_key) {
        ToolIntegration::DenyNativeTools => Some(HookMode::Replace),
        ToolIntegration::ShellRewrite => Some(HookMode::Hybrid),
        ToolIntegration::McpOnly => Some(HookMode::Mcp),
        ToolIntegration::None => None,
    };
    classify(agent_key, best_mode, true).level
}

/// The published coverage matrix (`docs/reference/generated/host-coverage.md`).
#[must_use]
pub fn matrix_markdown() -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "# Context gateway coverage by host\n\n\
         What the context gateway can see and stop for each host when fully set up.\n\
         `lean-ctx doctor` shows the coverage actually in effect on this machine.\n\n\
         | Level | Meaning |\n|---|---|\n\
         | enforced | Model traffic flows through the lean-ctx proxy; egress admission checks every byte before it leaves. A stopped proxy fails the request (fail-closed). |\n\
         | partial | lean-ctx tools are admitted and shell commands rewritten; host-native file tools still reach the model unchecked. Rewrite hooks fail open by design. |\n\
         | not_observable | MCP only: `ctx_*` calls are admitted, the host's own tools are invisible. |\n\
         | unsupported | No integration. |\n\n\
         `observed` (sees every call but cannot stop it) is reserved; no current integration delivers it, so none is claimed.\n\n\
         | Host | Tool integration | Proxy-routable | Best achievable | Still outside the gateway |\n\
         |---|---|---|---|---|\n",
    );
    for host in HOSTS {
        let best = match tool_integration(host) {
            ToolIntegration::DenyNativeTools => Some(HookMode::Replace),
            ToolIntegration::ShellRewrite => Some(HookMode::Hybrid),
            ToolIntegration::McpOnly => Some(HookMode::Mcp),
            ToolIntegration::None => None,
        };
        let coverage = classify(host, best, true);
        let tools = match coverage.tools {
            ToolIntegration::DenyNativeTools => "deny native tools",
            ToolIntegration::ShellRewrite => "shell rewrite",
            ToolIntegration::McpOnly => "MCP only",
            ToolIntegration::None => "none",
        };
        let _ = writeln!(
            out,
            "| {host} | {tools} | {} | {} | {} |",
            if egress_routable(host) { "yes" } else { "no" },
            coverage.level.as_str(),
            coverage.gap
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_proxy_routed_model_traffic_is_called_enforced() {
        // Replace mode alone still lets native Read through (Edit needs it).
        assert_eq!(
            classify("claude", Some(HookMode::Replace), false).level,
            CoverageLevel::Partial
        );
        let routed = classify("claude", Some(HookMode::Replace), true);
        assert_eq!(routed.level, CoverageLevel::Enforced);
        assert!(routed.egress_routed);
        // A routed flag on a host the proxy cannot carry is not believed.
        let cursor = classify("cursor", Some(HookMode::Replace), true);
        assert_eq!(cursor.level, CoverageLevel::Partial);
        assert!(!cursor.egress_routed);
    }

    #[test]
    fn missing_hooks_and_unknown_hosts_never_look_covered() {
        assert_eq!(
            classify("codex", None, false).level,
            CoverageLevel::NotObservable
        );
        assert_eq!(
            classify("codewhale", Some(HookMode::Hybrid), false).level,
            CoverageLevel::NotObservable
        );
        assert_eq!(
            classify("not-a-host", Some(HookMode::Replace), true).level,
            CoverageLevel::Unsupported
        );
    }

    #[test]
    fn every_registered_host_is_classified() {
        let mut registry: Vec<String> =
            crate::core::editor_registry::build_targets(std::path::Path::new("/nonexistent"))
                .into_iter()
                .map(|target| target.agent_key)
                .collect();
        registry.sort();
        registry.dedup();
        assert_eq!(
            registry, HOSTS,
            "editor registry and coverage hosts drifted"
        );
        for host in HOSTS {
            assert_ne!(achievable(host), CoverageLevel::Unsupported, "{host}");
        }
    }

    #[test]
    fn the_published_matrix_never_claims_observed() {
        for host in REPLACE_AGENTS
            .iter()
            .chain(HYBRID_AGENTS)
            .chain(MCP_ONLY_AGENTS)
        {
            assert_ne!(achievable(host), CoverageLevel::Observed, "{host}");
        }
        assert_eq!(achievable("codex"), CoverageLevel::Enforced);
        assert_eq!(achievable("cursor"), CoverageLevel::Partial);
    }
}
