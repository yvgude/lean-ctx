//! Import (resume) a context package into the current session (#293).

use std::path::Path;

use super::bundle::ContextPackage;
use crate::core::session::SessionState;

/// Resume a session from a previously saved context package.
///
/// Merges the package's session slice, summaries, and knowledge facts into
/// the provided live session. Does **not** overwrite; it augments.
pub(crate) fn resume_package(
    session: &mut SessionState,
    path: &Path,
) -> Result<ResumeReport, String> {
    let json = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    // Resuming is recovery: the package (possibly written elsewhere, or
    // before admission existed) passes the current policy first (G5).
    let json = crate::core::context_admission::recovery::admit_recovered(&json, "package")?;
    let pkg: ContextPackage = serde_json::from_str(&json).map_err(|e| format!("parse: {e}"))?;

    if !pkg.is_compatible() {
        return Err(format!(
            "package format_version {} is newer than supported ({})",
            pkg.format_version,
            super::bundle::FORMAT_VERSION
        ));
    }

    let mut report = ResumeReport::default();

    // Restore task if not already set.
    if session.task.is_none() {
        session.task.clone_from(&pkg.session.task);
        report.task_restored = session.task.is_some();
    }

    // Merge decisions (deduplicate by summary text).
    let existing_decisions: std::collections::HashSet<String> = session
        .decisions
        .iter()
        .map(|d| d.summary.clone())
        .collect();
    for d in &pkg.session.decisions {
        if !existing_decisions.contains(&d.summary) {
            session.decisions.push(d.clone());
            report.decisions_merged += 1;
        }
    }

    // Merge findings.
    let existing_findings: std::collections::HashSet<String> =
        session.findings.iter().map(|f| f.summary.clone()).collect();
    for f in &pkg.session.findings {
        if !existing_findings.contains(&f.summary) {
            session.findings.push(f.clone());
            report.findings_merged += 1;
        }
    }

    // Merge files (update or insert).
    for pf in &pkg.session.files {
        if !session.files_touched.iter().any(|f| f.path == pf.path) {
            session.files_touched.push(pf.clone());
            report.files_merged += 1;
        }
    }

    // Merge next_steps.
    let existing_next: std::collections::HashSet<String> =
        session.next_steps.iter().cloned().collect();
    for ns in &pkg.session.next_steps {
        if !existing_next.contains(ns) {
            session.next_steps.push(ns.clone());
            report.next_steps_merged += 1;
        }
    }

    // Restore test snapshot if none active.
    if session.test_results.is_none() {
        session.test_results.clone_from(&pkg.session.test_results);
    }

    // Replay knowledge facts and summaries into the package's project stores.
    // The session slice is already merged: a store that refuses the replay
    // (project absent here, policy, lock timeout) is reported as skipped
    // instead of failing a resume that has in part happened.
    if !pkg.knowledge.is_empty() {
        match replay_knowledge(&pkg) {
            Ok(merged) => report.knowledge_merged = merged,
            Err(reason) => report.skipped.push(format!("knowledge facts ({reason})")),
        }
    }
    if !pkg.summaries.is_empty() {
        match replay_summaries(&pkg) {
            Ok(merged) => report.summaries_merged = merged,
            Err(reason) => report.skipped.push(format!("summaries ({reason})")),
        }
    }

    report.source_session_id = pkg.session_id;
    Ok(report)
}

/// Report of what was merged.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResumeReport {
    pub source_session_id: String,
    pub task_restored: bool,
    pub decisions_merged: usize,
    pub findings_merged: usize,
    pub files_merged: usize,
    pub next_steps_merged: usize,
    pub knowledge_merged: usize,
    pub summaries_merged: usize,
    /// Package parts that were not replayed, each with its reason.
    pub skipped: Vec<String>,
}

impl ResumeReport {
    pub(crate) fn format(&self) -> String {
        let short = self
            .source_session_id
            .split('-')
            .next()
            .unwrap_or(&self.source_session_id);
        let mut lines = vec![format!("resumed from package [{}]", short)];
        if self.task_restored {
            lines.push("  + task restored".to_string());
        }
        if self.decisions_merged > 0 {
            lines.push(format!("  + {} decisions", self.decisions_merged));
        }
        if self.findings_merged > 0 {
            lines.push(format!("  + {} findings", self.findings_merged));
        }
        if self.files_merged > 0 {
            lines.push(format!("  + {} files", self.files_merged));
        }
        if self.next_steps_merged > 0 {
            lines.push(format!("  + {} next steps", self.next_steps_merged));
        }
        if self.knowledge_merged > 0 {
            lines.push(format!("  + {} knowledge facts", self.knowledge_merged));
        }
        if self.summaries_merged > 0 {
            lines.push(format!("  + {} summaries", self.summaries_merged));
        }
        for skipped in &self.skipped {
            lines.push(format!("  ! not replayed: {skipped}"));
        }
        lines.join("\n")
    }
}

/// Replays the package facts as one locked read-modify-write, so a parallel
/// writer of the same project store is never overwritten.
fn replay_knowledge(pkg: &ContextPackage) -> Result<usize, String> {
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    let (_, merged) =
        crate::core::knowledge::ProjectKnowledge::mutate_locked(&pkg.project_root, |pk| {
            let before = pk.facts.len();
            for fact in &pkg.knowledge {
                pk.remember_with_origin(
                    &fact.category,
                    &fact.key,
                    &fact.value,
                    &pkg.session_id,
                    fact.confidence,
                    &policy,
                    crate::core::knowledge::source_view::imported_origin(&fact.origin),
                );
            }
            pk.facts.len().saturating_sub(before)
        })
        .map_err(|e| format!("save knowledge: {e}"))?;
    Ok(merged)
}

fn replay_summaries(pkg: &ContextPackage) -> Result<usize, String> {
    let max_kept = crate::core::config::Config::load().summaries.max_kept as usize;
    crate::core::session_summary::store::SummaryStore::mutate_locked(&pkg.project_root, |store| {
        let before = store.summaries.len();
        let existing_ids: std::collections::HashSet<String> =
            store.summaries.iter().map(|s| s.id.clone()).collect();
        for s in &pkg.summaries {
            if !existing_ids.contains(&s.id) {
                store.push(s.clone(), max_kept);
            }
        }
        Some(store.summaries.len().saturating_sub(before))
    })
    .map(Option::unwrap_or_default)
    .map_err(|e| format!("save summaries: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_package::save_package;
    use crate::core::session::{Decision, FileTouched, Finding, TaskInfo};
    use chrono::Utc;

    fn sample_session() -> SessionState {
        let mut s = SessionState::new();
        s.task = Some(TaskInfo {
            description: "implement feature X".to_string(),
            intent: None,
            progress_pct: Some(50),
        });
        s.findings.push(Finding {
            file: Some("src/main.rs".to_string()),
            line: Some(10),
            summary: "entry point uses tokio".to_string(),
            timestamp: Utc::now(),
        });
        s.decisions.push(Decision {
            summary: "use bounded_lock for cache".to_string(),
            rationale: Some("avoid nested block_in_place".to_string()),
            timestamp: Utc::now(),
        });
        s.files_touched.push(FileTouched {
            path: "src/lib.rs".to_string(),
            file_ref: Some("F1".to_string()),
            read_count: 2,
            modified: true,
            last_mode: "full".to_string(),
            tokens: 1234,
            stale: false,
            context_item_id: None,
            summary: None,
        });
        s.next_steps.push("write tests".to_string());
        s
    }

    #[test]
    fn save_then_resume_round_trips_session_slice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg_path = dir.path().join("snapshot.ctx.json");
        let project_root = dir.path().to_string_lossy().to_string();

        let source = sample_session();
        let saved = save_package(
            &source,
            &project_root,
            Some("agent-1"),
            Some("test snapshot"),
            Some(&pkg_path),
        )
        .expect("save");
        assert!(saved.exists(), "package file should be written");

        let mut target = SessionState::new();
        let report = resume_package(&mut target, &saved).expect("resume");

        assert!(report.task_restored, "task should be restored");
        assert_eq!(report.decisions_merged, 1);
        assert_eq!(report.findings_merged, 1);
        assert_eq!(report.files_merged, 1);
        assert_eq!(report.next_steps_merged, 1);
        assert_eq!(
            target.task.as_ref().map(|t| t.description.as_str()),
            Some("implement feature X")
        );
        assert!(
            target
                .decisions
                .iter()
                .any(|d| d.summary == "use bounded_lock for cache")
        );
    }

    /// AP07-G2: the replay loaded and saved outside the project lock, so
    /// parallel imports into one project lost each other's facts.
    #[test]
    fn parallel_knowledge_replays_keep_every_fact() {
        use std::sync::{Arc, Barrier};
        const IMPORTS: usize = 8;
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("tempdir");
        let project_root = project.path().to_string_lossy().to_string();
        let barrier = Arc::new(Barrier::new(IMPORTS));
        let imports: Vec<_> = (0..IMPORTS)
            .map(|index| {
                let package = ContextPackage {
                    format_version: super::super::bundle::FORMAT_VERSION,
                    created_at: Utc::now(),
                    project_root: project_root.clone(),
                    session_id: format!("session-{index}"),
                    metadata: super::super::bundle::PackageMetadata {
                        agent_id: None,
                        description: None,
                        tool_calls: 0,
                        tokens_saved: 0,
                    },
                    session: super::super::bundle::SessionSlice {
                        task: None,
                        findings: Vec::new(),
                        decisions: Vec::new(),
                        files: Vec::new(),
                        next_steps: Vec::new(),
                        test_results: None,
                    },
                    summaries: Vec::new(),
                    knowledge: vec![super::super::bundle::KnowledgeFact {
                        origin: crate::core::knowledge::FactOrigin::Local,
                        category: "imported".into(),
                        key: format!("fact-{index}"),
                        value: format!("parallel import number {index}"),
                        confidence: 0.9,
                        created_at: Utc::now(),
                    }],
                };
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    replay_knowledge(&package)
                })
            })
            .collect();
        for import in imports {
            assert_eq!(import.join().expect("import thread"), Ok(1));
        }
        let stored = crate::core::knowledge::ProjectKnowledge::load(&project_root).expect("stored");
        for index in 0..IMPORTS {
            let key = format!("fact-{index}");
            assert!(
                stored.facts.iter().any(|fact| fact.key == key),
                "{key} was lost"
            );
        }
    }

    #[test]
    fn resume_missing_file_errors() {
        let mut target = SessionState::new();
        let result = resume_package(&mut target, Path::new("/nonexistent/pkg.ctx.json"));
        assert!(result.is_err(), "missing package must error, not panic");
    }

    #[test]
    fn resume_twice_does_not_duplicate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg_path = dir.path().join("snap.ctx.json");
        let project_root = dir.path().to_string_lossy().to_string();
        let saved = save_package(
            &sample_session(),
            &project_root,
            None,
            None,
            Some(&pkg_path),
        )
        .expect("save");

        let mut target = SessionState::new();
        resume_package(&mut target, &saved).expect("resume 1");
        let second = resume_package(&mut target, &saved).expect("resume 2");

        // Dedup by summary/path means a second resume merges nothing new.
        assert_eq!(second.decisions_merged, 0);
        assert_eq!(second.findings_merged, 0);
        assert_eq!(second.files_merged, 0);
    }
}
