// SPDX-License-Identifier: Apache-2.0
//! Evaluate decision `[[check]]`s against one file change.
//!
//! The check kinds are tree rules (docs/internal/decision-layer.md):
//! `forbid_path`: no file may match `paths`; `forbid_regex`: no file matching
//! `paths` may contain `pattern`; `require_regex`: at least one file matching
//! `paths` must contain `pattern`. Weave (D4) judges a whole candidate tree.
//! Here a check judges one file change and reports only what the change
//! *introduces*:
//!
//! - `forbid_path`: the change creates a file under `paths`.
//! - `forbid_regex`: the file holds more matches after than before.
//! - `require_regex`: the file held a match, holds none after, and no other
//!   file under `paths` holds one ([`OtherFiles`]).
//!
//! A violation already in the tree is not this change's, so the pre-edit hint
//! and `lean-ctx decisions check` stay quiet on legacy code and the strict
//! mode refuses only edits that make things worse.

use std::path::Path;

use super::{Check, CheckKind, Decision, DecisionSet};
use crate::core::pre_edit::{PreEditFinding, PreEditInput, Severity};

/// One violated check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub decision_id: String,
    pub title: String,
    pub statement: String,
    pub check_index: usize,
    pub kind: CheckKind,
    /// Root-relative path of the changed file.
    pub path: String,
    /// The check's message, else the decision's one-line statement.
    pub message: String,
    /// 1-based line of the offending text in the post-change content, when known.
    pub line: Option<usize>,
}

impl Violation {
    /// `dec-x (Title) at path:12 [forbid_regex]: message`
    pub fn render(&self) -> String {
        let at = self
            .line
            .map_or_else(|| self.path.clone(), |l| format!("{}:{l}", self.path));
        format!(
            "{} ({}) at {at} [{}]: {}",
            self.decision_id,
            self.title.trim(),
            self.kind.as_str(),
            self.message
        )
    }
}

/// Whether a file other than the changed one (the `&str`, root-relative)
/// that the check covers contains its pattern: the `require_regex` "at least
/// one file" rule. Answer `true` when that cannot be told cheaply; a missed
/// hint is better than a false one.
pub type OtherFiles<'a> = &'a dyn Fn(&Decision, &Check, &str) -> bool;

/// Most files a working-tree scan reads before it gives up (answering `true`).
const MAX_SCAN_FILES: usize = 5_000;
const MAX_SCAN_BYTES: u64 = 1_000_000;

/// [`OtherFiles`] over the working tree at `root` (gitignore respected).
pub fn worktree_other_files(root: &Path, d: &Decision, check: &Check, exclude: &str) -> bool {
    let Some(re) = check.regex() else {
        return true;
    };
    let mut seen = 0usize;
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(rel) = super::relative_path(root, entry.path()) else {
            continue;
        };
        if rel == exclude || rel.starts_with(".git/") || !check.covers(d, &rel) {
            continue;
        }
        seen += 1;
        if seen > MAX_SCAN_FILES || entry.metadata().is_ok_and(|m| m.len() > MAX_SCAN_BYTES) {
            return true;
        }
        if std::fs::read_to_string(entry.path()).is_ok_and(|t| re.is_match(&t)) {
            return true;
        }
    }
    false
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

/// The first match in `after` whose text is not among `before`'s matches
/// (by occurrence count), or the first match when that cannot be told apart.
fn first_new_match(re: &regex::Regex, before: &str, after: &str) -> Option<usize> {
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for m in re.find_iter(before) {
        *seen.entry(m.as_str()).or_default() += 1;
    }
    let mut first = None;
    for m in re.find_iter(after) {
        first.get_or_insert(m.start());
        match seen.get_mut(m.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => return Some(m.start()),
        }
    }
    first
}

fn violation(d: &Decision, i: usize, rel_path: &str, line: Option<usize>) -> Violation {
    let check = &d.checks[i];
    Violation {
        decision_id: d.id.clone(),
        title: d.title.clone(),
        statement: d.one_line(),
        check_index: i,
        kind: check.kind,
        path: rel_path.to_string(),
        message: check.message.clone().unwrap_or_else(|| d.one_line()),
        line,
    }
}

/// Violations a change of `rel_path` from `before` to `after` introduces,
/// over every active decision in `set`. `before = None`: the file is new;
/// `after = None`: the file is deleted (a deletion violates nothing).
/// `require_regex` looks for other files in the set's working tree.
pub fn evaluate_change(
    set: &DecisionSet,
    rel_path: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Vec<Violation> {
    let other = |d: &Decision, c: &Check, exclude: &str| {
        set.root
            .as_deref()
            .is_none_or(|root| worktree_other_files(root, d, c, exclude))
    };
    evaluate_change_with(set, rel_path, before, after, &other)
}

/// [`evaluate_change`] with an explicit [`OtherFiles`] (e.g. over a git tree).
pub fn evaluate_change_with(
    set: &DecisionSet,
    rel_path: &str,
    before: Option<&str>,
    after: Option<&str>,
    other_files: OtherFiles<'_>,
) -> Vec<Violation> {
    let Some(after) = after else {
        return Vec::new();
    };
    if before == Some(after) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in set.active().filter(|d| set.repo_matches(d)) {
        for (i, check) in d.checks.iter().enumerate() {
            if !check.covers(d, rel_path) {
                continue;
            }
            match check.kind {
                CheckKind::ForbidPath => {
                    if before.is_none() {
                        out.push(violation(d, i, rel_path, None));
                    }
                }
                CheckKind::ForbidRegex => {
                    let Some(re) = check.regex() else { continue };
                    let before_count = before.map_or(0, |b| re.find_iter(b).count());
                    if re.find_iter(after).count() > before_count {
                        let line = first_new_match(re, before.unwrap_or_default(), after)
                            .map(|off| line_of(after, off));
                        out.push(violation(d, i, rel_path, line));
                    }
                }
                CheckKind::RequireRegex => {
                    let Some(re) = check.regex() else { continue };
                    if !re.is_match(after)
                        && before.is_some_and(|b| re.is_match(b))
                        && !other_files(d, check, rel_path)
                    {
                        out.push(violation(d, i, rel_path, None));
                    }
                }
            }
        }
    }
    out
}

/// Findings for one pre-edit check of `set` under `enforce` (`hint` |
/// `refuse` | `off`). Introduced violations become hints (refusals when
/// strict). Path- or symbol-scoped decisions covering the file without a
/// violation become one [`Severity::Note`] (shown by LeanCTX's edit tools,
/// not by native hooks); repo-wide ones are left to the read suffix.
pub fn findings_for(
    set: &DecisionSet,
    rel_path: &str,
    input: &PreEditInput<'_>,
    enforce: &str,
) -> Vec<PreEditFinding> {
    if enforce == "off" {
        return Vec::new();
    }
    let severity = if enforce == "refuse" {
        Severity::Refuse
    } else {
        Severity::Hint
    };
    let violations = evaluate_change(set, rel_path, input.before, Some(input.after));
    let mut out: Vec<PreEditFinding> = violations
        .iter()
        .map(|v| PreEditFinding {
            source: "decision",
            id: v.decision_id.clone(),
            severity,
            text: format!("{} — decision: {}", v.render(), v.statement),
        })
        .collect();
    let scoped: Vec<&str> = set
        .for_path(rel_path)
        .into_iter()
        .filter(|d| !d.is_repo_wide() && !violations.iter().any(|v| v.decision_id == d.id))
        .map(|d| d.id.as_str())
        .collect();
    if !scoped.is_empty() {
        out.push(PreEditFinding {
            source: "decision",
            id: scoped.join(","),
            severity: Severity::Note,
            text: format!(
                "decisions in scope of {rel_path}: {} (ctx_knowledge action=decisions path={rel_path})",
                scoped.join(", ")
            ),
        });
    }
    out
}

/// The decisions checker registered in [`crate::core::pre_edit`]: finds the
/// project by its `.leanctx/decisions` directory and reads `[decisions]
/// enforce` from the config. Cheap (one directory probe per ancestor) when a
/// project has no decisions.
pub fn pre_edit_findings(input: &PreEditInput<'_>) -> Vec<PreEditFinding> {
    let Some(root) = super::find_root(input.path) else {
        return Vec::new();
    };
    let Some(rel) = super::relative_path(&root, input.path) else {
        return Vec::new();
    };
    let set = super::load(&root);
    if set.decisions.is_empty() {
        return Vec::new();
    }
    let cfg = crate::core::config::Config::load();
    findings_for(&set, &rel, input, cfg.decisions.enforce_mode())
}
