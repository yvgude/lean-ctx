// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx tasks …`: the project's tasks. A project linked to a Hive
//! vault uses Hive exactly as `lean-ctx hive tasks` does; every other
//! project keeps its tasks on this machine (lean-ctx-tasks), and each
//! transition is announced on the local bus.
//!
//! `--json` prints Local Read v1 documents (`"contract": "local-read/1"`,
//! `docs/contracts/local-read-v1.md` §3c): `tasks.list`, `tasks.task` and
//! `tasks.ack` for a write.

use std::path::{Path, PathBuf};

use serde::Serialize;

use lean_ctx_hive::tasks::local::{self, LocalFilter};
use lean_ctx_hive::tasks::{self as hive_tasks, EvidenceInput, TaskView};
use lean_ctx_protocol::hive::tasks::{TaskOutcome, TaskState, TaskVerdict, UsageV1};
use lean_ctx_tasks::{Actor, Amendment, Change, LocalTask, Proposal, Submission, TaskError};

use super::local_read::tasks::{TaskDetail, TaskRow, detail};
use super::local_read::{self, Code, Failure, flag, flags, positionals};

pub(crate) const HELP: &str = "\
lean-ctx tasks — the current project's tasks

A project linked to a Hive vault uses its Hive tasks (as `lean-ctx hive tasks`);
any other project keeps its tasks on this machine. A linked project's local
tasks that were not brought into the vault (left local on purpose, or the link
came in through git) stay reachable with --local until they are settled.

USAGE:
  lean-ctx tasks [list] [--state <state>] [--role <role>] [--mine --agent <id>]
  lean-ctx tasks show <number>
  lean-ctx tasks propose --title <t> --acceptance <a> [--goal <g>] [--role <role>]
  lean-ctx tasks decide <number> --accept|--decline [--reason <line>]
  lean-ctx tasks claim <number> --agent <id> [--minutes 5-240]
  lean-ctx tasks update <number> --summary <line> --agent <id> [--tokens <n>]
  lean-ctx tasks submit <number> --summary <line> --agent <id> --evidence <kind>:<ref>... [--failed] [--tokens <n>]
  lean-ctx tasks amend <number> --agent <id> --evidence <kind>:<ref>... [--summary <line>] [--reason <line>]
  lean-ctx tasks amend <number> [--agent <id>] --merge <sha> [--reason <line>]   (a done task)
  lean-ctx tasks release <number> [--agent <id>] [--reason <line>]
  lean-ctx tasks verify <number> --verdict accept|reject [--findings <line>] [--agent <id>]
  lean-ctx tasks stop <number> --reason <line>   (a person only)
  lean-ctx tasks priority <number> high|normal [--labels a,b]... [--clear-labels]   (a person only)
  lean-ctx tasks labels <number> <a,b,...>|--clear [--priority high|normal]       (a person only)
  lean-ctx tasks <action> … --local   (linked project: its local, not shared tasks)

Every command takes --json (Local Read v1) and --project-root <path>.
--agent <id>: the bus agent acting (`lean-ctx agent presence` lists them);
without it a person acts, named by --by <name> (default \"person\").
Anyone decides on a proposal; only agents claim; the agent that submitted
a result never verifies it, and a reject names its --findings; only a
person stops a task, with a --reason. Only the agent that submitted a result
amends it (no claim needed): while submitted, --evidence (the whole new list)
and --summary replace the old ones. A done task takes only --merge <sha>, once,
from its submitter, a person or a Queen session, never the reviewer whose
accept made it done (not_merger). The history keeps
old and new.
Lists show high priority first, then by number. Priority is high or normal;
labels are up to 16 of [a-z0-9-]{1,32} and a write replaces them all.
Local claims last --minutes (default 60) and are renewed by update.
";

/// A local task's transition as Local Read shows it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TransitionRow {
    pub seq: u64,
    pub event: &'static str,
    pub from: String,
    pub to: String,
    pub by: ByRow,
    pub at: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ByRow {
    pub kind: String,
    pub label: String,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SharedRow {
    pub vault: String,
    pub number: u64,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct VerificationRow {
    pub verdict: String,
    pub by: ByRow,
    pub at: String,
    pub reason: Option<String>,
}

/// A local task in full: the Hive task fields plus who moved it when.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LocalDetail {
    #[serde(flatten)]
    pub detail: TaskDetail,
    pub approved_by: Option<ByRow>,
    pub verification: Option<VerificationRow>,
    pub transitions: Vec<TransitionRow>,
    pub shared: Option<SharedRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalList {
    pub project_root: String,
    pub vault: Option<String>,
    pub tasks: Vec<TaskRow>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocalTaskDoc {
    pub project_root: String,
    pub vault: Option<String>,
    pub task: LocalDetail,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ChangeRow {
    pub number: u64,
    pub event: &'static str,
    pub state: String,
    pub seq: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Ack {
    pub project_root: String,
    pub vault: Option<String>,
    /// `null` when the write changed no state (a claim renewed).
    pub change: Option<ChangeRow>,
    /// The task after the write; `null` for a Hive vault (read it with show).
    pub task: Option<LocalDetail>,
}

fn wire<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

fn by_row(actor: &lean_ctx_protocol::hive::tasks::TaskActorV1) -> ByRow {
    ByRow {
        kind: wire(&actor.kind),
        label: actor.label.clone(),
        agent: actor.agent.clone(),
    }
}

/// The full Local Read view of `local` for the reader `agent`.
pub(crate) fn local_detail(local: &LocalTask, agent: Option<&str>) -> LocalDetail {
    let task = local::for_reader(&local.task, agent);
    let mut detail = detail(&TaskView {
        task: task.clone(),
        held_until: None,
    });
    if let Some(last) = local.transitions.last() {
        last.at.as_str().clone_into(&mut detail.row.updated);
    }
    LocalDetail {
        detail,
        approved_by: task.approved_by.as_ref().map(by_row),
        verification: task.verification.as_ref().map(|v| VerificationRow {
            verdict: wire(&v.verdict),
            by: by_row(&v.by),
            at: v.at.as_str().to_owned(),
            reason: v.reason.clone(),
        }),
        transitions: local
            .transitions
            .iter()
            .map(|t| TransitionRow {
                seq: t.seq,
                event: t.event.as_str(),
                from: wire(&t.from),
                to: wire(&t.to),
                by: by_row(&t.by),
                at: t.at.as_str().to_owned(),
                reason: t.reason.clone(),
            })
            .collect(),
        shared: local.shared.as_ref().map(|s| SharedRow {
            vault: s.vault.clone(),
            number: s.number,
            at: s.at.as_str().to_owned(),
        }),
    }
}

/// A local listing row: unlike a Hive listing it carries goal, acceptance
/// and the claim holder.
pub(crate) fn local_row(local: &LocalTask, agent: Option<&str>) -> TaskRow {
    local_detail(local, agent).detail.row
}

impl From<TaskError> for Failure {
    fn from(error: TaskError) -> Self {
        let code = match &error {
            TaskError::NotFound(_) => Code::NotFound,
            TaskError::InvalidState { .. } => Code::Conflict,
            TaskError::Forbidden { .. } => Code::Forbidden,
            TaskError::Invalid(_) => Code::InvalidArgument,
            TaskError::Store(_) => Code::Unavailable,
        };
        Self::new(code, error.to_string())
    }
}

fn number(args: &[String]) -> Result<u64, Failure> {
    positionals(args)
        .get(1)
        .and_then(|n| n.trim_start_matches('#').parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| Failure::invalid("a task number is required"))
}

fn state_filter(args: &[String]) -> Result<Option<TaskState>, Failure> {
    flag(args, "--state")?
        .map(|s| {
            serde_json::from_value::<TaskState>(serde_json::Value::String(s.to_owned()))
                .ok()
                .filter(|s| *s != TaskState::Unknown)
                .ok_or_else(|| Failure::invalid("unknown --state"))
        })
        .transpose()
}

fn usage(args: &[String]) -> Result<Option<UsageV1>, Failure> {
    flag(args, "--tokens")?
        .map(|t| {
            t.parse()
                .map(|tokens| UsageV1 {
                    tokens,
                    usd_micros: None,
                })
                .map_err(|_| Failure::invalid("--tokens is a number"))
        })
        .transpose()
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str, Failure> {
    flag(args, name)?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| Failure::invalid(format!("{name} is required")))
}

/// Who runs this process, from its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A person's process (Hive Desktop, a shell): no agent marker.
    Person,
    /// An agent that named itself (`LEAN_CTX_AGENT_ID` / `LCTX_AGENT_ID`).
    Agent(String),
    /// A process under an agent host (`CLAUDECODE`, `CODEX_THREAD_ID`,
    /// `CURSOR_TASK_ID`, `LEAN_CTX_AGENT`, `AI_AGENT`) without an agent id.
    UnnamedAgent,
}

impl Caller {
    pub(crate) fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent(id) => Some(id),
            _ => None,
        }
    }
}

/// [`Caller`] from `env` (the process environment in [`caller`]). An
/// explicit agent id wins; any agent marker without one is an unnamed agent.
pub(crate) fn caller_from(env: impl Fn(&str) -> Option<String>) -> Caller {
    let set = |key: &str| {
        env(key)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    if let Some(id) = set("LEAN_CTX_AGENT_ID")
        .or_else(|| set("LCTX_AGENT_ID"))
        .filter(|id| id != "local")
    {
        return Caller::Agent(id);
    }
    let marked = lean_ctx::core::agent_identity::host_from_env(&env).is_some()
        || set("LEAN_CTX_AGENT").is_some()
        || set("AI_AGENT").is_some();
    if marked {
        Caller::UnnamedAgent
    } else {
        Caller::Person
    }
}

/// The caller of this process.
pub(crate) fn caller() -> Caller {
    caller_from(|key| std::env::var(key).ok())
}

/// The agent a read is for (`mine`): the caller's id, else `--agent`.
fn reader(args: &[String], caller: &Caller) -> Result<Option<String>, Failure> {
    Ok(match caller.agent_id() {
        Some(id) => Some(id.to_owned()),
        None => flag(args, "--agent")?.map(str::to_owned),
    })
}

/// Who makes a write. The CLI authenticates no bus identity: it only keeps
/// an agent's process from acting as a person.
/// - A named agent acts as itself; `--by`, or an `--agent` naming another,
///   is refused.
/// - An unnamed agent (agent marker, no id) never acts as a person: `--by`
///   is refused, and `--agent <id>` is required.
/// - A person's process: `--agent <id>`, else a person (`--by <name>`).
fn actor(args: &[String], caller: &Caller) -> Result<Actor, Failure> {
    let named = flag(args, "--agent")?;
    let by = flag(args, "--by")?;
    match caller {
        Caller::Agent(id) => {
            if named.is_some_and(|named| named != id) {
                return Err(Failure::new(
                    Code::Forbidden,
                    format!("--agent names another agent than this process acts for ({id})"),
                ));
            }
            if by.is_some() {
                return Err(Failure::new(
                    Code::Forbidden,
                    format!("this process acts for agent {id}; --by (a person) is refused"),
                ));
            }
            Ok(Actor::agent(id.as_str()))
        }
        Caller::UnnamedAgent => {
            if by.is_some() {
                return Err(Failure::new(
                    Code::Forbidden,
                    "this process runs under an agent; it never acts as a person (--by is refused)",
                ));
            }
            named.map(Actor::agent).ok_or_else(|| {
                Failure::new(
                    Code::Forbidden,
                    "this process runs under an agent: name it with --agent <bus agent id> (it never acts as a person)",
                )
            })
        }
        Caller::Person => Ok(match named {
            Some(id) => Actor::agent(id),
            None => Actor::person(by.unwrap_or("person")),
        }),
    }
}

fn evidence_inputs(args: &[String]) -> Result<Vec<EvidenceInput>, Failure> {
    flags(args, "--evidence")?
        .into_iter()
        .map(|item| {
            item.split_once(':')
                .map(|(kind, reference)| EvidenceInput {
                    kind: kind.to_owned(),
                    reference: reference.to_owned(),
                })
                .ok_or_else(|| Failure::invalid("--evidence is <kind>:<ref>"))
        })
        .collect()
}

fn verdict(args: &[String]) -> Result<TaskVerdict, Failure> {
    match flag(args, "--verdict")? {
        Some("accept") => Ok(TaskVerdict::Accept),
        Some("reject") => Ok(TaskVerdict::Reject),
        _ => Err(Failure::invalid("--verdict is accept or reject")),
    }
}

/// `verify`: the verdict and its findings (required on reject).
fn verdict_findings(args: &[String]) -> Result<(TaskVerdict, Option<&str>), Failure> {
    let verdict = verdict(args)?;
    let findings = flag(args, "--findings")?.filter(|f| !f.trim().is_empty());
    if verdict == TaskVerdict::Reject && findings.is_none() {
        return Err(Failure::invalid(
            "verify --verdict reject needs --findings <line>",
        ));
    }
    Ok((verdict, findings))
}

/// `decide`: exactly one of `--accept` and `--decline`; true to accept.
fn decision(args: &[String]) -> Result<bool, Failure> {
    let has = |name: &str| args.iter().any(|a| a == name);
    match (has("--accept"), has("--decline")) {
        (true, false) => Ok(true),
        (false, true) => Ok(false),
        _ => Err(Failure::invalid(
            "decide needs exactly one of --accept and --decline",
        )),
    }
}

fn summary(args: &[String]) -> Result<String, Failure> {
    match flag(args, "--summary")? {
        Some(s) if !s.trim().is_empty() => Ok(s.to_owned()),
        _ => positionals(args)
            .get(2)
            .map(|s| (*s).to_owned())
            .ok_or_else(|| Failure::invalid("--summary is required")),
    }
}

/// One local write: what changed, and the task afterwards.
fn local_write(
    root: &Path,
    agent: Option<&str>,
    write: impl FnOnce(
        &mut lean_ctx_tasks::Board,
        chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<Change>, TaskError>,
) -> Result<(Option<Change>, Option<LocalDetail>), Failure> {
    let mut after = None;
    let change = local::write(root, |board, now| {
        let change = write(board, now)?;
        let number = change.as_ref().map(|c| c.number);
        after = number.and_then(|n| board.tasks.get(&n).cloned());
        Ok(change)
    })?;
    Ok((change, after.map(|t| local_detail(&t, agent))))
}

fn change_row(change: &Change) -> ChangeRow {
    ChangeRow {
        number: change.number,
        event: change.event.as_str(),
        state: wire(&change.state),
        seq: Some(change.seq),
    }
}

/// The write `action` names, on the local store.
fn local_action(
    action: &str,
    args: &[String],
    root: &Path,
    caller: &Caller,
) -> Result<(Option<Change>, Option<LocalDetail>), Failure> {
    let by = actor(args, caller)?;
    let agent = by.agent_id().map(str::to_owned);
    let agent = agent.as_deref();
    let reason = flag(args, "--reason")?.map(str::to_owned);
    let reason = reason.as_deref();
    match action {
        "propose" => {
            let proposal = Proposal {
                title: required(args, "--title")?.to_owned(),
                goal: flag(args, "--goal")?.map(str::to_owned),
                acceptance: required(args, "--acceptance")?.to_owned(),
                role: flag(args, "--role")?.map(str::to_owned),
            };
            local_write(root, agent, |b, now| {
                b.propose(&by, proposal, now).map(Some)
            })
        }
        "decide" => {
            let (n, accept) = (number(args)?, decision(args)?);
            local_write(root, agent, |b, now| {
                b.decide(n, &by, accept, reason, now).map(Some)
            })
        }
        "claim" => {
            let n = number(args)?;
            let minutes = flag(args, "--minutes")?
                .map(|m| {
                    m.parse()
                        .map_err(|_| Failure::invalid("--minutes is 5-240"))
                })
                .transpose()?;
            let (change, task) = local_write(root, agent, |b, now| b.claim(n, &by, minutes, now))?;
            // A renewal changes no state; the task is still shown.
            let task = match task {
                Some(task) => Some(task),
                None => local::board(root)?
                    .tasks
                    .get(&n)
                    .map(|t| local_detail(t, agent)),
            };
            Ok((change, task))
        }
        "update" => {
            let (n, line, used) = (number(args)?, summary(args)?, usage(args)?);
            local_write(root, agent, |b, now| {
                b.update(n, &by, &line, used, now).map(Some)
            })
        }
        "submit" => {
            let n = number(args)?;
            let submission = Submission {
                summary: summary(args)?,
                evidence: local::evidence(root, evidence_inputs(args)?)?,
                usage: usage(args)?,
                outcome: if args.iter().any(|a| a == "--failed") {
                    TaskOutcome::Failed
                } else {
                    TaskOutcome::Completed
                },
            };
            local_write(root, agent, |b, now| {
                b.submit(n, &by, submission, now).map(Some)
            })
        }
        "amend" => {
            let n = number(args)?;
            let amendment = Amendment {
                summary: flag(args, "--summary")?.map(str::to_owned),
                evidence: local::evidence(root, evidence_inputs(args)?)?,
                merge_commit: flag(args, "--merge")?.map(str::to_owned),
                queen: lean_ctx_hive::queens::this_session_is_queen(),
                reason: reason.map(str::to_owned),
            };
            local_write(root, agent, |b, now| {
                b.amend(n, &by, amendment, now).map(Some)
            })
        }
        "release" => {
            let n = number(args)?;
            local_write(root, agent, |b, now| {
                b.release(n, &by, reason, now).map(Some)
            })
        }
        "verify" => {
            let (n, (verdict, findings)) = (number(args)?, verdict_findings(args)?);
            local_write(root, agent, |b, now| {
                b.verify(n, &by, verdict, findings, now).map(Some)
            })
        }
        "stop" => {
            let n = number(args)?;
            local_write(root, agent, |b, now| b.stop(n, &by, reason, now).map(Some))
        }
        "priority" | "labels" => {
            person_only(args, caller)?;
            let (n, change) = local_read::tasks::fields_change(args)?;
            local_write(root, None, |b, now| {
                b.set_fields(n, &by, change.priority, change.labels, now)
                    .map(Some)
            })
        }
        other => Err(Failure::invalid(format!(
            "unknown tasks action '{other}' (see `lean-ctx tasks --help`)"
        ))),
    }
}

/// Priority and labels: a person's verbs. An agent's process, or a write
/// naming an `--agent`, is refused `forbidden`.
fn person_only(args: &[String], caller: &Caller) -> Result<(), Failure> {
    if !matches!(caller, Caller::Person) || flag(args, "--agent")?.is_some() {
        return Err(Failure::new(
            Code::Forbidden,
            local_read::tasks::AGENT_REFUSAL,
        ));
    }
    Ok(())
}

/// A write on a linked project: the same Hive calls as `hive tasks`.
fn hive_action(
    action: &str,
    args: &[String],
    root: &Path,
    caller: &Caller,
) -> Result<ChangeRow, Failure> {
    let agent = || {
        actor(args, caller)?
            .agent_id()
            .map(str::to_owned)
            .ok_or_else(|| Failure::invalid("--agent <bus agent id> is required for a Hive task"))
    };
    let ack = |ack: lean_ctx_protocol::hive::tasks::TaskAckV1, event: &'static str| ChangeRow {
        number: ack.number,
        event,
        state: wire(&ack.state),
        seq: None,
    };
    let reason = flag(args, "--reason")?;
    match action {
        "claim" => {
            let minutes = flag(args, "--minutes")?
                .map(|m| m.parse().map_err(|_| Failure::invalid("--minutes is 5-15")))
                .transpose()?;
            let granted = hive_tasks::claim(root, number(args)?, &agent()?, "cli", minutes)?;
            Ok(ChangeRow {
                number: granted.number,
                event: "claimed",
                state: "claimed".to_owned(),
                seq: None,
            })
        }
        "update" => Ok(ack(
            hive_tasks::update(
                root,
                number(args)?,
                &agent()?,
                "cli",
                &summary(args)?,
                usage(args)?,
            )?,
            "updated",
        )),
        "submit" => {
            let outcome = if args.iter().any(|a| a == "--failed") {
                TaskOutcome::Failed
            } else {
                TaskOutcome::Completed
            };
            let done = hive_tasks::submit(
                root,
                number(args)?,
                &agent()?,
                &summary(args)?,
                evidence_inputs(args)?,
                usage(args)?,
                outcome,
            )?;
            let event = if done.state == TaskState::Failed {
                "failed"
            } else {
                "submitted"
            };
            Ok(ack(done, event))
        }
        "release" => Ok(ack(
            hive_tasks::release(root, number(args)?, &agent()?, reason)?,
            "released",
        )),
        "amend" => {
            let input = hive_tasks::AmendInput {
                summary: flag(args, "--summary")?.map(str::to_owned),
                evidence: evidence_inputs(args)?,
                merge_commit: flag(args, "--merge")?.map(str::to_owned),
                queen: lean_ctx_hive::queens::this_session_is_queen(),
                reason: reason.map(str::to_owned),
            };
            Ok(ack(
                hive_tasks::amend(root, number(args)?, &agent()?, input)?,
                "amended",
            ))
        }
        "verify" => {
            let (verdict, findings) = verdict_findings(args)?;
            let done = hive_tasks::verify(root, number(args)?, &agent()?, verdict, findings)?;
            let event = if done.state == TaskState::Done {
                "verified"
            } else {
                "rejected"
            };
            Ok(ack(done, event))
        }
        "priority" | "labels" => {
            person_only(args, caller)?;
            let (n, change) = local_read::tasks::fields_change(args)?;
            Ok(ack(
                lean_ctx_hive::tasks::fields::set_fields(root, n, &change)?,
                "fields",
            ))
        }
        "decide" | "stop" | "propose" => Err(Failure::new(
            Code::Forbidden,
            format!(
                "{action} on a task of a linked vault happens in Hive (`lean-ctx hive tasks propose` queues a proposal)"
            ),
        )),
        other => Err(Failure::invalid(format!(
            "unknown tasks action '{other}' (see `lean-ctx tasks --help`)"
        ))),
    }
}

fn vault_of(root: &str) -> Option<String> {
    let store = lean_ctx_hive::HiveStore::open().ok()?;
    lean_ctx_hive::vault::VaultMap::load(store.data_dir())
        .for_root(root)
        .map(|r| format!("{}/{}", r.ns, r.name))
}

/// `tasks.list` of an unlinked project (also `hive tasks list --json`).
pub(crate) fn local_list(args: &[String], root: &str) -> Result<LocalList, Failure> {
    let agent = reader(args, &caller())?;
    let agent = agent.as_deref();
    let filter = LocalFilter {
        state: state_filter(args)?,
        role: flag(args, "--role")?.map(str::to_owned),
        mine: args.iter().any(|a| a == "--mine"),
    };
    let board = local::board(Path::new(root))?;
    Ok(LocalList {
        project_root: root.to_owned(),
        vault: None,
        tasks: local::select(&board, &filter, agent)
            .into_iter()
            .map(|t| local_row(t, agent))
            .collect(),
    })
}

/// `tasks.task` of an unlinked project (also `hive tasks show --json`).
pub(crate) fn local_show(args: &[String], root: &str) -> Result<LocalTaskDoc, Failure> {
    let (n, agent) = (number(args)?, reader(args, &caller())?);
    let board = local::board(Path::new(root))?;
    let task = board.tasks.get(&n).ok_or(TaskError::NotFound(n))?;
    Ok(LocalTaskDoc {
        project_root: root.to_owned(),
        vault: None,
        task: local_detail(task, agent.as_deref()),
    })
}

/// `lean-ctx tasks …`.
pub(crate) fn cmd_tasks(args: &[String]) {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        print!("{HELP}");
        return;
    }
    let mut args: Vec<String> = args.to_vec();
    // The action is the first positional, wherever the flags stand.
    if positionals(&args).is_empty() {
        args.insert(0, "list".to_owned());
    }
    let action = positionals(&args)[0].to_owned();
    let caller = caller();
    let caller = &caller;
    if local_read::wants_json(&args) {
        local_read::print(&json_answer(&args, caller, local_read::Via::Cli));
        return;
    }
    let root = match local_read::project_root(&args) {
        Ok(root) => root,
        Err(failure) => exit_text(&failure),
    };
    let path = PathBuf::from(&root);
    match local::linked(&path) {
        Ok(true) if !wants_local(&args) => {
            if let Err(failure) = release_text_write(&action, &args, &root) {
                exit_text(&failure);
            }
            return linked_text(&action, &args, &root, caller);
        }
        Ok(true) => {
            if let Err(failure) = local_in_linked(&path, &action, &args) {
                exit_text(&failure);
            }
        }
        Ok(false) => {}
        // Never fall back to local tasks when the link cannot be read.
        Err(message) => exit_text(&Failure::new(Code::Unavailable, message)),
    }
    match local_text_answer(&action, &args, &root, caller) {
        Ok(text) => println!("{text}"),
        Err(failure) => exit_text(&failure),
    }
}

/// An unlinked project's (or `--local`) plain-text answer: a write passes
/// the release check first (G08), a withheld one writes nothing.
pub(crate) fn local_text_answer(
    action: &str,
    args: &[String],
    root: &str,
    caller: &Caller,
) -> Result<String, Failure> {
    release_text_write(action, args, root)?;
    text_answer(action, args, Path::new(root), caller)
}

/// The plain-text command's error line and status.
fn exit_text(failure: &Failure) -> ! {
    eprintln!("lean-ctx tasks: {}", failure.message);
    std::process::exit(failure.code.exit_status());
}

/// `--local`: on a linked project, act on its own local tasks.
fn wants_local(args: &[String]) -> bool {
    args.iter().any(|a| a == "--local")
}

/// `--local` on a linked project: the project's own local tasks, kept
/// reachable after a link that left them local (or came in through git)
/// until they are settled; never one that was brought into the vault.
fn local_in_linked(path: &Path, action: &str, args: &[String]) -> Result<(), Failure> {
    lean_ctx_hive::bring_local::local_action_in_linked(path, action, number(args).ok())
        .map_err(|why| Failure::new(Code::Forbidden, why))
}

/// The `--json` document of `tasks <action> …` for `via`: the CLI prints
/// it, the local service sends it. Never ends the process — the socket
/// builds it inside the daemon. A linked project answers from Hive (its
/// list and show as `hive tasks`), any other from its local tasks.
pub(crate) fn json_answer(
    args: &[String],
    caller: &Caller,
    via: local_read::Via,
) -> local_read::Answer {
    let mut args: Vec<String> = args.to_vec();
    if positionals(&args).is_empty() {
        args.insert(0, "list".to_owned());
    }
    let action = positionals(&args)[0].to_owned();
    let root = match local_read::project_root(&args) {
        Ok(root) => root,
        Err(failure) => return local_read::Answer::failure(&failure),
    };
    let path = PathBuf::from(&root);
    let linked = match local::linked(&path) {
        Ok(true) if wants_local(&args) => match local_in_linked(&path, &action, &args) {
            Ok(()) => false,
            Err(failure) => return local_read::Answer::failure(&failure),
        },
        Ok(linked) => linked,
        // Never fall back to local tasks when the link cannot be read.
        Err(message) => {
            return local_read::Answer::failure(&Failure::new(Code::Unavailable, message));
        }
    };
    let preview = || write_preview(&action, &args);
    match (linked, action.as_str()) {
        (true, "list" | "show") => local_read::tasks::answer_for(&args, via),
        (true, _) => local_read::answer_released_write(
            "ctx_tasks",
            &root,
            "tasks.ack",
            via,
            &preview(),
            || {
                let change = hive_action(&action, &args, &path, caller)?;
                Ok(Ack {
                    project_root: root.clone(),
                    vault: vault_of(&root),
                    change: Some(change),
                    task: None,
                })
            },
        ),
        (false, "list") => {
            local_read::answer_released("ctx_tasks", &root, "tasks.list", via, || {
                local_list(&args, &root)
            })
        }
        (false, "show") => {
            local_read::answer_released("ctx_tasks", &root, "tasks.task", via, || {
                local_show(&args, &root)
            })
        }
        (false, _) => local_read::answer_released_write(
            "ctx_tasks",
            &root,
            "tasks.ack",
            via,
            &preview(),
            || {
                let (change, task) = local_action(&action, &args, &path, caller)?;
                Ok(Ack {
                    project_root: root.clone(),
                    vault: None,
                    change: change.as_ref().map(change_row),
                    task,
                })
            },
        ),
    }
}

/// A write's request as the release point sees it before anything is
/// written or sent to Hive (G08), for `--json`, the socket and plain text
/// alike: a withheld write writes nothing.
fn write_preview(action: &str, args: &[String]) -> serde_json::Value {
    let mut preview = local_read::request_preview(args, WRITE_FIELDS);
    if let serde_json::Value::Object(fields) = &mut preview {
        fields.insert("action".into(), action.into());
        // A summary may also stand as a positional (`update 3 "line"`).
        let text: Vec<&str> = positionals(args).into_iter().skip(2).collect();
        if !text.is_empty() {
            fields.insert("text".into(), text.join(" ").into());
        }
    }
    preview
}

/// The plain-text command's release check before a write (G08), the same
/// decision on the same preview as [`json_answer`]'s: `Err` when withheld,
/// and then nothing is written, locally or to Hive.
pub(crate) fn release_text_write(action: &str, args: &[String], root: &str) -> Result<(), Failure> {
    if matches!(action, "list" | "show") {
        return Ok(());
    }
    local_read::release_text_write("ctx_tasks", root, "tasks.ack", &write_preview(action, args))
}

/// Every text field a task write sends or stores (the release preview).
const WRITE_FIELDS: &[&str] = &[
    "--title",
    "--goal",
    "--acceptance",
    "--role",
    "--summary",
    "--evidence",
    "--merge",
    "--reason",
    "--findings",
    "--verdict",
    "--agent",
    "--by",
];

fn text_answer(
    action: &str,
    args: &[String],
    root: &Path,
    caller: &Caller,
) -> Result<String, Failure> {
    let agent = reader(args, caller)?;
    let agent = agent.as_deref();
    match action {
        "list" => {
            let filter = LocalFilter {
                state: state_filter(args)?,
                role: flag(args, "--role")?.map(str::to_owned),
                mine: args.iter().any(|a| a == "--mine"),
            };
            let board = local::board(root)?;
            Ok(local::render_list(
                &local::select(&board, &filter, agent),
                agent,
                100,
            ))
        }
        "show" => {
            let n = number(args)?;
            let board = local::board(root)?;
            let task = board.tasks.get(&n).ok_or(TaskError::NotFound(n))?;
            Ok(local::render_task(task, agent))
        }
        _ => {
            let (change, _) = local_action(action, args, root, caller)?;
            Ok(change.map_or_else(
                || "Your claim is renewed.".to_owned(),
                |c| local::render_change(&c),
            ))
        }
    }
}

/// A linked project's plain-text command: Hive, exactly as `lean-ctx hive
/// tasks` (the `--json` documents are [`json_answer`]'s); `list` first
/// names the local tasks that are not shared.
fn linked_text(action: &str, args: &[String], root: &str, caller: &Caller) {
    let ordered = hive_text_args(action, args, caller).unwrap_or_else(|f| exit_text(&f));
    if action == "list"
        && let Ok(left) = lean_ctx_hive::bring_local::local_not_shared(Path::new(root))
        && let Some(note) = lean_ctx_hive::bring_local::not_shared_note(&left)
    {
        println!("{note}");
    }
    if let Err(error) = super::hive_tasks_cmd::run(&ordered) {
        eprintln!("lean-ctx tasks: {error}");
        std::process::exit(1);
    }
}

/// The arguments `hive tasks` reads for a linked project's plain-text
/// command: positionals first (it reads the number and summary by
/// position), then the flags; a write carries the caller's agent as
/// `--agent`, exactly as the `--json` path resolves it.
fn hive_text_args(action: &str, args: &[String], caller: &Caller) -> Result<Vec<String>, Failure> {
    let (positional, flags) = local_read::split_positionals(args);
    let mut ordered: Vec<String> = positional.iter().map(|a| (*a).to_owned()).collect();
    // Priority and labels are a person's (no `--agent`; `hive tasks` checks).
    let writes = !matches!(action, "list" | "show" | "propose" | "priority" | "labels");
    let agent = if writes {
        Some(
            actor(args, caller)?
                .agent_id()
                .map(str::to_owned)
                .ok_or_else(|| {
                    Failure::invalid("--agent <bus agent id> is required for a Hive task")
                })?,
        )
    } else {
        None
    };
    let mut skip = false;
    for flag in flags {
        if skip {
            skip = false;
        } else if agent.is_some() && (flag == "--agent" || flag == "--by") {
            skip = true;
        } else {
            ordered.push(flag.to_owned());
        }
    }
    if let Some(agent) = agent {
        ordered.extend(["--agent".to_owned(), agent]);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    fn doc(kind: &str, body: &impl Serialize) -> Value {
        serde_json::from_str(&local_read::document(kind, body)).expect("one JSON object")
    }

    #[test]
    fn local_tasks_print_local_read_shapes_post_on_the_bus_and_a_linked_root_goes_to_hive() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().expect("project");
        let (root, rs) = (dir.path(), dir.path().to_str().expect("utf-8 path"));
        let step = |list: &[&str]| local_action(list[0], &args(list), root, &Caller::Person);
        step(&[
            "propose",
            "--title",
            "Fix the flaky login test",
            "--acceptance",
            "50 green runs",
            "--agent",
            "a1",
        ])
        .expect("proposed");
        step(&["decide", "1", "--accept", "--by", "Yves"]).expect("a person accepts");
        step(&["claim", "1", "--agent", "a1"]).expect("the proposer claims");
        step(&[
            "submit",
            "1",
            "--summary",
            "Fixed the fixture race.",
            "--evidence",
            "commit:7be16a9596",
            "--agent",
            "a1",
        ])
        .expect("submitted");
        let refused = step(&["verify", "1", "--verdict", "accept", "--agent", "a1"])
            .expect_err("self verify");
        assert_eq!(refused.code, Code::Forbidden);
        let (change, task) =
            step(&["verify", "1", "--verdict", "accept"]).expect("a person verifies");
        let ack = doc(
            "tasks.ack",
            &Ack {
                project_root: rs.to_owned(),
                vault: None,
                change: change.as_ref().map(change_row),
                task,
            },
        );
        assert_eq!(
            ack["change"],
            json!({"number": 1, "event": "verified", "state": "done", "seq": 5})
        );
        assert_eq!(ack["vault"], Value::Null);
        assert_eq!(ack["task"]["state"], "done");

        let shown = doc(
            "tasks.task",
            &local_show(&args(&["show", "1"]), rs).expect("show"),
        );
        let task = &shown["task"];
        assert_eq!(shown["vault"], Value::Null);
        assert_eq!(task["result"]["outcome"], "completed");
        assert_eq!(
            task["result"]["evidence"],
            json!([{"kind": "commit", "ref": "7be16a9596"}])
        );
        assert_eq!(
            task["approved_by"],
            json!({"kind": "person", "label": "Yves", "agent": null})
        );
        assert_eq!(task["verification"]["verdict"], "accept");
        assert_eq!(task["shared"], Value::Null);
        let events: Vec<&str> = task["transitions"]
            .as_array()
            .expect("transitions")
            .iter()
            .map(|t| t["event"].as_str().expect("event"))
            .collect();
        assert_eq!(
            events,
            ["proposed", "accepted", "claimed", "submitted", "verified"]
        );
        assert_eq!(
            task["transitions"][2]["by"],
            json!({"kind": "instance", "label": "a1", "agent": "a1"})
        );

        let listed = doc(
            "tasks.list",
            &local_list(&args(&["list"]), rs).expect("list"),
        );
        assert_eq!(listed["tasks"][0]["acceptance"], "50 green runs");
        assert_eq!(listed["tasks"][0]["updated"], task["transitions"][4]["at"]);

        // Each transition is on the bus, keyed task:<n>:<event>:<seq>:<i>.
        let registry = lean_ctx::core::agents::AgentRegistry::load().expect("bus");
        let keys: Vec<&str> = registry
            .scratchpad
            .iter()
            .filter(|e| e.from_agent == local::SENDER)
            .filter_map(|e| e.metadata.get("lean_ctx.request_id").map(String::as_str))
            .collect();
        assert!(keys.contains(&"task:1:proposed:1:0"), "{keys:?}");
        assert!(keys.contains(&"task:1:verified:5:0"), "{keys:?}");

        // A checkout linked to a vault keeps Hive's tasks.
        let store = lean_ctx_hive::HiveStore::open().expect("hive store");
        let mut map = lean_ctx_hive::vault::VaultMap::default();
        map.vaults.insert(
            "stable".into(),
            lean_ctx_hive::vault::VaultRecord {
                vault_id: "v_1".into(),
                ns: "acme".into(),
                name: "app".into(),
                sealed: false,
                roots: vec![rs.to_owned()],
            },
        );
        std::fs::create_dir_all(store.data_dir()).expect("hive dir");
        map.save(store.data_dir()).expect("vault map");
        assert_eq!(local::linked(root), Ok(true));
        let refused = hive_action(
            "decide",
            &args(&["decide", "1", "--accept"]),
            root,
            &Caller::Person,
        )
        .expect_err("decided in Hive");
        assert_eq!(refused.code, Code::Forbidden);
    }

    /// The socket builds `hive tasks list|show` inside the daemon: a link
    /// state that cannot be read is an `unavailable` error document, never
    /// an exit of the process.
    #[test]
    fn an_unreadable_link_answers_unavailable_and_never_exits() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path().to_str().expect("utf-8").to_owned();
        let store = lean_ctx_hive::HiveStore::open().expect("hive store");
        std::fs::create_dir_all(store.data_dir()).expect("hive dir");
        std::fs::write(
            store
                .data_dir()
                .join(lean_ctx::core::vault_map::VAULTS_FILE),
            b"not json",
        )
        .expect("a broken vault map");
        for action in ["list", "show"] {
            let answer =
                local_read::tasks::answer(&args(&[action, "1", "--json", "--project-root", &root]));
            assert_eq!(answer.failed, Some(Code::Unavailable), "{}", answer.text);
        }
    }

    /// The local service's task routes (tasks_part): body keys become the
    /// CLI flags, the socket's reader is a person, and stop is a person
    /// verb that needs a reason and takes no agent.
    #[test]
    fn the_task_routes_propose_decide_and_stop_as_a_person() {
        use local_read::{Via, routes::args_of, tasks_part};
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path().to_str().expect("utf-8").to_owned();
        let call = |path: &str, n: Option<&str>, body: Value| {
            let route = tasks_part::ROUTES
                .iter()
                .find(|r| r.path == path)
                .expect("a task route");
            let mut inputs: Vec<(String, Value)> = body
                .as_object()
                .expect("a body")
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            inputs.push(("project_root".into(), json!(root)));
            args_of(route, n, &inputs).map(|a| tasks_part::answer(route.kind, &a, Via::Socket))
        };
        let doc = |answer: local_read::Answer| -> Value {
            serde_json::from_str(&answer.text).expect("a document")
        };

        let proposed = call(
            "/v1/local/tasks/propose",
            None,
            json!({"title": "Fix the login test", "acceptance": "50 green runs"}),
        )
        .expect("routed");
        assert_eq!(doc(proposed)["change"]["event"], "proposed");
        let accepted = call(
            "/v1/local/tasks/{n}/decide",
            Some("1"),
            json!({"accept": true}),
        )
        .expect("routed");
        assert_eq!(doc(accepted)["change"]["state"], "open");

        assert!(
            call(
                "/v1/local/tasks/{n}/stop",
                Some("1"),
                json!({"agent": "w1", "reason": "x"})
            )
            .is_err(),
            "stop takes no agent"
        );
        let no_reason = call("/v1/local/tasks/{n}/stop", Some("1"), json!({})).expect("routed");
        assert_eq!(
            no_reason.failed,
            Some(Code::InvalidArgument),
            "{}",
            no_reason.text
        );
        let stopped = call(
            "/v1/local/tasks/{n}/stop",
            Some("1"),
            json!({"reason": "Not needed.", "by": "Yves"}),
        )
        .expect("routed");
        let stopped = doc(stopped);
        assert_eq!(stopped["change"]["event"], "stopped");
        assert_eq!(stopped["change"]["state"], "cancelled");
    }

    #[test]
    fn an_agent_process_acts_as_its_agent_and_never_verifies_its_own_result() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().expect("project");
        let root = dir.path();
        let step =
            |caller: &Caller, list: &[&str]| local_action(list[0], &args(list), root, caller);
        let worker = &Caller::Agent("w1".to_owned());
        step(
            worker,
            &[
                "propose",
                "--title",
                "Fix the flaky login test",
                "--acceptance",
                "50 green runs",
            ],
        )
        .expect("proposed as w1");
        step(worker, &["decide", "1", "--accept"]).expect("w1 accepts");
        step(worker, &["claim", "1"]).expect("w1 claims");
        step(
            worker,
            &[
                "submit",
                "1",
                "--summary",
                "Fixed it.",
                "--evidence",
                "commit:7be16a9596",
            ],
        )
        .expect("w1 submits");
        // Without --agent the worker's process is still the worker, not a person.
        let own = step(worker, &["verify", "1", "--verdict", "accept"]).expect_err("self verify");
        assert_eq!(own.code, Code::Forbidden);
        assert!(own.message.contains("self_verify"), "{}", own.message);
        for spoof in [
            &["verify", "1", "--verdict", "accept", "--by", "Yves"][..],
            &["verify", "1", "--verdict", "accept", "--agent", "w2"][..],
        ] {
            assert_eq!(
                step(worker, spoof).expect_err("spoofed").code,
                Code::Forbidden
            );
        }
        // A process under an agent host without an id never acts as a person.
        let unnamed = &Caller::UnnamedAgent;
        for as_person in [
            &["verify", "1", "--verdict", "accept"][..],
            &["verify", "1", "--verdict", "accept", "--by", "Yves"][..],
        ] {
            assert_eq!(
                step(unnamed, as_person).expect_err("no person").code,
                Code::Forbidden
            );
        }
        assert_eq!(
            step(
                unnamed,
                &["verify", "1", "--verdict", "accept", "--agent", "w1"]
            )
            .expect_err("still its own result")
            .code,
            Code::Forbidden
        );
        // Another agent's process checks it.
        let (change, _) = step(
            unnamed,
            &["verify", "1", "--verdict", "accept", "--agent", "w2"],
        )
        .expect("verified");
        assert_eq!(change.expect("a transition").state, TaskState::Done);
    }

    #[test]
    fn the_caller_comes_from_the_environment_and_an_agent_id_wins() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(caller_from(env(&[])), Caller::Person);
        for marker in [
            "CLAUDECODE",
            "CODEX_THREAD_ID",
            "CURSOR_TASK_ID",
            "LEAN_CTX_AGENT",
            "AI_AGENT",
        ] {
            let pairs: &'static [(&str, &str)] = Box::leak(Box::new([(marker, "1")]));
            assert_eq!(caller_from(env(pairs)), Caller::UnnamedAgent, "{marker}");
        }
        assert_eq!(
            caller_from(env(&[("CLAUDECODE", "1"), ("LEAN_CTX_AGENT_ID", "w1")])),
            Caller::Agent("w1".to_owned())
        );
        assert_eq!(
            caller_from(env(&[("LEAN_CTX_AGENT_ID", "local")])),
            Caller::Person
        );
        // The process environment itself, through `caller()`.
        let _id = lean_ctx::test_env::EnvVarGuard::set("LEAN_CTX_AGENT_ID", "w9");
        assert_eq!(caller(), Caller::Agent("w9".to_owned()));
    }

    #[test]
    fn a_linked_plain_text_write_carries_the_callers_agent() {
        let worker = &Caller::Agent("w1".to_owned());
        let ordered = hive_text_args(
            "submit",
            &args(&[
                "--failed",
                "submit",
                "3",
                "Gave up.",
                "--evidence",
                "commit:7be16a9596",
            ]),
            worker,
        )
        .expect("args");
        assert_eq!(
            ordered,
            args(&[
                "submit",
                "3",
                "Gave up.",
                "--failed",
                "--evidence",
                "commit:7be16a9596",
                "--agent",
                "w1"
            ])
        );
        let spoofed = hive_text_args("verify", &args(&["verify", "3", "--agent", "w2"]), worker);
        assert_eq!(spoofed.expect_err("another agent").code, Code::Forbidden);
        assert!(hive_text_args("list", &args(&["list"]), &Caller::UnnamedAgent).is_ok());
    }

    #[test]
    fn the_action_and_number_are_positionals_wherever_the_flags_stand() {
        let parsed = args(&["--json", "show", "1"]);
        assert_eq!(positionals(&parsed), ["show", "1"]);
        // --local and --accept are switches: neither takes the action.
        let parsed = args(&["--local", "decide", "1", "--accept"]);
        assert_eq!(positionals(&parsed), ["decide", "1"]);
        let parsed = args(&["--project-root", "/p", "show", "1", "--json"]);
        assert_eq!(positionals(&parsed), ["show", "1"]);
        assert_eq!(number(&parsed), Ok(1));
        let parsed = args(&["submit", "--agent", "w1", "3", "--failed", "Gave up."]);
        assert_eq!(number(&parsed), Ok(3));
        assert_eq!(summary(&parsed), Ok("Gave up.".to_owned()));
    }
}

#[cfg(test)]
mod fields_tests {
    use serde_json::{Value, json};

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn a_person_sets_a_local_tasks_priority_and_labels_and_lists_put_high_first() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().expect("project");
        let (root, rs) = (dir.path(), dir.path().to_str().expect("utf-8 path"));
        let step = |who: &Caller, list: &[&str]| local_action(list[0], &args(list), root, who);
        for title in ["first", "second"] {
            step(
                &Caller::Person,
                &[
                    "propose",
                    "--title",
                    title,
                    "--acceptance",
                    "done",
                    "--agent",
                    "a1",
                ],
            )
            .expect("proposed");
        }
        let (change, task) = step(
            &Caller::Person,
            &["priority", "2", "high", "--labels", "core,ui"],
        )
        .expect("a person sets it");
        let ack: Value = serde_json::from_str(&local_read::document(
            "tasks.ack",
            &Ack {
                project_root: rs.to_owned(),
                vault: None,
                change: change.as_ref().map(change_row),
                task,
            },
        ))
        .expect("one JSON object");
        assert_eq!(ack["change"]["event"], "fields");
        assert_eq!(ack["change"]["state"], "proposed");
        assert_eq!(ack["task"]["priority"], "high");
        assert_eq!(ack["task"]["labels"], json!(["core", "ui"]));
        assert_eq!(ack["task"]["fields_version"], 1);
        step(&Caller::Person, &["labels", "2", "--clear"]).expect("cleared");

        let list = local_list(&args(&["list"]), rs).expect("list");
        let order: Vec<u64> = list.tasks.iter().map(|t| t.number).collect();
        assert_eq!(order, [2, 1], "high first, then by number");
        assert!(list.tasks[0].labels.is_empty());
        assert_eq!(list.tasks[0].fields_version, Some(2));

        // An agent, or a person's write naming an agent, is refused.
        for (who, words) in [
            (
                Caller::Agent("a1".into()),
                vec!["priority", "1", "high", "--agent", "a1"],
            ),
            (
                Caller::UnnamedAgent,
                vec!["priority", "1", "high", "--agent", "a1"],
            ),
            (
                Caller::Person,
                vec!["priority", "1", "high", "--agent", "a1"],
            ),
        ] {
            let refused = step(&who, &words).expect_err("not a person");
            assert_eq!(refused.code, Code::Forbidden, "{who:?}");
        }
        // A wrong value is refused at once, naming what is allowed.
        let wrong = step(&Caller::Person, &["priority", "1", "urgent"]).expect_err("urgent");
        assert_eq!(wrong.code, Code::InvalidArgument);
        assert!(
            wrong.message.contains("high or normal"),
            "{}",
            wrong.message
        );
        let wrong = step(&Caller::Person, &["labels", "1", "Core"]).expect_err("upper case");
        assert_eq!(wrong.code, Code::InvalidArgument);
        assert_eq!(
            local_show(&args(&["show", "1"]), rs)
                .expect("show")
                .task
                .detail
                .row
                .fields_version,
            None,
            "nothing written"
        );
    }
}
