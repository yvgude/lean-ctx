// SPDX-License-Identifier: Apache-2.0
//! Draft decisions from the local git history (`lean-ctx decisions suggest`).
//!
//! Reads commit and merge-request messages through [`super::run_git`] and
//! derives *draft* decision records that a human confirms. Nothing here ever
//! writes into `.leanctx/decisions`: drafts go to the review file
//! [`REVIEW_DIR`]/[`REVIEW_FILE`] only.
//!
//! Each draft carries a trust level:
//!
//! * [`Trust::Confirmed`]: the message states the decision explicitly
//!   (`Decision:` trailer, `Owner decision:` line or a quoted owner answer);
//!   the verbatim line is kept as evidence.
//! * [`Trust::Reconstructed`]: derived from policy wording ("never", "no
//!   longer", "from now on", ...) without an explicit marker.
//! * [`Trust::Recheck`]: the evidence changed since the last suggestion (the
//!   quote differs, or the commit is no longer in the history).
//!
//! Superseded drafts are detected from reverts (`This reverts commit <sha>`),
//! `Supersedes:` trailers and a later draft on the same topic with the
//! opposite polarity.
//!
//! [`confirm`] turns one draft into a record in `.leanctx/decisions` when a
//! human asks for it (`lean-ctx decisions confirm <draft-id>`). The record keeps
//! the verbatim quote and its commit in `[evidence]`; every later suggestion
//! re-reads that evidence and reports the record's trust (`re-check` when the
//! commit left the history or no longer holds the quote) and whether a later
//! revert, trailer or opposite statement supersedes it (contract §38).
//!
//! The output is a deterministic function of the history and the previous
//! review file: no timestamps, no counters, stable ordering (#498).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::core::decisions::{self, NewRecord, RecordEvidence};

/// Review location, relative to the repository root. Deliberately outside
/// `.leanctx/decisions`, so no loader or sync ever treats a draft as a record.
pub const REVIEW_DIR: &str = ".leanctx/decisions-review";
/// The machine-owned review file inside [`REVIEW_DIR`].
pub const REVIEW_FILE: &str = "suggested.json";
/// Schema of the review file and of `--json`.
pub const SCHEMA: &str = "leanctx.decisions-suggest/v1";
/// Commits scanned by default (most recent first, then put in order).
pub const DEFAULT_MAX_COMMITS: usize = 1000;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const MIN_LINE: usize = 12;
const MAX_LINE: usize = 240;

/// How far a draft can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    Confirmed,
    Reconstructed,
    Recheck,
}

impl Trust {
    pub fn label(self) -> &'static str {
        match self {
            Trust::Confirmed => "confirmed",
            Trust::Reconstructed => "reconstructed",
            Trust::Recheck => "re-check",
        }
    }
}

/// Where a draft comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// Full commit id.
    pub commit: String,
    /// Author date of the commit (`YYYY-MM-DD`), part of the history.
    pub date: String,
    /// `merge-request` when the commit carries a GitLab MR footer, else `commit`.
    pub kind: String,
    /// `!<n>` for a merge request, the short commit id otherwise.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The message line, verbatim (trimmed).
    pub quote: String,
    /// blake3 of commit + quote: changes when the evidence changes.
    pub hash: String,
}

/// One draft decision awaiting human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub statement: String,
    /// `draft` or `superseded`. Never `active`: only a human confirms.
    pub status: String,
    pub trust: Trust,
    pub evidence: Evidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    /// Further commits that repeat the same statement (e.g. an MR merge body).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
}

/// The review file / `--json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema: String,
    /// The scanned revision range, e.g. `HEAD` or `v1.0..HEAD`.
    pub range: String,
    /// Drafts skipped because `.leanctx/decisions` already has the statement.
    #[serde(default)]
    pub already_recorded: Vec<String>,
    /// Active records that a later revert, `Supersedes:` trailer or opposite
    /// statement supersedes: re-check them (ids, sorted).
    #[serde(default)]
    pub records_superseded: Vec<String>,
    /// Trust of the active records that carry `[evidence]` or are superseded,
    /// sorted by id.
    #[serde(default)]
    pub records: Vec<RecordTrust>,
    pub drafts: Vec<Draft>,
}

/// Trust of one confirmed record, re-read from the history on every run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordTrust {
    pub id: String,
    /// Absent for a record without `[evidence]` (written by hand).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<Trust>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck_reason: Option<String>,
    /// A draft id, or `revert <commit>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// What a suggestion needs of one record in `.leanctx/decisions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordInfo {
    pub id: String,
    pub statement: String,
    /// `YYYY-MM-DD`.
    pub decided_at: String,
    /// In force: status `active` and not superseded by another record.
    pub active: bool,
    pub evidence: Option<RecordEvidence>,
}

/// Options of one suggestion run.
#[derive(Debug, Clone)]
pub struct Options {
    pub since: Option<String>,
    pub max_commits: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            since: None,
            max_commits: DEFAULT_MAX_COMMITS,
        }
    }
}

/// One commit as read from `git log`.
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub date: String,
    pub message: String,
}

/// Read the commits of `range` (oldest first), at most `max` of them.
///
/// Records are NUL-separated (`-z`; git refuses NUL in a message), and the
/// ids are cross-checked against a plain `--format=%H` listing, so no message
/// text can forge a commit record (fake id or date).
pub fn read_commits(root: &Path, range: &str, max: usize) -> Result<Vec<Commit>, String> {
    let max_arg = format!("--max-count={}", max.max(1));
    let log = |format: &str, nul: bool| -> Result<String, String> {
        let mut args = vec!["log", "--no-color", "--no-show-signature", &max_arg];
        if nul {
            args.push("-z");
        }
        args.extend([format, range, "--"]);
        super::run_git(&args, root, GIT_TIMEOUT, &[])?.ok_stdout()
    };
    let ids: Vec<String> = log("--format=%H", false)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let out = log("--format=%H%x1f%as%x1f%B", true)?;
    let mut commits = Vec::with_capacity(ids.len());
    for chunk in out.split('\0').filter(|c| !c.trim().is_empty()) {
        let chunk = chunk.trim_start_matches(['\n', '\r']);
        let mut parts = chunk.splitn(3, '\x1f');
        let sha = parts.next().unwrap_or("").trim().to_string();
        let date = parts.next().unwrap_or("").trim().to_string();
        let message = parts.next().unwrap_or("").to_string();
        if !is_hex_id(&sha) || !is_iso_date(&date) {
            return Err(format!(
                "git log output for {range} did not parse (commit record {})",
                commits.len() + 1
            ));
        }
        commits.push(Commit { sha, date, message });
    }
    if commits.len() != ids.len() || commits.iter().zip(&ids).any(|(c, id)| &c.sha != id) {
        return Err(format!(
            "git log output for {range} did not parse: the commit records do not match \
             the history (did HEAD move? run again)"
        ));
    }
    commits.reverse();
    Ok(commits)
}

/// The repository root of `cwd`.
pub fn repo_root(cwd: &Path) -> Result<std::path::PathBuf, String> {
    let out =
        super::run_git(&["rev-parse", "--show-toplevel"], cwd, GIT_TIMEOUT, &[])?.ok_stdout()?;
    Ok(std::path::PathBuf::from(out.trim()))
}

/// Run a full suggestion: read history, compare with the previous review
/// file, and return the new report (not yet written).
pub fn suggest(root: &Path, opts: &Options) -> Result<Report, String> {
    let range = match &opts.since {
        Some(since) => {
            if since.starts_with('-') {
                return Err(format!("invalid --since ref: {since}"));
            }
            format!("{since}..HEAD")
        }
        None => "HEAD".to_string(),
    };
    let commits = read_commits(root, &range, opts.max_commits)?;
    let records = read_records(root);
    let previous = read_previous(root);
    let history = GitHistory::load(root);
    Ok(build_report(
        &range,
        &commits,
        &records,
        previous.as_ref(),
        &history,
    ))
}

/// What the history says about one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InHistory {
    /// In HEAD's history, with its message.
    Message(String),
    /// Not in HEAD's history (rewritten or dropped).
    Gone,
    /// Git could not answer (error, timeout, shallow clone): keep what was
    /// known and ask again next run.
    Unknown,
}

/// Reachability and messages of commits, as [`build_report`] needs them.
pub trait History {
    /// `Some(true|false)` when known, `None` when git could not answer.
    fn contains(&self, sha: &str) -> Option<bool>;
    /// The message of `sha` when it is in the history.
    fn message(&self, sha: &str) -> InHistory;
}

/// [`History`] of a repository: one `git rev-list HEAD` for reachability,
/// one `git log -1` per message asked for.
pub struct GitHistory {
    root: std::path::PathBuf,
    /// `None` when `rev-list` failed: every answer is [`InHistory::Unknown`].
    reachable: Option<HashSet<String>>,
    /// In a shallow clone a missing commit may be beyond the boundary.
    shallow: bool,
}

impl GitHistory {
    pub fn load(root: &Path) -> Self {
        let reachable = super::run_git(&["rev-list", "HEAD"], root, GIT_TIMEOUT, &[])
            .ok()
            .and_then(|o| o.ok_stdout().ok())
            .map(|out| {
                out.lines()
                    .map(|l| l.trim().to_ascii_lowercase())
                    .filter(|l| !l.is_empty())
                    .collect()
            });
        let shallow = super::run_git(
            &["rev-parse", "--is-shallow-repository"],
            root,
            GIT_TIMEOUT,
            &[],
        )
        .ok()
        .and_then(|o| o.ok_stdout().ok())
        .is_none_or(|out| out.trim() != "false");
        Self {
            root: root.to_path_buf(),
            reachable,
            shallow,
        }
    }

    /// The full id of `sha` when it is reachable.
    fn resolve(&self, sha: &str) -> Result<Option<String>, ()> {
        if !is_hex_id(sha) {
            return Ok(None);
        }
        let Some(set) = &self.reachable else {
            return Err(());
        };
        let sha = sha.to_ascii_lowercase();
        if set.contains(&sha) {
            return Ok(Some(sha));
        }
        if sha.len() < 40 {
            let mut hits = set.iter().filter(|full| full.starts_with(&sha));
            if let (Some(full), None) = (hits.next(), hits.next()) {
                return Ok(Some(full.clone()));
            }
        }
        if self.shallow { Err(()) } else { Ok(None) }
    }
}

impl History for GitHistory {
    fn contains(&self, sha: &str) -> Option<bool> {
        self.resolve(sha).ok().map(|full| full.is_some())
    }

    fn message(&self, sha: &str) -> InHistory {
        match self.resolve(sha) {
            Err(()) => InHistory::Unknown,
            Ok(None) => InHistory::Gone,
            Ok(Some(full)) => match super::run_git(
                &[
                    "log",
                    "-1",
                    "--no-color",
                    "--no-show-signature",
                    "--format=%B",
                    &full,
                    "--",
                ],
                &self.root,
                GIT_TIMEOUT,
                &[],
            )
            .ok()
            .and_then(|o| o.ok_stdout().ok())
            {
                Some(m) => InHistory::Message(m),
                None => InHistory::Unknown,
            },
        }
    }
}

/// 7 to 64 hex digits.
fn is_hex_id(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A calendar date `YYYY-MM-DD` (month 01-12, day valid for the month).
pub fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[r];
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// Whether `message` still yields exactly this (quote, statement, trust).
fn extracts(message: &str, quote: &str, statement: &str, trust: Trust) -> bool {
    extract(message)
        .into_iter()
        .any(|(q, st, t)| q == quote && st == statement && t == trust)
}

/// Author date (`YYYY-MM-DD`) of `sha`, read from git.
fn commit_date(root: &Path, sha: &str) -> Result<String, String> {
    if !is_hex_id(sha) {
        return Err(format!("invalid commit id {}", short(sha)));
    }
    let out = super::run_git(
        &["log", "-1", "--no-color", "--format=%as", sha, "--"],
        root,
        GIT_TIMEOUT,
        &[],
    )?
    .ok_stdout()?;
    let date = out.trim();
    if is_iso_date(date) {
        Ok(date.to_string())
    } else {
        Err(format!(
            "git could not read the date of commit {}",
            short(sha)
        ))
    }
}

/// The trust stored in a record's `[evidence]`.
fn stored_trust(label: &str) -> Trust {
    if label == Trust::Reconstructed.label() {
        Trust::Reconstructed
    } else {
        Trust::Confirmed
    }
}

/// Pure core of [`suggest`]: history in, report out.
pub fn build_report(
    range: &str,
    commits: &[Commit],
    records: &[RecordInfo],
    previous: Option<&Report>,
    history: &dyn History,
) -> Report {
    let mut drafts: Vec<Draft> = Vec::new();
    let mut by_norm: BTreeMap<String, usize> = BTreeMap::new();
    let mut reverts: Vec<(String, String)> = Vec::new();
    let mut supersede_refs: Vec<(String, String)> = Vec::new();
    let mut already: BTreeSet<String> = BTreeSet::new();
    let mut records_superseded: BTreeSet<String> = BTreeSet::new();
    let mut record_superseded_by: BTreeMap<String, String> = BTreeMap::new();
    let record_norms: BTreeMap<String, String> = records
        .iter()
        .map(|r| (normalize(&r.statement), r.id.clone()))
        .collect();

    for commit in commits {
        if let Some(target) = reverted_commit(&commit.message) {
            reverts.push((target, commit.sha.clone()));
        }
        let (kind, reference) = source_of(commit);
        let trailer_supersedes = supersedes_trailers(&commit.message);
        let first_new = drafts.len();
        for (quote, statement, trust) in extract(&commit.message) {
            let norm = normalize(&statement);
            if norm.is_empty() {
                continue;
            }
            if let Some(id) = record_norms.get(&norm) {
                already.insert(id.clone());
                continue;
            }
            if let Some(&idx) = by_norm.get(&norm) {
                let d = &mut drafts[idx];
                if d.evidence.commit != commit.sha && !d.also_in.contains(&commit.sha) {
                    d.also_in.push(commit.sha.clone());
                }
                // An explicit statement later upgrades a reconstructed one;
                // the evidence moves to that commit, so the quote stays in it.
                if trust == Trust::Confirmed && d.trust == Trust::Reconstructed {
                    let old = std::mem::replace(&mut d.evidence.commit, commit.sha.clone());
                    d.also_in.retain(|c| c != &commit.sha);
                    if !d.also_in.contains(&old) {
                        d.also_in.push(old);
                    }
                    d.trust = Trust::Confirmed;
                    // Statement and quote both come from that commit, so
                    // `confirm` finds the exact extraction there.
                    d.title = title_of(&statement);
                    d.statement = statement;
                    d.evidence.date.clone_from(&commit.date);
                    d.evidence.kind.clone_from(&kind);
                    d.evidence.reference.clone_from(&reference);
                    d.evidence.hash = evidence_hash(&commit.sha, &quote);
                    d.evidence.quote = quote;
                }
                continue;
            }
            // Two statements of one commit can share their slug; suffix in
            // extraction order so ids stay unique and deterministic.
            let base = draft_id(&statement, &commit.sha);
            let mut id = base.clone();
            let mut n = 2;
            while drafts.iter().any(|d| d.id == id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            by_norm.insert(norm, drafts.len());
            drafts.push(Draft {
                id,
                title: title_of(&statement),
                statement,
                status: "draft".into(),
                trust,
                evidence: Evidence {
                    commit: commit.sha.clone(),
                    date: commit.date.clone(),
                    kind: kind.clone(),
                    reference: reference.clone(),
                    hash: evidence_hash(&commit.sha, &quote),
                    quote,
                },
                recheck_reason: None,
                superseded_by: None,
                supersedes: Vec::new(),
                also_in: Vec::new(),
            });
        }
        for target in trailer_supersedes {
            if records.iter().any(|r| r.active && r.id == target) {
                let by = drafts
                    .get(first_new)
                    .map_or_else(|| short(&commit.sha).to_string(), |d| d.id.clone());
                record_superseded_by.entry(target.clone()).or_insert(by);
            }
            if drafts.len() > first_new {
                for d in &mut drafts[first_new..] {
                    if !d.supersedes.contains(&target) {
                        d.supersedes.push(target.clone());
                    }
                }
                supersede_refs.push((target, drafts[first_new].id.clone()));
            }
        }
    }

    // Reverts supersede every draft of the reverted commit.
    for (target, revert_sha) in &reverts {
        for d in &mut drafts {
            if d.superseded_by.is_none() && sha_matches(&d.evidence.commit, target) {
                d.superseded_by = Some(format!("revert {}", short(revert_sha)));
            }
        }
    }
    // `Supersedes: <draft id | commit>` trailers.
    for (target, newer) in &supersede_refs {
        for d in &mut drafts {
            if &d.id != newer
                && d.superseded_by.is_none()
                && (d.id == *target || sha_matches(&d.evidence.commit, target))
            {
                d.superseded_by = Some(newer.clone());
            }
        }
    }
    // Same topic, opposite polarity, later commit: the later one wins.
    let topics: Vec<(Vec<String>, bool)> = drafts.iter().map(|d| topic_of(&d.statement)).collect();
    for later in 1..drafts.len() {
        let (topic, neg) = &topics[later];
        if topic.is_empty() {
            continue;
        }
        let newer_id = drafts[later].id.clone();
        for earlier in 0..later {
            let (t, n) = &topics[earlier];
            if t == topic && n != neg && drafts[earlier].superseded_by.is_none() {
                drafts[earlier].superseded_by = Some(newer_id.clone());
                if !drafts[later].supersedes.contains(&drafts[earlier].id) {
                    let id = drafts[earlier].id.clone();
                    drafts[later].supersedes.push(id);
                }
            }
        }
    }
    for d in &mut drafts {
        if d.superseded_by.is_some() {
            d.status = "superseded".into();
        }
    }

    // Confirmed records: re-read their evidence and look for what replaces them.
    let mut record_rows: Vec<RecordTrust> = Vec::new();
    for rec in records.iter().filter(|r| r.active) {
        let mut superseded_by = record_superseded_by.get(&rec.id).cloned();
        if superseded_by.is_none()
            && let Some(ev) = &rec.evidence
        {
            superseded_by = reverts
                .iter()
                .find(|(target, _)| same_commit(&ev.commit, target))
                .map(|(_, revert_sha)| format!("revert {}", short(revert_sha)));
        }
        if superseded_by.is_none() {
            let (topic, neg) = topic_of(&rec.statement);
            if !topic.is_empty()
                && let Some(d) = drafts.iter_mut().rev().find(|d| {
                    let (t, n) = topic_of(&d.statement);
                    d.superseded_by.is_none()
                        && d.evidence.date.as_str() >= rec.decided_at.as_str()
                        && t == topic
                        && n != neg
                })
            {
                if !d.supersedes.contains(&rec.id) {
                    d.supersedes.push(rec.id.clone());
                }
                superseded_by = Some(d.id.clone());
            }
        }
        let (trust, recheck_reason) = match &rec.evidence {
            None => (None, None),
            Some(ev) => match history.message(&ev.commit) {
                // Git could not answer: pass the stored trust on, ask again
                // next run.
                InHistory::Unknown => (Some(stored_trust(&ev.trust)), None),
                InHistory::Gone => (
                    Some(Trust::Recheck),
                    Some(format!(
                        "evidence commit {} is no longer in the history",
                        short(&ev.commit)
                    )),
                ),
                InHistory::Message(message) if quote_in(&message, &ev.quote) => {
                    (Some(stored_trust(&ev.trust)), None)
                }
                InHistory::Message(_) => (
                    Some(Trust::Recheck),
                    Some(format!(
                        "the quote is no longer in the message of commit {}",
                        short(&ev.commit)
                    )),
                ),
            },
        };
        if superseded_by.is_some() {
            records_superseded.insert(rec.id.clone());
        }
        if trust.is_some() || superseded_by.is_some() {
            record_rows.push(RecordTrust {
                id: rec.id.clone(),
                trust,
                recheck_reason,
                superseded_by,
            });
        }
    }
    record_rows.sort_by(|a, b| a.id.cmp(&b.id));

    // Compare with the last suggestion.
    if let Some(prev) = previous {
        let current: BTreeMap<String, usize> = drafts
            .iter()
            .enumerate()
            .map(|(i, d)| (d.id.clone(), i))
            .collect();
        for old in &prev.drafts {
            if let Some(&i) = current.get(&old.id) {
                let d = &mut drafts[i];
                if d.evidence.hash != old.evidence.hash {
                    d.trust = Trust::Recheck;
                    d.recheck_reason = Some(format!(
                        "evidence changed since the last suggestion (was: \"{}\")",
                        old.evidence.quote
                    ));
                } else if old.trust == Trust::Recheck {
                    // Stays flagged until a human clears the review file.
                    d.trust = Trust::Recheck;
                    d.recheck_reason.clone_from(&old.recheck_reason);
                }
            } else {
                let norm = normalize(&old.statement);
                if let Some(id) = record_norms.get(&norm) {
                    // Confirmed since the last suggestion.
                    already.insert(id.clone());
                    continue;
                }
                if let Some(&i) = by_norm.get(&norm) {
                    // The same statement, now drafted from another commit
                    // (a narrower --since): one draft per statement.
                    let d = &mut drafts[i];
                    let c = &old.evidence.commit;
                    if !same_commit(&d.evidence.commit, c) && !d.also_in.contains(c) {
                        d.also_in.push(c.clone());
                        d.also_in.sort();
                    }
                    continue;
                }
                let mut d = old.clone();
                // The file is untrusted: the title is derived again.
                d.title = title_of(&d.statement);
                // Outside the scanned range: keep it while it is in the
                // history and its commit still yields this exact draft,
                // or while git cannot say (asked again next run).
                match history.message(&old.evidence.commit) {
                    InHistory::Gone => {
                        d.trust = Trust::Recheck;
                        d.recheck_reason = Some(format!(
                            "evidence commit {} is no longer in the history",
                            short(&old.evidence.commit)
                        ));
                    }
                    InHistory::Message(m)
                        if old.trust != Trust::Recheck
                            && !extracts(&m, &old.evidence.quote, &old.statement, old.trust) =>
                    {
                        d.trust = Trust::Recheck;
                        d.recheck_reason = Some(format!(
                            "the draft no longer matches commit {}",
                            short(&old.evidence.commit)
                        ));
                    }
                    InHistory::Message(_) | InHistory::Unknown => {}
                }
                by_norm.insert(norm, drafts.len());
                drafts.push(d);
            }
        }
    }

    drafts.sort_by(|a, b| {
        a.evidence
            .date
            .cmp(&b.evidence.date)
            .then_with(|| a.id.cmp(&b.id))
    });
    Report {
        schema: SCHEMA.into(),
        range: range.into(),
        already_recorded: already.into_iter().collect(),
        records_superseded: records_superseded.into_iter().collect(),
        records: record_rows,
        drafts,
    }
}

/// Write `report` to the review file. Refuses a symlinked `.leanctx` or
/// review directory, so the write cannot escape the repository.
pub fn write_review(root: &Path, report: &Report) -> Result<std::path::PathBuf, String> {
    let leanctx = root.join(".leanctx");
    let dir = root.join(REVIEW_DIR);
    for p in [&leanctx, &dir] {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!("{} is not a plain directory", p.display()));
            }
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{REVIEW_DIR}: {e}"))?;
    let file = dir.join(REVIEW_FILE);
    if let Ok(meta) = std::fs::symlink_metadata(&file) {
        if !meta.file_type().is_file() {
            return Err(format!("{REVIEW_DIR}/{REVIEW_FILE} is not a regular file"));
        }
    }
    // Drafts stay local: the review directory ignores itself. `create_new`
    // never follows a planted link.
    let ignore = dir.join(".gitignore");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ignore)
    {
        Ok(mut f) => f
            .write_all(b"# lean-ctx decision drafts stay local (contract \xc2\xa738)\n*\n")
            .map_err(|e| format!("{}: {e}", ignore.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", ignore.display())),
    }
    let body = to_json(report)?;
    // A fresh, randomly named file (O_EXCL) in the same directory, renamed
    // over the review file: a planted temp-file link is never written through.
    let mut tmp = tempfile::Builder::new()
        .prefix(".suggested.")
        .suffix(".tmp")
        .tempfile_in(&dir)
        .map_err(|e| format!("{REVIEW_DIR}: {e}"))?;
    tmp.write_all(body.as_bytes())
        .map_err(|e| format!("{REVIEW_DIR}: {e}"))?;
    tmp.persist(&file)
        .map_err(|e| format!("{}: {}", file.display(), e.error))?;
    Ok(file)
}

/// Deterministic JSON of a report (pretty, trailing newline).
pub fn to_json(report: &Report) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map(|s| s + "\n")
        .map_err(|e| e.to_string())
}

fn read_previous(root: &Path) -> Option<Report> {
    let file = root.join(REVIEW_DIR).join(REVIEW_FILE);
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if !meta.file_type().is_file() || meta.len() > 16 * 1024 * 1024 {
        return None;
    }
    let text = std::fs::read_to_string(&file).ok()?;
    let mut report = serde_json::from_str::<Report>(&text)
        .ok()
        .filter(|r| r.schema == SCHEMA)?;
    // The file sits in the working tree and may come from anyone: keep only
    // drafts whose ids and commit ids have the shape this module writes.
    report.drafts.retain(|d| {
        is_draft_id(&d.id) && is_hex_id(&d.evidence.commit) && is_iso_date(&d.evidence.date)
    });
    for d in &mut report.drafts {
        d.also_in.retain(|c| is_hex_id(c));
        d.supersedes.retain(|s| is_draft_id(s) || is_hex_id(s));
    }
    report.already_recorded.retain(|id| is_draft_id(id));
    Some(report)
}

/// `[a-z0-9-]`, at most 128 bytes: the ids drafts and records use.
fn is_draft_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The records of `.leanctx/decisions` (D1 loader; invalid records are left
/// out here and reported by `lean-ctx decisions check`).
pub fn read_records(root: &Path) -> Vec<RecordInfo> {
    let set = decisions::load(root);
    set.decisions
        .iter()
        .map(|d| RecordInfo {
            id: d.id.clone(),
            statement: d.statement.clone(),
            decided_at: d.decided_at.clone(),
            active: set.effective_status(d) == decisions::Status::Active,
            evidence: d.evidence.clone(),
        })
        .collect()
}

/// Whether `message` still holds `quote` as one (trimmed) line.
fn quote_in(message: &str, quote: &str) -> bool {
    let quote = quote.trim();
    !quote.is_empty() && message.lines().any(|l| l.trim() == quote)
}

/// Two commit ids (full or abbreviated, at least 7 digits) name one commit.
fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    a.len() >= 7 && b.len() >= 7 && (a.starts_with(&b) || b.starts_with(&a))
}

/// Options of `lean-ctx decisions confirm`.
#[derive(Debug, Clone, Default)]
pub struct ConfirmOptions {
    /// Record id or slug (`dec-` is added); default: from the statement.
    pub id: Option<String>,
    /// `owner`, `team` or `review`; default: `owner` when the quote names the
    /// owner, else `team`.
    pub decided_by: Option<String>,
}

/// What [`confirm`] recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedRecord {
    pub id: String,
    /// Root-relative record file.
    pub file: String,
    pub trust: Trust,
    /// Records the new one supersedes.
    pub supersedes: Vec<String>,
}

/// Turn draft `draft_id` of the review file into a record in
/// `.leanctx/decisions` (a human asked for it). Refuses a superseded draft, a
/// draft flagged `re-check`, and evidence that no longer holds; never
/// overwrites a record. The draft leaves the review file and its record id
/// joins `already_recorded`.
pub fn confirm(
    root: &Path,
    draft_id: &str,
    opts: &ConfirmOptions,
) -> Result<ConfirmedRecord, String> {
    let mut report = read_previous(root).ok_or_else(|| {
        format!("no review file {REVIEW_DIR}/{REVIEW_FILE}: run `lean-ctx decisions suggest` first")
    })?;
    let idx = report
        .drafts
        .iter()
        .position(|d| d.id == draft_id)
        .ok_or_else(|| format!("no draft `{draft_id}` in {REVIEW_DIR}/{REVIEW_FILE}"))?;
    let draft = report.drafts[idx].clone();
    if let Some(by) = &draft.superseded_by {
        return Err(format!(
            "draft `{draft_id}` is superseded by {by}; confirm the newer decision instead"
        ));
    }
    if draft.trust == Trust::Recheck {
        return Err(format!(
            "draft `{draft_id}` needs a re-check ({}); read the commit, then remove \
             {REVIEW_DIR}/{REVIEW_FILE} and run `lean-ctx decisions suggest` again",
            draft
                .recheck_reason
                .as_deref()
                .unwrap_or("evidence changed")
        ));
    }
    // Everything below uses the full id git resolved, never the id from the
    // file (an abbreviated id may also name a branch or tag).
    let history = GitHistory::load(root);
    let gone = || {
        format!(
            "evidence commit {} is no longer in the history",
            short(&draft.evidence.commit)
        )
    };
    let unknown = || {
        format!(
            "git could not read commit {}; try again",
            short(&draft.evidence.commit)
        )
    };
    let full = match history.resolve(&draft.evidence.commit) {
        Ok(Some(full)) => full,
        Ok(None) => return Err(gone()),
        Err(()) => return Err(unknown()),
    };
    let message = match history.message(&full) {
        InHistory::Message(m) => m,
        InHistory::Gone => return Err(gone()),
        InHistory::Unknown => return Err(unknown()),
    };
    if !quote_in(&message, &draft.evidence.quote) {
        return Err(format!(
            "the quote is no longer in the message of commit {}",
            short(&draft.evidence.commit)
        ));
    }
    // The review file is untrusted (contract §38): the commit must yield this
    // exact (quote, statement, trust) again; date, title and source come from
    // git, not from the file.
    if !extracts(
        &message,
        &draft.evidence.quote,
        &draft.statement,
        draft.trust,
    ) {
        return Err(format!(
            "draft `{draft_id}` does not match commit {}: the quote, statement or trust in \
             {REVIEW_DIR}/{REVIEW_FILE} differs from the history; run `lean-ctx decisions suggest` again",
            short(&draft.evidence.commit)
        ));
    }
    let date = commit_date(root, &full)?;
    let source = Commit {
        sha: full.clone(),
        date: date.clone(),
        message: message.clone(),
    };
    let (kind, reference) = source_of(&source);
    let id = match &opts.id {
        Some(slug) => decisions::id_from_slug(slug)
            .ok_or_else(|| format!("`{slug}` has no letters or digits to build an id from"))?,
        None => record_id_of(&draft.statement).ok_or_else(|| {
            "no record id can be built from the statement; pass --id <slug>".to_string()
        })?,
    };
    let decided_by = match opts.decided_by.as_deref() {
        Some(v @ ("owner" | "team" | "review")) => v.to_string(),
        Some(other) => {
            return Err(format!(
                "--decided-by `{other}`: expected owner, team or review"
            ));
        }
        None if draft.evidence.quote.to_ascii_lowercase().contains("owner") => "owner".into(),
        None => "team".into(),
    };
    let set = decisions::load(root);
    if set.get(&id).is_some() {
        return Err(format!(
            "{}/{id}.toml exists; pass another --id",
            decisions::DECISIONS_DIR
        ));
    }
    // `supersedes` of the file only narrows what git yields for this commit:
    // an edited review file can never retire a record by itself.
    let from_git = supersedes_from_git(&message, &draft.statement, &date, &read_records(root));
    let mut supersedes: Vec<String> = draft
        .supersedes
        .iter()
        .filter(|s| from_git.contains(*s) && set.get(s).is_some())
        .cloned()
        .collect();
    supersedes.sort();
    supersedes.dedup();
    let (source_kind, source_ref) = if kind == "merge-request" {
        ("mr", reference)
    } else {
        ("commit", short(&full).to_string())
    };
    let record = NewRecord {
        id: id.clone(),
        title: title_of(&draft.statement),
        statement: draft.statement.clone(),
        supersedes: supersedes.clone(),
        decided_by,
        decided_at: date,
        source_kind: source_kind.into(),
        source_ref,
        evidence: RecordEvidence {
            commit: full.clone(),
            quote: draft.evidence.quote.clone(),
            trust: draft.trust.label().into(),
        },
    };
    let text = decisions::render_record(&record);
    let rel = format!("{}/{id}.toml", decisions::DECISIONS_DIR);
    decisions::parse_decision(&text, &id, &rel)
        .map_err(|e| format!("the record would not be valid: {e}"))?;
    write_record(root, &id, &text)?;

    report.drafts.remove(idx);
    if !report.already_recorded.contains(&id) {
        report.already_recorded.push(id.clone());
        report.already_recorded.sort();
    }
    write_review(root, &report)
        .map_err(|e| format!("recorded {rel}, but the review file was not updated: {e}"))?;
    Ok(ConfirmedRecord {
        id,
        file: rel,
        trust: draft.trust,
        supersedes,
    })
}

/// Records a draft of `message` may supersede, read from git as
/// [`build_report`] does: the commit's `Supersedes:` trailers, and active
/// records of the same topic and opposite polarity decided on or before `date`.
fn supersedes_from_git(
    message: &str,
    statement: &str,
    date: &str,
    records: &[RecordInfo],
) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = supersedes_trailers(message)
        .into_iter()
        .filter(|t| records.iter().any(|r| &r.id == t))
        .collect();
    let (topic, neg) = topic_of(statement);
    if !topic.is_empty() {
        for r in records.iter().filter(|r| r.active) {
            let (t, n) = topic_of(&r.statement);
            if t == topic && n != neg && date >= r.decided_at.as_str() {
                out.insert(r.id.clone());
            }
        }
    }
    out
}

/// `dec-<first words of the statement>`, at most 6 words.
fn record_id_of(statement: &str) -> Option<String> {
    let normalized = normalize(statement);
    let words: Vec<&str> = normalized
        .split_whitespace()
        .filter(|w| w.is_ascii())
        .take(6)
        .collect::<Vec<_>>();
    let norm = words.join(" ");
    decisions::id_from_slug(&norm).or_else(|| {
        decisions::id_from_slug(
            &norm
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" "),
        )
    })
}

/// Create `<root>/.leanctx/decisions/<id>.toml` (never overwriting), only in
/// a records directory that stays inside the project.
fn write_record(root: &Path, id: &str, text: &str) -> Result<(), String> {
    let leanctx = root.join(".leanctx");
    if let Ok(meta) = std::fs::symlink_metadata(&leanctx)
        && (meta.file_type().is_symlink() || !meta.is_dir())
    {
        return Err(format!("{} is not a plain directory", leanctx.display()));
    }
    decisions::list_records(root)?;
    let dir = root.join(decisions::DECISIONS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", decisions::DECISIONS_DIR))?;
    decisions::list_records(root)?;
    let file = dir.join(format!("{id}.toml"));
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "{}/{id}.toml exists; pass another --id",
                    decisions::DECISIONS_DIR
                )
            } else {
                format!("{}: {e}", file.display())
            }
        })
}

/// Decision lines of one message: (verbatim quote, statement, trust).
pub fn extract(message: &str) -> Vec<(String, String, Trust)> {
    let mut out = Vec::new();
    for raw in message.lines() {
        let quote = raw.trim();
        // Control characters (escape sequences, record separators) never
        // become a quote.
        if quote.len() < MIN_LINE
            || quote.len() > MAX_LINE
            || is_noise(quote)
            || quote.chars().any(|c| c.is_control() && c != '\t')
        {
            continue;
        }
        let line = quote
            .trim_start_matches(['-', '*', '+'])
            .trim_start()
            .to_string();
        if let Some((statement, trust)) = classify(&line) {
            let statement = clean_statement(&statement);
            if statement.len() >= MIN_LINE {
                out.push((quote.to_string(), statement, trust));
            }
        }
    }
    out
}

fn is_noise(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    const SKIP: [&str; 8] = [
        "co-authored-by:",
        "signed-off-by:",
        "reviewed-by:",
        "see merge request",
        "this reverts commit",
        "supersedes:",
        "merge branch ",
        "#",
    ];
    SKIP.iter().any(|p| lower.starts_with(p)) || lower.starts_with("revert \"")
}

fn classify(line: &str) -> Option<(String, Trust)> {
    // ASCII lowering keeps byte offsets equal to `line`'s.
    let lower = line.to_ascii_lowercase();
    for marker in ["decision:", "owner decision:", "decided:"] {
        if lower.starts_with(marker) {
            let rest = line[marker.len()..].trim();
            let statement = quoted(rest).unwrap_or_else(|| rest.to_string());
            return Some((statement, Trust::Confirmed));
        }
    }
    // An owner answer quoted after its verb (`Owner answered "…"`). A bare
    // `Owner:` line or a quote that merely mentions an owner is not explicit;
    // it can still be reconstructed from policy wording below.
    const ANSWER: [&str; 5] = [
        "owner answered",
        "owner decided",
        "owner chose",
        "owner replied",
        "owner said",
    ];
    if let Some(at) = ANSWER.iter().filter_map(|p| lower.find(p)).min()
        && let Some(q) = quoted(&line[at..])
    {
        return Some((q, Trust::Confirmed));
    }
    const POLICY: [&str; 9] = [
        "never ",
        "always ",
        "must not ",
        "no longer ",
        "from now on",
        "instead of ",
        "we decided",
        "decided to ",
        "policy: ",
    ];
    let padded = format!(" {lower}");
    if POLICY.iter().any(|p| padded.contains(&format!(" {p}"))) {
        return Some((line.to_string(), Trust::Reconstructed));
    }
    None
}

/// The first quoted segment ("…", “…”, „…“) of at least [`MIN_LINE`] chars.
fn quoted(line: &str) -> Option<String> {
    for (open, close) in [
        ('"', '"'),
        ('\u{201c}', '\u{201d}'),
        ('\u{201e}', '\u{201c}'),
    ] {
        if let Some(start) = line.find(open) {
            let after = &line[start + open.len_utf8()..];
            if let Some(end) = after.find(close) {
                let q = after[..end].trim();
                if q.len() >= MIN_LINE {
                    return Some(q.to_string());
                }
            }
        }
    }
    None
}

fn clean_statement(s: &str) -> String {
    let s = s
        .trim()
        .trim_matches(['"', '\u{201c}', '\u{201d}', '\u{201e}']);
    let mut out: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(stripped) = out.strip_suffix(['.', ';', ',']) {
        out = stripped.to_string();
    }
    out
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const NEGATIONS: [&str; 5] = ["never", "not", "no", "longer", "dont"];
const STOP: [&str; 24] = [
    "the", "and", "for", "with", "from", "that", "this", "are", "was", "will", "must", "always",
    "now", "on", "of", "to", "in", "a", "an", "be", "is", "we", "use", "only",
];

/// Topic words (sorted, polarity and stop words removed) and negative polarity.
fn topic_of(statement: &str) -> (Vec<String>, bool) {
    let norm = normalize(&statement.replace('\'', ""));
    let mut neg = false;
    let mut words = BTreeSet::new();
    for w in norm.split_whitespace() {
        if NEGATIONS.contains(&w) {
            neg = true;
        } else if !STOP.contains(&w) && w.len() >= 3 {
            words.insert(w.to_string());
        }
    }
    (words.into_iter().collect(), neg)
}

fn title_of(statement: &str) -> String {
    const MAX: usize = 72;
    if statement.chars().count() <= MAX {
        return statement.to_string();
    }
    let cut: String = statement.chars().take(MAX - 1).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a);
    format!("{cut}…")
}

fn draft_id(statement: &str, sha: &str) -> String {
    let slug: Vec<String> = normalize(statement)
        .split_whitespace()
        .filter(|w| w.is_ascii())
        .take(5)
        .map(str::to_string)
        .collect();
    let slug = if slug.is_empty() {
        "decision".to_string()
    } else {
        slug.join("-")
    };
    format!("dec-sug-{slug}-{}", &sha[..sha.len().min(7)])
}

fn evidence_hash(commit: &str, quote: &str) -> String {
    let h = blake3::hash(format!("{commit}\n{quote}").as_bytes());
    h.to_hex()[..16].to_string()
}

/// The first 12 characters (char-safe: the value may come from a file).
pub fn short(sha: &str) -> &str {
    sha.char_indices().nth(12).map_or(sha, |(i, _)| &sha[..i])
}

fn sha_matches(full: &str, given: &str) -> bool {
    given.len() >= 7 && given.chars().all(|c| c.is_ascii_hexdigit()) && full.starts_with(given)
}

/// `merge-request` + `!N` when the message has a GitLab MR footer.
fn source_of(commit: &Commit) -> (String, String) {
    for line in commit.message.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("See merge request ") {
            if let Some(pos) = rest.rfind('!') {
                let n: String = rest[pos + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if !n.is_empty() {
                    return ("merge-request".into(), format!("!{n}"));
                }
            }
        }
    }
    (
        "commit".into(),
        commit.sha[..commit.sha.len().min(12)].to_string(),
    )
}

fn reverted_commit(message: &str) -> Option<String> {
    message.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("This reverts commit ")?;
        let sha: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
        (sha.len() >= 7).then_some(sha)
    })
}

fn supersedes_trailers(message: &str) -> Vec<String> {
    const KEY: &str = "supersedes:";
    let mut out = Vec::new();
    for l in message.lines() {
        let l = l.trim();
        if l.get(..KEY.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(KEY))
        {
            for t in l[KEY.len()..].split([',', ' ']) {
                let t = t.trim();
                if !t.is_empty() && !out.iter().any(|o| o == t) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "decision_history_tests.rs"]
mod tests;
