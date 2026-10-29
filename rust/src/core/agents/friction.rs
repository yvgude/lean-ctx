// SPDX-License-Identifier: Apache-2.0
//! Friction signals of agent coordination, counted locally for the retro
//! loop: `<data>/agents/friction.jsonl`, one `{"at", "signal", "agent"?,
//! "root"?}` line per event (fixed keys and ids, never message text),
//! summed by `lean-ctx agent friction [--since <dur>]` and per hour, worker
//! and vault by `core::hive::friction` for the vault bus (V16.1).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};

/// A task claim Hive refused because the agent was not on the bus.
pub(crate) const CLAIM_REFUSED_PRESENCE: &str = "claim_refused_presence";
/// A task claim Hive refused because the agent was on the bus in another role.
pub(crate) const CLAIM_REFUSED_ROLE: &str = "claim_refused_role";
/// A task claim Hive refused for a role or consent reason it did not name.
pub(crate) const CLAIM_REFUSED_PERMIT: &str = "claim_refused_permit";
/// A claim this machine held ended before its agent finished (stale or closed).
pub(crate) const CLAIM_EXPIRED: &str = "claim_expired";
/// An agent's registration had expired and a heartbeat renewed it.
pub(crate) const REGISTRATION_RENEWED: &str = "registration_renewed";
/// `ctx_agent wait` / `lean-ctx agent wait` ended without a message.
pub(crate) const WAIT_TIMEOUT: &str = "wait_timeout";
/// The bound checkout had unmerged paths while an agent worked in it.
pub(crate) const MERGE_CONFLICT: &str = "merge_conflict";

const FILE: &str = "friction.jsonl";
/// Past this size the log keeps its newest half, so it stays small.
const MAX_BYTES: u64 = 256 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct Line {
    at: String,
    signal: String,
    /// The agent the signal belongs to, when the call site knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    /// The checkout it happened in, when known (else the agent's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
}

/// One recorded signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Event {
    pub at: DateTime<Utc>,
    pub signal: String,
    pub agent: Option<String>,
    pub root: Option<String>,
}

/// Count one `signal` now for `agent` in checkout `root` (either unknown).
/// Best effort: a failure is logged, never raised.
pub(crate) fn record_for(signal: &str, agent: Option<&str>, root: Option<&str>) {
    let Ok(dir) = super::persistence::agents_dir() else {
        return;
    };
    if let Err(error) = record_in(&dir, signal, agent, root, Utc::now()) {
        tracing::debug!("lean-ctx: friction log: {error}");
    }
}

fn record_in(
    dir: &Path,
    signal: &str,
    agent: Option<&str>,
    root: Option<&str>,
    at: DateTime<Utc>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let kept = lines[lines.len() / 2..].join("\n");
        crate::core::atomic_fs::try_atomic_write(&path, format!("{kept}\n").as_bytes(), None)
            .map_err(std::io::Error::other)?;
    }
    let line = serde_json::to_string(&Line {
        at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        signal: signal.to_string(),
        agent: agent.map(str::to_string),
        root: root.map(str::to_string),
    })
    .map_err(std::io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    writeln!(file, "{line}")
}

/// The signals recorded in `[from, to)`, oldest first.
pub(crate) fn events(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Event> {
    super::persistence::agents_dir()
        .map(|dir| events_in(&dir, from, to))
        .unwrap_or_default()
}

pub(crate) fn events_in(dir: &Path, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Event> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Line>(line).ok())
        .filter_map(|line| {
            let at = DateTime::parse_from_rfc3339(&line.at)
                .ok()?
                .with_timezone(&Utc);
            (from <= at && at < to).then_some(Event {
                at,
                signal: line.signal,
                agent: line.agent,
                root: line.root,
            })
        })
        .collect()
}

/// Signal → count of the events at or after `since` (all when `None`).
pub(crate) fn summary(since: Option<DateTime<Utc>>) -> BTreeMap<String, u64> {
    super::persistence::agents_dir()
        .map(|dir| summary_in(&dir, since))
        .unwrap_or_default()
}

fn summary_in(dir: &Path, since: Option<DateTime<Utc>>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return counts;
    };
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Line>(line) else {
            continue;
        };
        let Ok(at) = DateTime::parse_from_rfc3339(&line.at) else {
            continue;
        };
        if since.is_none_or(|since| at >= since) {
            *counts.entry(line.signal).or_insert(0) += 1;
        }
    }
    counts
}

/// The summary as text: one `signal count` line each, sorted by signal.
pub(crate) fn render(counts: &BTreeMap<String, u64>) -> String {
    if counts.is_empty() {
        return "No friction recorded.".to_string();
    }
    let width = counts.keys().map(String::len).max().unwrap_or(0);
    counts
        .iter()
        .map(|(signal, n)| format!("{signal:<width$}  {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `30m`, `12h`, `7d` or plain seconds.
pub(crate) fn parse_duration(text: &str) -> Option<chrono::Duration> {
    let text = text.trim();
    let (number, unit) = text
        .find(|c: char| !c.is_ascii_digit())
        .map_or((text, ""), |at| text.split_at(at));
    let n: i64 = number.parse().ok()?;
    match unit {
        "" | "s" => Some(chrono::Duration::seconds(n)),
        "m" => Some(chrono::Duration::minutes(n)),
        "h" => Some(chrono::Duration::hours(n)),
        "d" => Some(chrono::Duration::days(n)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_are_counted_by_key_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        record_in(
            dir.path(),
            WAIT_TIMEOUT,
            None,
            None,
            at("2026-10-24T08:00:00Z"),
        )
        .unwrap();
        record_in(
            dir.path(),
            WAIT_TIMEOUT,
            None,
            None,
            at("2026-10-24T10:00:00Z"),
        )
        .unwrap();
        record_in(
            dir.path(),
            CLAIM_REFUSED_PRESENCE,
            None,
            None,
            at("2026-10-24T10:30:00Z"),
        )
        .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE))
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let all = summary_in(dir.path(), None);
        assert_eq!(all[WAIT_TIMEOUT], 2);
        assert_eq!(all[CLAIM_REFUSED_PRESENCE], 1);
        let recent = summary_in(dir.path(), Some(at("2026-10-24T09:00:00Z")));
        assert_eq!(recent[WAIT_TIMEOUT], 1);
        assert_eq!(
            render(&recent),
            "claim_refused_presence  1\nwait_timeout            1"
        );
        assert_eq!(render(&BTreeMap::new()), "No friction recorded.");
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            text.lines()
                .next()
                .unwrap()
                .contains(r#""signal":"wait_timeout""#)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn the_log_stays_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        for _ in 0..6000 {
            record_in(dir.path(), CLAIM_EXPIRED, None, None, now).unwrap();
        }
        assert!(std::fs::metadata(dir.path().join(FILE)).unwrap().len() <= MAX_BYTES + 128);
        assert!(summary_in(dir.path(), None)[CLAIM_EXPIRED] > 1000);
    }

    /// V16.1: a signal carries its agent and checkout when the call site
    /// knows them; a window query returns `[from, to)` only.
    #[test]
    fn events_carry_agent_and_checkout_and_respect_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        record_in(
            dir.path(),
            WAIT_TIMEOUT,
            Some("mcp-1"),
            Some("/w/a"),
            at("2026-10-24T08:10:00Z"),
        )
        .unwrap();
        record_in(
            dir.path(),
            CLAIM_EXPIRED,
            None,
            None,
            at("2026-10-24T09:00:00Z"),
        )
        .unwrap();
        let hour = events_in(
            dir.path(),
            at("2026-10-24T08:00:00Z"),
            at("2026-10-24T09:00:00Z"),
        );
        assert_eq!(
            hour,
            [Event {
                at: at("2026-10-24T08:10:00Z"),
                signal: WAIT_TIMEOUT.into(),
                agent: Some("mcp-1".into()),
                root: Some("/w/a".into()),
            }]
        );
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(
            text.lines()
                .nth(1)
                .unwrap()
                .ends_with(r#""signal":"claim_expired"}"#),
            "unknown agent and checkout are left out: {text}"
        );
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration("90"), Some(chrono::Duration::seconds(90)));
        assert_eq!(parse_duration("30m"), Some(chrono::Duration::minutes(30)));
        assert_eq!(parse_duration("12h"), Some(chrono::Duration::hours(12)));
        assert_eq!(parse_duration("7d"), Some(chrono::Duration::days(7)));
        assert_eq!(parse_duration("7w"), None);
        assert_eq!(parse_duration("h"), None);
    }
}
