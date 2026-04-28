//! Session diffing — structured comparison of two session states.
//!
//! Produces a diff showing added/removed/changed files, findings,
//! decisions, and tool-call pattern differences between sessions.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::core::session::SessionState;

#[derive(Debug, Clone, Serialize)]
pub struct SessionDiff {
    pub session_a: String,
    pub session_b: String,
    pub files: FilesDiff,
    pub findings: CountDiff,
    pub decisions: CountDiff,
    pub stats: StatsDiff,
    pub modes: ModesDiff,
}

#[derive(Debug, Clone, Serialize)]
pub struct FilesDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed_mode: Vec<FileModeChange>,
    pub common_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileModeChange {
    pub path: String,
    pub mode_a: String,
    pub mode_b: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountDiff {
    pub count_a: usize,
    pub count_b: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatsDiff {
    pub tool_calls_a: u32,
    pub tool_calls_b: u32,
    pub tokens_saved_a: u64,
    pub tokens_saved_b: u64,
    pub files_read_a: u32,
    pub files_read_b: u32,
    pub commands_a: u32,
    pub commands_b: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModesDiff {
    pub modes_a: HashMap<String, usize>,
    pub modes_b: HashMap<String, usize>,
}

pub fn diff_sessions(a: &SessionState, b: &SessionState) -> SessionDiff {
    SessionDiff {
        session_a: a.id.clone(),
        session_b: b.id.clone(),
        files: diff_files(a, b),
        findings: diff_findings(a, b),
        decisions: diff_decisions(a, b),
        stats: diff_stats(a, b),
        modes: diff_modes(a, b),
    }
}

fn diff_files(a: &SessionState, b: &SessionState) -> FilesDiff {
    let paths_a: HashSet<&str> = a.files_touched.iter().map(|f| f.path.as_str()).collect();
    let paths_b: HashSet<&str> = b.files_touched.iter().map(|f| f.path.as_str()).collect();

    let added: Vec<String> = paths_b
        .difference(&paths_a)
        .map(ToString::to_string)
        .collect();
    let removed: Vec<String> = paths_a
        .difference(&paths_b)
        .map(ToString::to_string)
        .collect();

    let common: HashSet<&&str> = paths_a.intersection(&paths_b).collect();
    let common_count = common.len();

    let mode_map_a: HashMap<&str, &str> = a
        .files_touched
        .iter()
        .map(|f| (f.path.as_str(), f.last_mode.as_str()))
        .collect();
    let mode_map_b: HashMap<&str, &str> = b
        .files_touched
        .iter()
        .map(|f| (f.path.as_str(), f.last_mode.as_str()))
        .collect();

    let mut changed_mode = Vec::new();
    for path in &common {
        if let (Some(&ma), Some(&mb)) = (mode_map_a.get(**path), mode_map_b.get(**path)) {
            if ma != mb {
                changed_mode.push(FileModeChange {
                    path: path.to_string(),
                    mode_a: ma.to_string(),
                    mode_b: mb.to_string(),
                });
            }
        }
    }

    FilesDiff {
        added,
        removed,
        changed_mode,
        common_count,
    }
}

fn diff_findings(a: &SessionState, b: &SessionState) -> CountDiff {
    let summaries_a: HashSet<&str> = a.findings.iter().map(|f| f.summary.as_str()).collect();
    let summaries_b: HashSet<&str> = b.findings.iter().map(|f| f.summary.as_str()).collect();

    CountDiff {
        count_a: a.findings.len(),
        count_b: b.findings.len(),
        added: summaries_b
            .difference(&summaries_a)
            .map(ToString::to_string)
            .collect(),
        removed: summaries_a
            .difference(&summaries_b)
            .map(ToString::to_string)
            .collect(),
    }
}

fn diff_decisions(a: &SessionState, b: &SessionState) -> CountDiff {
    let summaries_a: HashSet<&str> = a.decisions.iter().map(|d| d.summary.as_str()).collect();
    let summaries_b: HashSet<&str> = b.decisions.iter().map(|d| d.summary.as_str()).collect();

    CountDiff {
        count_a: a.decisions.len(),
        count_b: b.decisions.len(),
        added: summaries_b
            .difference(&summaries_a)
            .map(ToString::to_string)
            .collect(),
        removed: summaries_a
            .difference(&summaries_b)
            .map(ToString::to_string)
            .collect(),
    }
}

fn diff_stats(a: &SessionState, b: &SessionState) -> StatsDiff {
    StatsDiff {
        tool_calls_a: a.stats.total_tool_calls,
        tool_calls_b: b.stats.total_tool_calls,
        tokens_saved_a: a.stats.total_tokens_saved,
        tokens_saved_b: b.stats.total_tokens_saved,
        files_read_a: a.stats.files_read,
        files_read_b: b.stats.files_read,
        commands_a: a.stats.commands_run,
        commands_b: b.stats.commands_run,
    }
}

fn diff_modes(a: &SessionState, b: &SessionState) -> ModesDiff {
    let mut modes_a: HashMap<String, usize> = HashMap::new();
    for f in &a.files_touched {
        *modes_a.entry(f.last_mode.clone()).or_insert(0) += 1;
    }
    let mut modes_b: HashMap<String, usize> = HashMap::new();
    for f in &b.files_touched {
        *modes_b.entry(f.last_mode.clone()).or_insert(0) += 1;
    }
    ModesDiff { modes_a, modes_b }
}

impl SessionDiff {
    pub fn format_summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "Session Diff: {} vs {}",
            &self.session_a[..8.min(self.session_a.len())],
            &self.session_b[..8.min(self.session_b.len())]
        ));

        lines.push(format!(
            "Files: {} common, +{} added, -{} removed, ~{} mode-changed",
            self.files.common_count,
            self.files.added.len(),
            self.files.removed.len(),
            self.files.changed_mode.len()
        ));

        if !self.files.added.is_empty() {
            for f in &self.files.added {
                lines.push(format!("  + {f}"));
            }
        }
        if !self.files.removed.is_empty() {
            for f in &self.files.removed {
                lines.push(format!("  - {f}"));
            }
        }
        for mc in &self.files.changed_mode {
            lines.push(format!("  ~ {} ({} -> {})", mc.path, mc.mode_a, mc.mode_b));
        }

        lines.push(format!(
            "Findings: {} vs {} (+{} / -{})",
            self.findings.count_a,
            self.findings.count_b,
            self.findings.added.len(),
            self.findings.removed.len()
        ));

        lines.push(format!(
            "Decisions: {} vs {} (+{} / -{})",
            self.decisions.count_a,
            self.decisions.count_b,
            self.decisions.added.len(),
            self.decisions.removed.len()
        ));

        lines.push(format!(
            "Stats: calls {}/{}, saved {}/{}, files {}/{}, cmds {}/{}",
            self.stats.tool_calls_a,
            self.stats.tool_calls_b,
            self.stats.tokens_saved_a,
            self.stats.tokens_saved_b,
            self.stats.files_read_a,
            self.stats.files_read_b,
            self.stats.commands_a,
            self.stats.commands_b,
        ));

        lines.join("\n")
    }

    pub fn format_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str) -> SessionState {
        let mut s = SessionState::new();
        s.id = id.to_string();
        s
    }

    #[test]
    fn empty_sessions_produce_empty_diff() {
        let a = make_session("aaa");
        let b = make_session("bbb");
        let d = diff_sessions(&a, &b);
        assert!(d.files.added.is_empty());
        assert!(d.files.removed.is_empty());
        assert_eq!(d.files.common_count, 0);
    }

    #[test]
    fn added_files_detected() {
        let a = make_session("a");
        let mut b = make_session("b");
        b.touch_file("src/new.rs", None, "full", 100);
        let d = diff_sessions(&a, &b);
        assert_eq!(d.files.added, vec!["src/new.rs"]);
    }

    #[test]
    fn removed_files_detected() {
        let mut a = make_session("a");
        a.touch_file("src/old.rs", None, "full", 100);
        let b = make_session("b");
        let d = diff_sessions(&a, &b);
        assert_eq!(d.files.removed, vec!["src/old.rs"]);
    }

    #[test]
    fn mode_changes_detected() {
        let mut a = make_session("a");
        let mut b = make_session("b");
        a.touch_file("src/lib.rs", None, "full", 500);
        b.touch_file("src/lib.rs", None, "signatures", 100);
        let d = diff_sessions(&a, &b);
        assert_eq!(d.files.changed_mode.len(), 1);
        assert_eq!(d.files.changed_mode[0].mode_a, "full");
        assert_eq!(d.files.changed_mode[0].mode_b, "signatures");
    }

    #[test]
    fn findings_diff() {
        let mut a = make_session("a");
        let mut b = make_session("b");
        a.add_finding(None, None, "old finding");
        b.add_finding(None, None, "new finding");
        let d = diff_sessions(&a, &b);
        assert_eq!(d.findings.added, vec!["new finding"]);
        assert_eq!(d.findings.removed, vec!["old finding"]);
    }

    #[test]
    fn format_summary_includes_key_info() {
        let mut a = make_session("session_aaa");
        let mut b = make_session("session_bbb");
        a.touch_file("src/main.rs", None, "full", 500);
        b.touch_file("src/main.rs", None, "map", 100);
        b.touch_file("src/new.rs", None, "full", 200);
        let d = diff_sessions(&a, &b);
        let summary = d.format_summary();
        assert!(summary.contains("session_"));
        assert!(summary.contains("+ src/new.rs"));
        assert!(summary.contains("full -> map"));
    }
}
