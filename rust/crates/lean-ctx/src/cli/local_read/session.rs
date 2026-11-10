// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx session stats|cost --json --session ID`: one app session as a
//! local reader (Hive Desktop) sees it. The app names its sessions with
//! `LEAN_CTX_SESSION_ID` (and the proxy header `x-leanctx-session-id`);
//! `--session` takes that id or a LeanCTX session id.
//!
//! `session.stats`: the LeanCTX sessions attributed to the id, their token
//! counters, what was kept out of the context (security counts), and waste
//! signals from the activity stream (files read again, failing commands).
//! `session.cost`: the proxy's measured spend of the id.

use std::collections::BTreeMap;

use serde::Serialize;

use lean_ctx::core::activity::{Event, Filter, Kind, Outcome};
use lean_ctx::core::security_events::SecurityCounts;
use lean_ctx::core::session::SessionState;
use lean_ctx_proxy::usage_meter::{self, ModelSpend};

use super::{Code, Failure, rfc3339};

/// Rows of each waste list.
const TOP: usize = 10;

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SessionStatsDoc {
    pub session_id: String,
    pub lean_sessions: Vec<String>,
    pub project_root: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub tokens: Tokens,
    pub kept_out: KeptOut,
    pub waste: Waste,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct Tokens {
    pub input: u64,
    pub saved: u64,
    pub tool_calls: u64,
    pub cache_hits: u64,
    pub files_read: u64,
    pub commands_run: u64,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct KeptOut {
    pub total: u64,
    pub secrets_redacted: u64,
    pub pii_redacted: u64,
    pub content_withheld: u64,
    pub shell_blocked: u64,
    pub path_blocked: u64,
    pub injection_flagged: u64,
    pub coverage_incomplete: u64,
}

impl KeptOut {
    fn add(&mut self, c: &SecurityCounts) {
        self.secrets_redacted += c.secrets_redacted;
        self.pii_redacted += c.pii_redacted;
        self.content_withheld += c.content_withheld;
        self.shell_blocked += c.shell_blocked;
        self.path_blocked += c.path_blocked;
        self.injection_flagged += c.injection_flagged;
        self.coverage_incomplete += c.coverage_incomplete;
        self.total = self.secrets_redacted
            + self.pii_redacted
            + self.content_withheld
            + self.shell_blocked
            + self.path_blocked
            + self.injection_flagged
            + self.coverage_incomplete;
    }
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct Waste {
    pub repeated_reads: RepeatedReads,
    pub failing_commands: FailingCommands,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct RepeatedReads {
    pub files: usize,
    pub extra_reads: u64,
    pub top: Vec<ReadRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ReadRow {
    pub path: String,
    pub reads: u64,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct FailingCommands {
    pub failures: u64,
    pub top: Vec<CommandRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct CommandRow {
    pub command: String,
    pub failures: u64,
    pub last_at: String,
}

/// Files read more than once: per path the larger of LeanCTX's own count
/// (`files_touched`) and the activity stream's `file_read` events.
fn repeated_reads(sessions: &[SessionState], events: &[Event]) -> RepeatedReads {
    let mut lean: BTreeMap<&str, u64> = BTreeMap::new();
    for file in sessions.iter().flat_map(|s| &s.files_touched) {
        *lean.entry(file.path.as_str()).or_default() += u64::from(file.read_count);
    }
    let mut seen: BTreeMap<&str, u64> = BTreeMap::new();
    for event in events.iter().filter(|e| e.kind == Kind::FileRead) {
        if let Some(path) = event.target.as_deref() {
            *seen.entry(path).or_default() += 1;
        }
    }
    for (path, n) in lean {
        let slot = seen.entry(path).or_default();
        *slot = (*slot).max(n);
    }
    let mut rows: Vec<ReadRow> = seen
        .into_iter()
        .filter(|(_, reads)| *reads > 1)
        .map(|(path, reads)| ReadRow {
            path: path.to_owned(),
            reads,
        })
        .collect();
    let files = rows.len();
    let extra_reads = rows.iter().map(|r| r.reads - 1).sum();
    rows.sort_by(|a, b| b.reads.cmp(&a.reads).then_with(|| a.path.cmp(&b.path)));
    rows.truncate(TOP);
    RepeatedReads {
        files,
        extra_reads,
        top: rows,
    }
}

/// Shell events that ended in an error, grouped by command.
fn failing_commands(events: &[Event]) -> FailingCommands {
    let mut by_command: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
    for event in events
        .iter()
        .filter(|e| e.kind == Kind::Shell && e.outcome == Some(Outcome::Error))
    {
        let Some(command) = event.target.as_deref() else {
            continue;
        };
        let slot = by_command.entry(command).or_insert((0, ""));
        slot.0 += 1;
        if event.at.as_str() > slot.1 {
            slot.1 = event.at.as_str();
        }
    }
    let failures = by_command.values().map(|(n, _)| n).sum();
    let mut top: Vec<CommandRow> = by_command
        .into_iter()
        .map(|(command, (failures, last_at))| CommandRow {
            command: command.to_owned(),
            failures,
            last_at: last_at.to_owned(),
        })
        .collect();
    top.sort_by(|a, b| {
        b.failures
            .cmp(&a.failures)
            .then_with(|| b.last_at.cmp(&a.last_at))
            .then_with(|| a.command.cmp(&b.command))
    });
    top.truncate(TOP);
    FailingCommands { failures, top }
}

/// The `session.stats` body of `id` from its sessions (oldest first) and
/// its activity events; `not_found` when neither knows the id.
pub(crate) fn stats_of(
    id: &str,
    sessions: &[SessionState],
    events: &[Event],
) -> Result<SessionStatsDoc, Failure> {
    if sessions.is_empty() && events.is_empty() {
        return Err(Failure::new(
            Code::NotFound,
            format!("no session is known as {id}"),
        ));
    }
    let mut tokens = Tokens::default();
    let mut kept_out = KeptOut::default();
    for s in sessions {
        tokens.input += s.stats.total_tokens_input;
        tokens.saved += s.stats.total_tokens_saved;
        tokens.tool_calls += u64::from(s.stats.total_tool_calls);
        tokens.cache_hits += u64::from(s.stats.cache_hits);
        tokens.files_read += u64::from(s.stats.files_read);
        tokens.commands_run += u64::from(s.stats.commands_run);
        kept_out.add(&s.stats.security);
    }
    let mut lean_sessions: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
    lean_sessions.sort();
    Ok(SessionStatsDoc {
        session_id: id.to_owned(),
        lean_sessions,
        project_root: sessions
            .iter()
            .rev()
            .find_map(|s| s.project_root.clone())
            .or_else(|| events.last().map(|e| e.project_root.clone())),
        started_at: sessions.first().map(|s| rfc3339(s.started_at)),
        updated_at: sessions.iter().map(|s| s.updated_at).max().map(rfc3339),
        tokens,
        kept_out,
        waste: Waste {
            repeated_reads: repeated_reads(sessions, events),
            failing_commands: failing_commands(events),
        },
    })
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SessionCostDoc {
    pub session_id: String,
    pub available: bool,
    pub source: &'static str,
    pub total_usd: f64,
    pub measured_usd: f64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub per_model: Vec<CostRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct CostRow {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub pricing_estimated: bool,
}

/// USD rounded to 6 decimals, so the same totals print the same bytes.
pub(crate) fn usd(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

pub(crate) fn cost_row(r: &ModelSpend) -> CostRow {
    CostRow {
        model: r.model.clone(),
        requests: r.requests,
        input_tokens: r.input_tokens,
        output_tokens: r.output_tokens,
        cache_read_tokens: r.cache_read_tokens,
        cache_write_tokens: r.cache_write_tokens,
        cost_usd: usd(r.cost_usd),
        pricing_estimated: r.pricing_estimated,
    }
}

/// The `session.cost` body of `id` from the proxy's rows (`None`: the proxy
/// saw no turn of it). Models sorted by cost desc, then name.
pub(crate) fn cost_of(id: &str, rows: Option<Vec<ModelSpend>>) -> SessionCostDoc {
    let mut rows = rows.unwrap_or_default();
    rows.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then_with(|| a.model.cmp(&b.model))
    });
    SessionCostDoc {
        session_id: id.to_owned(),
        available: !rows.is_empty(),
        source: "proxy",
        total_usd: usd(rows.iter().map(|r| r.cost_usd).sum()),
        measured_usd: usd(rows.iter().map(|r| r.measured_cost_usd).sum()),
        requests: rows.iter().map(|r| r.requests).sum(),
        input_tokens: rows.iter().map(|r| r.input_tokens).sum(),
        output_tokens: rows.iter().map(|r| r.output_tokens).sum(),
        cache_read_tokens: rows.iter().map(|r| r.cache_read_tokens).sum(),
        cache_write_tokens: rows.iter().map(|r| r.cache_write_tokens).sum(),
        per_model: rows.iter().map(cost_row).collect(),
    }
}

/// `--session ID`, validated like the app's id.
fn session_arg(args: &[String]) -> Result<String, Failure> {
    super::flag(args, "--session")?
        .and_then(lean_ctx::core::activity::session_id)
        .ok_or_else(|| Failure::invalid("--session <id> is required (1-128 of [A-Za-z0-9._:-])"))
}

/// The read and shell events of `ids` (newest kept per id).
fn events_of(ids: &[&str]) -> Vec<Event> {
    let Some(store) = lean_ctx::core::activity::store() else {
        return Vec::new();
    };
    let mut events: Vec<Event> = Vec::new();
    for id in ids {
        let filter = Filter {
            session: Some((*id).to_owned()),
            project_root: None,
            since: None,
            kinds: vec![Kind::FileRead, Kind::Shell],
            limit: lean_ctx::core::activity::MAX_LIMIT,
        };
        if let Ok(listing) = store.list(&filter) {
            events.extend(listing.events);
        }
    }
    events.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.seq.cmp(&b.seq)));
    events.dedup();
    events
}

/// `session stats --json --session ID`.
pub(crate) fn build_stats(args: &[String]) -> Result<SessionStatsDoc, Failure> {
    let id = session_arg(args)?;
    let sessions = SessionState::load_attributed(&id);
    let mut ids: Vec<&str> = vec![id.as_str()];
    ids.extend(sessions.iter().map(|s| s.id.as_str()).filter(|s| *s != id));
    let events = events_of(&ids);
    stats_of(&id, &sessions, &events)
}

/// `session cost --json --session ID`.
pub(crate) fn build_cost(args: &[String]) -> Result<SessionCostDoc, Failure> {
    let id = session_arg(args)?;
    lean_ctx::core::gain::live_pricing::ensure_loaded();
    Ok(cost_of(&id, usage_meter::persisted_session(&id)))
}

/// `session stats|cost --json …`.
pub(crate) fn run(action: &str, args: &[String]) {
    let kind = if action == "cost" {
        "session.cost"
    } else {
        "session.stats"
    };
    super::kinds::run(kind, args);
}
#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::activity::Source;

    fn event(kind: Kind, target: &str, outcome: Option<Outcome>, at: &str) -> Event {
        Event {
            seq: 1,
            at: at.into(),
            session_id: "app-1".into(),
            agent: "claude".into(),
            project_root: "/p".into(),
            kind,
            source: Source::Hook,
            tool: None,
            target: Some(target.into()),
            outcome,
            text: None,
            truncated: false,
        }
    }

    #[test]
    fn stats_sum_sessions_and_derive_waste_from_activity() {
        let mut a = SessionState::new();
        a.id = "lean-b".into();
        a.project_root = Some("/p".into());
        a.stats.total_tokens_input = 100;
        a.stats.total_tokens_saved = 60;
        a.stats.security.secrets_redacted = 2;
        a.files_touched.push(lean_ctx::core::session::FileTouched {
            path: "src/a.rs".into(),
            file_ref: None,
            read_count: 3,
            modified: false,
            last_mode: "full".into(),
            tokens: 0,
            stale: false,
            context_item_id: None,
            summary: None,
        });
        let mut b = SessionState::new();
        b.id = "lean-a".into();
        b.stats.total_tokens_input = 50;
        b.stats.security.content_withheld = 1;
        let events = vec![
            event(
                Kind::FileRead,
                "src/a.rs",
                Some(Outcome::Ok),
                "2026-11-04T09:00:00.000Z",
            ),
            event(
                Kind::FileRead,
                "src/b.rs",
                Some(Outcome::Ok),
                "2026-11-04T09:00:01.000Z",
            ),
            event(
                Kind::FileRead,
                "src/b.rs",
                Some(Outcome::Ok),
                "2026-11-04T09:00:02.000Z",
            ),
            event(
                Kind::Shell,
                "cargo test",
                Some(Outcome::Error),
                "2026-11-04T09:01:00.000Z",
            ),
            event(
                Kind::Shell,
                "cargo test",
                Some(Outcome::Error),
                "2026-11-04T09:02:00.000Z",
            ),
            event(
                Kind::Shell,
                "ls",
                Some(Outcome::Ok),
                "2026-11-04T09:03:00.000Z",
            ),
        ];
        let doc = stats_of("app-1", &[a, b], &events).expect("known");
        assert_eq!(doc.lean_sessions, ["lean-a", "lean-b"]);
        assert_eq!((doc.tokens.input, doc.tokens.saved), (150, 60));
        assert_eq!((doc.kept_out.total, doc.kept_out.content_withheld), (3, 1));
        assert_eq!(doc.project_root.as_deref(), Some("/p"));
        let reads = &doc.waste.repeated_reads;
        assert_eq!((reads.files, reads.extra_reads), (2, 3));
        assert_eq!(
            reads.top[0],
            ReadRow {
                path: "src/a.rs".into(),
                reads: 3
            }
        );
        let failing = &doc.waste.failing_commands;
        assert_eq!(failing.failures, 2);
        assert_eq!(failing.top[0].last_at, "2026-11-04T09:02:00.000Z");
        assert_eq!(
            stats_of("nobody", &[], &[]).map(|_| ()).unwrap_err().code,
            Code::NotFound
        );
    }

    #[test]
    fn cost_without_proxy_turns_is_unavailable_and_zero() {
        let doc = cost_of("app-1", None);
        assert!(!doc.available);
        assert_eq!((doc.total_usd, doc.requests), (0.0, 0));
        assert!(doc.per_model.is_empty());
    }
}
