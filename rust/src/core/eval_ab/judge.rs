//! Real LLM-as-judge scorer for free-form QA (#611).
//!
//! Exact-match / F1 ([`super::scorers::QaScorer`]) is the right tool when the gold
//! answer is short and canonical, but real-repo questions ("how does X work?")
//! have many correct phrasings that token overlap punishes. The testbench therefore
//! grades free-form answers with the *same pinned model* used to produce them: given
//! the question, the reference answer(s) and a candidate, the judge returns a strict
//! `PASS`/`FAIL` plus a `0.0–1.0` correctness score.
//!
//! Determinism is preserved exactly as for answers: the judge call goes through the
//! same [`ModelRunner`] (so it is replayed from a recording in CI), and
//! [`parse_verdict`] is a pure function of the judge's text. There is no silent
//! fallback — an unparsable judge reply is a hard error, never a guessed score.

use anyhow::{Result, bail};

use super::model::{ModelRequest, ModelRunner};
use super::scorers::Score;
use super::suite::Task;

/// System framing for the grader. Kept terse and format-locked so [`parse_verdict`]
/// has a stable contract to read.
pub const JUDGE_SYSTEM: &str = "You are a strict, impartial grader for a question-answering benchmark. \
You are given a QUESTION, one or more REFERENCE answers (ground truth) and a CANDIDATE answer. \
Decide whether the candidate is factually correct and responsive to the question, using the \
reference(s) as ground truth. Ignore style, length, ordering and phrasing — judge only correctness. \
Reply with EXACTLY two lines and nothing else:\n\
Line 1: PASS or FAIL\n\
Line 2: a correctness score from 0.0 to 1.0";

/// LLM-as-judge scorer. Stateless; the model is supplied per call so the same
/// runner (and recording) covers both answering and grading.
#[derive(Debug, Clone, Default)]
pub struct LlmJudge;

impl LlmJudge {
    /// Builds the deterministic grading request for one candidate answer. Exposed so
    /// a recording generator can pre-compute the exact replay keys.
    pub fn request(task: &Task, candidate: &str) -> ModelRequest {
        let references = if task.answers.is_empty() {
            "(none provided)".to_string()
        } else {
            task.answers
                .iter()
                .map(|a| format!("- {a}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        ModelRequest {
            system: JUDGE_SYSTEM.to_string(),
            user: format!(
                "QUESTION:\n{}\n\nREFERENCE ANSWER(S):\n{references}\n\nCANDIDATE ANSWER:\n{candidate}",
                task.prompt
            ),
        }
    }

    /// Grades `candidate` against the task's reference answers via `runner`.
    pub fn score(&self, runner: &dyn ModelRunner, task: &Task, candidate: &str) -> Result<Score> {
        let resp = runner.run(&Self::request(task, candidate))?;
        parse_verdict(&resp.text)
    }
}

/// Parses a judge reply into a [`Score`]. Deterministic and tolerant of minor
/// formatting drift (extra prose, reversed lines), but it refuses to invent a
/// verdict: a reply with neither a `PASS`/`FAIL` token nor a numeric score errors.
pub fn parse_verdict(text: &str) -> Result<Score> {
    let verdict = first_verdict_token(text);
    let score = first_unit_float(text);

    if verdict.is_none() && score.is_none() {
        bail!("judge reply has neither a PASS/FAIL verdict nor a 0.0-1.0 score: {text:?}");
    }

    // A score below 0.5 with no explicit token reads as a fail, and vice-versa, so
    // the binary `passed` flag always agrees with the continuous value when the
    // judge only returned one of the two.
    let passed = match verdict {
        Some(p) => p,
        None => score.is_some_and(|s| s >= 0.5),
    };
    let value = score
        .unwrap_or(if passed { 1.0 } else { 0.0 })
        .clamp(0.0, 1.0);

    Ok(Score {
        value,
        passed,
        metric: "llm_judge".to_string(),
        detail: format!("judge passed={passed} score={value:.2}"),
    })
}

/// Returns the first standalone `PASS` (→ `true`) or `FAIL` (→ `false`) token,
/// case-insensitive. Token boundaries avoid matching "passed" / "failure".
fn first_verdict_token(text: &str) -> Option<bool> {
    for tok in text.split(|c: char| !c.is_ascii_alphabetic()) {
        match tok.to_ascii_uppercase().as_str() {
            "PASS" => return Some(true),
            "FAIL" => return Some(false),
            _ => {}
        }
    }
    None
}

/// Returns the first numeric literal in `[0.0, 1.0]` found in `text`.
fn first_unit_float(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < bytes.len() && ((bytes[i] as char).is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if let Ok(v) = text[start..i].parse::<f64>()
                && (0.0..=1.0).contains(&v)
            {
                return Some(v);
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{
        ModelFingerprint, ModelParams, ModelResponse, PROVIDER_RECORDED, RecordedRunner, Recording,
    };
    use crate::core::eval_ab::suite::Domain;

    fn task() -> Task {
        Task {
            id: "q".into(),
            domain: Domain::Qa,
            task_class: None,
            prompt: "What stores does consolidation persist to?".into(),
            workspace: ".".into(),
            retrieval_query: None,
            answers: vec!["bm25, graph, knowledge, session".into()],
            target_file: None,
            test_cmd: None,
        }
    }

    #[test]
    fn parses_two_line_pass() {
        let s = parse_verdict("PASS\n1.0").unwrap();
        assert!(s.passed);
        assert_eq!(s.value, 1.0);
        assert_eq!(s.metric, "llm_judge");
    }

    #[test]
    fn parses_fail_with_low_score() {
        let s = parse_verdict("FAIL\n0.0").unwrap();
        assert!(!s.passed);
        assert_eq!(s.value, 0.0);
    }

    #[test]
    fn tolerates_reversed_and_noisy_lines() {
        let s = parse_verdict("Score: 0.8\nVerdict: PASS — looks right").unwrap();
        assert!(s.passed);
        assert!((s.value - 0.8).abs() < 1e-9);
    }

    #[test]
    fn verdict_token_does_not_match_substrings() {
        // "passed"/"failure" must NOT be read as PASS/FAIL tokens; the 0.9 drives it.
        let s = parse_verdict("the candidate passed expectations 0.9").unwrap();
        assert!(s.passed);
        assert!((s.value - 0.9).abs() < 1e-9);
    }

    #[test]
    fn score_only_below_half_is_a_fail() {
        let s = parse_verdict("0.30").unwrap();
        assert!(!s.passed);
        assert!((s.value - 0.30).abs() < 1e-9);
    }

    #[test]
    fn empty_or_unparsable_reply_errors() {
        assert!(parse_verdict("").is_err());
        assert!(parse_verdict("I am not sure how to grade this.").is_err());
    }

    #[test]
    fn out_of_range_numbers_are_ignored() {
        // 2024 is not a unit score; only PASS drives the verdict, value defaults to 1.0.
        let s = parse_verdict("PASS (reference doc dated 2024)").unwrap();
        assert!(s.passed);
        assert_eq!(s.value, 1.0);
    }

    #[test]
    fn score_via_recorded_runner_is_deterministic() {
        let t = task();
        let req = LlmJudge::request(&t, "bm25, graph, knowledge and session stores");
        let fp = ModelFingerprint {
            provider: PROVIDER_RECORDED.into(),
            endpoint: "rec".into(),
            params: ModelParams::default(),
        };
        let mut rec = Recording::new(fp);
        rec.entries
            .insert(req.key(), ModelResponse::new("PASS\n0.95"));
        let runner = RecordedRunner::new(rec);

        let s = LlmJudge
            .score(&runner, &t, "bm25, graph, knowledge and session stores")
            .unwrap();
        assert!(s.passed);
        assert!((s.value - 0.95).abs() < 1e-9);
    }
}
