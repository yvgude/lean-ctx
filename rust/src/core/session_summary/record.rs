//! Persisted session-summary record + the lock-free candidate built under the
//! session lock (#292).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted, recallable session summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SummaryRecord {
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
    pub(crate) fn searchable_text(&self) -> String {
        let mut t = String::with_capacity(self.title.len() + self.body.len() + 16);
        t.push_str(&self.title);
        t.push('\n');
        t.push_str(&self.body);
        t
    }

    /// This record with every text field passed through `admit`; `None` when
    /// one of them is refused. Ids, timestamps, counters and file paths are
    /// metadata and are kept as they are.
    fn with_text(&self, admit: impl Fn(&str) -> Option<String>) -> Option<Self> {
        // An empty field carries nothing to decide about.
        let admit = |text: &str| {
            if text.is_empty() {
                Some(String::new())
            } else {
                admit(text)
            }
        };
        let each = |items: &[String]| -> Option<Vec<String>> {
            items.iter().map(|item| admit(item)).collect()
        };
        Some(Self {
            title: admit(&self.title)?,
            body: admit(&self.body)?,
            decisions: each(&self.decisions)?,
            next_steps: each(&self.next_steps)?,
            ..self.clone()
        })
    }

    /// The record as it may be PERSISTED (E3): every text field admitted for
    /// a derived store under the current policy. A summary is distilled from
    /// session state and has no source to bind to, so the content rules
    /// decide: a field that is withheld or restricted refuses the whole
    /// record — a summary is never stored with a hole in it — and a value the
    /// gateway masks is stored masked.
    pub(crate) fn admitted_for_store(&self) -> Option<Self> {
        self.with_text(|text| crate::core::context_admission::carrier::admit_for_store(text).ok())
    }

    /// The record as it may be SERVED now (G04). A stored summary carries no
    /// source binding, so it follows the originless rule of
    /// `context_admission::stored::serve_stored`: withheld while protection
    /// is active in any layer, re-admitted under today's content rules
    /// otherwise. `None`: not served.
    pub(crate) fn served(&self) -> Option<Self> {
        self.with_text(|text| {
            crate::core::context_admission::stored::serve_stored(text, None, None).ok()
        })
    }
}

/// An owned snapshot built while holding the session lock, then persisted off the
/// hot path. Keeps the lock hold minimal (no disk I/O under the lock).
#[derive(Debug, Clone)]
pub(crate) struct SummaryCandidate {
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
    pub(crate) fn into_record(self, seq: u32) -> SummaryRecord {
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
