use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskCategory {
    Coding,
    Debugging,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Knowledge,
    Architecture,
    Review,
    General,
}

impl TaskCategory {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            TaskCategory::Coding => "Coding",
            TaskCategory::Debugging => "Debugging",
            TaskCategory::Refactoring => "Refactoring",
            TaskCategory::Testing => "Testing",
            TaskCategory::Exploration => "Exploration",
            TaskCategory::Planning => "Planning",
            TaskCategory::Delegation => "Delegation",
            TaskCategory::Git => "Git",
            TaskCategory::BuildDeploy => "Build/Deploy",
            TaskCategory::Knowledge => "Knowledge",
            TaskCategory::Architecture => "Architecture",
            TaskCategory::Review => "Review",
            TaskCategory::General => "General",
        }
    }
}

pub struct TaskClassifier;

impl TaskClassifier {
    #[must_use]
    pub fn classify_tool(tool_name: &str) -> TaskCategory {
        let t = normalize(tool_name);
        match t.as_str() {
            "ctx_edit" | "ctx_fill" => TaskCategory::Refactoring,
            "ctx_read" | "ctx_multi_read" | "ctx_smart_read" | "ctx_delta" | "ctx_tree"
            | "ctx_search" | "ctx_outline" | "ctx_graph" | "ctx_callgraph" => {
                TaskCategory::Exploration
            }
            "ctx_semantic_search" | "ctx_architecture" | "ctx_impact" => TaskCategory::Architecture,
            "ctx_overview" | "ctx_preload" | "ctx_task" | "ctx_intent" | "ctx_workflow" => {
                TaskCategory::Planning
            }
            "ctx_handoff" | "ctx_agent" | "ctx_share" => TaskCategory::Delegation,
            "ctx_session" | "ctx_knowledge" | "ctx_compress_memory" => TaskCategory::Knowledge,
            "ctx_cost" | "ctx_gain" | "ctx_metrics" | "ctx_heatmap" => TaskCategory::Review,
            "ctx_shell" | "ctx_execute" => TaskCategory::Debugging,
            _ => TaskCategory::General,
        }
    }

    #[must_use]
    pub fn classify_command_key(cmd_key: &str) -> TaskCategory {
        let k = normalize(cmd_key);
        if k.is_empty() {
            return TaskCategory::General;
        }

        // lean-ctx records its own activity as "commands" too: MCP tool calls
        // (`ctx_*`) and CLI read-mode / hook keys (`cli_*`). Route those through the
        // tool/mode classifier so reads land in Exploration, edits in Refactoring, etc.
        // — instead of collapsing everything into General (the shell heuristics below
        // only understand real shell commands like git/cargo/grep).
        if k.starts_with("ctx_") {
            return Self::classify_tool(&k);
        }
        if let Some(mode) = k.strip_prefix("cli_") {
            return Self::classify_cli_mode(mode);
        }

        if k.starts_with("git ") || k == "git" {
            return TaskCategory::Git;
        }

        if k.starts_with("cargo ") {
            let sub = k.trim_start_matches("cargo ").trim();
            if matches!(sub, "test" | "nextest" | "llvm-cov" | "tarpaulin") {
                return TaskCategory::Testing;
            }
            if matches!(sub, "build" | "check" | "clippy" | "fmt" | "run" | "doc") {
                return TaskCategory::BuildDeploy;
            }
            return TaskCategory::BuildDeploy;
        }

        if k.contains("test") || k.contains("pytest") || k.contains("jest") || k.contains("vitest")
        {
            return TaskCategory::Testing;
        }
        if k.contains("build")
            || k.contains("deploy")
            || k.contains("docker")
            || k.contains("compose")
            || k.contains("kubectl")
            || k.contains("helm")
            || k.contains("terraform")
        {
            return TaskCategory::BuildDeploy;
        }
        if k.contains("lint") || k.contains("clippy") || k.contains("fmt") || k.contains("format") {
            return TaskCategory::BuildDeploy;
        }
        if k.contains("grep") || k.contains("rg") || k.contains("ripgrep") {
            return TaskCategory::Exploration;
        }

        TaskCategory::General
    }

    /// Classifies a CLI command key with the `cli_` prefix already stripped. These are the
    /// shell-hook compression modes: read/inspect modes and search map to Exploration; the
    /// catch-all `shell` bucket aggregates arbitrary shell commands, so it stays General.
    fn classify_cli_mode(mode: &str) -> TaskCategory {
        match mode {
            "grep" | "rg" | "ripgrep" | "search" | "find" | "ls" | "tree" => {
                TaskCategory::Exploration
            }
            "full" | "map" | "signatures" | "aggressive" | "entropy" | "diff" | "lines"
            | "reference" | "task" | "auto" | "outline" | "read" => TaskCategory::Exploration,
            _ => TaskCategory::General,
        }
    }
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_git() {
        assert_eq!(
            TaskClassifier::classify_command_key("git status"),
            TaskCategory::Git
        );
    }

    #[test]
    fn classify_tools() {
        assert_eq!(
            TaskClassifier::classify_tool("ctx_semantic_search"),
            TaskCategory::Architecture
        );
    }

    #[test]
    fn ctx_command_keys_route_through_tool_classifier() {
        // The regression behind the "everything is General" task breakdown: lean-ctx's own
        // tool/mode command keys must not collapse into General.
        assert_eq!(
            TaskClassifier::classify_command_key("ctx_search"),
            TaskCategory::Exploration
        );
        assert_eq!(
            TaskClassifier::classify_command_key("ctx_read"),
            TaskCategory::Exploration
        );
        assert_eq!(
            TaskClassifier::classify_command_key("ctx_edit"),
            TaskCategory::Refactoring
        );
        assert_eq!(
            TaskClassifier::classify_command_key("ctx_knowledge"),
            TaskCategory::Knowledge
        );
    }

    #[test]
    fn cli_mode_keys_are_classified() {
        assert_eq!(
            TaskClassifier::classify_command_key("cli_grep"),
            TaskCategory::Exploration
        );
        assert_eq!(
            TaskClassifier::classify_command_key("cli_full"),
            TaskCategory::Exploration
        );
        assert_eq!(
            TaskClassifier::classify_command_key("cli_signatures"),
            TaskCategory::Exploration
        );
        // The mixed shell bucket stays General (it aggregates arbitrary commands).
        assert_eq!(
            TaskClassifier::classify_command_key("cli_shell"),
            TaskCategory::General
        );
    }

    #[test]
    fn real_shell_commands_still_classify() {
        assert_eq!(
            TaskClassifier::classify_command_key("cargo build"),
            TaskCategory::BuildDeploy
        );
        assert_eq!(
            TaskClassifier::classify_command_key("git status"),
            TaskCategory::Git
        );
    }
}
