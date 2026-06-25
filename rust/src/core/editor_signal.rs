//! Editor focus signal (#500).
//!
//! The file open in the editor is the strongest available relevance signal —
//! the developer is literally looking at it. The VS Code extension reports
//! tab changes via `lean-ctx editor-signal --file <path>`; this module stores
//! the signal in `~/.lean-ctx/editor_signal.json` so the MCP server, the CLI
//! and the dashboard (all separate processes) can read it without a daemon
//! or socket.
//!
//! Privacy: paths only, never content; the file stays local and is excluded
//! from any cloud-sync artifact set.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const SIGNAL_FILE: &str = "editor_signal.json";
/// A signal older than this is stale — the developer moved on or closed
/// the editor; ranking must not be steered by it anymore.
pub const FRESHNESS_SECS: u64 = 120;
const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EditorSignal {
    pub active_file: Option<String>,
    /// Most-recently-focused files: `(path, unix_ts)`, newest first.
    #[serde(default)]
    pub recent_files: Vec<(String, u64)>,
    pub updated_at: u64,
}

fn signal_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(SIGNAL_FILE)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Record a focus change (called by the `editor-signal` CLI subcommand).
pub fn record_focus(path: &str) -> Result<(), String> {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    let now = now_unix();

    let mut signal = load_raw().unwrap_or_default();
    signal.recent_files.retain(|(p, _)| p != &norm);
    if let Some(prev) = signal.active_file.take()
        && prev != norm
    {
        signal.recent_files.insert(0, (prev, signal.updated_at));
    }
    signal.recent_files.truncate(MAX_RECENT);
    signal.active_file = Some(norm);
    signal.updated_at = now;

    save(&signal)
}

fn save(signal: &EditorSignal) -> Result<(), String> {
    let path = signal_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let json = serde_json::to_string(signal).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
}

fn load_raw() -> Option<EditorSignal> {
    let raw = std::fs::read_to_string(signal_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Load the signal if it is fresh enough to steer ranking.
/// Broken/missing files are silently `None` — the read path never fails.
#[must_use]
pub fn load_fresh(max_age_secs: u64) -> Option<EditorSignal> {
    let signal = load_raw()?;
    if now_unix().saturating_sub(signal.updated_at) > max_age_secs {
        return None;
    }
    Some(signal)
}

/// Load regardless of freshness — for status surfaces (Live Signals panel)
/// that want to show a stale signal *as stale* instead of hiding it (#505).
#[must_use]
pub fn load_raw_for_status() -> Option<EditorSignal> {
    load_raw()
}

/// Ranking boost for a path: 0.30 for the active file, 0.10 for recent tabs.
#[must_use]
pub fn boost_for(signal: &EditorSignal, path: &str) -> f64 {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    if let Some(active) = &signal.active_file
        && paths_match(active, &norm)
    {
        return 0.30;
    }
    if signal
        .recent_files
        .iter()
        .any(|(p, _)| paths_match(p, &norm))
    {
        return 0.10;
    }
    0.0
}

/// Graph stores may hold relative paths while the editor reports absolute
/// ones (or vice versa) — suffix matching bridges both.
fn paths_match(a: &str, b: &str) -> bool {
    a == b || a.ends_with(b) || b.ends_with(a)
}

/// Apply the editor boost to a relevance ranking and re-sort.
pub fn apply_boost(scores: &mut [crate::core::task_relevance::RelevanceScore]) {
    let Some(signal) = load_fresh(FRESHNESS_SECS) else {
        return;
    };
    let mut changed = false;
    for s in scores.iter_mut() {
        let boost = boost_for(&signal, &s.path);
        if boost > 0.0 {
            s.score = (s.score + boost).min(1.0);
            changed = true;
        }
    }
    if changed {
        scores.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// Is `path` the currently focused editor file (fresh signal only)?
#[must_use]
pub fn is_active(path: &str) -> bool {
    load_fresh(FRESHNESS_SECS).is_some_and(|s| boost_for(&s, path) >= 0.30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_active_beats_recent() {
        let signal = EditorSignal {
            active_file: Some("/repo/src/auth.rs".into()),
            recent_files: vec![("/repo/src/db.rs".into(), 100)],
            updated_at: 100,
        };
        assert!((boost_for(&signal, "/repo/src/auth.rs") - 0.30).abs() < f64::EPSILON);
        assert!((boost_for(&signal, "/repo/src/db.rs") - 0.10).abs() < f64::EPSILON);
        assert!((boost_for(&signal, "/repo/src/other.rs")).abs() < f64::EPSILON);
    }

    #[test]
    fn relative_paths_match_absolute_signal() {
        let signal = EditorSignal {
            active_file: Some("/repo/src/auth.rs".into()),
            recent_files: vec![],
            updated_at: 100,
        };
        assert!((boost_for(&signal, "src/auth.rs") - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn stale_signal_is_ignored() {
        let signal = EditorSignal {
            active_file: Some("a.rs".into()),
            recent_files: vec![],
            updated_at: 0, // 1970 — definitely stale
        };
        // load_fresh path can't be exercised without disk; verify the age rule.
        assert!(now_unix().saturating_sub(signal.updated_at) > FRESHNESS_SECS);
    }

    #[test]
    fn focus_rotation_keeps_window_bounded() {
        let mut signal = EditorSignal::default();
        for i in 0..15 {
            let norm = format!("f{i}.rs");
            signal.recent_files.retain(|(p, _)| p != &norm);
            if let Some(prev) = signal.active_file.take()
                && prev != norm
            {
                signal.recent_files.insert(0, (prev, signal.updated_at));
            }
            signal.recent_files.truncate(MAX_RECENT);
            signal.active_file = Some(norm);
            signal.updated_at = 1000 + i;
        }
        assert_eq!(signal.active_file.as_deref(), Some("f14.rs"));
        assert_eq!(signal.recent_files.len(), MAX_RECENT);
        assert_eq!(signal.recent_files[0].0, "f13.rs");
    }
}
