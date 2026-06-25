//! `LoCoMo` benchmark dataset schema + loader (#291).
//!
//! Accepts both NDJSON (one [`LocomoSample`] per line, `#` comments allowed) and a
//! plain JSON array, so the bundled reference suite and the official `LoCoMo` dataset
//! can be loaded by the same code.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One `LoCoMo` sample: a multi-session conversation plus its question/answer set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocomoSample {
    pub id: String,
    pub sessions: Vec<Session>,
    pub qa: Vec<QaItem>,
}

/// An ordered conversation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub session_id: String,
    pub turns: Vec<Turn>,
}

/// A single dialog turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub speaker: String,
    pub text: String,
}

/// A question with its acceptable gold answers and `LoCoMo` category
/// (1=single-hop, 2=multi-hop, 3=temporal, 4=open-domain, 5=adversarial).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaItem {
    pub question: String,
    pub answers: Vec<String>,
    #[serde(default = "default_category")]
    pub category: u8,
}

fn default_category() -> u8 {
    1
}

impl LocomoSample {
    /// Flattened transcript (`Speaker: text` per turn) — the baseline an agent
    /// would otherwise dump into context wholesale.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut lines = Vec::new();
        for session in &self.sessions {
            for turn in &session.turns {
                lines.push(format!("{}: {}", turn.speaker, turn.text));
            }
        }
        lines.join("\n")
    }

    /// Total number of conversation turns across all sessions.
    #[must_use]
    pub fn turn_count(&self) -> usize {
        self.sessions.iter().map(|s| s.turns.len()).sum()
    }
}

/// Parse a suite from raw text (NDJSON or JSON array).
pub fn parse_suite(raw: &str) -> Result<Vec<LocomoSample>, String> {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('[') {
        return serde_json::from_str(trimmed).map_err(|e| format!("invalid JSON array: {e}"));
    }
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let sample: LocomoSample =
            serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(sample);
    }
    if out.is_empty() {
        return Err("suite contained no samples".to_string());
    }
    Ok(out)
}

/// Load a suite from a file path.
pub fn load_suite(path: &Path) -> Result<Vec<LocomoSample>, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse_suite(&raw)
}

/// The committed reference suite (real, verifiable facts; every gold answer is
/// grounded in a turn and objectively true).
pub const REFERENCE_SUITE: &str = include_str!("../../../data/locomo/reference-suite.ndjson");

/// Parse the bundled reference suite. Panics only if the committed fixture is
/// malformed, which a unit test guards against.
#[must_use]
pub fn reference_samples() -> Vec<LocomoSample> {
    parse_suite(REFERENCE_SUITE).expect("bundled reference suite must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_suite_parses_and_is_grounded() {
        let samples = reference_samples();
        assert!(!samples.is_empty());
        for s in &samples {
            assert!(!s.sessions.is_empty(), "{} has no sessions", s.id);
            assert!(!s.qa.is_empty(), "{} has no QA", s.id);
            let transcript = s.transcript().to_lowercase();
            // Every gold answer must be grounded somewhere in the transcript.
            for qa in &s.qa {
                assert!(!qa.answers.is_empty(), "QA without answers in {}", s.id);
                let grounded = qa
                    .answers
                    .iter()
                    .any(|a| transcript.contains(&a.to_lowercase()));
                assert!(
                    grounded,
                    "answer for '{}' not grounded in transcript of {}",
                    qa.question, s.id
                );
            }
        }
    }

    #[test]
    fn parses_json_array_form() {
        let raw = r#"[{"id":"x","sessions":[{"session_id":"s","turns":[{"speaker":"A","text":"hi"}]}],"qa":[{"question":"q","answers":["hi"]}]}]"#;
        let s = parse_suite(raw).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].qa[0].category, 1, "default category applied");
    }

    #[test]
    fn empty_suite_errors() {
        assert!(parse_suite("# only a comment\n").is_err());
    }
}
