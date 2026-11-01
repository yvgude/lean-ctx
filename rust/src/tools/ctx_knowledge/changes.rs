// SPDX-License-Identifier: Apache-2.0
//! `ctx_knowledge` actions that read the store's change journal and keep
//! anchored facts current without rewriting them:
//!
//! - `changes`: what changed since an LSN (`query=<lsn>`), or the facts a
//!   session wrote (`query=session`, or `query=session:<id>`);
//! - `repin`: re-anchor a fact marked to verify to the code as it is now,
//!   when the change under it does not make the fact wrong (`category`,
//!   `key`) — the fact keeps its text, nothing is retyped;
//! - `coverage`: the most edited files of the last 90 days that no fact is
//!   anchored to, a worklist for what to record next.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use crate::core::knowledge::anchors::{self, AnchorState};
use crate::core::knowledge::journal::{self, JournalRow};
use crate::core::knowledge::{FactReviewReason, ProjectKnowledge};

const DEFAULT_ROWS: usize = 30;
const MAX_ROWS: usize = 200;
const COVERAGE_FILES: usize = 15;
const CHURN_WINDOW: &str = "--since=90.days";

pub(crate) fn handle_changes(project_root: &str, query: Option<&str>, session_id: &str) -> String {
    let dir = match crate::core::knowledge::knowledge_dir_of(project_root) {
        Ok(dir) => dir,
        Err(error) => return format!("Error: {error}"),
    };
    let head = journal::head_lsn(&dir);
    let query = query.map(str::trim).unwrap_or("");
    let (since, session) = match query {
        "" => (head.saturating_sub(DEFAULT_ROWS as u64), None),
        "session" => (0, Some(session_id.to_owned())),
        other => match other.strip_prefix("session:") {
            Some(id) => (0, Some(id.trim().to_owned())),
            None => match other.parse::<u64>() {
                Ok(lsn) => (lsn, None),
                Err(_) => {
                    return "Error: query is an LSN, `session` or `session:<id>`".to_owned();
                }
            },
        },
    };
    let rows: Vec<JournalRow> = journal::rows_since(&dir, since, usize::MAX)
        .into_iter()
        .filter(|row| session.as_ref().is_none_or(|s| &row.session == s))
        .collect();
    let shown = &rows[rows.len().saturating_sub(MAX_ROWS)..];
    format_changes(
        head,
        since,
        session.as_deref(),
        shown,
        &visible_facts(project_root),
    )
}

/// How many journal rows the facts of `session_id` wrote (for a handoff).
pub(crate) fn session_change_count(project_root: &str, session_id: &str) -> usize {
    crate::core::knowledge::knowledge_dir_of(project_root).map_or(0, |dir| {
        journal::rows_since(&dir, 0, usize::MAX)
            .iter()
            .filter(|row| row.session == session_id)
            .count()
    })
}

/// Identities of the facts the caller's admitted view shows: a row names its
/// key only for those, so the journal never reveals a withheld fact.
fn visible_facts(project_root: &str) -> HashSet<(String, String, String)> {
    ProjectKnowledge::load(project_root)
        .map(|knowledge| {
            knowledge
                .facts
                .iter()
                .map(|fact| {
                    (
                        fact.category.clone(),
                        crate::core::redaction::redact_text(&fact.key),
                        fact.created_at.to_rfc3339(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn format_changes(
    head: u64,
    since: u64,
    session: Option<&str>,
    rows: &[JournalRow],
    visible: &HashSet<(String, String, String)>,
) -> String {
    let scope = match session {
        Some(id) => format!("written by session {id}"),
        None => format!("after lsn {since}"),
    };
    let mut out = format!(
        "knowledge journal: head lsn {head}, {} change(s) {scope}\n",
        rows.len()
    );
    for row in rows {
        let shown = visible.contains(&(row.category.clone(), row.key.clone(), row.created.clone()));
        let key = if shown {
            row.key.as_str()
        } else {
            "(not in view)"
        };
        let fields = if row.fields.is_empty() {
            String::new()
        } else {
            format!(" [{}]", row.fields.join(","))
        };
        let _ = writeln!(
            out,
            "  {} {} {}/{key}{fields}",
            row.lsn, row.op, row.category
        );
    }
    if session.is_none() && head > 0 {
        let _ = writeln!(out, "next: ctx_knowledge action=changes query={head}");
    }
    out
}

pub(crate) fn handle_repin(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
) -> String {
    let (Some(category), Some(key)) = (category, key) else {
        return "Error: repin needs category and key".to_owned();
    };
    let root = Path::new(project_root);
    let outcome = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let Some(fact) = knowledge
            .facts
            .iter_mut()
            .find(|f| f.category == category && f.key == key && f.valid_until.is_none())
        else {
            return Err(format!("no current fact {category}/{key}"));
        };
        if fact.anchors.is_empty() {
            return Err(format!("{category}/{key} has no anchors to repin"));
        }
        repin_fact(root, fact)
    });
    match outcome {
        Ok((_, Ok(summary))) => summary,
        Ok((_, Err(error))) | Err(error) => format!("Error: {error}"),
    }
}

/// Re-capture every anchor of `fact` where its code is now; a missing one
/// refuses the whole repin (the fact may be wrong, not just moved).
fn repin_fact(
    root: &Path,
    fact: &mut crate::core::knowledge::KnowledgeFact,
) -> Result<String, String> {
    let mut renewed = Vec::with_capacity(fact.anchors.len());
    let mut moved = 0usize;
    let mut changed = 0usize;
    for anchor in &fact.anchors {
        let current = match anchors::resolve(root, anchor) {
            AnchorState::Current => anchor.clone(),
            AnchorState::Changed => {
                changed += 1;
                anchors::capture(root, &anchor.path, anchor.symbol.as_deref())?
            }
            AnchorState::Moved { path } => {
                moved += 1;
                anchors::capture(root, &path, anchor.symbol.as_deref())?
            }
            AnchorState::Missing => {
                return Err(format!(
                    "{}{} is gone: rewrite or remove the fact instead",
                    anchor.path,
                    anchor
                        .symbol
                        .as_deref()
                        .map(|s| format!("::{s}"))
                        .unwrap_or_default()
                ));
            }
        };
        renewed.push(current);
    }
    fact.anchors = anchors::canonical(renewed);
    let cleared = fact.review.as_ref().is_some_and(|review| {
        matches!(
            review.reason,
            FactReviewReason::AnchorChanged
                | FactReviewReason::AnchorMoved
                | FactReviewReason::AnchorMissing
        )
    });
    if cleared {
        fact.clear_review();
    }
    Ok(format!(
        "repinned {}/{}: {} anchor(s), {changed} changed, {moved} moved{}",
        fact.category,
        fact.key,
        fact.anchors.len(),
        if cleared { ", no longer to verify" } else { "" }
    ))
}

pub(crate) fn handle_coverage(project_root: &str) -> String {
    let churn = match edit_counts(Path::new(project_root)) {
        Ok(churn) => churn,
        Err(error) => return format!("Error: {error}"),
    };
    let anchored: HashSet<String> = ProjectKnowledge::load(project_root)
        .map(|knowledge| {
            knowledge
                .facts
                .iter()
                .filter(|fact| fact.valid_until.is_none())
                .flat_map(|fact| fact.anchors.iter().map(|a| a.path.clone()))
                .collect()
        })
        .unwrap_or_default();
    format_coverage(&churn, &anchored)
}

fn format_coverage(churn: &BTreeMap<String, usize>, anchored: &HashSet<String>) -> String {
    let mut ranked: Vec<(&String, &usize)> = churn.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let top: Vec<_> = ranked.iter().take(50).collect();
    let covered = top
        .iter()
        .filter(|(path, _)| anchored.contains(*path))
        .count();
    let mut out = format!(
        "knowledge coverage: {covered} of the {} most edited files (90 days) carry anchored facts\n",
        top.len()
    );
    for (path, edits) in top
        .iter()
        .filter(|(path, _)| !anchored.contains(*path))
        .take(COVERAGE_FILES)
    {
        let _ = writeln!(out, "  {edits:>4} edits  {path}");
    }
    out
}

/// Edits per existing file over the churn window, from `git log`.
fn edit_counts(root: &Path) -> Result<BTreeMap<String, usize>, String> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args([
            "log",
            CHURN_WINDOW,
            "--name-only",
            "--no-renames",
            "--pretty=format:",
        ])
        .stdin(std::process::Stdio::null());
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(std::time::Duration::from_secs(15)),
        16 * 1024 * 1024,
        16_384,
    )
    .map_err(|_| "git log failed or exceeded its output bound".to_owned())?;
    if captured.timed_out || !captured.output.status.success() {
        return Err("git log failed (not a git checkout?)".to_owned());
    }
    let mut counts = BTreeMap::new();
    for line in String::from_utf8_lossy(&captured.output.stdout).lines() {
        let path = line.trim();
        if !path.is_empty() && root.join(path).is_file() {
            *counts.entry(path.to_owned()).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(lsn: u64, op: &str, key: &str, session: &str) -> JournalRow {
        JournalRow {
            lsn,
            at: "2026-10-31T10:00:00+00:00".into(),
            op: op.into(),
            category: "decision".into(),
            key: key.into(),
            created: "2026-10-31T09:00:00+00:00".into(),
            fields: if op == "changed" {
                vec!["value".into()]
            } else {
                Vec::new()
            },
            session: session.into(),
        }
    }

    #[test]
    fn a_change_list_names_only_facts_in_the_readers_view() {
        let visible: HashSet<_> = [(
            "decision".to_owned(),
            "db".to_owned(),
            "2026-10-31T09:00:00+00:00".to_owned(),
        )]
        .into();
        let out = format_changes(
            12,
            10,
            None,
            &[
                row(11, "changed", "db", "s1"),
                row(12, "removed", "secret-plan", "s2"),
            ],
            &visible,
        );
        assert!(out.starts_with("knowledge journal: head lsn 12, 2 change(s) after lsn 10\n"));
        assert!(out.contains("  11 changed decision/db [value]\n"));
        assert!(out.contains("  12 removed decision/(not in view)\n"));
        assert!(!out.contains("secret-plan"));
        assert!(out.ends_with("next: ctx_knowledge action=changes query=12\n"));
    }

    #[test]
    fn coverage_lists_the_most_edited_files_without_anchored_facts() {
        let churn: BTreeMap<String, usize> = [("a.rs", 9), ("b.rs", 5), ("c.rs", 5), ("d.rs", 1)]
            .into_iter()
            .map(|(p, n)| (p.to_owned(), n))
            .collect();
        let anchored: HashSet<String> = ["a.rs".to_owned()].into();
        let out = format_coverage(&churn, &anchored);
        assert!(out.starts_with("knowledge coverage: 1 of the 4 most edited files"));
        let listed: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|l| l.split_whitespace().last().unwrap())
            .collect();
        assert_eq!(listed, vec!["b.rs", "c.rs", "d.rs"]);
    }

    #[test]
    fn repin_follows_changed_code_and_refuses_a_missing_anchor() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.md"), "# A\n\nfirst\n").unwrap();
        let anchor = anchors::capture(root.path(), "a.md", None).unwrap();
        let mut fact: crate::core::knowledge::KnowledgeFact =
            serde_json::from_value(serde_json::json!({
                "category": "decision", "key": "doc", "value": "A says first",
                "source_session": "s", "confidence": 0.9,
                "created_at": "2026-10-31T09:00:00Z", "last_confirmed": "2026-10-31T09:00:00Z",
            }))
            .unwrap();
        fact.anchors = vec![anchor.clone()];
        fact.mark_review(
            crate::core::knowledge::FactReviewState::ToVerify,
            FactReviewReason::AnchorChanged,
            chrono::Utc::now(),
        );
        std::fs::write(root.path().join("a.md"), "# A\n\nfirst, reworded\n").unwrap();
        let summary = repin_fact(root.path(), &mut fact).unwrap();
        assert!(
            summary.contains("1 changed") && summary.contains("no longer to verify"),
            "{summary}"
        );
        assert_ne!(fact.anchors[0].digest, anchor.digest);
        assert!(fact.review.is_none());

        std::fs::remove_file(root.path().join("a.md")).unwrap();
        assert!(
            repin_fact(root.path(), &mut fact)
                .unwrap_err()
                .contains("is gone")
        );
    }
}
