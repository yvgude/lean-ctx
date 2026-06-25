//! Build a deterministic session-summary candidate from `SessionState` (#292).
//!
//! No LLM, no randomness: the same session always yields the same summary, which
//! is what makes the benchmark/recall reproducible.

use crate::core::session::SessionState;

use super::record::SummaryCandidate;

const MAX_FILES: usize = 12;
const MAX_DECISIONS: usize = 6;
const MAX_FINDINGS: usize = 6;
const MAX_NEXT: usize = 6;

/// Build an owned candidate snapshot of the current session.
#[must_use]
pub fn build_candidate(session: &SessionState) -> SummaryCandidate {
    let title = session
        .task
        .as_ref()
        .map(|t| t.description.trim().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| inferred_title(session));

    let files: Vec<String> = session
        .files_touched
        .iter()
        .map(|f| f.path.clone())
        .take(MAX_FILES)
        .collect();
    let decisions: Vec<String> = session
        .decisions
        .iter()
        .rev()
        .map(|d| d.summary.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(MAX_DECISIONS)
        .collect();
    let findings: Vec<String> = session
        .findings
        .iter()
        .rev()
        .map(|f| f.summary.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(MAX_FINDINGS)
        .collect();
    let next_steps: Vec<String> = session
        .next_steps
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(MAX_NEXT)
        .collect();

    let body = render_body(session, &title, &files, &decisions, &findings, &next_steps);
    let has_content = session.task.is_some()
        || !files.is_empty()
        || !decisions.is_empty()
        || !findings.is_empty();

    SummaryCandidate {
        session_id: session.id.clone(),
        created_at: chrono::Utc::now(),
        title,
        body,
        files,
        decisions,
        next_steps,
        tool_calls: u64::from(session.stats.total_tool_calls),
        has_content,
    }
}

fn inferred_title(session: &SessionState) -> String {
    if let Some(modified) = session.files_touched.iter().find(|f| f.modified) {
        return format!("Worked on {}", short_path(&modified.path));
    }
    if let Some(first) = session.files_touched.first() {
        return format!("Explored {}", short_path(&first.path));
    }
    "Session".to_string()
}

fn short_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn render_body(
    session: &SessionState,
    title: &str,
    files: &[String],
    decisions: &[String],
    findings: &[String],
    next_steps: &[String],
) -> String {
    let mut out = String::new();
    if let Some(task) = &session.task {
        let pct = task
            .progress_pct
            .map(|p| format!(" ({p}%)"))
            .unwrap_or_default();
        out.push_str(&format!("Task: {}{}\n", task.description.trim(), pct));
    } else {
        out.push_str(&format!("Focus: {title}\n"));
    }

    let modified: Vec<&String> = session
        .files_touched
        .iter()
        .filter(|f| f.modified)
        .map(|f| &f.path)
        .collect();
    if !modified.is_empty() {
        out.push_str(&format!(
            "Modified ({}): {}\n",
            modified.len(),
            join_short(modified.iter().map(|s| s.as_str()), 8)
        ));
    }
    if !files.is_empty() {
        out.push_str(&format!(
            "Touched ({}): {}\n",
            files.len(),
            join_short(files.iter().map(String::as_str), 8)
        ));
    }
    if !decisions.is_empty() {
        out.push_str("Decisions:\n");
        for d in decisions {
            out.push_str(&format!("  - {d}\n"));
        }
    }
    if !findings.is_empty() {
        out.push_str("Findings:\n");
        for f in findings {
            out.push_str(&format!("  - {f}\n"));
        }
    }
    if !next_steps.is_empty() {
        out.push_str("Next:\n");
        for n in next_steps {
            out.push_str(&format!("  - {n}\n"));
        }
    }
    out.push_str(&format!(
        "Stats: {} tool calls, {} tokens saved\n",
        session.stats.total_tool_calls, session.stats.total_tokens_saved
    ));
    out
}

fn join_short<'a>(paths: impl Iterator<Item = &'a str>, max: usize) -> String {
    let names: Vec<String> = paths.take(max).map(short_path).collect();
    names.join(", ")
}
