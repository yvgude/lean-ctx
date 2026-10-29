// SPDX-License-Identifier: Apache-2.0
//! The project's local git history → knowledge facts (K1).
//!
//! Reads `git log` of the project (no merges, newest first, at most
//! `limit` commits) and keeps three kinds of commit, each as one fact with
//! the commit as its anchor (`git:<full sha>`) and the commit time as
//! `valid_from`:
//!
//! - a **revert** → `dead_end`: what was tried and taken back;
//! - a **fix** (`fix:`/`fix(…):`/`hotfix`, "Fix …") → `experience`: what broke
//!   and was repaired;
//! - a **decision** (the message decides, chooses, switches, replaces,
//!   migrates or drops) → `decision`.
//!
//! Everything else (features, chores, version bumps) is no fact. Each
//! commit MESSAGE is admitted as a whole for a store before anything is
//! taken from it — a message the policy or the gateway withholds or
//! restricts yields nothing and is counted — and each fact built from it is
//! admitted again (`super::push_repo_fact`). No file content, no diff and
//! no author identity is read.
//!
//! On a machine connected to Hive, `lean-ctx import git` queues the facts it
//! added or changed as import drafts, each sourced `import:git:<sha>`
//! ([`crate::core::hive::knowledge::import_draft`]).

use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};

use super::{ImportResult, ImportSource, RepoFact, SourceRefusal, push_repo_fact};

/// Commits read by default and at most.
pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 5000;
/// The `git log` output is read up to this many bytes; a log above it is
/// cut at the last complete commit.
const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

/// One commit as `git log` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub time: Option<DateTime<Utc>>,
    pub subject: String,
    pub body: String,
}

/// What a commit is to the knowledge store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Revert,
    Fix,
    Decision,
}

impl Kind {
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::Revert => "dead_end",
            Self::Fix => "experience",
            Self::Decision => "decision",
        }
    }
}

const DECISION_WORDS: [&str; 12] = [
    "decide",
    "decided",
    "decision",
    "adr",
    "switch to",
    "switched to",
    "replace ",
    "instead of",
    "migrate to",
    "migrated to",
    "drop support",
    "deprecate",
];

/// Classify one commit; `None` for everything that is no fact.
#[must_use]
pub fn classify(commit: &Commit) -> Option<Kind> {
    let subject = commit.subject.trim();
    let lower = subject.to_lowercase();
    if lower.starts_with("revert ") || commit.body.contains("This reverts commit ") {
        return Some(Kind::Revert);
    }
    let conventional_fix = ["fix", "bugfix", "hotfix"].iter().any(|kind| {
        lower.strip_prefix(kind).is_some_and(|rest| {
            rest.starts_with(':') || rest.starts_with('(') || rest.starts_with("!:")
        })
    });
    if conventional_fix || lower.starts_with("fix ") || lower.starts_with("fixed ") {
        return Some(Kind::Fix);
    }
    let message = format!("{lower}\n{}", commit.body.to_lowercase());
    if DECISION_WORDS.iter().any(|word| message.contains(word)) {
        return Some(Kind::Decision);
    }
    None
}

/// `fix(parser): handle x` → `handle x`; a plain subject stays as it is.
fn without_prefix(subject: &str) -> &str {
    match subject.split_once(": ") {
        Some((head, rest))
            if !head.is_empty()
                && head.len() <= 40
                && head
                    .chars()
                    .all(|c| c.is_alphanumeric() || "()!/-_.,".contains(c)) =>
        {
            rest
        }
        _ => subject,
    }
}

/// A git trailer (`Co-Authored-By: …`, `Signed-off-by: …`): one token, a
/// colon, a value.
fn is_trailer(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(key, _)| {
        !key.is_empty() && key.len() <= 40 && key.chars().all(|c| c.is_alphanumeric() || c == '-')
    })
}

/// Whether `line` names a person: an email address anywhere in it.
fn names_a_person(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| "<>()[],;\"'".contains(c));
        word.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.')
        })
    })
}

/// The body without its trailers (`Co-Authored-By`, `Signed-off-by`, …) and
/// without any line that names a person by email: identities never reach
/// admission, a fact or the store.
#[must_use]
pub fn without_identities(body: &str) -> String {
    body.lines()
        .filter(|line| !is_trailer(line.trim()) && !names_a_person(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// The first non-empty body line that is neither a trailer nor a revert note.
fn reason(body: &str) -> Option<&str> {
    body.lines().map(str::trim).find(|line| {
        !line.is_empty() && !line.starts_with("This reverts commit ") && !is_trailer(line)
    })
}

/// The statement a fact makes of `commit`.
#[must_use]
pub fn statement(commit: &Commit, kind: Kind) -> String {
    let short = &commit.sha[..commit.sha.len().min(12)];
    let subject = commit.subject.trim();
    let mut text = match kind {
        Kind::Revert => {
            let reverted = subject
                .strip_prefix("Revert \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(subject);
            format!("Tried and reverted: {}", without_prefix(reverted))
        }
        Kind::Fix => format!("Fixed: {}", without_prefix(subject)),
        Kind::Decision => format!("Decided: {}", without_prefix(subject)),
    };
    if let Some(reason) = reason(&commit.body) {
        text.push_str(" — ");
        text.push_str(reason);
    }
    text.push_str(&format!(" (commit {short})"));
    text
}

/// Parse the output of `git log --format=%H%x1f%cI%x1f%s%x1f%b%x1e`.
#[must_use]
pub fn parse_log(log: &str) -> Vec<Commit> {
    log.split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches('\n');
            let mut fields = record.splitn(4, FIELD);
            let sha = fields.next()?.trim();
            if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let time = fields
                .next()
                .and_then(|time| DateTime::parse_from_rfc3339(time.trim()).ok())
                .map(|time| time.with_timezone(&Utc));
            Some(Commit {
                sha: sha.to_owned(),
                time,
                subject: fields.next().unwrap_or_default().trim().to_owned(),
                body: fields.next().unwrap_or_default().trim().to_owned(),
            })
        })
        .collect()
}

/// `git log` of the repository at `root`; `None` when git is missing or
/// `root` is no repository.
fn read_log(root: &Path, limit: usize) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.quotepath=off",
            "log",
            "--no-merges",
            "--no-color",
            &format!("--max-count={limit}"),
            "--format=%H%x1f%cI%x1f%s%x1f%b%x1e",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()?
        .take(MAX_LOG_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let _ = child.kill();
    let status = child.wait().ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if bytes.len() >= MAX_LOG_BYTES {
        // Cut at the last complete commit.
        return Some(
            text.rsplit_once(RECORD)
                .map(|(head, _)| head.to_owned())
                .unwrap_or_default(),
        );
    }
    status.success().then_some(text)
}

/// Mine the git history of `project_root` (at most `limit` commits, capped
/// at [`MAX_LIMIT`]) into facts. Nothing is stored here.
#[must_use]
pub fn import(project_root: &Path, limit: usize) -> ImportResult {
    let mut result = ImportResult::default();
    let Some(log) = read_log(project_root, limit.clamp(1, MAX_LIMIT)) else {
        result.refuse(SourceRefusal::Unreadable);
        return result;
    };
    let gateway = crate::core::context_admission::carrier::gateway_snapshot();
    let rules = Some(gateway.admission_policy());
    let mut seen = HashSet::new();
    for mut commit in parse_log(&log) {
        result.sessions_found += 1;
        commit.body = without_identities(&commit.body);
        let Some(kind) = classify(&commit) else {
            continue;
        };
        // The whole message first: a marker anywhere in it decides for every
        // fact taken from it (G01).
        let message = format!("{}\n\n{}", commit.subject, commit.body);
        if crate::core::context_admission::carrier::admit_for_store_under(&message, rules)
            .ok()
            .is_none_or(|admitted| admitted != message)
        {
            result.facts_skipped += 1;
            continue;
        }
        let short = commit.sha[..commit.sha.len().min(12)].to_owned();
        push_repo_fact(
            &mut result,
            &mut seen,
            ImportSource::Git,
            &format!("git:{short}"),
            rules,
            RepoFact {
                category: kind.category(),
                key: format!("git:{}", commit.sha),
                value: statement(&commit, kind),
                confidence: 0.6,
                validity: None,
                valid_from: commit.time,
                anchors: Vec::new(),
            },
        );
    }
    result.facts_extracted = result.facts.len();
    result
}

#[cfg(test)]
#[path = "git_history_tests.rs"]
mod tests;
