#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    Generate,
    FixBug,
    Refactor,
    Explore,
    Test,
    Debug,
    Config,
    Deploy,
    Review,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::FixBug => "fix_bug",
            Self::Refactor => "refactor",
            Self::Explore => "explore",
            Self::Test => "test",
            Self::Debug => "debug",
            Self::Config => "config",
            Self::Deploy => "deploy",
            Self::Review => "review",
        }
    }

    pub fn thinking_budget(&self) -> ThinkingBudget {
        match self {
            Self::Generate => ThinkingBudget::Minimal,
            Self::FixBug => ThinkingBudget::Minimal,
            Self::Refactor => ThinkingBudget::Medium,
            Self::Explore => ThinkingBudget::Medium,
            Self::Test => ThinkingBudget::Minimal,
            Self::Debug => ThinkingBudget::Medium,
            Self::Config => ThinkingBudget::Minimal,
            Self::Deploy => ThinkingBudget::Minimal,
            Self::Review => ThinkingBudget::Medium,
        }
    }

    pub fn output_format(&self) -> OutputFormat {
        match self {
            Self::Generate => OutputFormat::CodeOnly,
            Self::FixBug => OutputFormat::DiffOnly,
            Self::Refactor => OutputFormat::DiffOnly,
            Self::Explore => OutputFormat::ExplainConcise,
            Self::Test => OutputFormat::CodeOnly,
            Self::Debug => OutputFormat::Trace,
            Self::Config => OutputFormat::CodeOnly,
            Self::Deploy => OutputFormat::StepList,
            Self::Review => OutputFormat::ExplainConcise,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingBudget {
    Minimal,
    Medium,
    Trace,
    Deep,
}

impl ThinkingBudget {
    pub fn instruction(&self) -> &'static str {
        match self {
            Self::Minimal => "THINKING: Skip analysis. The task is clear — generate code directly.",
            Self::Medium => "THINKING: 2-3 step analysis max. Identify what to change, then act. Do not over-analyze.",
            Self::Trace => "THINKING: Short trace only. Identify root cause in 3 steps max, then generate fix.",
            Self::Deep => "THINKING: Analyze structure and dependencies. Summarize findings concisely.",
        }
    }

    pub fn suppresses_thinking(&self) -> bool {
        matches!(self, Self::Minimal)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    CodeOnly,
    DiffOnly,
    ExplainConcise,
    Trace,
    StepList,
}

impl OutputFormat {
    pub fn instruction(&self) -> &'static str {
        match self {
            Self::CodeOnly => {
                "OUTPUT-HINT: Prefer code blocks. Minimize prose unless user asks for explanation."
            }
            Self::DiffOnly => "OUTPUT-HINT: Prefer showing only changed lines as +/- diffs.",
            Self::ExplainConcise => "OUTPUT-HINT: Brief summary, then code/data if relevant.",
            Self::Trace => "OUTPUT-HINT: Show cause→effect chain with code references.",
            Self::StepList => "OUTPUT-HINT: Numbered action list, one step at a time.",
        }
    }
}

#[derive(Debug)]
pub struct TaskClassification {
    pub task_type: TaskType,
    pub confidence: f64,
    pub targets: Vec<String>,
    pub keywords: Vec<String>,
}

const PHRASE_RULES: &[(&[&str], TaskType, f64)] = &[
    (
        &[
            "add",
            "create",
            "implement",
            "build",
            "write",
            "generate",
            "make",
            "new feature",
            "new",
        ],
        TaskType::Generate,
        0.9,
    ),
    (
        &[
            "fix",
            "bug",
            "broken",
            "crash",
            "error in",
            "not working",
            "fails",
            "wrong output",
        ],
        TaskType::FixBug,
        0.95,
    ),
    (
        &[
            "refactor",
            "clean up",
            "restructure",
            "rename",
            "move",
            "extract",
            "simplify",
            "split",
        ],
        TaskType::Refactor,
        0.9,
    ),
    (
        &[
            "how",
            "what",
            "where",
            "explain",
            "understand",
            "show me",
            "describe",
            "why does",
        ],
        TaskType::Explore,
        0.85,
    ),
    (
        &[
            "test",
            "spec",
            "coverage",
            "assert",
            "unit test",
            "integration test",
            "mock",
        ],
        TaskType::Test,
        0.9,
    ),
    (
        &[
            "debug",
            "trace",
            "inspect",
            "log",
            "breakpoint",
            "step through",
            "stack trace",
        ],
        TaskType::Debug,
        0.9,
    ),
    (
        &[
            "config",
            "setup",
            "install",
            "env",
            "configure",
            "settings",
            "dotenv",
        ],
        TaskType::Config,
        0.85,
    ),
    (
        &[
            "deploy", "release", "publish", "ship", "ci/cd", "pipeline", "docker",
        ],
        TaskType::Deploy,
        0.85,
    ),
    (
        &[
            "review",
            "check",
            "audit",
            "look at",
            "evaluate",
            "assess",
            "pr review",
        ],
        TaskType::Review,
        0.8,
    ),
];

pub fn classify(query: &str) -> TaskClassification {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();

    let mut best_type = TaskType::Explore;
    let mut best_score = 0.0_f64;

    for &(phrases, task_type, base_confidence) in PHRASE_RULES {
        let mut match_count = 0usize;
        for phrase in phrases {
            if phrase.contains(' ') {
                if q.contains(phrase) {
                    match_count += 2;
                }
            } else if words.contains(phrase) {
                match_count += 1;
            }
        }
        if match_count > 0 {
            let score = base_confidence * (match_count as f64).min(2.0) / 2.0;
            if score > best_score {
                best_score = score;
                best_type = task_type;
            }
        }
    }

    let targets = extract_targets(query);
    let keywords = extract_keywords(&q);

    if best_score < 0.1 {
        best_type = TaskType::Explore;
        best_score = 0.3;
    }

    TaskClassification {
        task_type: best_type,
        confidence: best_score,
        targets,
        keywords,
    }
}

fn extract_targets(query: &str) -> Vec<String> {
    let mut targets = Vec::new();

    for word in query.split_whitespace() {
        if word.contains('.') && !word.starts_with('.') {
            let clean = word.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
            });
            if looks_like_path(clean) {
                targets.push(clean.to_string());
            }
        }
        if word.contains('/') && !word.starts_with("//") && !word.starts_with("http") {
            let clean = word.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-'
            });
            if clean.len() > 2 {
                targets.push(clean.to_string());
            }
        }
    }

    for word in query.split_whitespace() {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if w.contains('_') && w.len() > 3 && !targets.contains(&w.to_string()) {
            targets.push(w.to_string());
        }
        if w.chars().any(|c| c.is_uppercase())
            && w.len() > 2
            && !is_stop_word(w)
            && !targets.contains(&w.to_string())
        {
            targets.push(w.to_string());
        }
    }

    targets.truncate(5);
    targets
}

fn looks_like_path(s: &str) -> bool {
    let exts = [
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".toml", ".yaml", ".yml", ".json", ".md",
    ];
    exts.iter().any(|ext| s.ends_with(ext)) || s.contains('/')
}

fn is_stop_word(w: &str) -> bool {
    matches!(
        w.to_lowercase().as_str(),
        "the"
            | "this"
            | "that"
            | "with"
            | "from"
            | "into"
            | "have"
            | "please"
            | "could"
            | "would"
            | "should"
            | "also"
            | "just"
            | "then"
            | "when"
            | "what"
            | "where"
            | "which"
            | "there"
            | "here"
            | "these"
            | "those"
            | "does"
            | "will"
            | "shall"
            | "can"
            | "may"
            | "must"
            | "need"
            | "want"
            | "like"
            | "make"
            | "take"
    )
}

fn extract_keywords(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .filter(|w| !is_stop_word(w))
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .take(8)
        .collect()
}

pub fn format_briefing_header(classification: &TaskClassification) -> String {
    format!(
        "[TASK:{} CONF:{:.0}% TARGETS:{} KW:{}]",
        classification.task_type.as_str(),
        classification.confidence * 100.0,
        if classification.targets.is_empty() {
            "-".to_string()
        } else {
            classification.targets.join(",")
        },
        classification.keywords.join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_fix_bug() {
        let r = classify("fix the bug in entropy.rs where token_entropy returns NaN");
        assert_eq!(r.task_type, TaskType::FixBug);
        assert!(r.confidence > 0.5);
        assert!(r.targets.iter().any(|t| t.contains("entropy.rs")));
    }

    #[test]
    fn classify_generate() {
        let r = classify("add a new function normalized_token_entropy to entropy.rs");
        assert_eq!(r.task_type, TaskType::Generate);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn classify_refactor() {
        let r = classify("refactor the compression pipeline to split into smaller modules");
        assert_eq!(r.task_type, TaskType::Refactor);
    }

    #[test]
    fn classify_explore() {
        let r = classify("how does the session cache work?");
        assert_eq!(r.task_type, TaskType::Explore);
    }

    #[test]
    fn classify_debug() {
        let r = classify("debug why the compression ratio drops for large files");
        assert_eq!(r.task_type, TaskType::Debug);
    }

    #[test]
    fn classify_test() {
        let r = classify("write unit tests for the token_optimizer module");
        assert_eq!(r.task_type, TaskType::Test);
    }

    #[test]
    fn targets_extract_paths() {
        let r = classify("fix entropy.rs and update core/mod.rs");
        assert!(r.targets.iter().any(|t| t.contains("entropy.rs")));
        assert!(r.targets.iter().any(|t| t.contains("core/mod.rs")));
    }

    #[test]
    fn targets_extract_identifiers() {
        let r = classify("refactor SessionCache to use LRU eviction");
        assert!(r.targets.iter().any(|t| t == "SessionCache"));
    }

    #[test]
    fn fallback_to_explore() {
        let r = classify("xyz qqq bbb");
        assert_eq!(r.task_type, TaskType::Explore);
        assert!(r.confidence < 0.5);
    }
}
