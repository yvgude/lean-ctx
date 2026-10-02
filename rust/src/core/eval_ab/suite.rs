//! Eval suite + fixtures (#233): the deterministic task definitions an A/B run scores.
//!
//! A *suite* is an NDJSON file (one [`Task`] per line, `#`-comments + blank lines allowed).
//! Each task carries everything the harness needs to (a) assemble context from a workspace,
//! (b) prompt the pinned model, and (c) score the answer objectively. Two domains are
//! supported today: free-form [`Domain::Qa`] (scored with EM / F1 / containment) and
//! [`Domain::Code`] (scored by running a unit-test command against the model output).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::core::benchmark_spec::types::{is_safe_relative_file, is_safe_shell_test};

/// What kind of task this is — selects the scorer and how the model output is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// Retrieval-augmented question answering, scored with EM / F1 / containment.
    Qa,
    /// Code task, scored by running a unit-test command against the model's output.
    Code,
}

impl Domain {
    /// Stable lowercase label used in digests and reports.
    pub fn label(self) -> &'static str {
        match self {
            Domain::Qa => "qa",
            Domain::Code => "code",
        }
    }
}

fn is_safe_relative_workspace(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && !value.as_bytes().contains(&0)
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// One scored unit of work. Fixtures are stored as NDJSON (one task per line) so suites are
/// diff-friendly and stream without loading the whole file into a single JSON value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable, unique identifier (used in reports + the determinism digest).
    pub id: String,
    /// Selects the scorer and the meaning of the remaining fields.
    pub domain: Domain,
    /// Optional evaluation category, added without breaking existing suite files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    /// The instruction shown to the model (the "user turn").
    pub prompt: String,
    /// Repo / corpus directory the context is assembled from. Safe relative paths resolve
    /// against the suite file's parent directory.
    pub workspace: String,
    /// Query used to retrieve context in the lean-ctx condition. Defaults to `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_query: Option<String>,

    // --- Domain::Qa --------------------------------------------------------
    /// Accepted gold answers. Any match counts (EM/F1 take the best over this set).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,

    // --- Domain::Code ------------------------------------------------------
    /// File inside a sandbox copy of `workspace` that the model output replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_file: Option<String>,
    /// Shell command run inside the sandbox; exit code 0 = pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_cmd: Option<String>,
}

impl Task {
    /// The retrieval query for the lean-ctx condition (falls back to the prompt).
    pub fn query(&self) -> &str {
        self.retrieval_query.as_deref().unwrap_or(&self.prompt)
    }

    /// Absolute workspace directory, resolved against `suite_dir` for relative paths.
    pub fn workspace_path(&self, suite_dir: &Path) -> PathBuf {
        let p = Path::new(&self.workspace);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            suite_dir.join(p)
        }
    }

    /// Canonical workspace constrained to the suite directory.
    pub(crate) fn resolve_workspace_path(&self, suite_dir: &Path) -> Result<PathBuf> {
        if let Err(reason) = self.validate() {
            bail!("invalid task {}: {reason}", self.id);
        }
        let root = std::fs::canonicalize(suite_dir)
            .with_context(|| format!("canonicalizing suite directory {}", suite_dir.display()))?;
        let workspace = std::fs::canonicalize(root.join(&self.workspace))
            .with_context(|| format!("canonicalizing workspace {}", self.workspace))?;
        if !workspace.starts_with(&root) {
            bail!(
                "task {}: workspace {} escapes suite directory",
                self.id,
                self.workspace
            );
        }
        if !workspace.is_dir() {
            bail!(
                "task {}: workspace {} is not a directory",
                self.id,
                workspace.display()
            );
        }
        Ok(workspace)
    }

    /// Validates the per-domain invariants. Returns a human-readable reason on failure.
    pub(crate) fn validate(&self) -> std::result::Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("task id is empty".into());
        }
        if self
            .task_class
            .as_deref()
            .is_some_and(|class| class.trim().is_empty())
        {
            return Err(format!("task {}: task_class is empty", self.id));
        }
        if self.prompt.trim().is_empty() {
            return Err(format!("task {}: prompt is empty", self.id));
        }
        if !is_safe_relative_workspace(&self.workspace) {
            return Err(format!(
                "task {}: workspace must be a safe relative path",
                self.id
            ));
        }
        match self.domain {
            Domain::Qa => {
                if self.answers.iter().all(|a| a.trim().is_empty()) {
                    return Err(format!(
                        "task {}: qa task has no non-empty answers",
                        self.id
                    ));
                }
            }
            Domain::Code => self.validate_code_evaluation()?,
        }
        Ok(())
    }

    /// Validates code-evaluator fields independently of suite-relative workspace parsing.
    pub(crate) fn validate_code_evaluation(&self) -> std::result::Result<(), String> {
        if self.domain != Domain::Code {
            return Err(format!(
                "task {}: code evaluator requires a code task",
                self.id
            ));
        }
        if self.target_file.as_deref().unwrap_or("").trim().is_empty() {
            return Err(format!("task {}: code task needs target_file", self.id));
        }
        if !self
            .target_file
            .as_deref()
            .is_some_and(is_safe_relative_file)
        {
            return Err(format!(
                "task {}: target_file must be a safe relative file path",
                self.id
            ));
        }
        if self.test_cmd.as_deref().unwrap_or("").trim().is_empty() {
            return Err(format!("task {}: code task needs test_cmd", self.id));
        }
        if !self.test_cmd.as_deref().is_some_and(is_safe_shell_test) {
            return Err(format!(
                "task {}: test_cmd must be exactly `sh <relative .sh file>`",
                self.id
            ));
        }
        Ok(())
    }
}

/// A loaded, validated suite: the tasks plus the directory used to resolve relative workspaces.
#[derive(Debug, Clone)]
pub struct EvalSuite {
    /// Directory of the suite file (the resolution root for relative workspaces).
    pub dir: PathBuf,
    /// Validated tasks in file order.
    pub tasks: Vec<Task>,
}

impl EvalSuite {
    /// Parses + validates an NDJSON suite file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading suite {}", path.display()))?;
        let dir = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self::parse(&raw, dir)
    }

    /// Pure parser (testable without touching disk for the suite body itself).
    pub fn parse(raw: &str, dir: PathBuf) -> Result<Self> {
        let mut tasks = Vec::new();
        for (lineno, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let task: Task = serde_json::from_str(trimmed)
                .with_context(|| format!("parsing task on line {}", lineno + 1))?;
            if let Err(reason) = task.validate() {
                bail!("invalid task on line {}: {reason}", lineno + 1);
            }
            tasks.push(task);
        }
        if tasks.is_empty() {
            bail!("suite contains no tasks");
        }
        // Unique ids keep the determinism digest unambiguous.
        let mut seen = std::collections::HashSet::new();
        for t in &tasks {
            if !seen.insert(t.id.as_str()) {
                bail!("duplicate task id: {}", t.id);
            }
        }
        Ok(Self { dir, tasks })
    }

    /// Revalidates a suite assembled by callers instead of parsed from NDJSON.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.tasks.is_empty() {
            bail!("suite contains no tasks");
        }
        let mut seen = std::collections::HashSet::new();
        for task in &self.tasks {
            if let Err(reason) = task.validate() {
                bail!("invalid task: {reason}");
            }
            if !seen.insert(task.id.as_str()) {
                bail!("duplicate task id: {}", task.id);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qa_line() -> &'static str {
        r#"{"id":"q1","domain":"qa","prompt":"What stores does consolidation write to?","workspace":"corpus","answers":["bm25, graph, knowledge, session"]}"#
    }

    fn code_line() -> &'static str {
        r#"{"id":"c1","domain":"code","prompt":"Implement add","workspace":"code","target_file":"solution.sh","test_cmd":"sh test.sh"}"#
    }

    #[test]
    fn parses_qa_and_code_skipping_comments_and_blanks() {
        let raw = format!("# header\n\n{}\n{}\n", qa_line(), code_line());
        let suite = EvalSuite::parse(&raw, PathBuf::from("/suites")).unwrap();
        assert_eq!(suite.tasks.len(), 2);
        assert_eq!(suite.tasks[0].domain, Domain::Qa);
        assert_eq!(suite.tasks[1].domain, Domain::Code);
        assert_eq!(suite.tasks[0].query(), suite.tasks[0].prompt);
        assert_eq!(suite.tasks[0].task_class, None);

        let classified = r#"{"id":"q2","domain":"qa","task_class":"needle_retrieval","prompt":"Find the owner","workspace":"corpus","answers":["registry"]}"#;
        let classified = EvalSuite::parse(classified, PathBuf::from("/suites")).unwrap();
        assert_eq!(
            classified.tasks[0].task_class.as_deref(),
            Some("needle_retrieval")
        );
    }

    #[test]
    fn relative_workspace_resolves_against_suite_dir() {
        let suite = EvalSuite::parse(qa_line(), PathBuf::from("/suites")).unwrap();
        assert_eq!(
            suite.tasks[0].workspace_path(&suite.dir),
            PathBuf::from("/suites/corpus")
        );
    }

    #[test]
    fn rejects_qa_without_answers() {
        let bad = r#"{"id":"q","domain":"qa","prompt":"p","workspace":"w"}"#;
        assert!(EvalSuite::parse(bad, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_code_without_test_cmd() {
        let bad = r#"{"id":"c","domain":"code","prompt":"p","workspace":"w","target_file":"f"}"#;
        assert!(EvalSuite::parse(bad, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let raw = format!("{}\n{}", qa_line(), qa_line());
        assert!(EvalSuite::parse(&raw, PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_empty_suite() {
        assert!(EvalSuite::parse("# only comments\n\n", PathBuf::from(".")).is_err());
    }

    #[test]
    fn rejects_unsafe_workload_and_code_paths() {
        for workspace in ["/tmp/outside", "../outside", "nested/../../outside"] {
            let raw = format!(
                r#"{{"id":"q","domain":"qa","prompt":"p","workspace":"{workspace}","answers":["a"]}}"#
            );
            assert!(EvalSuite::parse(&raw, PathBuf::from(".")).is_err());
        }
        for target_file in [
            "/tmp/solution.sh",
            "../solution.sh",
            "nested/../../solution.sh",
        ] {
            let raw = format!(
                r#"{{"id":"c","domain":"code","prompt":"p","workspace":".","target_file":"{target_file}","test_cmd":"sh test.sh"}}"#
            );
            assert!(EvalSuite::parse(&raw, PathBuf::from(".")).is_err());
        }
        for test_cmd in [
            "sh ../test.sh",
            "sh test.sh; touch /tmp/pwned",
            "cat /etc/passwd",
        ] {
            let raw = format!(
                r#"{{"id":"c","domain":"code","prompt":"p","workspace":".","target_file":"solution.sh","test_cmd":"{test_cmd}"}}"#
            );
            assert!(EvalSuite::parse(&raw, PathBuf::from(".")).is_err());
        }
    }

    #[test]
    fn committed_task_class_corpus_parses_without_gold_in_task_metadata() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("eval/quality-suite.ndjson");
        // A mechanism fixture: one synthetic task per class. It proves the harness and
        // the gold-leak guard, never quality.
        let suite = EvalSuite::load(&path).expect("committed task-class suite must parse");
        let expected = [
            "needle_retrieval",
            "long_context_qa",
            "structured_data",
            "code_understanding",
            "test_failure_diagnosis",
            "compiler_output_interpretation",
            "shell_log_analysis",
            "config_reasoning",
            "dependency_tracing",
            "security_relevant_reasoning",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        let actual = suite
            .tasks
            .iter()
            .filter_map(|task| task.task_class.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        let normalize = |value: &str| {
            value
                .chars()
                .filter(|character| character.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        };

        for task in &suite.tasks {
            task.resolve_workspace_path(&suite.dir)
                .unwrap_or_else(|error| panic!("resolve {}: {error:#}", task.id));
            for answer in &task.answers {
                let gold = normalize(answer);
                if gold.is_empty() {
                    continue;
                }
                let metadata = [
                    task.id.as_str(),
                    task.prompt.as_str(),
                    task.query(),
                    task.task_class.as_deref().unwrap_or_default(),
                ];
                for field in metadata {
                    assert!(
                        !normalize(field).contains(&gold),
                        "gold leaked into task metadata for {}",
                        task.id
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn canonical_workspace_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let suite_root = root.path().join("suite");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&suite_root).unwrap();
        std::os::unix::fs::symlink(outside.path(), suite_root.join("link")).unwrap();
        let raw = r#"{"id":"q","domain":"qa","prompt":"p","workspace":"link","answers":["a"]}"#;
        let suite = EvalSuite::parse(raw, suite_root.clone()).unwrap();
        assert!(suite.tasks[0].resolve_workspace_path(&suite.dir).is_err());
    }

    #[test]
    fn canonical_workspace_preserves_dot_fixture() {
        let root = tempfile::tempdir().unwrap();
        let raw = r#"{"id":"q","domain":"qa","prompt":"p","workspace":".","answers":["a"]}"#;
        let suite = EvalSuite::parse(raw, root.path().to_path_buf()).unwrap();
        assert_eq!(
            suite.tasks[0].resolve_workspace_path(&suite.dir).unwrap(),
            std::fs::canonicalize(root.path()).unwrap()
        );
    }
}
