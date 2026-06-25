//! Persisted session-summary record + the lock-free candidate built under the
//! session lock (#292).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted, recallable session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRecord {
    /// Stable id: `<session-id>-<seq>`.
    pub id: String,
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    /// One-line headline (task description or inferred focus).
    pub title: String,
    /// Deterministic multi-line narrative of the session.
    pub body: String,
    pub files: Vec<String>,
    pub decisions: Vec<String>,
    pub next_steps: Vec<String>,
    /// Tool-call count at the time of recording (also the cadence watermark).
    pub tool_calls: u64,
}

impl SummaryRecord {
    /// Text used for both lexical and semantic recall.
    #[must_use]
    pub fn searchable_text(&self) -> String {
        let mut t = String::with_capacity(self.title.len() + self.body.len() + 16);
        t.push_str(&self.title);
        t.push('\n');
        t.push_str(&self.body);
        t
    }
}

/// An owned snapshot built while holding the session lock, then persisted off the
/// hot path. Keeps the lock hold minimal (no disk I/O under the lock).
#[derive(Debug, Clone)]
pub struct SummaryCandidate {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub title: String,
    pub body: String,
    pub files: Vec<String>,
    pub decisions: Vec<String>,
    pub next_steps: Vec<String>,
    pub tool_calls: u64,
    /// Whether the session carried anything worth summarizing.
    pub has_content: bool,
}

impl SummaryCandidate {
    /// Finalize into a persisted record with a sequence number.
    #[must_use]
    pub fn into_record(self, seq: u32) -> SummaryRecord {
        let short = self
            .session_id
            .split('-')
            .next()
            .unwrap_or(&self.session_id);
        SummaryRecord {
            id: format!("{short}-{seq:04}"),
            session_id: self.session_id,
            created_at: self.created_at,
            title: self.title,
            body: self.body,
            files: self.files,
            decisions: self.decisions,
            next_steps: self.next_steps,
            tool_calls: self.tool_calls,
        }
    }
}
