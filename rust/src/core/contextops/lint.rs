use serde::{Deserialize, Serialize};

use super::config::RulesConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for LintSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => write!(f, "ERROR"),
            Self::Warning => write!(f, "WARNING"),
            Self::Info => write!(f, "INFO"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintWarning {
    pub severity: LintSeverity,
    pub code: String,
    pub message: String,
    pub target: Option<String>,
}

const KNOWN_TOOLS: &[&str] = &[
    "ctx_read",
    "ctx_shell",
    "ctx_search",
    "ctx_tree",
    "ctx_compress",
    "ctx_edit",
    "ctx_overview",
    "ctx_session",
    "ctx_knowledge",
    "ctx_semantic_search",
    "ctx_benchmark",
    "ctx_workflow",
    "ctx_heatmap",
    "ctx_cost",
    "ctx_metrics",
    "ctx_call",
    "ctx_callgraph",
    "ctx_gain",
    "ctx_provider",
    "ctx_pack",
    "ctx_review",
    "ctx_multi_read",
    "ctx_graph",
    "ctx_plugins",
    "ctx_repomap",
    "ctx_rules",
    "ctx_multi_repo",
    "ctx_agent",
    "ctx_dedup",
    "ctx_preload",
];

const REQUIRED_SECTIONS: &[&str] = &["Mode Selection", "File Editing"];

#[must_use]
pub fn lint(config: &RulesConfig, home: &std::path::Path) -> Vec<LintWarning> {
    let mut warnings = Vec::new();

    lint_core_content(&config.rules.core.content, &mut warnings);
    lint_version(&config.rules.version, &mut warnings);
    lint_agent_consistency(config, &mut warnings);
    lint_targets(home, &mut warnings);

    warnings
}

fn lint_core_content(content: &str, warnings: &mut Vec<LintWarning>) {
    if content.trim().is_empty() {
        warnings.push(LintWarning {
            severity: LintSeverity::Error,
            code: "EMPTY_CORE".to_string(),
            message: "Core rules content is empty".to_string(),
            target: None,
        });
        return;
    }

    for section in REQUIRED_SECTIONS {
        if !content.contains(section) {
            warnings.push(LintWarning {
                severity: LintSeverity::Warning,
                code: "MISSING_SECTION".to_string(),
                message: format!("Core rules missing required section: {section}"),
                target: None,
            });
        }
    }

    check_tool_references(content, None, warnings);
}

fn lint_version(version: &str, warnings: &mut Vec<LintWarning>) {
    if version.is_empty() {
        warnings.push(LintWarning {
            severity: LintSeverity::Error,
            code: "NO_VERSION".to_string(),
            message: "Rules version is not set".to_string(),
            target: None,
        });
    }

    let expected = format!(
        "<!-- version: {} -->",
        crate::core::rules_canonical::RULES_VERSION
    );
    if !expected.contains(version) && !version.contains("1.0") {
        warnings.push(LintWarning {
            severity: LintSeverity::Info,
            code: "VERSION_MISMATCH".to_string(),
            message: format!(
                "Config version '{version}' does not match current rules version '{expected}'"
            ),
            target: None,
        });
    }
}

fn lint_agent_consistency(config: &RulesConfig, warnings: &mut Vec<LintWarning>) {
    for (agent_name, agent_rules) in &config.rules.agent {
        check_tool_references(&agent_rules.extra, Some(agent_name), warnings);

        if agent_rules.extra.contains("NEVER") && config.rules.core.content.contains("ALWAYS") {
            let never_lines: Vec<&str> = agent_rules
                .extra
                .lines()
                .filter(|l| l.contains("NEVER"))
                .collect();
            let always_lines: Vec<&str> = config
                .rules
                .core
                .content
                .lines()
                .filter(|l| l.contains("ALWAYS"))
                .collect();

            for never_line in &never_lines {
                for always_line in &always_lines {
                    if lines_reference_same_tool(never_line, always_line) {
                        warnings.push(LintWarning {
                            severity: LintSeverity::Warning,
                            code: "CONFLICT".to_string(),
                            message: format!(
                                "Agent '{agent_name}' has NEVER rule that may conflict with core ALWAYS rule"
                            ),
                            target: Some(agent_name.clone()),
                        });
                        break;
                    }
                }
            }
        }
    }
}

fn lint_targets(home: &std::path::Path, warnings: &mut Vec<LintWarning>) {
    let statuses = crate::rules_inject::collect_rules_status(home);
    for status in &statuses {
        if status.detected && status.state == "outdated" {
            warnings.push(LintWarning {
                severity: LintSeverity::Warning,
                code: "OUTDATED_TARGET".to_string(),
                message: format!("{} has outdated rules (version mismatch)", status.name),
                target: Some(status.name.clone()),
            });
        }
    }
}

fn check_tool_references(content: &str, agent: Option<&str>, warnings: &mut Vec<LintWarning>) {
    for word in content.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if cleaned.starts_with("ctx_") && !KNOWN_TOOLS.contains(&cleaned) {
            warnings.push(LintWarning {
                severity: LintSeverity::Warning,
                code: "UNKNOWN_TOOL".to_string(),
                message: format!("References unknown tool: {cleaned}"),
                target: agent.map(String::from),
            });
        }
    }
}

fn lines_reference_same_tool(line_a: &str, line_b: &str) -> bool {
    for tool in KNOWN_TOOLS {
        if line_a.contains(tool) && line_b.contains(tool) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::contextops::config::{AgentRules, CoreRules, RulesSection};

    fn make_config(core_content: &str) -> RulesConfig {
        RulesConfig {
            rules: RulesSection {
                version: "1.0".to_string(),
                core: CoreRules {
                    content: core_content.to_string(),
                },
                agent: std::collections::HashMap::new(),
            },
        }
    }

    #[test]
    fn lint_severity_display() {
        assert_eq!(LintSeverity::Error.to_string(), "ERROR");
        assert_eq!(LintSeverity::Warning.to_string(), "WARNING");
        assert_eq!(LintSeverity::Info.to_string(), "INFO");
    }

    #[test]
    fn lint_empty_core() {
        let config = make_config("");
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        assert!(warnings.iter().any(|w| w.code == "EMPTY_CORE"));
    }

    #[test]
    fn lint_missing_sections() {
        let config = make_config("some rules without required sections");
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        let missing: Vec<_> = warnings
            .iter()
            .filter(|w| w.code == "MISSING_SECTION")
            .collect();
        assert!(!missing.is_empty());
    }

    #[test]
    fn lint_unknown_tool() {
        let config = make_config("## Mode Selection\n## File Editing\nUse ctx_nonexistent_tool");
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        assert!(warnings.iter().any(|w| w.code == "UNKNOWN_TOOL"));
    }

    #[test]
    fn lint_known_tools_pass() {
        let config = make_config("## Mode Selection\n## File Editing\nUse ctx_read and ctx_shell");
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        assert!(!warnings.iter().any(|w| w.code == "UNKNOWN_TOOL"));
    }

    #[test]
    fn lint_conflict_detection() {
        let mut config = make_config("## Mode Selection\n## File Editing\nALWAYS use ctx_read");
        config.rules.agent.insert(
            "test_agent".to_string(),
            AgentRules {
                extra: "NEVER use ctx_read".to_string(),
            },
        );
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        assert!(warnings.iter().any(|w| w.code == "CONFLICT"));
    }

    #[test]
    fn lint_no_version() {
        let mut config = make_config("## Mode Selection\n## File Editing\nrules");
        config.rules.version = String::new();
        let home = std::path::PathBuf::from("/tmp/fake_lint_test");
        let warnings = lint(&config, &home);
        assert!(warnings.iter().any(|w| w.code == "NO_VERSION"));
    }

    #[test]
    fn lines_reference_same_tool_true() {
        assert!(lines_reference_same_tool(
            "NEVER use ctx_read for context",
            "ALWAYS use ctx_read for editing"
        ));
    }

    #[test]
    fn lines_reference_same_tool_false() {
        assert!(!lines_reference_same_tool(
            "NEVER use ctx_read",
            "ALWAYS use ctx_shell"
        ));
    }
}
