use crate::core::intent_engine::{TaskClassification, TaskType, classify};

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
    format_briefing_with(briefing, true)
}

/// Render a briefing, optionally without its `OUTPUT-HINT:` line (#1763).
///
/// The hint tells the assistant how to *format its answer*. That is a
/// behaviour nudge, not retrieval output, so callers that surface a briefing
/// inside a tool result gate it on the same `behavior_nudges` config key the
/// nudge system honours — `off` must silence every answer-shaping directive,
/// not just the ones emitted post-dispatch.
pub fn format_briefing_with(briefing: &TaskBriefing, include_output_hint: bool) -> String {
    let mut parts = Vec::new();

    parts.push(format!(
        "[TASK:{} {}]",
        briefing.classification.task_type.as_str(),
        briefing.completeness_signal.as_str(),
    ));

    if include_output_hint {
        parts.push(briefing.output_instruction.to_string());
    }

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
pub mod tests {
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
        assert!(
            !briefing.lab_thinking_instruction.is_empty(),
            "`briefing.lab_thinking_instruction` is empty"
        );
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
    fn format_briefing_without_output_hint_keeps_classification_and_hints() {
        // #1763: with nudges off the answer-shaping directive is dropped, but
        // the task classification and the context hints still render.
        let files = vec![("src/core/entropy.rs".to_string(), 120)];
        let briefing = build_briefing("how does the entropy scorer work?", &files);
        let quiet = format_briefing_with(&briefing, false);
        assert!(quiet.contains("[TASK:"));
        assert!(quiet.contains("SCOPE:"));
        assert!(!quiet.contains("OUTPUT-HINT:"), "{quiet}");
        assert!(
            !briefing.context_hints.is_empty(),
            "explore tasks carry hints"
        );
        for hint in &briefing.context_hints {
            assert!(quiet.contains(hint), "hint '{hint}' must survive");
        }
        // The default rendering is unchanged.
        assert_eq!(
            format_briefing(&briefing),
            format_briefing_with(&briefing, true)
        );
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
                "Task '{task}' should be classified as '{expected_type}'",
            );
            let formatted = format_briefing(&briefing);
            assert!(formatted.contains("[TASK:"));
            assert!(formatted.contains("OUTPUT-HINT:"));
        }
    }
}
