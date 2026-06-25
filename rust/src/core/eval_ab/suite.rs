//! Eval suite + fixtures (#233): the deterministic task definitions an A/B run scores.
//!
//! A *suite* is an NDJSON file (one [`Task`] per line, `#`-comments + blank lines allowed).
//! Each task carries everything the harness needs to (a) assemble context from a workspace,
//! (b) prompt the pinned model, and (c) score the answer objectively. Two domains are
//! supported today: free-form [`Domain::Qa`] (scored with EM / F1 / containment) and
//! [`Domain::Code`] (scored by running a unit-test command against the model output).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Domain::Qa => "qa",
            Domain::Code => "code",
        }
    }
}

/// One scored unit of work. Fixtures are stored as NDJSON (one task per line) so suites are
/// diff-friendly and stream without loading the whole file into a single JSON value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Stable, unique identifier (used in reports + the determinism digest).
    pub id: String,
    /// Selects the scorer and the meaning of the remaining fields.
    pub domain: Domain,
    /// The instruction shown to the model (the "user turn").
    pub prompt: String,
    /// Repo / corpus directory the context is assembled from. Relative paths resolve against
    /// the suite file's parent directory; absolute paths are used as-is.
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
    #[must_use]
    pub fn query(&self) -> &str {
        self.retrieval_query.as_deref().unwrap_or(&self.prompt)
    }

    /// Absolute workspace directory, resolved against `suite_dir` for relative paths.
    #[must_use]
    pub fn workspace_path(&self, suite_dir: &Path) -> PathBuf {
        let p = Path::new(&self.workspace);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            suite_dir.join(p)
        }
    }

    /// Validates the per-domain invariants. Returns a human-readable reason on failure.
    fn validate(&self) -> std::result::Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("task id is empty".into());
        }
        if self.prompt.trim().is_empty() {
            return Err(format!("task {}: prompt is empty", self.id));
        }
        if self.workspace.trim().is_empty() {
            return Err(format!("task {}: workspace is empty", self.id));
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
            Domain::Code => {
                if self.target_file.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(format!("task {}: code task needs target_file", self.id));
                }
                if self.test_cmd.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(format!("task {}: code task needs test_cmd", self.id));
                }
            }
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
}
