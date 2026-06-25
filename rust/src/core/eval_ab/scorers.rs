//! Deterministic scorers (#236): objective, reproducible scoring of a model answer.
//!
//! * [`QaScorer`] — SQuAD-style normalization → exact-match, token-overlap F1 and containment.
//! * [`CodeScorer`] — copies the workspace into a throwaway sandbox, writes the model output to
//!   the target file, runs the task's unit-test command and reports pass/fail by exit code.
//!
//! Both are fully deterministic given a fixed model output, which is what lets the harness
//! prove a non-regression rather than estimate one.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::suite::{Domain, Task};

/// A scored answer. `value` is the continuous metric in `[0,1]`; `passed` is the binary verdict
/// used for win/tie/loss accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub value: f64,
    pub passed: bool,
    /// `f1`, `exact_match` or `unit_test`.
    pub metric: String,
    /// Short human-readable explanation (e.g. `em=1 f1=1.00 contain=1`).
    pub detail: String,
}

/// Anything that can deterministically score a model answer for a task.
pub trait Scorer {
    fn score(&self, task: &Task, output: &str, workspace: &Path) -> Result<Score>;
}

/// Dispatches to the scorer for the task's domain.
pub fn score_task(task: &Task, output: &str, workspace: &Path) -> Result<Score> {
    match task.domain {
        Domain::Qa => QaScorer.score(task, output, workspace),
        Domain::Code => CodeScorer::default().score(task, output, workspace),
    }
}

// ---------------------------------------------------------------------------
// QA scorer
// ---------------------------------------------------------------------------

/// SQuAD-style QA scorer (exact-match / F1 / containment over a set of gold answers).
pub struct QaScorer;

impl Scorer for QaScorer {
    fn score(&self, task: &Task, output: &str, _workspace: &Path) -> Result<Score> {
        let pred = normalize(output);
        let pred_tokens: Vec<&str> = pred.split_whitespace().collect();

        let mut best_em = false;
        let mut best_contain = false;
        let mut best_f1 = 0.0f64;
        for gold in &task.answers {
            let g = normalize(gold);
            if g.is_empty() {
                continue;
            }
            let g_tokens: Vec<&str> = g.split_whitespace().collect();
            best_em |= pred == g;
            best_contain |= !g.is_empty() && pred.contains(&g);
            best_f1 = best_f1.max(token_f1(&pred_tokens, &g_tokens));
        }

        Ok(Score {
            value: best_f1,
            passed: best_em || best_contain,
            metric: "f1".to_string(),
            detail: format!(
                "em={} f1={best_f1:.2} contain={}",
                u8::from(best_em),
                u8::from(best_contain)
            ),
        })
    }
}

/// `SQuAD` token-overlap F1 between a prediction and a single gold answer.
/// Reusable building block for other harnesses (e.g. the `LoCoMo` memory bench, #291).
#[must_use]
pub fn qa_f1(pred: &str, gold: &str) -> f64 {
    let p = normalize(pred);
    let g = normalize(gold);
    let pt: Vec<&str> = p.split_whitespace().collect();
    let gt: Vec<&str> = g.split_whitespace().collect();
    token_f1(&pt, &gt)
}

/// `SQuAD` exact match between a prediction and a gold answer (after normalization).
#[must_use]
pub fn qa_exact_match(pred: &str, gold: &str) -> bool {
    normalize(pred) == normalize(gold)
}

/// True iff the normalized gold answer is contained in the normalized prediction.
#[must_use]
pub fn qa_contains(pred: &str, gold: &str) -> bool {
    let g = normalize(gold);
    !g.is_empty() && normalize(pred).contains(&g)
}

/// `SQuAD` normalization: lowercase, drop punctuation, drop articles, collapse whitespace.
fn normalize(s: &str) -> String {
    let lowered = s.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .filter(|w| !matches!(*w, "a" | "an" | "the"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Token-overlap F1 over multisets (`SQuAD` definition).
fn token_f1(pred: &[&str], gold: &[&str]) -> f64 {
    if pred.is_empty() && gold.is_empty() {
        return 1.0;
    }
    if pred.is_empty() || gold.is_empty() {
        return 0.0;
    }
    let mut gold_counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for &t in gold {
        *gold_counts.entry(t).or_insert(0) += 1;
    }
    let mut common = 0i64;
    let mut pred_counts: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    for &t in pred {
        let entry = pred_counts.entry(t).or_insert(0);
        *entry += 1;
        if *entry <= gold_counts.get(t).copied().unwrap_or(0) {
            common += 1;
        }
    }
    if common == 0 {
        return 0.0;
    }
    let precision = common as f64 / pred.len() as f64;
    let recall = common as f64 / gold.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

// ---------------------------------------------------------------------------
// Code scorer
// ---------------------------------------------------------------------------

/// Runs the task's unit-test command against the model output inside a sandbox copy.
pub struct CodeScorer {
    /// Wall-clock cap for the test command.
    pub timeout: Duration,
}

impl Default for CodeScorer {
    fn default() -> Self {
        // Seconds granularity is the natural unit for a test timeout.
        #[allow(clippy::duration_suboptimal_units)]
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

impl Scorer for CodeScorer {
    fn score(&self, task: &Task, output: &str, workspace: &Path) -> Result<Score> {
        let target = task
            .target_file
            .as_deref()
            .ok_or_else(|| anyhow!("code task {} has no target_file", task.id))?;
        let test_cmd = task
            .test_cmd
            .as_deref()
            .ok_or_else(|| anyhow!("code task {} has no test_cmd", task.id))?;

        let sandbox = tempfile::tempdir().context("creating sandbox")?;
        copy_dir_all(workspace, sandbox.path())
            .with_context(|| format!("copying workspace {}", workspace.display()))?;

        let target_path = sandbox.path().join(target);
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&target_path, extract_code(output))
            .with_context(|| format!("writing solution to {}", target_path.display()))?;

        let passed = run_with_timeout(test_cmd, sandbox.path(), self.timeout)?;
        Ok(Score {
            value: f64::from(u8::from(passed)),
            passed,
            metric: "unit_test".to_string(),
            detail: format!("test_cmd={test_cmd:?} passed={passed}"),
        })
    }
}

/// Strips a single Markdown code fence if the model wrapped its answer in one; otherwise returns
/// the trimmed text. Keeps the sandboxed file syntactically valid.
fn extract_code(output: &str) -> String {
    let trimmed = output.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Drop the optional language tag on the opening fence, then everything up to the close.
        let after_lang = rest.find('\n').map_or(rest, |i| &rest[i + 1..]);
        if let Some(end) = after_lang.rfind("```") {
            return after_lang[..end].trim_end().to_string();
        }
    }
    trimmed.to_string()
}

/// Recursively copies `src` into `dst` (which must already exist).
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Runs `cmd` via the POSIX shell in `dir`, returning `true` iff it exits 0 within `timeout`.
fn run_with_timeout(cmd: &str, dir: &Path, timeout: Duration) -> Result<bool> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning test command: {cmd}"))?;

    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qa_task() -> Task {
        Task {
            id: "q".into(),
            domain: Domain::Qa,
            prompt: "p".into(),
            workspace: "w".into(),
            retrieval_query: None,
            answers: vec!["bm25 graph knowledge session".into()],
            target_file: None,
            test_cmd: None,
        }
    }

    #[test]
    fn qa_exact_match_scores_one() {
        let s = QaScorer
            .score(
                &qa_task(),
                "BM25, graph, knowledge, session.",
                Path::new("."),
            )
            .unwrap();
        assert!(s.passed);
        assert_eq!(s.value, 1.0);
    }

    #[test]
    fn qa_partial_overlap_gives_fractional_f1() {
        let s = QaScorer
            .score(&qa_task(), "the graph and session stores", Path::new("."))
            .unwrap();
        assert!(s.value > 0.0 && s.value < 1.0, "f1 was {}", s.value);
    }

    #[test]
    fn qa_unrelated_answer_scores_zero() {
        let s = QaScorer
            .score(&qa_task(), "cats and weather forecasts", Path::new("."))
            .unwrap();
        assert_eq!(s.value, 0.0);
        assert!(!s.passed);
    }

    #[test]
    fn extract_code_unwraps_fence() {
        assert_eq!(extract_code("```sh\necho hi\n```"), "echo hi");
        assert_eq!(extract_code("plain text"), "plain text");
    }

    #[cfg(unix)]
    #[test]
    fn code_scorer_runs_unit_test() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("test.sh"),
            ". ./solution.sh\n[ \"$(add 2 3)\" = \"5\" ] || exit 1\n",
        )
        .unwrap();
        std::fs::write(ws.path().join("solution.sh"), "add() { echo 0; }\n").unwrap();

        let task = Task {
            id: "c".into(),
            domain: Domain::Code,
            prompt: "implement add".into(),
            workspace: "code".into(),
            retrieval_query: None,
            answers: vec![],
            target_file: Some("solution.sh".into()),
            test_cmd: Some("sh test.sh".into()),
        };

        let good = CodeScorer::default()
            .score(&task, "add() { echo $(( $1 + $2 )); }", ws.path())
            .unwrap();
        assert!(good.passed, "correct solution should pass: {good:?}");

        let bad = CodeScorer::default()
            .score(&task, "add() { echo 99; }", ws.path())
            .unwrap();
        assert!(!bad.passed, "wrong solution should fail");
    }
}
