//! Maps lean-ctx security features to the OWASP Top 10 for Agentic Applications (2025).
//! Used by `lean-ctx audit` CLI and `/v1/audit/events` endpoint.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct OwaspMapping {
    pub owasp_id: &'static str,
    pub owasp_title: &'static str,
    pub risk_description: &'static str,
    pub lean_ctx_mitigations: Vec<Mitigation>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mitigation {
    pub feature: &'static str,
    pub module: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub enum Coverage {
    Full,
    Partial,
    Minimal,
}

#[must_use]
pub fn alignment() -> Vec<OwaspMapping> {
    vec![
        OwaspMapping {
            owasp_id: "OWASP-AGENT-01",
            owasp_title: "Excessive Agency",
            risk_description: "Agent performs actions beyond intended scope or without proper authorization",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Capability System",
                    module: "core/capabilities.rs",
                    description: "Fine-grained capability declarations per tool (fs:read, fs:write, exec, net)",
                },
                Mitigation {
                    feature: "Role Guard",
                    module: "server/role_guard.rs",
                    description: "5 built-in roles with tool allowlists and shell policy",
                },
                Mitigation {
                    feature: "Shell Allowlist",
                    module: "core/shell_allowlist.rs",
                    description: "Opt-in command allowlist restricting which binaries agents can execute",
                },
                Mitigation {
                    feature: "Context Budget",
                    module: "core/agent_budget.rs",
                    description: "Per-agent token budgets preventing resource exhaustion",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-02",
            owasp_title: "Prompt Injection",
            risk_description: "Malicious instructions injected via data processed by the agent",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Context Compression",
                    module: "core/terse/",
                    description: "Deterministic compression reduces attack surface in injected content",
                },
                Mitigation {
                    feature: "Secret Detection",
                    module: "core/secret_detection.rs",
                    description: "Pre-read scanning detects and optionally redacts sensitive patterns",
                },
                Mitigation {
                    feature: "I/O Boundary",
                    module: "core/io_boundary.rs",
                    description: "Content filtering and secret-like path blocking before agent consumption",
                },
            ],
            coverage: Coverage::Partial,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-03",
            owasp_title: "Sensitive Information Disclosure",
            risk_description: "Agent exposes confidential data through outputs or tool interactions",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "PathJail",
                    module: "core/pathjail.rs",
                    description: "Filesystem jail prevents reads outside project root",
                },
                Mitigation {
                    feature: "I/O Boundary",
                    module: "core/io_boundary.rs",
                    description: "Secret-like path detection (.env, .ssh, credentials)",
                },
                Mitigation {
                    feature: "Secret Detection",
                    module: "core/secret_detection.rs",
                    description: "Regex-based detection of API keys, tokens, passwords in file content",
                },
                Mitigation {
                    feature: "Proxy Header Allowlist",
                    module: "proxy/forward.rs",
                    description: "Prevents leaking Set-Cookie and other sensitive headers",
                },
                Mitigation {
                    feature: "Memory Boundary",
                    module: "core/memory_boundary.rs",
                    description: "Cross-project access control with audit trail",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-04",
            owasp_title: "Denial of Service",
            risk_description: "Agent overwhelms system resources or causes service disruption",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Rate Limiter",
                    module: "core/a2a/rate_limiter.rs",
                    description: "Per-agent per-tool rate limiting",
                },
                Mitigation {
                    feature: "Memory Guard",
                    module: "core/config/memory.rs",
                    description: "RAM usage caps and idle cleanup",
                },
                Mitigation {
                    feature: "Budget Tracker",
                    module: "core/agent_budget.rs",
                    description: "Token budget enforcement with hard limits",
                },
                Mitigation {
                    feature: "Loop Detection",
                    module: "config loop_detection",
                    description: "Detects and throttles repetitive tool call patterns",
                },
                Mitigation {
                    feature: "Tool Timeout",
                    module: "engine/mod.rs",
                    description: "120s timeout on tool execution prevents indefinite hangs",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-05",
            owasp_title: "Supply Chain Vulnerabilities",
            risk_description: "Compromised tools, plugins, or dependencies affect agent behavior",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Signed Handoff Bundles",
                    module: "core/handoff_transfer_bundle.rs",
                    description: "Ed25519 signatures verify integrity and provenance of transferred data",
                },
                Mitigation {
                    feature: "Audit Trail",
                    module: "core/audit_trail.rs",
                    description: "SHA-256 chained append-only log of all tool calls and security events",
                },
                Mitigation {
                    feature: "Agent Identity",
                    module: "core/agent_identity.rs",
                    description: "Per-agent Ed25519 keypairs for cryptographic identity",
                },
            ],
            coverage: Coverage::Partial,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-06",
            owasp_title: "Insufficient Logging and Monitoring",
            risk_description: "Lack of visibility into agent actions and security events",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Audit Trail",
                    module: "core/audit_trail.rs",
                    description: "Every tool call logged with agent ID, role, input hash, output tokens",
                },
                Mitigation {
                    feature: "Compliance Reports",
                    module: "cli/audit_report.rs",
                    description: "CLI command to generate aggregated compliance reports",
                },
                Mitigation {
                    feature: "Context OS Events",
                    module: "core/context_os.rs",
                    description: "Real-time event bus with SSE streaming for dashboard",
                },
                Mitigation {
                    feature: "Proxy Metrics",
                    module: "proxy/metrics.rs",
                    description: "Atomic counters for requests, tokens saved, bytes compressed",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-07",
            owasp_title: "Insecure Code Execution",
            risk_description: "Agent executes arbitrary or unsafe code without proper sandboxing",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Sandbox Level 0",
                    module: "core/sandbox.rs",
                    description: "Subprocess isolation with env_clear and timeout",
                },
                Mitigation {
                    feature: "Sandbox Level 1 (macOS)",
                    module: "core/sandbox_seatbelt.rs",
                    description: "OS-level Seatbelt profiles restricting filesystem and network",
                },
                Mitigation {
                    feature: "Sandbox Level 1 (Linux)",
                    module: "core/sandbox_landlock.rs",
                    description: "Landlock LSM restricting filesystem access",
                },
                Mitigation {
                    feature: "Command Blocklist",
                    module: "tools ctx_shell",
                    description: "Dangerous command patterns blocked before execution",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-08",
            owasp_title: "Broken Access Control",
            risk_description: "Agent accesses resources or performs actions beyond its permissions",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "RBAC",
                    module: "core/roles.rs",
                    description: "5 built-in roles (viewer, coder, admin, ci, restricted) with granular policies",
                },
                Mitigation {
                    feature: "Capability System",
                    module: "core/capabilities.rs",
                    description: "Tool-level capability requirements checked against role grants",
                },
                Mitigation {
                    feature: "PathJail",
                    module: "core/pathjail.rs",
                    description: "All path arguments jailed to project root",
                },
                Mitigation {
                    feature: "Boundary Policy",
                    module: "core/memory_boundary.rs",
                    description: "Cross-project access control configurable per policy",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-09",
            owasp_title: "Improper Multi-Agent Orchestration",
            risk_description: "Coordination failures between agents lead to conflicts or data corruption",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Per-Agent Ledger",
                    module: "core/context_ledger.rs",
                    description: "Isolated context tracking per agent, preventing cross-contamination",
                },
                Mitigation {
                    feature: "Agent Registry",
                    module: "core/agents.rs",
                    description: "HTTP-backed registration with heartbeat and auto-deregistration",
                },
                Mitigation {
                    feature: "TaskStore File Locks",
                    module: "core/a2a/task.rs",
                    description: "Advisory file locks prevent lost updates from concurrent access",
                },
                Mitigation {
                    feature: "Atomic Writes",
                    module: "core/context_ledger.rs",
                    description: "Crash-safe temp+rename writes for all JSON stores",
                },
            ],
            coverage: Coverage::Full,
        },
        OwaspMapping {
            owasp_id: "OWASP-AGENT-10",
            owasp_title: "Insufficient Governance",
            risk_description: "Lack of organizational policies and controls over agent behavior",
            lean_ctx_mitigations: vec![
                Mitigation {
                    feature: "Policy Engine",
                    module: "core/context_policies.rs",
                    description: "Declarative policies with agent, content, and time-based conditions",
                },
                Mitigation {
                    feature: "Compliance Reports",
                    module: "cli/audit_report.rs",
                    description: "Aggregated reports: reads, compressions, denials, budget usage",
                },
                Mitigation {
                    feature: "Auto-Reroot Protection",
                    module: "tools/server_paths.rs",
                    description: "Opt-in control over project root changes, audited",
                },
                Mitigation {
                    feature: "Config-Driven",
                    module: "core/config/mod.rs",
                    description: "All security features configurable via config.toml",
                },
            ],
            coverage: Coverage::Full,
        },
    ]
}

/// Returns a compact summary suitable for CLI output.
#[must_use]
pub fn summary() -> String {
    let mappings = alignment();
    let mut out = String::from("OWASP Top 10 for Agentic Applications — lean-ctx Alignment\n");
    out.push_str(&"=".repeat(60));
    out.push('\n');
    for m in &mappings {
        let icon = match m.coverage {
            Coverage::Full => "●",
            Coverage::Partial => "◐",
            Coverage::Minimal => "○",
        };
        out.push_str(&format!(
            "\n{icon} {} — {}\n  Mitigations: {}\n",
            m.owasp_id,
            m.owasp_title,
            m.lean_ctx_mitigations
                .iter()
                .map(|m| m.feature)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let full = mappings
        .iter()
        .filter(|m| m.coverage == Coverage::Full)
        .count();
    let partial = mappings
        .iter()
        .filter(|m| m.coverage == Coverage::Partial)
        .count();
    out.push_str(&format!(
        "\nCoverage: {full}/10 Full, {partial}/10 Partial\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_covers_all_ten() {
        let a = alignment();
        assert_eq!(a.len(), 10);
        for (i, m) in a.iter().enumerate() {
            assert_eq!(m.owasp_id, format!("OWASP-AGENT-{:02}", i + 1));
            assert!(!m.lean_ctx_mitigations.is_empty());
        }
    }

    #[test]
    fn summary_contains_all_ids() {
        let s = summary();
        for i in 1..=10 {
            assert!(s.contains(&format!("OWASP-AGENT-{i:02}")));
        }
    }
}
