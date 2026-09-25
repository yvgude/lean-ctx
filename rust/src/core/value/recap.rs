//! Turn and session recaps: when lean-ctx says something, and what.
//!
//! Hosts identify a conversation by their own session id (Claude's Stop and
//! SessionStart payloads), which is not lean-ctx's session id. Per host
//! session, [`TurnState`] counts turns and remembers the numbers at the last
//! recap; the numbers themselves come from the project snapshot. A recap only
//! ever describes the window since the previous one — never the whole lean-ctx
//! session as if it happened in the last few turns.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::config::{ValueDisplayConfig, ValueDisplayMode};
use crate::core::security_events::SecurityCounts;
use crate::core::wrapped::format_tokens;

use super::format::{Style, security_phrases};
use super::snapshot::{ValueSnapshot, read_path};

/// Host-session turn files older than this are pruned.
const TURN_STATE_TTL: Duration = Duration::from_hours(24 * 7);
/// A last-session recap is only offered for a session active this recently.
const RECAP_MAX_AGE: Duration = Duration::from_hours(24 * 7);
const DIGEST_EVERY: chrono::TimeDelta = chrono::TimeDelta::days(7);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TurnState {
    pub created_at: Option<DateTime<Utc>>,
    /// The lean-ctx session the baseline below belongs to.
    pub lean_session: String,
    pub turns: u32,
    pub last_recap_turn: u32,
    pub base_saved: u64,
    pub base_security: SecurityCounts,
}

impl TurnState {
    /// A new host session, baselined at `snap` so nothing that happened before
    /// it is attributed to its turns.
    pub fn start(now: DateTime<Utc>, snap: Option<&ValueSnapshot>) -> Self {
        let mut state = Self {
            created_at: Some(now),
            ..Self::default()
        };
        if let Some(snap) = snap {
            state.rebase(snap);
        }
        state
    }

    fn rebase(&mut self, snap: &ValueSnapshot) {
        self.lean_session.clone_from(&snap.session_id);
        self.base_saved = snap.tokens_saved;
        self.base_security = snap.security;
    }
}

/// What happened in one recap window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnRecap {
    pub turns: u32,
    pub saved: u64,
    pub security: SecurityCounts,
}

/// Counts one finished turn and decides whether it ends a recap window.
///
/// A window closes after `recap_every_turns` turns, and only when it saved at
/// least `recap_min_tokens` or had a security event (`verbose`: anything
/// measured). Otherwise it keeps growing, and the recap names the real length.
pub fn advance(
    state: &mut TurnState,
    snap: Option<&ValueSnapshot>,
    cfg: &ValueDisplayConfig,
    verbose: bool,
) -> Option<TurnRecap> {
    state.turns = state.turns.saturating_add(1);
    let snap = snap?;
    if snap.session_id != state.lean_session {
        // A lean-ctx session that began inside this host session counts from
        // zero; one carried over from earlier counts from now on.
        let began_here = matches!(
            (snap.started_at, state.created_at),
            (Some(started), Some(created)) if started >= created
        );
        state.rebase(snap);
        if began_here {
            state.base_saved = 0;
            state.base_security = SecurityCounts::default();
        }
    }
    let window = state.turns.saturating_sub(state.last_recap_turn);
    if window < cfg.recap_every_turns.max(1) {
        return None;
    }
    let saved = snap.tokens_saved.saturating_sub(state.base_saved);
    let security = snap.security.since(&state.base_security);
    let worth = !security.is_empty()
        || if verbose {
            saved > 0
        } else {
            saved > 0 && saved >= cfg.recap_min_tokens
        };
    if !worth {
        return None;
    }
    state.last_recap_turn = state.turns;
    state.rebase(snap);
    Some(TurnRecap {
        turns: window,
        saved,
        security,
    })
}

fn line(style: Style, label: &str, saved: u64, security: &SecurityCounts) -> String {
    let mut parts = Vec::new();
    if saved > 0 {
        parts.push(format!("{}{} tokens", style.minus(), format_tokens(saved)));
    }
    parts.extend(security_phrases(security));
    let sep = style.sep();
    format!("{} lean-ctx{sep}{label}: {}", style.mark(), parts.join(sep))
}

/// `◆ lean-ctx · last 10 turns: −312.0K tokens · 2 secrets kept out of context`
pub fn turn_line(recap: &TurnRecap, style: Style) -> String {
    let label = if recap.turns == 1 {
        "last turn".to_string()
    } else {
        format!("last {} turns", recap.turns)
    };
    line(style, &label, recap.saved, &recap.security)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

pub fn turn_state_path(dir: &Path, host_session: &str) -> Option<PathBuf> {
    let ok = !host_session.is_empty()
        && host_session.len() <= 128
        && !host_session.starts_with('.')
        && host_session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ok.then(|| dir.join("turns").join(format!("{host_session}.json")))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    let Ok(json) = serde_json::to_vec(value) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = crate::core::atomic_fs::try_atomic_write(path, &json, None) {
        tracing::debug!("lean-ctx: value recap state write failed: {error}");
    }
}

pub fn load_turn_state(dir: &Path, host_session: &str) -> Option<TurnState> {
    read_json(&turn_state_path(dir, host_session)?)
}

/// The Stop-hook entry point: counts the turn and returns the recap line when
/// this turn closes a window worth mentioning.
pub fn on_turn_end(
    dir: &Path,
    host_session: &str,
    snap: Option<&ValueSnapshot>,
    cfg: &ValueDisplayConfig,
    now: DateTime<Utc>,
    style: Style,
) -> Option<String> {
    let path = turn_state_path(dir, host_session)?;
    // No state means the hook was installed mid-conversation: baseline now,
    // so earlier work is never claimed for these turns.
    let mut state = read_json(&path).unwrap_or_else(|| TurnState::start(now, snap));
    let verbose = cfg.effective_mode() == ValueDisplayMode::Verbose;
    let recap = advance(&mut state, snap, cfg, verbose);
    write_json(&path, &state);
    recap.map(|r| turn_line(&r, style))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecapState {
    /// The lean-ctx session that already got its "last session" recap.
    pub last_session_recapped: String,
    pub last_digest_at: Option<DateTime<Utc>>,
}

fn recap_state_path(dir: &Path) -> PathBuf {
    dir.join("recap_state.json")
}

/// The SessionStart entry point. Starts the host session's turn count and, on
/// a fresh start (not a resume or compaction), returns either the weekly
/// digest (at most once per 7 days) or a one-time recap of the project's last
/// lean-ctx session.
pub fn on_session_start(
    dir: &Path,
    host_session: &str,
    source: &str,
    snap: Option<&ValueSnapshot>,
    cfg: &ValueDisplayConfig,
    now: DateTime<Utc>,
    style: Style,
) -> Option<String> {
    prune_turn_states(dir);
    if let Some(path) = turn_state_path(dir, host_session)
        && !path.exists()
    {
        write_json(&path, &TurnState::start(now, snap));
    }
    if !matches!(source, "" | "startup") {
        return None;
    }
    let state_path = recap_state_path(dir);
    let mut state: RecapState = read_json(&state_path).unwrap_or_default();
    let verbose = cfg.effective_mode() == ValueDisplayMode::Verbose;

    let digest_due = state
        .last_digest_at
        .is_none_or(|at| now.signed_duration_since(at) >= DIGEST_EVERY);
    if digest_due && let Some(digest) = weekly_digest(dir, now) {
        state.last_digest_at = Some(now);
        if let Some(snap) = snap {
            state.last_session_recapped.clone_from(&snap.session_id);
        }
        write_json(&state_path, &state);
        return Some(digest_line(&digest, style));
    }

    let snap = snap?;
    let worth = !snap.security.is_empty()
        || snap.tokens_saved > 0 && (verbose || snap.tokens_saved >= cfg.recap_min_tokens);
    if !worth || snap.session_id == state.last_session_recapped || !snap.is_fresh(RECAP_MAX_AGE) {
        return None;
    }
    state.last_session_recapped.clone_from(&snap.session_id);
    write_json(&state_path, &state);
    Some(session_line(snap, style))
}

/// `◆ lean-ctx · last session (62% of tool input): −1.4M tokens · 3 secrets kept out of context`
pub fn session_line(snap: &ValueSnapshot, style: Style) -> String {
    let label = match snap.saved_pct() {
        Some(pct) if snap.tokens_saved > 0 => format!("last session ({pct:.0}% of tool input)"),
        _ => "last session".to_string(),
    };
    line(style, &label, snap.tokens_saved, &snap.security)
}

/// Totals over every session snapshot updated in the last 7 days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Digest {
    pub sessions: u64,
    pub saved: u64,
    pub security: SecurityCounts,
}

pub fn weekly_digest(dir: &Path, now: DateTime<Utc>) -> Option<Digest> {
    let since = now - DIGEST_EVERY;
    let window = DIGEST_EVERY.to_std().unwrap_or_default();
    let mut digest = Digest::default();
    for entry in std::fs::read_dir(dir.join("sessions")).ok()?.flatten() {
        // The file time is a cheap pre-filter; the snapshot's own clock decides.
        let recent = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age <= window);
        if !recent {
            continue;
        }
        let Some(snap) = read_path(&entry.path()) else {
            continue;
        };
        if snap.updated_at.is_none_or(|at| at < since) || snap.is_empty() {
            continue;
        }
        digest.sessions += 1;
        digest.saved = digest.saved.saturating_add(snap.tokens_saved);
        digest.security.merge(&snap.security);
    }
    (digest.sessions > 0).then_some(digest)
}

/// `◆ lean-ctx · this week (12 sessions): −8.4M tokens · 3 secrets kept out of context`
pub fn digest_line(digest: &Digest, style: Style) -> String {
    let label = if digest.sessions == 1 {
        "this week (1 session)".to_string()
    } else {
        format!("this week ({} sessions)", digest.sessions)
    };
    line(style, &label, digest.saved, &digest.security)
}

fn prune_turn_states(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir.join("turns")) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > TURN_STATE_TTL);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
#[path = "recap_tests.rs"]
mod tests;
