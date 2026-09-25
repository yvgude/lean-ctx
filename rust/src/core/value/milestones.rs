//! Milestones: the rare moments lean-ctx speaks up on its own — a native
//! desktop notification when a lifetime threshold is first crossed.
//!
//! Every milestone is computed from the *verified* chains ([`super::proof`]),
//! never from display counters, and a tampered chain celebrates nothing. At
//! most one notification per day; when several milestones are new at once the
//! most significant is shown and the rest wait for later days. Of a ladder
//! (1M → 10M → …) only the highest newly reached rung is shown — lower ones are
//! marked as seen, so an upgrade with a large ledger means one notification,
//! not a burst.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::proof::Proof;
use crate::core::config::{ValueDisplayConfig, ValueDisplayMode};

const TOKEN_LADDER: [(u64, &str); 4] = [
    (1_000_000, "1M"),
    (10_000_000, "10M"),
    (100_000_000, "100M"),
    (1_000_000_000, "1B"),
];
const STREAK_LADDER: [u32; 3] = [7, 30, 100];
const MIN_GAP_SECS: i64 = 24 * 60 * 60;
/// Re-walking the chains is cheap but not free: once per half hour at most.
const CHECK_EVERY_SECS: u64 = 30 * 60;

static LAST_CHECK: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    pub id: String,
    pub body: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    shown: BTreeSet<String>,
    /// Unix seconds of the last notification.
    last_notified: i64,
    /// Which process claimed the last write — two MCP servers crossing the
    /// same threshold must not both notify.
    claim: String,
}

/// One rung per ladder: the highest reached, plus every lower one to mark.
struct Rung {
    notify: Milestone,
    lower: Vec<String>,
}

fn token_rung(net_saved: u64) -> Option<Rung> {
    let reached: Vec<_> = TOKEN_LADDER
        .iter()
        .filter(|(n, _)| net_saved >= *n)
        .collect();
    let (_, top) = reached.last()?;
    Some(Rung {
        notify: Milestone {
            id: format!("tokens_{top}"),
            body: format!("{top} tokens kept out of your model's context so far."),
        },
        lower: reached[..reached.len() - 1]
            .iter()
            .map(|(_, label)| format!("tokens_{label}"))
            .collect(),
    })
}

fn streak_rung(longest: u32) -> Option<Rung> {
    let reached: Vec<_> = STREAK_LADDER.iter().filter(|n| longest >= **n).collect();
    let top = **reached.last()?;
    Some(Rung {
        notify: Milestone {
            id: format!("streak_{top}"),
            body: format!("{top} days in a row with lean-ctx."),
        },
        lower: reached[..reached.len() - 1]
            .iter()
            .map(|n| format!("streak_{n}"))
            .collect(),
    })
}

/// Longest run of consecutive UTC days in `days` (`YYYY-MM-DD`).
pub fn longest_streak(days: &BTreeSet<String>) -> u32 {
    let mut longest = 0;
    let mut run = 0;
    let mut prev: Option<NaiveDate> = None;
    for day in days {
        let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
            continue;
        };
        run = match prev {
            Some(p) if p.succ_opt() == Some(date) => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        prev = Some(date);
    }
    longest
}

/// Every rung the proof has reached, most significant first.
fn reached(proof: &Proof) -> Vec<Rung> {
    let mut out = Vec::new();
    out.extend(token_rung(proof.net_saved()));
    if proof.security.secrets_redacted > 0 {
        out.push(Rung {
            notify: Milestone {
                id: "first_secret".into(),
                body: "A secret was kept out of your model's context for the first time.".into(),
            },
            lower: Vec::new(),
        });
    }
    if proof.security.shell_blocked > 0 {
        out.push(Rung {
            notify: Milestone {
                id: "first_block".into(),
                body: "lean-ctx blocked a risky command for the first time.".into(),
            },
            lower: Vec::new(),
        });
    }
    out.extend(streak_rung(longest_streak(&proof.active_days)));
    out
}

/// Decides against `state` (and updates it): the milestone to notify now,
/// if any. Pure apart from `state`, so the policy is testable.
fn decide(proof: &Proof, state: &mut State, now: i64) -> Option<Milestone> {
    if proof.tampered() {
        return None;
    }
    let rungs = reached(proof);
    let pending: Vec<&Rung> = rungs
        .iter()
        .filter(|r| !state.shown.contains(&r.notify.id))
        .collect();
    if pending.is_empty() || now - state.last_notified < MIN_GAP_SECS {
        return None;
    }
    let rung = pending[0];
    state.shown.insert(rung.notify.id.clone());
    state.shown.extend(rung.lower.iter().cloned());
    state.last_notified = now;
    Some(rung.notify.clone())
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("milestones.json")
}

fn load_state(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Checks `proof` against the state in `dir` and claims the notification for
/// this process. Returns what to show.
fn check_in(dir: &Path, proof: &Proof, now: i64) -> Option<Milestone> {
    let path = state_path(dir);
    let mut state = load_state(&path);
    let milestone = decide(proof, &mut state, now)?;
    state.claim = format!("{}-{now}-{}", std::process::id(), milestone.id);
    let json = serde_json::to_vec(&state).ok()?;
    let _ = std::fs::create_dir_all(dir);
    crate::core::atomic_fs::try_atomic_write(&path, &json, None).ok()?;
    // Another server may have written in between; only the last writer shows.
    (load_state(&path).claim == state.claim).then_some(milestone)
}

/// A desktop notification interrupts, so it is opt-in: `mode = milestones`
/// (or `verbose`) and `notifications` not turned off.
fn notifications_enabled(cfg: &ValueDisplayConfig) -> bool {
    cfg.notifications
        && matches!(
            cfg.effective_mode(),
            ValueDisplayMode::Milestones | ValueDisplayMode::Verbose
        )
}

/// Called after tool calls (off the async runtime). Throttled; does nothing
/// unless notifications are on.
pub fn maybe_notify() {
    let now = chrono::Utc::now().timestamp();
    let now_u = u64::try_from(now).unwrap_or(0);
    let last = LAST_CHECK.load(Ordering::Relaxed);
    if now_u.saturating_sub(last) < CHECK_EVERY_SECS
        || LAST_CHECK
            .compare_exchange(last, now_u, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    if !notifications_enabled(&crate::core::config::Config::load_arc().value_display) {
        return;
    }
    let Some(dir) = super::snapshot::value_dir() else {
        return;
    };
    let proof = super::proof::build(None, true);
    if let Some(m) = check_in(&dir, &proof, now) {
        super::notify::send(
            "lean-ctx",
            &format!("{}\nProof: lean-ctx value --all", m.body),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(net: u64, secrets: u64, days: &[&str]) -> Proof {
        let mut p = super::super::proof::build_from(None, None, None, None);
        p.tokens_saved = net;
        p.security.secrets_redacted = secrets;
        p.active_days = days.iter().map(ToString::to_string).collect();
        p
    }

    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn only_the_highest_new_rung_is_shown_and_lower_ones_are_marked() {
        let mut state = State::default();
        let m = decide(&proof(42_000_000, 0, &[]), &mut state, DAY).unwrap();
        assert_eq!(m.id, "tokens_10M");
        assert!(m.body.starts_with("10M tokens"));
        assert!(state.shown.contains("tokens_1M"));
        // Next day: nothing new on the token ladder.
        assert_eq!(
            decide(&proof(42_000_000, 0, &[]), &mut state, 3 * DAY),
            None
        );
    }

    #[test]
    fn at_most_one_notification_per_day() {
        let mut state = State::default();
        let p = proof(2_000_000, 1, &[]);
        assert_eq!(decide(&p, &mut state, DAY).unwrap().id, "tokens_1M");
        assert_eq!(decide(&p, &mut state, DAY + 60), None);
        assert_eq!(decide(&p, &mut state, 2 * DAY).unwrap().id, "first_secret");
    }

    #[test]
    fn a_tampered_chain_celebrates_nothing() {
        let mut p = proof(5_000_000, 0, &[]);
        p.ledger.intact = false;
        assert_eq!(decide(&p, &mut State::default(), DAY), None);
    }

    #[test]
    fn streaks_count_consecutive_utc_days() {
        let days: BTreeSet<String> = [
            "2026-09-01",
            "2026-09-02",
            "2026-09-03",
            "2026-09-05",
            "2026-09-06",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(longest_streak(&days), 3);
        let week: Vec<String> = (1..=8).map(|d| format!("2026-08-{d:02}")).collect();
        let refs: Vec<&str> = week.iter().map(String::as_str).collect();
        let m = decide(&proof(0, 0, &refs), &mut State::default(), DAY).unwrap();
        assert_eq!(m.id, "streak_7");
    }

    #[test]
    fn notifications_are_opt_in_via_the_milestones_mode() {
        let mut cfg = ValueDisplayConfig::default();
        assert!(!notifications_enabled(&cfg), "default minimal stays silent");
        cfg.mode = ValueDisplayMode::Milestones;
        assert!(notifications_enabled(&cfg));
        cfg.mode = ValueDisplayMode::Verbose;
        assert!(notifications_enabled(&cfg));
        cfg.notifications = false;
        assert!(!notifications_enabled(&cfg));
    }

    #[test]
    fn nothing_reached_means_nothing_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(check_in(dir.path(), &proof(10, 0, &[]), DAY), None);
        assert!(!state_path(dir.path()).exists());
    }

    #[test]
    fn a_milestone_is_claimed_once_across_processes() {
        let dir = tempfile::tempdir().unwrap();
        let p = proof(1_500_000, 0, &[]);
        assert!(check_in(dir.path(), &p, DAY).is_some());
        // A second server seeing the same ledger a day later has nothing new.
        assert_eq!(check_in(dir.path(), &p, 3 * DAY), None);
    }
}
