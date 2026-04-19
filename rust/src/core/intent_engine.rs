#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

pub fn classify_complexity(
    query: &str,
    classification: &TaskClassification,
) -> super::adaptive::TaskComplexity {
    use super::adaptive::TaskComplexity;

    let q = query.to_lowercase();
    let word_count = q.split_whitespace().count();
    let target_count = classification.targets.len();

    let has_multi_file = target_count >= 3;
    let has_cross_cutting = q.contains("all files")
        || q.contains("across")
        || q.contains("everywhere")
        || q.contains("every")
        || q.contains("migration")
        || q.contains("architecture");

    let is_simple = word_count < 8
        && target_count <= 1
        && matches!(
            classification.task_type,
            TaskType::Generate | TaskType::Config
        );

    if is_simple {
        TaskComplexity::Mechanical
    } else if has_multi_file || has_cross_cutting {
        TaskComplexity::Architectural
    } else {
        TaskComplexity::Standard
    }
}

pub fn detect_multi_intent(query: &str) -> Vec<TaskClassification> {
    let delimiters = [" and then ", " then ", " also ", " + ", ". "];

    let mut parts: Vec<&str> = vec![query];
    for delim in &delimiters {
        let mut new_parts = Vec::new();
        for part in &parts {
            for sub in part.split(delim) {
                let trimmed = sub.trim();
                if !trimmed.is_empty() {
                    new_parts.push(trimmed);
                }
            }
        }
        parts = new_parts;
    }

    if parts.len() <= 1 {
        return vec![classify(query)];
    }

    parts.iter().map(|part| classify(part)).collect()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentScope {
    SingleFile,
    MultiFile,
    CrossModule,
    ProjectWide,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StructuredIntent {
    pub task_type: TaskType,
    pub confidence: f64,
    pub targets: Vec<String>,
    pub keywords: Vec<String>,
    pub scope: IntentScope,
    pub language_hint: Option<String>,
    pub urgency: f64,
    pub action_verb: Option<String>,
}

impl StructuredIntent {
    pub fn from_query(query: &str) -> Self {
        let classification = classify(query);
        let complexity = classify_complexity(query, &classification);
        let file_targets = classification
            .targets
            .iter()
            .filter(|t| t.contains('.') || t.contains('/'))
            .count();
        let scope = match complexity {
            super::adaptive::TaskComplexity::Mechanical => IntentScope::SingleFile,
            super::adaptive::TaskComplexity::Standard => {
                if file_targets > 1 {
                    IntentScope::MultiFile
                } else {
                    IntentScope::SingleFile
                }
            }
            super::adaptive::TaskComplexity::Architectural => {
                let q = query.to_lowercase();
                if q.contains("all files") || q.contains("everywhere") || q.contains("migration") {
                    IntentScope::ProjectWide
                } else {
                    IntentScope::CrossModule
                }
            }
        };

        let language_hint = detect_language_hint(query, &classification.targets);
        let urgency = detect_urgency(query);
        let action_verb = extract_action_verb(query);

        StructuredIntent {
            task_type: classification.task_type,
            confidence: classification.confidence,
            targets: classification.targets,
            keywords: classification.keywords,
            scope,
            language_hint,
            urgency,
            action_verb,
        }
    }

    pub fn from_query_with_session(query: &str, touched_files: &[String]) -> Self {
        let mut intent = Self::from_query(query);

        if intent.language_hint.is_none() && !touched_files.is_empty() {
            intent.language_hint = detect_language_from_files(touched_files);
        }

        if intent.scope == IntentScope::SingleFile && touched_files.len() > 3 {
            let dirs: std::collections::HashSet<&str> = touched_files
                .iter()
                .filter_map(|f| std::path::Path::new(f).parent()?.to_str())
                .collect();
            if dirs.len() > 2 {
                intent.scope = IntentScope::MultiFile;
            }
        }

        intent
    }

    pub fn format_header(&self) -> String {
        format!(
            "[TASK:{} SCOPE:{} CONF:{:.0}%{}{}]",
            self.task_type.as_str(),
            match self.scope {
                IntentScope::SingleFile => "single",
                IntentScope::MultiFile => "multi",
                IntentScope::CrossModule => "cross",
                IntentScope::ProjectWide => "project",
            },
            self.confidence * 100.0,
            self.language_hint
                .as_ref()
                .map(|l| format!(" LANG:{l}"))
                .unwrap_or_default(),
            if self.urgency > 0.5 { " URGENT" } else { "" },
        )
    }
}

fn detect_language_hint(query: &str, targets: &[String]) -> Option<String> {
    for t in targets {
        let ext = std::path::Path::new(t).extension().and_then(|e| e.to_str());
        match ext {
            Some("rs") => return Some("rust".into()),
            Some("ts" | "tsx") => return Some("typescript".into()),
            Some("js" | "jsx") => return Some("javascript".into()),
            Some("py") => return Some("python".into()),
            Some("go") => return Some("go".into()),
            Some("rb") => return Some("ruby".into()),
            Some("java") => return Some("java".into()),
            Some("swift") => return Some("swift".into()),
            Some("zig") => return Some("zig".into()),
            _ => {}
        }
    }

    let q = query.to_lowercase();
    let lang_keywords: &[(&str, &str)] = &[
        ("rust", "rust"),
        ("python", "python"),
        ("typescript", "typescript"),
        ("javascript", "javascript"),
        ("golang", "go"),
        (" go ", "go"),
        ("ruby", "ruby"),
        ("java ", "java"),
        ("swift", "swift"),
    ];
    for &(kw, lang) in lang_keywords {
        if q.contains(kw) {
            return Some(lang.into());
        }
    }

    None
}

fn detect_language_from_files(files: &[String]) -> Option<String> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in files {
        let ext = std::path::Path::new(f)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang = match ext {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "py" => "python",
            "go" => "go",
            "rb" => "ruby",
            "java" => "java",
            _ => continue,
        };
        *counts.entry(lang).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(l, _)| l.to_string())
}

fn detect_urgency(query: &str) -> f64 {
    let q = query.to_lowercase();
    let urgent_words = [
        "urgent",
        "asap",
        "immediately",
        "critical",
        "hotfix",
        "emergency",
        "blocker",
        "breaking",
    ];
    let hits = urgent_words.iter().filter(|w| q.contains(*w)).count();
    (hits as f64 * 0.4).min(1.0)
}

fn extract_action_verb(query: &str) -> Option<String> {
    let verbs = [
        "fix",
        "add",
        "create",
        "implement",
        "refactor",
        "debug",
        "test",
        "write",
        "update",
        "remove",
        "delete",
        "rename",
        "move",
        "extract",
        "split",
        "merge",
        "deploy",
        "review",
        "check",
        "build",
        "generate",
        "optimize",
        "clean",
    ];
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    for v in &verbs {
        if words.first() == Some(v) || words.get(1) == Some(v) {
            return Some(v.to_string());
        }
    }
    for v in &verbs {
        if words.contains(v) {
            return Some(v.to_string());
        }
    }
    None
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

    #[test]
    fn multi_intent_detection() {
        let results = detect_multi_intent("fix the bug in auth.rs and then write unit tests");
        assert!(results.len() >= 2);
        assert_eq!(results[0].task_type, TaskType::FixBug);
        assert_eq!(results[1].task_type, TaskType::Test);
    }

    #[test]
    fn single_intent_no_split() {
        let results = detect_multi_intent("fix the bug in auth.rs");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_type, TaskType::FixBug);
    }

    #[test]
    fn complexity_mechanical() {
        let r = classify("add a comment");
        let c = classify_complexity("add a comment", &r);
        assert_eq!(c, super::super::adaptive::TaskComplexity::Mechanical);
    }

    #[test]
    fn complexity_architectural() {
        let r = classify("refactor auth across all files and update the migration");
        let c = classify_complexity(
            "refactor auth across all files and update the migration",
            &r,
        );
        assert_eq!(c, super::super::adaptive::TaskComplexity::Architectural);
    }

    #[test]
    fn structured_intent_from_fixbug_query() {
        let intent = StructuredIntent::from_query("fix the NaN bug in entropy.rs");
        assert_eq!(intent.task_type, TaskType::FixBug);
        assert!(intent.targets.iter().any(|t| t.contains("entropy.rs")));
        assert_eq!(intent.language_hint.as_deref(), Some("rust"));
        assert_eq!(intent.action_verb.as_deref(), Some("fix"));
        assert_eq!(intent.scope, IntentScope::SingleFile);
    }

    #[test]
    fn structured_intent_project_wide() {
        let intent =
            StructuredIntent::from_query("refactor auth across all files and update migration");
        assert_eq!(intent.task_type, TaskType::Refactor);
        assert_eq!(intent.scope, IntentScope::ProjectWide);
    }

    #[test]
    fn structured_intent_urgency() {
        let normal = StructuredIntent::from_query("add a new function");
        let urgent = StructuredIntent::from_query("urgent hotfix for critical auth bug");
        assert!(urgent.urgency > normal.urgency);
        assert!(urgent.urgency >= 0.8);
    }

    #[test]
    fn structured_intent_language_from_targets() {
        let intent = StructuredIntent::from_query("fix main.py auth handler");
        assert_eq!(intent.language_hint.as_deref(), Some("python"));
    }

    #[test]
    fn structured_intent_with_session() {
        let files = vec![
            "src/core/session.rs".to_string(),
            "src/core/litm.rs".to_string(),
            "src/tools/ctx_read.rs".to_string(),
        ];
        let intent = StructuredIntent::from_query_with_session("how does this work?", &files);
        assert_eq!(intent.language_hint.as_deref(), Some("rust"));
    }

    #[test]
    fn structured_intent_header_format() {
        let intent = StructuredIntent::from_query("fix bug in entropy.rs");
        let header = intent.format_header();
        assert!(header.contains("TASK:fix_bug"));
        assert!(header.contains("SCOPE:"));
        assert!(header.contains("LANG:rust"));
    }

    #[test]
    fn detect_urgency_none() {
        assert_eq!(detect_urgency("add a function"), 0.0);
    }

    #[test]
    fn detect_urgency_multiple() {
        let u = detect_urgency("urgent critical blocker");
        assert!(u > 0.9);
    }

    #[test]
    fn action_verb_extraction() {
        assert_eq!(extract_action_verb("fix the bug"), Some("fix".to_string()));
        assert_eq!(
            extract_action_verb("please add logging"),
            Some("add".to_string())
        );
        assert_eq!(extract_action_verb("xyz qqq"), None);
    }
}
