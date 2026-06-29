//! Restore / resume from a Context Snapshot (#1026).
//!
//! Phase 3 of the Context Time Machine: take a stored snapshot and bring the
//! live working state back to it. Two halves, mirroring the vision verbs:
//!
//! - **continue** — merge the snapshot's distilled session slice (task,
//!   progress, decisions, touched files) into the project's live session so the
//!   next agent picks up exactly where that snapshot left off.
//! - **reproduce** — optionally check out the snapshot's git anchor so the code
//!   matches what the model saw (guarded: never discards a dirty tree).
//!
//! `merge_session` is a pure function over an in-memory [`SessionState`]
//! (unit-tested); loading/saving and git are the impure shell around it.

use std::path::Path;
use std::time::Duration;

use crate::core::session::SessionState;

use super::types::{ContextSnapshotV1, SnapshotSessionV1};

/// What [`restore`] should do beyond the always-on session resume.
pub struct RestoreOptions {
    /// Project the snapshot belongs to (selects the live session + git repo).
    pub project_root: String,
    /// Check out the snapshot's git commit (refused if the tree is dirty).
    pub checkout_git: bool,
}

/// Git side-effect outcome of a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRestore {
    /// Caller didn't ask to touch git.
    Skipped,
    /// Snapshot has no commit anchor (or git is unavailable) to check out.
    NoAnchor,
    /// Refused: the working tree had uncommitted changes.
    DirtyTree,
    /// Checked out the given commit.
    CheckedOut(String),
    /// `git checkout` ran but failed (carries the trimmed git error).
    Failed(String),
}

/// What a session merge changed — pure, drives tests and CLI output.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionMerge {
    pub task: Option<String>,
    pub progress_pct: Option<u8>,
    pub decisions_added: usize,
    pub files_added: usize,
}

/// Full outcome of a [`restore`].
pub struct RestoreOutcome {
    pub session: SessionMerge,
    pub git: GitRestore,
    /// Whether the snapshot actually carried a session slice to resume from.
    pub had_session_slice: bool,
}

/// Restore a snapshot into the live project state: resume its session, and —
/// when `opts.checkout_git` — check out its git anchor (guarded).
pub fn restore(
    snapshot: &ContextSnapshotV1,
    opts: &RestoreOptions,
) -> Result<RestoreOutcome, String> {
    let mut session =
        SessionState::load_latest_for_project_root(&opts.project_root).unwrap_or_default();
    if session.project_root.is_none() {
        session.project_root = Some(opts.project_root.clone());
    }

    let had_session_slice = snapshot.session.is_some();
    let merge = match snapshot.session.as_ref() {
        Some(slice) => {
            let m = merge_session(&mut session, slice);
            session.save()?;
            m
        }
        None => SessionMerge::default(),
    };

    let git = if opts.checkout_git {
        checkout_anchor(snapshot, &opts.project_root)
    } else {
        GitRestore::Skipped
    };

    Ok(RestoreOutcome {
        session: merge,
        git,
        had_session_slice,
    })
}

/// Pure merge of a snapshot session slice into a live session. Task + progress
/// are overwritten (the snapshot is the source of truth being restored);
/// decisions and touched files are appended, de-duplicated against what the
/// live session already holds.
fn merge_session(session: &mut SessionState, slice: &SnapshotSessionV1) -> SessionMerge {
    let mut report = SessionMerge::default();

    if let Some(task) = slice
        .task
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        session.set_task(task, None);
        if let Some(t) = session.task.as_mut() {
            t.progress_pct = slice.progress_pct;
        }
        report.task = Some(task.to_string());
        report.progress_pct = slice.progress_pct;
    }

    for decision in &slice.decisions {
        let summary = decision.trim();
        if summary.is_empty() || session.decisions.iter().any(|e| e.summary == summary) {
            continue;
        }
        session.add_decision(summary, None);
        report.decisions_added += 1;
    }

    for path in &slice.files_touched {
        let p = path.trim();
        if p.is_empty() || session.files_touched.iter().any(|f| f.path == p) {
            continue;
        }
        session.touch_file(p, None, "full", 0);
        report.files_added += 1;
    }

    report
}

/// Check out the snapshot's git anchor, refusing to clobber a dirty tree.
fn checkout_anchor(snapshot: &ContextSnapshotV1, project_root: &str) -> GitRestore {
    let Some(commit) = snapshot.git.commit.as_deref().filter(|c| !c.is_empty()) else {
        return GitRestore::NoAnchor;
    };
    if !crate::core::git::git_available() {
        return GitRestore::NoAnchor;
    }
    let root = Path::new(project_root);
    let dirty = crate::core::git::run_git(
        &["status", "--porcelain"],
        root,
        Duration::from_secs(5),
        &[],
    )
    .is_ok_and(|o| o.success && !o.stdout.trim().is_empty());
    if dirty {
        return GitRestore::DirtyTree;
    }
    match crate::core::git::run_git(&["checkout", commit], root, Duration::from_secs(20), &[]) {
        Ok(o) if o.success => GitRestore::CheckedOut(commit.to_string()),
        Ok(o) => GitRestore::Failed(o.stderr.trim().to_string()),
        Err(e) => GitRestore::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(
        task: Option<&str>,
        progress: Option<u8>,
        decisions: &[&str],
        files: &[&str],
    ) -> SnapshotSessionV1 {
        SnapshotSessionV1 {
            session_id: None,
            task: task.map(str::to_string),
            decisions: decisions.iter().map(|d| (*d).to_string()).collect(),
            files_touched: files.iter().map(|f| (*f).to_string()).collect(),
            progress_pct: progress,
        }
    }

    #[test]
    fn merge_sets_task_and_progress() {
        let mut session = SessionState::new();
        let s = slice(Some("Resume the timeline"), Some(60), &[], &[]);
        let report = merge_session(&mut session, &s);
        assert_eq!(report.task.as_deref(), Some("Resume the timeline"));
        assert_eq!(report.progress_pct, Some(60));
        assert_eq!(session.task.as_ref().and_then(|t| t.progress_pct), Some(60));
    }

    #[test]
    fn merge_appends_and_dedups_decisions() {
        let mut session = SessionState::new();
        session.add_decision("Use JSONL index", None);
        let s = slice(
            None,
            None,
            &["Use JSONL index", "Sign with ed25519", "  "],
            &[],
        );
        let report = merge_session(&mut session, &s);
        // Only the genuinely new, non-empty decision is added.
        assert_eq!(report.decisions_added, 1);
        assert!(
            session
                .decisions
                .iter()
                .any(|d| d.summary == "Sign with ed25519")
        );
        assert_eq!(
            session
                .decisions
                .iter()
                .filter(|d| d.summary == "Use JSONL index")
                .count(),
            1
        );
    }

    #[test]
    fn merge_appends_and_dedups_files() {
        let mut session = SessionState::new();
        session.touch_file("src/a.rs", None, "full", 100);
        let s = slice(None, None, &[], &["src/a.rs", "src/b.rs", ""]);
        let report = merge_session(&mut session, &s);
        assert_eq!(report.files_added, 1);
        assert!(session.files_touched.iter().any(|f| f.path == "src/b.rs"));
    }

    #[test]
    fn merge_of_empty_slice_changes_nothing() {
        let mut session = SessionState::new();
        let report = merge_session(&mut session, &slice(None, None, &[], &[]));
        assert_eq!(report, SessionMerge::default());
        assert!(session.task.is_none());
    }
}
