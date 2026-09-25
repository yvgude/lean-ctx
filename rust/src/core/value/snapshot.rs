//! Per-session value snapshot: the small file every display channel reads.
//!
//! Written by the MCP server after tool calls (throttled, atomic) to
//! `<data_dir>/value/sessions/<id>.json` plus `value/current.json`. Readers
//! (status line, prompt segment, IDE) never open the ledger on their hot path;
//! `lean-ctx value` re-derives every number from the chains instead.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::security_events::SecurityCounts;

pub const SCHEMA: u32 = 1;

/// Minimum interval between two snapshot writes of one process.
const WRITE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ValueSnapshot {
    pub schema: u32,
    pub session_id: String,
    pub project_root: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub tool_calls: u64,
    /// Tokens lean-ctx's tools would have sent without compression.
    pub tokens_input: u64,
    /// Tokens kept out of the model's context (measured, per tool result).
    pub tokens_saved: u64,
    pub cache_hits: u64,
    pub files_read: u64,
    pub commands_run: u64,
    pub security: SecurityCounts,
}

impl ValueSnapshot {
    pub fn from_session(session: &crate::core::session::SessionState) -> Self {
        let s = &session.stats;
        Self {
            schema: SCHEMA,
            session_id: session.id.clone(),
            project_root: session.project_root.clone(),
            started_at: Some(session.started_at),
            updated_at: Some(Utc::now()),
            tool_calls: u64::from(s.total_tool_calls),
            tokens_input: s.total_tokens_input,
            tokens_saved: s.total_tokens_saved,
            cache_hits: u64::from(s.cache_hits),
            files_read: u64::from(s.files_read),
            commands_run: u64::from(s.commands_run),
            security: s.security,
        }
    }

    /// Share of tool input kept out of context, in percent (derived).
    pub fn saved_pct(&self) -> Option<f64> {
        (self.tokens_input > 0).then(|| self.tokens_saved as f64 * 100.0 / self.tokens_input as f64)
    }

    /// True when the snapshot was updated within `max_age`. Display channels
    /// show nothing for a stale snapshot — never a misleading old number.
    pub fn is_fresh(&self, max_age: Duration) -> bool {
        let Some(updated) = self.updated_at else {
            return false;
        };
        let age = Utc::now().signed_duration_since(updated);
        age.to_std().map_or(true, |age| age <= max_age)
    }

    /// Nothing worth showing yet.
    pub fn is_empty(&self) -> bool {
        self.tokens_saved == 0 && self.security.is_empty()
    }
}

pub fn value_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("value"))
}

fn sanitize_id(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !id.starts_with('.');
    ok.then_some(id)
}

pub fn session_path(dir: &Path, id: &str) -> Option<PathBuf> {
    sanitize_id(id).map(|id| dir.join("sessions").join(format!("{id}.json")))
}

pub fn current_path(dir: &Path) -> PathBuf {
    dir.join("current.json")
}

/// Writes `snap` as the session's snapshot and as `current.json`.
pub fn write_to(dir: &Path, snap: &ValueSnapshot) -> Result<(), String> {
    let path = session_path(dir, &snap.session_id).ok_or("invalid session id")?;
    let json = serde_json::to_vec(snap).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::core::atomic_fs::try_atomic_write(&path, &json, None).map_err(|e| e.to_string())?;
    crate::core::atomic_fs::try_atomic_write(&current_path(dir), &json, None)
        .map_err(|e| e.to_string())
}

/// Throttled write used by the MCP server: at most once per
/// `WRITE_INTERVAL` (1 s), except that a changed security tally always writes so a
/// blocked command or a kept-out secret shows up immediately.
pub fn write_throttled(snap: &ValueSnapshot) {
    static LAST: Mutex<Option<(Instant, SecurityCounts)>> = Mutex::new(None);
    {
        let Ok(mut last) = LAST.lock() else { return };
        if let Some((at, security)) = *last
            && at.elapsed() < WRITE_INTERVAL
            && security == snap.security
        {
            return;
        }
        *last = Some((Instant::now(), snap.security));
    }
    let Some(dir) = value_dir() else { return };
    if let Err(error) = write_to(&dir, snap) {
        tracing::debug!("lean-ctx: value snapshot write failed: {error}");
    }
}

pub fn read_path(path: &Path) -> Option<ValueSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The snapshot of session `id`, or of the most recently active session.
pub fn load(id: Option<&str>) -> Option<ValueSnapshot> {
    let dir = value_dir()?;
    match id {
        Some(id) => read_path(&session_path(&dir, id)?),
        None => read_path(&current_path(&dir)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str) -> ValueSnapshot {
        ValueSnapshot {
            schema: SCHEMA,
            session_id: id.into(),
            updated_at: Some(Utc::now()),
            tokens_input: 1_000,
            tokens_saved: 400,
            ..ValueSnapshot::default()
        }
    }

    #[test]
    fn roundtrip_through_session_and_current_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = snap("sess-1");
        write_to(dir.path(), &s).unwrap();
        assert_eq!(
            read_path(&session_path(dir.path(), "sess-1").unwrap()),
            Some(s.clone())
        );
        assert_eq!(read_path(&current_path(dir.path())), Some(s));
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let dir = tempfile::tempdir().unwrap();
        assert!(session_path(dir.path(), "../evil").is_none());
        assert!(session_path(dir.path(), "a/b").is_none());
        assert!(session_path(dir.path(), "").is_none());
        assert!(write_to(dir.path(), &snap("../x")).is_err());
    }

    #[test]
    fn staleness_is_detected() {
        let mut s = snap("s");
        assert!(s.is_fresh(Duration::from_mins(1)));
        s.updated_at = Some(Utc::now() - chrono::Duration::hours(2));
        assert!(!s.is_fresh(Duration::from_mins(1)));
        s.updated_at = None;
        assert!(!s.is_fresh(Duration::from_mins(1)));
    }

    #[test]
    fn saved_pct_is_derived_from_measured_counts() {
        assert_eq!(snap("s").saved_pct(), Some(40.0));
        assert_eq!(ValueSnapshot::default().saved_pct(), None);
    }

    #[test]
    fn unknown_fields_and_missing_fields_are_tolerated() {
        let s: ValueSnapshot =
            serde_json::from_str(r#"{"session_id":"x","future_field":1}"#).unwrap();
        assert_eq!(s.session_id, "x");
        assert!(s.is_empty());
    }
}
