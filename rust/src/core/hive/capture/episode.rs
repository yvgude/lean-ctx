// SPDX-License-Identifier: Apache-2.0
//! Episodes (SPEC-ENGINE §3.2.3): one node per ended session, built from
//! its record — the goal (last task), the outcome (final progress or an
//! explicit keyword), dead ends (`ctx_session finding tag=dead_end` and
//! revert commits of the session window), the files it changed and the
//! commits it made. An episode that fails the episode rules (§3.1.4) is not
//! captured (`episode_shape`, or `no_reference` without files and commits);
//! nothing of it leaves the machine then.

use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::refs::Checkout;
use super::{ReasonCode, rules};
use crate::core::hive::HiveError;
use crate::core::hive::events::build::{EVENT_SCHEMA, Place, Signer};
use crate::core::hive::events::hlc::Stamp;
use crate::core::hive::events::{ids, sign};
use crate::core::hive::knowledge::screening::Screen;
use crate::core::import::git_history::{self, Commit, Kind};
use crate::core::session::SessionState;

const TEXT_MAX: usize = 300;
const GOAL_MIN: usize = 8;
const MAX_DEAD_ENDS: usize = 5;
const MAX_TOUCHED: usize = 50;
const MAX_COMMITS: usize = 50;
/// Commits of a session window read at most.
const MAX_LOG_COMMITS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Abandoned,
    Partial,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
            Self::Partial => "partial",
        }
    }

    /// `100%` (or `[done]`/`[complete]`) succeeded; an explicit `failed` or
    /// `abandoned` word wins over the percentage; anything else is partial.
    fn of(description: &str, progress: Option<u8>) -> Self {
        let lower = description.to_lowercase();
        let has = |word: &str| {
            lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|token| token == word)
        };
        if has("failed") {
            Self::Failed
        } else if has("abandoned") {
            Self::Abandoned
        } else if progress == Some(100)
            || lower.contains("[100%]")
            || lower.contains("[done]")
            || lower.contains("[complete]")
        {
            Self::Succeeded
        } else {
            Self::Partial
        }
    }
}

/// An episode as it is captured; [`Episode::body`] is its contract body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub goal: String,
    pub outcome: Outcome,
    pub summary: String,
    pub dead_ends: Vec<String>,
    /// Code anchors `{path, digest}` of the files the session changed, at `HEAD`.
    pub touched: Vec<Value>,
    pub commits: Vec<String>,
    pub session_ref: String,
    pub started_at: u64,
    pub ended_at: u64,
}

/// `fix: retry [45%]` → `fix: retry`: the progress marker is no part of a goal.
fn goal_of(description: &str) -> String {
    let text = description.trim();
    match text.rsplit_once('[') {
        Some((head, tail)) if tail.ends_with(']') && !head.trim().is_empty() => {
            head.trim().to_string()
        }
        _ => text.to_string(),
    }
}

/// `path` relative to `root`, `None` outside it.
fn relative(path: &str, root: &Path) -> Option<String> {
    let p = Path::new(path);
    let rel = if p.is_absolute() {
        p.strip_prefix(root).ok()?
    } else {
        p
    };
    let text = rel.to_str()?.replace('\\', "/");
    (!text.is_empty() && !text.starts_with("..")).then_some(text)
}

fn ms(t: DateTime<Utc>) -> u64 {
    t.timestamp_millis().max(0).unsigned_abs()
}

/// The episode of `session`: `dead_ends` are its tagged findings,
/// `commits` the commits of its window (newest first, as `git log` lists
/// them). Texts that would break a rule are left out where the rules allow
/// it (a dead end, the summary); the rest is checked by [`check`].
pub fn assemble(
    session: &SessionState,
    root: &Path,
    dead_ends: &[String],
    commits: &[Commit],
    checkout: &mut Checkout,
) -> Result<Episode, ReasonCode> {
    let task = session.task.as_ref().ok_or(ReasonCode::EpisodeShape)?;
    let goal = goal_of(&task.description);
    let outcome = Outcome::of(&task.description, task.progress_pct);
    let fits = |t: &str| {
        rules::check_length(t, 1, TEXT_MAX).is_ok()
            && rules::check_single_line(t).is_ok()
            && rules::check_log(t).is_ok()
    };
    let reverts = commits
        .iter()
        .filter(|c| git_history::classify(c) == Some(Kind::Revert))
        .map(|c| git_history::statement(c, Kind::Revert));
    let mut dead: Vec<String> = Vec::new();
    for text in dead_ends.iter().cloned().chain(reverts) {
        let text = text.trim().to_string();
        if dead.len() < MAX_DEAD_ENDS && fits(&text) && !dead.contains(&text) {
            dead.push(text);
        }
    }
    let mut touched = Vec::new();
    for file in session.files_touched.iter().filter(|f| f.modified) {
        let Some(path) = relative(&file.path, root) else {
            continue;
        };
        if touched.len() == MAX_TOUCHED || touched.iter().any(|t: &Value| t["path"] == *path) {
            continue;
        }
        // Only what `HEAD` holds is anchored; a file the session deleted or
        // never committed is no reference.
        if let Some(bytes) = checkout.blob_bytes(&path) {
            touched.push(json!({"path": path, "digest": hex::encode(Sha256::digest(bytes))}));
        }
    }
    let commits: Vec<String> = commits
        .iter()
        .take(MAX_COMMITS)
        .map(|c| c.sha.to_lowercase())
        .collect();
    let summary = session
        .decisions
        .last()
        .map(|d| d.summary.trim().to_string())
        .filter(|s| fits(s))
        .unwrap_or_else(|| {
            format!(
                "{} after {} commit(s) changing {} file(s).",
                match outcome {
                    Outcome::Succeeded => "Succeeded",
                    Outcome::Failed => "Failed",
                    Outcome::Abandoned => "Abandoned",
                    Outcome::Partial => "Partly done",
                },
                commits.len(),
                touched.len()
            )
        });
    let episode = Episode {
        goal,
        outcome,
        summary,
        dead_ends: dead,
        touched,
        commits,
        session_ref: crate::core::hive::knowledge::draft::session_ref(&session.id),
        started_at: ms(session.started_at),
        ended_at: ms(session.updated_at),
    };
    check(&episode)?;
    Ok(episode)
}

/// The episode rules (SPEC-ENGINE §3.1.4).
pub fn check(e: &Episode) -> Result<(), ReasonCode> {
    let shape = |ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(ReasonCode::EpisodeShape)
        }
    };
    let line = |t: &str, min: usize| {
        rules::check_length(t, min, TEXT_MAX).is_ok() && rules::check_single_line(t).is_ok()
    };
    shape(line(&e.goal, GOAL_MIN))?;
    shape(line(&e.summary, 1))?;
    shape(e.dead_ends.len() <= MAX_DEAD_ENDS)?;
    for d in &e.dead_ends {
        shape(line(d, 1) && rules::check_log(d).is_ok())?;
    }
    shape(e.touched.len() <= MAX_TOUCHED && e.commits.len() <= MAX_COMMITS)?;
    shape(
        e.commits
            .iter()
            .all(|c| (7..=64).contains(&c.len()) && c.chars().all(|c| c.is_ascii_hexdigit())),
    )?;
    shape(e.ended_at >= e.started_at)?;
    if e.touched.is_empty() && e.commits.is_empty() {
        return Err(ReasonCode::NoReference);
    }
    Ok(())
}

/// Screen every text of the episode (all node text is screened).
pub fn screen(e: &Episode, screen: &mut Screen) -> Result<(), ReasonCode> {
    let texts = [&e.goal, &e.summary].into_iter().chain(&e.dead_ends);
    for text in texts {
        screen.check(text).map_err(|b| ReasonCode::of_screen(&b))?;
    }
    Ok(())
}

impl Episode {
    /// The contract body (`episode_body`).
    pub fn body(&self) -> Result<Value, HiveError> {
        let mut body = Map::new();
        body.insert("body_nonce".into(), Value::String(sign::new_body_nonce()?));
        body.insert("goal".into(), Value::from(self.goal.clone()));
        body.insert("outcome".into(), Value::from(self.outcome.as_str()));
        body.insert("summary".into(), Value::from(self.summary.clone()));
        if !self.dead_ends.is_empty() {
            body.insert("dead_ends".into(), json!(self.dead_ends));
        }
        if !self.touched.is_empty() {
            body.insert("touched".into(), Value::Array(self.touched.clone()));
        }
        if !self.commits.is_empty() {
            body.insert("commits".into(), json!(self.commits));
        }
        body.insert("session_ref".into(), Value::from(self.session_ref.clone()));
        body.insert("started_at".into(), Value::from(self.started_at));
        body.insert("ended_at".into(), Value::from(self.ended_at));
        Ok(Value::Object(body))
    }

    /// A signed `node.add` of the episode. Returns the event and its node id.
    pub fn node_add(
        &self,
        place: &Place,
        stamp: Stamp,
        signer: &Signer<'_>,
    ) -> Result<(Value, String), HiveError> {
        let body = self.body()?;
        let target = ids::new_random_id("n")?;
        let body_commit = sign::body_commit(&place.pk_id, &body).map_err(HiveError::Local)?;
        let rules = format!("leanctx.hive.capture.v{}:episode", super::GATE_VERSION);
        let mut event = json!({
            "schema": EVENT_SCHEMA,
            "event_id": ids::new_event_id(stamp.wall_ms)?,
            "place": {"kind": place.kind, "id": place.id},
            "op": "node.add",
            "target": target,
            "node_type": "episode",
            "body": body,
            "body_commit": body_commit,
            "hlc": {"wall_ms": stamp.wall_ms, "counter": stamp.counter, "node": signer.hlc_node},
            "causal_base": place.causal_base,
            "actor": {"kind": "worker"},
            "worker_id": signer.worker_id,
            "provenance_claim": "own",
            "capture": {
                "gate_version": super::GATE_VERSION,
                "evidence": "observed",
                "reasons_passed_hash": blake3::hash(rules.as_bytes()).to_hex().to_string(),
            },
            "screening": {
                "counts": {"forbidden": 0, "local_path": 0, "pii": 0, "redaction": 0, "secret": 0},
                "scanner_version": crate::core::hive::knowledge::screening::SCANNER_VERSION,
            },
        });
        sign::sign(&mut event, signer.key).map_err(HiveError::Local)?;
        Ok((event, target))
    }
}

/// Commits the git user made in `root` between `since` and `until`
/// (newest first). Empty when git, the user or the repository is missing.
pub fn session_commits(root: &Path, since: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Commit> {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let Some(author) = git(&["config", "user.email"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return Vec::new();
    };
    git(&[
        "log",
        "--no-merges",
        "--no-color",
        &format!("--max-count={MAX_LOG_COMMITS}"),
        &format!("--since={}", since.to_rfc3339()),
        &format!("--until={}", until.to_rfc3339()),
        &format!("--author={author}"),
        "--format=%H%x1f%cI%x1f%s%x1f%b%x1e",
    ])
    .map(|log| git_history::parse_log(&log))
    .unwrap_or_default()
}

fn dead_end_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("capture").join("dead_ends")
}

/// The dead ends recorded for a session (`ctx_session finding tag=dead_end`).
pub fn dead_ends(data_dir: &Path, session_id: &str) -> Vec<String> {
    let file = dead_end_dir(data_dir).join(format!(
        "{}.json",
        crate::core::hive::knowledge::draft::session_ref(session_id)
    ));
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Record a dead end of a session. Kept private beside the capture state;
/// it leaves the machine only inside a gated, screened episode.
pub fn record_dead_end(data_dir: &Path, session_id: &str, text: &str) -> Result<(), HiveError> {
    let mut all = dead_ends(data_dir, session_id);
    let text = text.trim().to_string();
    if text.is_empty() || all.contains(&text) {
        return Ok(());
    }
    all.push(text);
    let bytes = serde_json::to_vec(&all).map_err(|e| HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(
        &dead_end_dir(data_dir),
        &format!(
            "{}.json",
            crate::core::hive::knowledge::draft::session_ref(session_id)
        ),
        &bytes,
    )
}

/// A session not written for this long has ended.
const ENDED_AFTER_MS: u64 = 30 * 60_000;
/// Replica meta: sessions that ended before this are never turned into
/// episodes (no back-fill of the history before the first run).
const EPISODES_SINCE: &str = "episodes_since";

fn done_key(session_ref: &str) -> String {
    format!("episode:{session_ref}")
}

/// Queue a signed episode for each session of the checkout at `root` that
/// ended since the last run (once per session). Refusals are counted in the
/// run, never kept as text. Returns the number queued and the clock.
pub fn emit_ended(
    target: &super::emit::Target<'_>,
    root: &Path,
    data_dir: &Path,
    run: &mut super::CaptureRun<'_>,
    mut clock: Stamp,
    now_ms: u64,
) -> Result<(usize, Stamp), HiveError> {
    let replica = target.replica;
    let Some(since) = replica
        .meta(EPISODES_SINCE)?
        .and_then(|v| std::str::from_utf8(&v).ok()?.parse::<u64>().ok())
    else {
        replica.set_meta(EPISODES_SINCE, now_ms.to_string().as_bytes())?;
        return Ok((0, clock));
    };
    let root_text = root.to_string_lossy();
    let mut queued = 0;
    for summary in SessionState::list_sessions() {
        let ended = ms(summary.updated_at);
        if summary
            .project_root
            .as_deref()
            .map(|r| r.trim_end_matches('/'))
            != Some(root_text.trim_end_matches('/'))
            || ended <= since
            || ended + ENDED_AFTER_MS > now_ms
        {
            continue;
        }
        let session_ref = crate::core::hive::knowledge::draft::session_ref(&summary.id);
        if replica.meta(&done_key(&session_ref))?.is_some() {
            continue;
        }
        let Some(session) = SessionState::load_by_id(&summary.id) else {
            continue;
        };
        run.counts.considered += 1;
        let commits = session_commits(root, session.started_at, session.updated_at);
        let built = assemble(
            &session,
            root,
            &dead_ends(data_dir, &session.id),
            &commits,
            &mut run.checkout,
        )
        .and_then(|e| screen(&e, run.screen).map(|()| e));
        replica.set_meta(&done_key(&session_ref), b"1")?;
        let episode = match built {
            Ok(e) => e,
            Err(reason) => {
                run.counts.reject(reason);
                continue;
            }
        };
        clock = crate::core::hive::events::hlc::send(clock, now_ms);
        let (mut event, node_id) = episode.node_add(target.place, clock, target.signer)?;
        if let Some(seal) = target.seal {
            crate::core::hive::events::sealed_body::seal_event(
                &mut event,
                seal,
                target.signer.key,
            )?;
        }
        let event_id = event["event_id"]
            .as_str()
            .ok_or_else(|| HiveError::Local("built event without id".into()))?
            .to_string();
        let bytes = serde_json::to_vec(&event).map_err(|e| HiveError::Local(e.to_string()))?;
        replica.add_pending(&event_id, clock.wall_ms, clock.counter, None, &bytes)?;
        replica.put_own_source(
            &node_id,
            "episode",
            &json!({"project_hash": target.project_hash, "session_ref": session_ref}),
        )?;
        run.counts.captured += 1;
        queued += 1;
    }
    Ok((queued, clock))
}

/// `ctx_session action=finding tag=dead_end`: record it for the session's
/// episode when this machine is connected to Hive. Never an error for the
/// caller (a failure is recorded for `hive status`).
pub fn note_dead_end(session_id: &str, text: &str) {
    let Ok(store) = crate::core::hive::state::HiveStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    if let Err(error) = record_dead_end(store.data_dir(), session_id, text) {
        store.record_error(&format!("dead end: {error}"));
    }
}

#[cfg(test)]
#[path = "episode_tests.rs"]
mod tests;
