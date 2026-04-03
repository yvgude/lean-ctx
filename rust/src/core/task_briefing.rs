use crate::core::intent_engine::{classify, TaskClassification, TaskType};

#[derive(Debug)]
pub struct TaskBriefing {
    pub classification: TaskClassification,
    pub completeness_signal: CompletenessSignal,
    pub output_instruction: &'static str,
    pub context_hints: Vec<String>,
    /// Lab-only: thinking instruction for direct LLM API calls.
    /// NEVER inject into MCP tool outputs — would override user's model thinking behavior.
    pub lab_thinking_instruction: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum CompletenessSignal {
    SingleFile,
    MultiFile,
    CrossModule,
    Unknown,
}

impl CompletenessSignal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleFile => "SCOPE:single-file",
            Self::MultiFile => "SCOPE:multi-file",
            Self::CrossModule => "SCOPE:cross-module",
            Self::Unknown => "SCOPE:unknown",
        }
    }
}

pub fn build_briefing(task: &str, file_context: &[(String, usize)]) -> TaskBriefing {
    let classification = classify(task);

    let completeness = estimate_completeness(&classification, file_context);
    let context_hints = build_context_hints(&classification, file_context);

    let output_instruction = classification.task_type.output_format().instruction();
    let lab_thinking_instruction = classification.task_type.thinking_budget().instruction();

    TaskBriefing {
        classification,
        completeness_signal: completeness,
        output_instruction,
        context_hints,
        lab_thinking_instruction,
    }
}

fn estimate_completeness(
    classification: &TaskClassification,
    file_context: &[(String, usize)],
) -> CompletenessSignal {
    if file_context.is_empty() {
        return CompletenessSignal::Unknown;
    }

    let unique_dirs: std::collections::HashSet<&str> = file_context
        .iter()
        .filter_map(|(path, _)| std::path::Path::new(path).parent().and_then(|p| p.to_str()))
        .collect();

    if classification.targets.len() <= 1 && unique_dirs.len() <= 1 {
        CompletenessSignal::SingleFile
    } else if unique_dirs.len() <= 3 {
        CompletenessSignal::MultiFile
    } else {
        CompletenessSignal::CrossModule
    }
}

fn build_context_hints(
    classification: &TaskClassification,
    file_context: &[(String, usize)],
) -> Vec<String> {
    let mut hints = Vec::new();

    match classification.task_type {
        TaskType::Generate => {
            hints.push("Pattern: match existing code style in context".to_string());
            if !classification.targets.is_empty() {
                hints.push(format!(
                    "Insert near: {}",
                    classification.targets.join(", ")
                ));
            }
        }
        TaskType::FixBug => {
            hints.push("Focus: identify root cause, minimal fix".to_string());
            if let Some(largest) = file_context.iter().max_by_key(|(_, lines)| *lines) {
                hints.push(format!("Primary file: {} ({}L)", largest.0, largest.1));
            }
        }
        TaskType::Refactor => {
            hints.push("Preserve: all public APIs and behavior".to_string());
            hints.push(format!("Files in scope: {}", file_context.len()));
        }
        TaskType::Explore => {
            hints.push("Depth: signatures + key logic, skip boilerplate".to_string());
        }
        TaskType::Test => {
            hints.push("Pattern: follow existing test patterns in codebase".to_string());
        }
        TaskType::Debug => {
            hints.push("Trace: follow data flow through call chain".to_string());
        }
        _ => {}
    }

    hints
}

pub fn format_briefing(briefing: &TaskBriefing) -> String {
    let mut parts = Vec::new();

    parts.push(format!(
        "[TASK:{} {}]",
        briefing.classification.task_type.as_str(),
        briefing.completeness_signal.as_str(),
    ));

    parts.push(briefing.output_instruction.to_string());

    if !briefing.context_hints.is_empty() {
        for hint in &briefing.context_hints {
            parts.push(format!("• {hint}"));
        }
    }

    parts.join("\n")
}

pub fn inject_into_instructions(base_instructions: &str, task: &str) -> String {
    if task.trim().is_empty() {
        return base_instructions.to_string();
    }

    let file_context: Vec<(String, usize)> = Vec::new();
    let briefing = build_briefing(task, &file_context);
    let briefing_block = format_briefing(&briefing);

    format!("{base_instructions}\n\n{briefing_block}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn briefing_for_generate_task() {
        let files = vec![("src/core/entropy.rs".to_string(), 120)];
        let briefing = build_briefing("add normalized_token_entropy to entropy.rs", &files);
        assert_eq!(briefing.classification.task_type, TaskType::Generate);
        assert!(briefing.output_instruction.contains("code blocks"));
        assert!(briefing.lab_thinking_instruction.contains("Skip analysis"));
    }

    #[test]
    fn briefing_for_fix_bug() {
        let files = vec![
            ("src/core/entropy.rs".to_string(), 200),
            ("src/core/tokens.rs".to_string(), 50),
        ];
        let briefing = build_briefing("fix the NaN bug in token_entropy", &files);
        assert_eq!(briefing.classification.task_type, TaskType::FixBug);
        assert!(briefing.output_instruction.contains("changed lines"));
        assert!(!briefing.lab_thinking_instruction.is_empty());
    }

    #[test]
    fn completeness_single_file() {
        let files = vec![("src/core/entropy.rs".to_string(), 200)];
        let briefing = build_briefing("add a function", &files);
        matches!(briefing.completeness_signal, CompletenessSignal::SingleFile);
    }

    #[test]
    fn completeness_cross_module() {
        let files = vec![
            ("src/core/a.rs".to_string(), 100),
            ("src/tools/b.rs".to_string(), 100),
            ("src/server.rs".to_string(), 100),
            ("tests/integration.rs".to_string(), 100),
        ];
        let briefing = build_briefing("refactor compression pipeline", &files);
        matches!(
            briefing.completeness_signal,
            CompletenessSignal::CrossModule
        );
    }

    #[test]
    fn format_briefing_includes_all_sections() {
        let files = vec![("src/core/entropy.rs".to_string(), 120)];
        let briefing = build_briefing("fix bug in entropy.rs", &files);
        let formatted = format_briefing(&briefing);
        assert!(formatted.contains("[TASK:"));
        assert!(formatted.contains("OUTPUT-HINT:"));
        assert!(formatted.contains("SCOPE:"));
    }

    #[test]
    fn inject_empty_task_unchanged() {
        let base = "some instructions";
        let result = inject_into_instructions(base, "");
        assert_eq!(result, base);
    }

    #[test]
    fn briefing_covers_all_task_types() {
        let scenarios: &[(&str, &str)] = &[
            ("add a new function to entropy.rs", "generate"),
            ("fix the bug in token_optimizer.rs", "fix_bug"),
            ("how does the session cache work?", "explore"),
            ("refactor compression pipeline", "refactor"),
            ("write unit tests for entropy", "test"),
            ("debug why compression ratio drops", "debug"),
        ];
        for &(task, expected_type) in scenarios {
            let briefing = build_briefing(task, &[("src/main.rs".to_string(), 100)]);
            assert_eq!(
                briefing.classification.task_type.as_str(),
                expected_type,
                "Task '{}' should be classified as '{}'",
                task,
                expected_type,
            );
            let formatted = format_briefing(&briefing);
            assert!(formatted.contains("[TASK:"));
            assert!(formatted.contains("OUTPUT-HINT:"));
        }
    }
}
