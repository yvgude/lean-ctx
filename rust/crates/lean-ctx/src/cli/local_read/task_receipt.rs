// SPDX-License-Identifier: Apache-2.0
//! Task receipt v1 (`task.receipt`, `docs/contracts/task-receipt-v1.md`):
//! the engine side of the per-task receipt of a finished board task (h8e
//! launch criterion C7/C8).
//!
//! `lean-ctx task receipt <task> --json --session ID [--session ID …]`
//! (socket: `GET /v1/local/task/{id}/receipt?session=…`). The caller (the
//! vault) names the task and the session of every stage; LeanCTX answers,
//! per session, which context was delivered to which agent under which
//! policy, with the hash-chained ledger references a third person can check
//! against the local chains — the same chains `session.value` re-walks.
//!
//! Content-free: ids, counts, digests and chain hashes only. No path, no
//! file content, no prompt, no timestamp, no counter: the document is a
//! function of the stored data and the request. Free for everyone: no plan
//! check. Wired as one `local_read::Part`.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use lean_ctx::core::context_store::lineage::{DeliverySummaryV1, TaskLineageV1};
use lean_ctx::core::execution_ledger::{ExecutionEvent, ExecutionLedgerStore};
use lean_ctx::core::security_events::SecurityCounts;
use lean_ctx::core::session::SessionState;
use lean_ctx::core::value::proof::{ChainCheck, Evidence, Proof};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The schema id every receipt names (`receipt_schema`).
pub(crate) const RECEIPT_SCHEMA: &str = "leanctx.task-receipt/v1";

/// At most this many sessions per receipt (one per stage of the task).
const MAX_SESSIONS: usize = 64;

/// At most this many MCP tasks are resolved per session; more is a gap.
const MAX_MCP_TASKS: usize = 64;

/// At most this many MCP tasks are resolved per receipt, over all its
/// sessions in request order; more is the gap `mcp_tasks_truncated`.
const MAX_RECEIPT_MCP_TASKS: usize = 256;

/// Set while this process builds a receipt. A socket timeout ends the
/// response, not the blocking work behind it, so a second request while one
/// is still running is refused instead of piling up another full build.
static BUILDING: AtomicBool = AtomicBool::new(false);

/// The claim on a flag like [`BUILDING`]; released when dropped (also on
/// unwind).
struct Claim<'a>(&'a AtomicBool);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Claims `flag`, or `None` while another claim holds it.
fn claim(flag: &AtomicBool) -> Option<Claim<'_>> {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .ok()
        .map(|_| Claim(flag))
}

const SESSION: Param = Param {
    name: "session",
    flag: "--session",
    shape: Shape::Repeat,
};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[Route {
    kind: "task.receipt",
    method: Method::Get,
    path: "/v1/local/task/{id}/receipt",
    cli: &["task", "receipt", "<id>", "--json"],
    action: &["receipt"],
    params: &[SESSION],
    project: Project::None,
    names: None,
}];

/// This part's document kinds (sorted).
pub(crate) const KINDS: &[&str] = &["task.receipt"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("task", &["receipt"])];

/// The release root: the receipt names no project.
const NO_PROJECT: &str = "";

/// One hash chain's verification, without its local path.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ChainDoc {
    pub intact: bool,
    pub entries: usize,
    pub first_invalid_at: Option<usize>,
}

impl From<&ChainCheck> for ChainDoc {
    fn from(check: &ChainCheck) -> Self {
        Self {
            intact: check.intact,
            entries: check.entries,
            first_invalid_at: check.first_invalid_at,
        }
    }
}

/// The savings-ledger token sums of the session.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TokensDoc {
    pub baseline: u64,
    pub saved: u64,
    pub bounced: u64,
}

/// An agent that worked in the session, as its engine evidence names it.
#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AgentDoc {
    pub agent_id: Option<String>,
    /// The MCP client (`claude-code`, `codex`, …).
    pub client: Option<String>,
}

/// One governed delivery of an MCP task, from its verified Decision Receipt.
#[derive(Debug, Serialize)]
pub(crate) struct DeliveryDoc {
    /// `sha256:…` of the stored Decision Receipt.
    pub digest: String,
    /// The receipt was found and its digest matched.
    pub verified: bool,
    /// `missing`, `unreadable` or `tampered` when not verified.
    pub error: Option<&'static str>,
    /// Outcome, destination, policy digest, counts and the final-context
    /// digest of the delivery.
    pub summary: Option<DeliverySummaryV1>,
}

/// One MCP task (the engine's `mcp-task-…` id) of the session.
#[derive(Debug, Serialize)]
pub(crate) struct McpTaskDoc {
    pub task_id: String,
    /// The last recorded outcome, else `unknown`.
    pub outcome: String,
    pub deliveries: Vec<DeliveryDoc>,
    /// The missing links of its lineage (`inspect --task` gaps).
    pub gaps: Vec<&'static str>,
}

/// The receipt of one session of the task.
#[derive(Debug, Serialize)]
pub(crate) struct SessionReceipt {
    pub session_id: String,
    /// Engine sessions saved under this id (itself or as app session id).
    pub engine_sessions: usize,
    /// A chain failed verification: the numbers are not proof.
    pub chain_broken: bool,
    pub ledger: ChainDoc,
    pub audit: ChainDoc,
    /// First and last `entry_hash` of the session's savings-ledger entries.
    pub ledger_evidence: Evidence,
    /// First and last `entry_hash` of the session's audit-trail entries.
    pub audit_evidence: Evidence,
    pub tokens: TokensDoc,
    pub security: SecurityCounts,
    pub agents: Vec<AgentDoc>,
    pub mcp_tasks: Vec<McpTaskDoc>,
    pub gaps: Vec<&'static str>,
}

/// The `task.receipt` body.
#[derive(Debug, Serialize)]
pub(crate) struct TaskReceiptDoc {
    pub receipt_schema: &'static str,
    /// The board task the caller named.
    pub task: String,
    /// Any session's chain failed verification (h8e C8 fails).
    pub chain_broken: bool,
    /// One per requested session, in request order.
    pub sessions: Vec<SessionReceipt>,
    /// Every session gap, sorted, once.
    pub gaps: Vec<&'static str>,
}

/// Where the receipt's facts come from (the local stores, or a test's).
/// Each store is read once per receipt, whatever the number of sessions.
pub(crate) struct Sources<'a> {
    /// The value proofs over the two chains, one per group of session ids
    /// (a requested id and its engine sessions' ids), in group order.
    pub proofs: &'a dyn Fn(&[BTreeSet<String>]) -> Vec<Proof>,
    /// Every engine session saved under one of the ids (as its id or as its
    /// app session id), oldest first.
    pub sessions: &'a dyn Fn(&[String]) -> Vec<SessionState>,
    /// The lineage of one MCP task within one tenant/project scope.
    pub lineage: &'a dyn Fn(&str, &str) -> TaskLineageV1,
}

/// The tenant/project scope the MCP tasks of `session` are indexed under:
/// MCP tasks carry the session's project root as their project id.
fn scope_of(session: &SessionState) -> Option<String> {
    let root = session.project_root.clone()?;
    let project = lean_ctx_protocol::ProjectId::new(root).ok()?;
    Some(lean_ctx::core::context_store::task_scope(None, &project))
}

fn mcp_task(scope: Option<&str>, task_id: &str, sources: &Sources<'_>) -> McpTaskDoc {
    let Some(scope) = scope else {
        return McpTaskDoc {
            task_id: task_id.to_owned(),
            outcome: "unknown".to_owned(),
            deliveries: Vec::new(),
            gaps: vec!["no_project_scope"],
        };
    };
    let lineage = (sources.lineage)(scope, task_id);
    McpTaskDoc {
        task_id: task_id.to_owned(),
        outcome: lineage.outcome,
        deliveries: lineage
            .deliveries
            .into_iter()
            .map(|d| DeliveryDoc {
                digest: d.digest,
                verified: d.verified,
                error: d.error,
                summary: d.summary,
            })
            .collect(),
        gaps: lineage.gaps,
    }
}

/// The receipt of the requested session `id`, from its engine sessions and
/// the proof over `{id}` and their ids. `budget` is what is left of
/// [`MAX_RECEIPT_MCP_TASKS`].
fn session_receipt(
    id: &str,
    engine: &[&SessionState],
    proof: Proof,
    budget: &mut usize,
    sources: &Sources<'_>,
) -> SessionReceipt {
    let mut agents: BTreeSet<AgentDoc> = BTreeSet::new();
    // (scope, MCP task id), sorted and once.
    let mut tasks: BTreeSet<(Option<String>, String)> = BTreeSet::new();
    for session in engine {
        let scope = scope_of(session);
        if let Some(client) = &session.stats.client {
            agents.insert(AgentDoc {
                agent_id: None,
                client: Some(client.clone()),
            });
        }
        for record in &session.evidence {
            if record.agent_id.is_some() || record.client_name.is_some() {
                agents.insert(AgentDoc {
                    agent_id: record.agent_id.clone(),
                    client: record.client_name.clone(),
                });
            }
            if let Some(task) = &record.task_id {
                tasks.insert((scope.clone(), task.clone()));
            }
        }
    }
    // A client named with an agent id needs no client-only row.
    let named: BTreeSet<String> = agents
        .iter()
        .filter(|a| a.agent_id.is_some())
        .filter_map(|a| a.client.clone())
        .collect();
    agents.retain(|a| a.agent_id.is_some() || a.client.as_ref().is_none_or(|c| !named.contains(c)));

    let mut gaps = Vec::new();
    let take = tasks.len().min(MAX_MCP_TASKS).min(*budget);
    *budget -= take;
    let truncated = take < tasks.len();
    if truncated {
        gaps.push("mcp_tasks_truncated");
    }
    let mcp_tasks: Vec<McpTaskDoc> = tasks
        .iter()
        .take(take)
        .map(|(scope, task)| mcp_task(scope.as_deref(), task, sources))
        .collect();
    if engine.is_empty() {
        gaps.push("no_engine_session");
    }
    if proof.ledger_evidence.entries == 0 {
        gaps.push("no_ledger_evidence");
    }
    // Unresolved tasks may hold a delivery: truncation is the gap then.
    if !truncated && !mcp_tasks.iter().any(|t| !t.deliveries.is_empty()) {
        gaps.push("no_context_delivery");
    }
    if mcp_tasks
        .iter()
        .flat_map(|t| &t.deliveries)
        .any(|d| !d.verified)
    {
        gaps.push("delivery_unverified");
    }
    gaps.sort_unstable();

    SessionReceipt {
        session_id: id.to_owned(),
        engine_sessions: engine.len(),
        chain_broken: proof.tampered(),
        ledger: ChainDoc::from(&proof.ledger),
        audit: ChainDoc::from(&proof.audit),
        tokens: TokensDoc {
            baseline: proof.tokens_baseline,
            saved: proof.tokens_saved,
            bounced: proof.tokens_bounced,
        },
        ledger_evidence: proof.ledger_evidence,
        audit_evidence: proof.audit_evidence,
        security: proof.security,
        agents: agents.into_iter().collect(),
        mcp_tasks,
        gaps,
    }
}

/// The receipt of `task` over `sessions` (validated, deduplicated).
///
/// The sessions directory, the two chains and the execution ledger are each
/// read once for all sessions together; at most [`MAX_RECEIPT_MCP_TASKS`]
/// MCP tasks are resolved.
pub(crate) fn build(task: &str, sessions: &[String], sources: &Sources<'_>) -> TaskReceiptDoc {
    let saved = (sources.sessions)(sessions);
    let engines: Vec<Vec<&SessionState>> = sessions
        .iter()
        .map(|id| {
            saved
                .iter()
                .filter(|s| s.id == *id || s.app_session_id.as_deref() == Some(id.as_str()))
                .collect()
        })
        .collect();
    // Ledger and audit entries carry the engine session id: a requested app
    // session id is proven over itself and its engine sessions' ids.
    let groups: Vec<BTreeSet<String>> = sessions
        .iter()
        .zip(&engines)
        .map(|(id, engine)| {
            std::iter::once(id.clone())
                .chain(engine.iter().map(|s| s.id.clone()))
                .collect()
        })
        .collect();
    let mut proofs = (sources.proofs)(&groups).into_iter();
    let mut budget = MAX_RECEIPT_MCP_TASKS;
    let sessions: Vec<SessionReceipt> = sessions
        .iter()
        .zip(&engines)
        .map(|(id, engine)| {
            let proof = proofs.next().unwrap_or_else(|| {
                lean_ctx::core::value::proof::build_from(None, None, Some(id), None)
            });
            session_receipt(id, engine, proof, &mut budget, sources)
        })
        .collect();
    let gaps: BTreeSet<&'static str> = sessions
        .iter()
        .flat_map(|s| s.gaps.iter().copied())
        .collect();
    TaskReceiptDoc {
        receipt_schema: RECEIPT_SCHEMA,
        task: task.to_owned(),
        chain_broken: sessions.iter().any(|s| s.chain_broken),
        sessions,
        gaps: gaps.into_iter().collect(),
    }
}

/// The verified execution ledger, read and verified once, by task id.
fn execution_by_task() -> Result<BTreeMap<String, Vec<ExecutionEvent>>, String> {
    let events = ExecutionLedgerStore::from_default()
        .and_then(|store| store.load_verified())
        .map_err(|error| error.to_string())?;
    let mut by_task: BTreeMap<String, Vec<ExecutionEvent>> = BTreeMap::new();
    for event in events {
        by_task
            .entry(event.task_id().to_owned())
            .or_default()
            .push(event);
    }
    Ok(by_task)
}

/// [`build`] over this machine's chains, sessions and receipt stores.
fn build_local(task: &str, sessions: &[String]) -> TaskReceiptDoc {
    let ledger = lean_ctx::core::savings_ledger::store::default_path();
    let trail = lean_ctx::core::audit_trail::default_trail_path();
    let proofs = |groups: &[BTreeSet<String>]| {
        lean_ctx::core::value::proof::build_sessions(ledger.as_deref(), trail.as_deref(), groups)
    };
    let attributed = SessionState::load_attributed_all;
    // Read on the first MCP task with a scope, then shared by all of them.
    let execution: OnceCell<Result<BTreeMap<String, Vec<ExecutionEvent>>, String>> =
        OnceCell::new();
    let lineage = |scope: &str, task_id: &str| {
        let events = match execution.get_or_init(execution_by_task) {
            Ok(by_task) => Ok(by_task.get(task_id).cloned().unwrap_or_default()),
            Err(error) => Err(error.clone()),
        };
        lean_ctx::core::context_store::lineage::load_with_events(scope, task_id, events)
    };
    build(
        task,
        sessions,
        &Sources {
            proofs: &proofs,
            sessions: &attributed,
            lineage: &lineage,
        },
    )
}

/// One id of a request: 1–128 of `[A-Za-z0-9._:-]`, without surrounding
/// whitespace (the receipt echoes what it was asked for).
fn id_of(raw: &str) -> Option<String> {
    if raw.trim() != raw {
        return None;
    }
    lean_ctx::core::activity::session_id(raw)
}

/// The task and the sessions of a request: `receipt <task> --session ID…`.
fn request(args: &[String]) -> Result<(String, Vec<String>), Failure> {
    const ID: &str = "1-128 of [A-Za-z0-9._:-]";
    let raw = super::positionals(args)
        .get(1)
        .copied()
        .ok_or_else(|| Failure::invalid("task receipt <task> is required"))?;
    let task = id_of(raw).ok_or_else(|| Failure::invalid(format!("the task id is {ID}")))?;
    let mut sessions: Vec<String> = Vec::new();
    for raw in super::flags(args, "--session")? {
        let id = id_of(raw).ok_or_else(|| Failure::invalid(format!("--session is {ID}")))?;
        if !sessions.contains(&id) {
            sessions.push(id);
        }
    }
    if sessions.is_empty() {
        return Err(Failure::invalid(
            "--session <id> is required, once per session of the task",
        ));
    }
    if sessions.len() > MAX_SESSIONS {
        return Err(Failure::invalid(format!(
            "at most {MAX_SESSIONS} sessions per receipt"
        )));
    }
    Ok((task, sessions))
}

/// The document of `kind` for `args` (same bytes on the CLI and the socket).
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        "task.receipt" => super::answer_released("ctx_proof", NO_PROJECT, kind, via, || {
            let (task, sessions) = request(args)?;
            let _building = claim(&BUILDING).ok_or_else(|| {
                Failure::new(
                    Code::Unavailable,
                    "a task receipt is still being built; retry later",
                )
            })?;
            Ok(build_local(&task, &sessions))
        }),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// The kind `lean-ctx <command> <verb> … --json` prints, when it is this part's.
fn kind_of(command: &str, rest: &[String]) -> Option<&'static str> {
    let verb = super::positionals(rest).first().copied()?;
    match (command, verb) {
        ("task", "receipt") => Some("task.receipt"),
        _ => None,
    }
}

/// `lean-ctx task receipt <task> --json …`: prints the document and returns
/// `true`; anything else is left to the dispatcher.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    if !super::wants_json(rest) {
        return false;
    }
    let Some(kind) = kind_of(command, rest) else {
        return false;
    };
    super::print(&answer(kind, rest, Via::Cli));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::context_store::lineage::LineageDeliveryV1;
    use lean_ctx::core::savings_ledger::{self, SavingsEvent};
    use lean_ctx::core::session::{EvidenceKind, EvidenceRecord};
    use serde_json::json;
    use std::path::Path;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    fn ledger_event(saved: u64, session: &str) -> SavingsEvent {
        serde_json::from_value(json!({
            "ts": "2026-11-15T10:00:00+00:00",
            "tool": "ctx_read",
            "mechanism": "compression",
            "model_id": "m",
            "tokenizer": "o200k_base",
            "baseline_tokens": saved + 100,
            "actual_tokens": 100,
            "saved_tokens": saved,
            "bounce_adjustment": 0,
            "unit_price_per_m_usd": 3.0,
            "saved_usd": 0.0,
            "repo_hash": "r",
            "agent_id": "a",
            "prev_hash": "",
            "entry_hash": "",
            "version": "3.12.3",
            "session_id": session,
        }))
        .unwrap()
    }

    fn evidence(task: Option<&str>, agent: Option<&str>, client: Option<&str>) -> EvidenceRecord {
        EvidenceRecord {
            kind: EvidenceKind::ToolCall,
            key: "tool:ctx_read".into(),
            value: None,
            tool: Some("ctx_read".into()),
            input_md5: None,
            output_md5: None,
            agent_id: agent.map(str::to_owned),
            client_name: client.map(str::to_owned),
            task_id: task.map(str::to_owned),
            timestamp: lean_ctx_time::UtcDateTime::now(),
        }
    }

    fn engine_session(id: &str, root: &Path) -> SessionState {
        let mut session = SessionState::new();
        session.id = format!("{id}-engine");
        session.app_session_id = Some(id.to_owned());
        session.project_root = Some(root.to_string_lossy().into_owned());
        session.stats.client = Some("claude-code".into());
        session.evidence = vec![
            evidence(Some("mcp-task-1"), Some("agent-a"), Some("claude-code")),
            evidence(Some("mcp-task-1"), Some("agent-a"), Some("claude-code")),
            evidence(None, None, None),
        ];
        session
    }

    fn lineage(scope: &str, task: &str) -> TaskLineageV1 {
        TaskLineageV1 {
            schema_version: 1,
            task_id: task.to_owned(),
            scope: Some(scope.to_owned()),
            steps: Vec::new(),
            deliveries: vec![LineageDeliveryV1 {
                digest: format!("sha256:{}", "a".repeat(64)),
                verified: true,
                error: None,
                summary: Some(DeliverySummaryV1 {
                    outcome: "delivered".into(),
                    delivery_semantics: "returned_to_host",
                    destination: "mcp-host".into(),
                    policy_digest: Some(format!("sha256:{}", "b".repeat(64))),
                    inspected: 3,
                    delivered: 2,
                    withheld: 1,
                    redactions: 0,
                    tokens_original: 900,
                    tokens_delivered: 300,
                    final_context: Some(format!("sha256:{}", "c".repeat(64))),
                }),
                egress: None,
            }],
            ledger_error: None,
            outcome: "accepted".into(),
            gaps: vec!["no_plan_recorded"],
        }
    }

    /// Which chain a fixture breaks after recording.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Tamper {
        Nothing,
        Ledger,
        Trail,
    }

    /// Records one security event in the trail at `path`.
    fn security(path: &Path, action: &str) {
        use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
        audit_trail::record_at(
            path,
            AuditEntryData {
                agent_id: "a".into(),
                tool: "ctx_shell".into(),
                action: Some(action.into()),
                input_hash: String::new(),
                output_tokens: 0,
                role: "coder".into(),
                event_type: AuditEventType::SecretDetected,
            },
        );
    }

    /// The receipt of task 418 over `s1` (an app session id: one engine
    /// session `s1-engine` with a delivery) and `s2` (nothing recorded).
    /// The ledger and the trail record under the engine id, as the engine
    /// does (`value::current_session()` is the engine session id). With
    /// `trail`, the audit trail holds two signed entries; its hashes cover a
    /// clock and a key, so a byte comparison across runs leaves it out. The
    /// data dir is isolated: the trail is signed with its key.
    fn receipt(
        tamper: Tamper,
        trail: bool,
    ) -> (
        lean_ctx::core::data_dir::IsolatedDataDir,
        serde_json::Value,
        String,
    ) {
        let dir = lean_ctx::core::data_dir::isolated_data_dir();
        let ledger = dir.path().join("ledger.jsonl");
        let trail_path = dir.path().join("trail.jsonl");
        savings_ledger::store::append(&ledger, ledger_event(1_000, "s1-engine")).unwrap();
        savings_ledger::store::append(&ledger, ledger_event(500, "other")).unwrap();
        if trail {
            security(&trail_path, "secret_redacted:2|session=s1-engine");
            security(&trail_path, "shell_blocked:1|session=other");
        }
        let trail = trail_path;
        match tamper {
            Tamper::Nothing => {}
            Tamper::Ledger => {
                let text = std::fs::read_to_string(&ledger).unwrap();
                std::fs::write(
                    &ledger,
                    text.replacen("\"saved_tokens\":500", "\"saved_tokens\":900", 1),
                )
                .unwrap();
            }
            Tamper::Trail => {
                let text = std::fs::read_to_string(&trail).unwrap();
                std::fs::write(
                    &trail,
                    text.replacen("shell_blocked:1", "shell_blocked:7", 1),
                )
                .unwrap();
            }
        }
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let proofs = |groups: &[BTreeSet<String>]| {
            lean_ctx::core::value::proof::build_sessions(Some(&ledger), Some(&trail), groups)
        };
        let sessions = |ids: &[String]| {
            assert_eq!(ids, words(&["s1", "s2"]), "one read for every id");
            vec![engine_session("s1", &root)]
        };
        let doc = build(
            "418",
            &words(&["s1", "s2"]),
            &Sources {
                proofs: &proofs,
                sessions: &sessions,
                lineage: &lineage,
            },
        );
        let text = super::super::document("task.receipt", &doc);
        let value = serde_json::from_str(&text).unwrap();
        (dir, value, text)
    }

    #[test]
    fn the_task_receipt_kind_is_served_and_listed() {
        use super::super::{capabilities, routes};
        let table = routes::table_json();
        let caps = capabilities::build_with(table.clone(), routes::events_json());
        let commands = capabilities::commands_of(&table);
        let path = "/v1/local/task/{id}/receipt";
        let route = routes::all()
            .find(|r| r.path == path)
            .expect("the receipt is routed");
        assert_eq!(route.kind, "task.receipt");
        let row = table
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == path)
            .expect("listed in capabilities.routes");
        assert_eq!(row["method"], "GET");
        assert_eq!(row["project"], json!(false));
        assert_eq!(row["command"], "task");
        assert_eq!(row["verb"], "receipt");
        assert_eq!(row["params"], json!(["session"]));
        assert!(
            commands["task"]
                .as_array()
                .is_some_and(|v| v.contains(&json!({"verb": "receipt", "via": ["cli", "socket"]}))),
            "{commands:?}"
        );
        assert!(caps.kinds.contains(&"task.receipt"));
        assert!(super::super::part_of("task.receipt").is_some());
    }

    #[test]
    fn the_part_lists_exactly_its_routes() {
        let mut routed: Vec<&str> = ROUTES.iter().map(|r| r.kind).collect();
        routed.sort_unstable();
        assert_eq!(routed, KINDS);
    }

    #[test]
    fn requests_become_the_cli_arguments() {
        use super::super::routes;
        let args = routes::args_of(
            &ROUTES[0],
            Some("418"),
            &[("session".into(), json!(["s1", "s2", "s1"]))],
        )
        .unwrap();
        assert_eq!(args[..3], words(&["receipt", "418", "--json"])[..]);
        assert_eq!(
            request(&args).unwrap(),
            ("418".to_owned(), words(&["s1", "s2"]))
        );
        let missing = routes::args_of(&ROUTES[0], None, &[]).unwrap_err();
        assert_eq!(missing.code, super::super::Code::InvalidArgument);
        assert!(request(&words(&["receipt", "418", "--json"])).is_err());
        assert!(request(&words(&["receipt", "--json", "--session", "s1"])).is_err());
        assert!(request(&words(&["receipt", "../x", "--session", "s1"])).is_err());
        assert!(request(&words(&["receipt", "418", "--session", "a/b"])).is_err());
    }

    #[test]
    fn the_cli_claims_only_its_json_verb() {
        assert_eq!(
            kind_of(
                "task",
                &words(&["--session", "s1", "receipt", "418", "--json"])
            ),
            Some("task.receipt")
        );
        assert_eq!(kind_of("task", &words(&["show", "--json"])), None);
        assert_eq!(kind_of("tasks", &words(&["receipt", "--json"])), None);
        assert!(!try_run("task", &words(&["receipt", "418"])));
    }

    /// Each session carries its chains, ledger hashes, agents and the
    /// verified deliveries of its MCP tasks, with policy and final-context
    /// digests — and nothing that names a local path.
    #[test]
    fn a_receipt_binds_each_session_to_its_chains_agents_and_deliveries() {
        let (dir, doc, text) = receipt(Tamper::Nothing, true);
        assert_eq!(doc["contract"], "local-read/1");
        assert_eq!(doc["kind"], "task.receipt");
        assert_eq!(doc["receipt_schema"], RECEIPT_SCHEMA);
        assert_eq!(doc["task"], "418");
        assert_eq!(doc["chain_broken"], false);

        let s1 = &doc["sessions"][0];
        assert_eq!(s1["session_id"], "s1");
        assert_eq!(s1["engine_sessions"], 1);
        assert_eq!(s1["chain_broken"], false);
        assert_eq!(
            s1["ledger"],
            json!({"intact": true, "entries": 2, "first_invalid_at": null})
        );
        assert_eq!(
            s1["audit"],
            json!({"intact": true, "entries": 2, "first_invalid_at": null})
        );
        assert_eq!(s1["ledger_evidence"]["entries"], 1);
        assert!(s1["ledger_evidence"]["first_hash"].is_string());
        assert_eq!(s1["tokens"]["saved"], 1_000);
        assert_eq!(s1["audit_evidence"]["entries"], 1);
        assert_eq!(s1["security"]["secrets_redacted"], 2);
        assert_eq!(s1["security"]["shell_blocked"], 0);
        assert_eq!(
            s1["agents"],
            json!([{"agent_id": "agent-a", "client": "claude-code"}])
        );
        let task = &s1["mcp_tasks"][0];
        assert_eq!(task["task_id"], "mcp-task-1");
        assert_eq!(task["outcome"], "accepted");
        let delivery = &task["deliveries"][0];
        assert_eq!(delivery["verified"], true);
        assert_eq!(
            delivery["summary"]["policy_digest"],
            format!("sha256:{}", "b".repeat(64))
        );
        assert_eq!(delivery["summary"]["delivered"], 2);
        assert_eq!(s1["mcp_tasks"].as_array().unwrap().len(), 1);
        assert_eq!(s1["gaps"], json!([]));

        let s2 = &doc["sessions"][1];
        assert_eq!(s2["session_id"], "s2");
        assert_eq!(s2["engine_sessions"], 0);
        assert_eq!(
            s2["gaps"],
            json!([
                "no_context_delivery",
                "no_engine_session",
                "no_ledger_evidence"
            ])
        );
        assert_eq!(doc["gaps"], s2["gaps"]);

        let tmp = dir.path().to_string_lossy();
        assert!(!text.contains(tmp.as_ref()), "no local path: {text}");
        assert!(!text.contains("\"path\""), "no path field: {text}");
        assert!(!text.contains("2026-11-15"), "no timestamp: {text}");
    }

    /// h8e C8: a tampered chain marks every session it covers and the receipt.
    #[test]
    fn a_tampered_ledger_breaks_the_receipt() {
        let (_dir, doc, _) = receipt(Tamper::Ledger, true);
        assert_eq!(doc["chain_broken"], true);
        for session in doc["sessions"].as_array().unwrap() {
            assert_eq!(session["chain_broken"], true, "{session}");
            assert_eq!(session["ledger"]["intact"], false);
            assert!(session["ledger"]["first_invalid_at"].is_number());
            assert_eq!(session["audit"]["intact"], true);
        }
    }

    /// h8e C8 through the receipt for the audit trail too: an entry of
    /// another session is edited, and every session is discredited.
    #[test]
    fn a_tampered_trail_breaks_the_receipt() {
        let (_dir, doc, _) = receipt(Tamper::Trail, true);
        assert_eq!(doc["chain_broken"], true);
        for session in doc["sessions"].as_array().unwrap() {
            assert_eq!(session["chain_broken"], true, "{session}");
            assert_eq!(session["audit"]["intact"], false);
            assert!(session["audit"]["first_invalid_at"].is_number());
            assert_eq!(session["ledger"]["intact"], true);
        }
    }

    /// The vault asks with its app session id; the engine records ledger and
    /// audit entries under the engine session id. The receipt proves the app
    /// id over its engine sessions' ids (review !378, blocking 2).
    #[test]
    fn an_app_session_id_carries_its_engine_sessions_evidence() {
        let (dir, doc, _) = receipt(Tamper::Nothing, true);
        let s1 = &doc["sessions"][0];
        assert_eq!(s1["session_id"], "s1");
        assert!(
            s1["ledger_evidence"]["entries"].as_u64().unwrap() > 0,
            "{s1}"
        );
        assert!(
            s1["audit_evidence"]["entries"].as_u64().unwrap() > 0,
            "{s1}"
        );
        assert_eq!(s1["tokens"]["saved"], 1_000);
        assert!(
            !s1["gaps"]
                .as_array()
                .unwrap()
                .contains(&json!("no_ledger_evidence")),
            "{s1}"
        );
        // Before the fix the proof was built for the requested id alone,
        // which no ledger or audit entry carries.
        let alone = lean_ctx::core::value::proof::build_from(
            Some(&dir.path().join("ledger.jsonl")),
            Some(&dir.path().join("trail.jsonl")),
            Some("s1"),
            None,
        );
        assert_eq!(alone.ledger_evidence.entries, 0);
        assert_eq!(alone.audit_evidence.entries, 0);
    }

    /// Bounded cost: at most `MAX_RECEIPT_MCP_TASKS` lineages per receipt,
    /// whatever the number of sessions; the rest is the gap
    /// `mcp_tasks_truncated` per session and for the receipt.
    #[test]
    fn mcp_tasks_are_capped_across_the_whole_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let ids: Vec<String> = (0..6).map(|i| format!("app-{i}")).collect();
        let sessions = |wanted: &[String]| {
            wanted
                .iter()
                .map(|id| {
                    let mut session = engine_session(id, &root);
                    session.evidence = (0..MAX_MCP_TASKS)
                        .map(|t| evidence(Some(format!("mcp-{id}-{t:03}").as_str()), None, None))
                        .collect();
                    session
                })
                .collect()
        };
        let proof_calls = std::cell::Cell::new(0);
        let proofs = |groups: &[BTreeSet<String>]| {
            proof_calls.set(proof_calls.get() + 1);
            lean_ctx::core::value::proof::build_sessions(None, None, groups)
        };
        let resolved = std::cell::Cell::new(0);
        let counted = |scope: &str, task: &str| {
            resolved.set(resolved.get() + 1);
            lineage(scope, task)
        };
        let doc = build(
            "418",
            &ids,
            &Sources {
                proofs: &proofs,
                sessions: &sessions,
                lineage: &counted,
            },
        );
        assert_eq!(proof_calls.get(), 1, "one chain pass for every session");
        assert_eq!(resolved.get(), MAX_RECEIPT_MCP_TASKS);
        let per_session: Vec<usize> = doc.sessions.iter().map(|s| s.mcp_tasks.len()).collect();
        assert_eq!(per_session, vec![64, 64, 64, 64, 0, 0]);
        assert!(doc.gaps.contains(&"mcp_tasks_truncated"));
        assert!(!doc.sessions[3].gaps.contains(&"mcp_tasks_truncated"));
        assert!(doc.sessions[4].gaps.contains(&"mcp_tasks_truncated"));
        assert!(!doc.sessions[4].gaps.contains(&"no_context_delivery"));
    }

    /// A socket timeout ends the response, not the build: while one build
    /// holds the claim, another is refused instead of piling up.
    #[test]
    fn only_one_receipt_is_built_at_a_time() {
        let flag = AtomicBool::new(false);
        let first = claim(&flag).expect("free");
        assert!(claim(&flag).is_none(), "busy while the first build runs");
        drop(first);
        assert!(claim(&flag).is_some(), "free again once it ends");
    }

    #[test]
    fn ids_with_surrounding_whitespace_are_rejected() {
        assert!(request(&words(&["receipt", " 418 ", "--session", "s1"])).is_err());
        assert!(request(&words(&["receipt", "418", "--session", "s1 "])).is_err());
    }

    #[test]
    fn the_same_stores_give_the_same_bytes() {
        // Each run has its own data dir and chains; the hashes depend only on
        // the events, so the documents are byte-identical. One isolated data
        // dir at a time.
        let first = receipt(Tamper::Nothing, false).2;
        let second = receipt(Tamper::Nothing, false).2;
        assert_eq!(first, second);
    }
}
