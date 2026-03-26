#![allow(dead_code)]
use crate::core::cache::SessionCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    Mechanical,
    Standard,
    Architectural,
}

impl TaskComplexity {
    pub fn instruction_suffix(&self) -> &'static str {
        match self {
            TaskComplexity::Mechanical => {
                "TASK COMPLEXITY: mechanical\n\
                 Minimal reasoning needed. Act immediately, report result in one line."
            }
            TaskComplexity::Standard => {
                "TASK COMPLEXITY: standard\n\
                 Brief reasoning allowed. Summarize approach in 1-2 lines, then act."
            }
            TaskComplexity::Architectural => {
                "TASK COMPLEXITY: architectural\n\
                 Full reasoning expected. Outline approach, consider edge cases, then act."
            }
        }
    }

    pub fn encoded_suffix(&self) -> String {
        use crate::core::protocol::encode_instructions;
        match self {
            TaskComplexity::Mechanical => encode_instructions("mechanical"),
            TaskComplexity::Standard => encode_instructions("standard"),
            TaskComplexity::Architectural => encode_instructions("architectural"),
        }
    }

    fn complexity_label(&self) -> &'static str {
        match self {
            TaskComplexity::Mechanical => "mechanical",
            TaskComplexity::Standard => "standard",
            TaskComplexity::Architectural => "architectural",
        }
    }
}

pub fn classify_from_context(cache: &SessionCache) -> TaskComplexity {
    let stats = cache.get_stats();
    let unique_files = cache.get_all_entries().len();
    let total_reads = stats.total_reads;

    if unique_files <= 1 && total_reads <= 3 {
        return TaskComplexity::Mechanical;
    }

    if unique_files >= 5 || total_reads >= 15 {
        return TaskComplexity::Architectural;
    }

    TaskComplexity::Standard
}

pub fn classify_from_signals(
    file_count: usize,
    has_tests: bool,
    has_multi_lang: bool,
) -> TaskComplexity {
    if has_tests && file_count >= 5 {
        return TaskComplexity::Architectural;
    }

    if has_multi_lang || file_count >= 3 {
        return TaskComplexity::Standard;
    }

    TaskComplexity::Mechanical
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mechanical_classification() {
        let result = classify_from_signals(1, false, false);
        assert_eq!(result, TaskComplexity::Mechanical);
    }

    #[test]
    fn test_standard_classification() {
        let result = classify_from_signals(3, false, false);
        assert_eq!(result, TaskComplexity::Standard);
    }

    #[test]
    fn test_architectural_classification() {
        let result = classify_from_signals(5, true, false);
        assert_eq!(result, TaskComplexity::Architectural);
    }

    #[test]
    fn test_multi_lang_triggers_standard() {
        let result = classify_from_signals(1, false, true);
        assert_eq!(result, TaskComplexity::Standard);
    }

    #[test]
    fn test_instruction_suffix_not_empty() {
        assert!(!TaskComplexity::Mechanical.instruction_suffix().is_empty());
        assert!(!TaskComplexity::Standard.instruction_suffix().is_empty());
        assert!(!TaskComplexity::Architectural
            .instruction_suffix()
            .is_empty());
    }

    #[test]
    fn test_context_based_mechanical() {
        let cache = SessionCache::new();
        let result = classify_from_context(&cache);
        assert_eq!(result, TaskComplexity::Mechanical);
    }
}
