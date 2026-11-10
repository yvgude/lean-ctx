// SPDX-License-Identifier: Apache-2.0
//! Process v1 on submit (E47) and verify (E48), decided in the Instance
//! before anything is sent:
//!
//! - **Submit of a process task** (a task view with `process`): G6 runs
//!   here ([`super::gate`]): the locked-path check of the judged commit
//!   and the handed-in worktree, then the gate's own rerun of the judged
//!   commit. The Instance does not read CI (§5.1), so what it sends is its
//!   part of the record, `blocked` on `ci_pipeline` only, which Hive
//!   completes with `ci`; a red record, or a view that lacks what G6 needs,
//!   refuses the submit and nothing is sent.
//! - **Verify, every task: checker ≠ author** by the per-session identity
//!   (§3.1). The checker is a **live bus presence of this Instance**
//!   (the local registry, never a free string, never the machine's shared
//!   worker id), held by the calling process (MCP) or by a process the
//!   session that started the presence's MCP server is an ancestor of (CLI
//!   from that session's own shell, by process ancestry; the environment,
//!   `LEAN_CTX_SESSION_ID` included, never binds). A result submitted on this Instance is compared with the
//!   session that claimed and the one that submitted it
//!   ([`claims::WorkTrace`]): the same agent, app session or registering
//!   process never checks it. Without a per-session identity on either
//!   side the verify is refused. For a process task the checker also has
//!   a verifying role (tester, code reviewer, security reviewer).
//! - **Verify of a process task:** Hive's G6 record for the judged commit
//!   must be green, `ci` included, and the locked paths are checked again
//!   here, commit against `spec_commit`, in a private copy.
//!
//! **Known gap, server missing:** Hive does not yet check the verifier
//! against a live `bus_presence` row of the token's Instance in the gate's
//! role (§3.1 E48), nor add `ci` to G6 (§5.1). An agent running as the same
//! Unix user can still edit this Instance's files (the claims file, the bus
//! registry) or run a command under another session's process tree; only
//! Hive's per-session identity (task #21) closes that. Every refusal says
//! why and sends nothing.

use std::path::Path;

use lean_ctx_protocol::hive::gate::{G6RecordV1, GateResult, HiveG6RecordV1, NotGreen};
use lean_ctx_protocol::hive::tasks::{
    EvidenceKind, EvidenceV1, TaskActorKind, TaskOutcome, TaskV1,
};

use super::claims::{SessionIdentity, WorkTrace};
use super::gate::{self, Missing};
use crate::HiveError;

/// The roles that may verify a process task (§3.1: the gate's role; the
/// old id `reviewer` is the code reviewer).
pub(crate) const VERIFYING_ROLES: [&str; 4] =
    ["tester", "code_reviewer", "reviewer", "security_reviewer"];

/// A live bus presence of this Instance, as the local registry has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Presence {
    pub agent: String,
    /// The role id it registered with.
    pub role: Option<String>,
    /// The app session of the process that registered it.
    pub app_session: Option<String>,
    /// The registering process, `<pid>@<start marker>`.
    pub process: Option<String>,
    /// Whether the calling process may act as it ([`binds_caller`]): it
    /// registered the presence, or it runs under the session that started
    /// the presence's MCP server.
    pub bound_here: bool,
}

impl Presence {
    pub(crate) fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            agent: self.agent.clone(),
            session: self.app_session.clone(),
            process: self.process.clone(),
        }
    }
}

/// Where a bus agent's live presence is looked up.
pub(crate) type PresenceLookup<'a> = &'a (dyn Fn(&str) -> Option<Presence> + Sync);

fn process_key(pid: u32, identity: &lean_ctx::ipc::process::ProcessIdentity) -> String {
    format!("{pid}@{}", identity.start_marker)
}

/// The live presence of `agent` in this machine's bus registry: listed,
/// not finished, and its registering process still the one that
/// registered it.
pub(crate) fn local_presence(agent: &str) -> Option<Presence> {
    use lean_ctx::core::agents::{AgentRegistry, AgentStatus};
    let registry = AgentRegistry::load()?;
    let entry = registry
        .agents
        .iter()
        .find(|a| a.agent_id == agent && a.status != AgentStatus::Finished)?;
    let identity = entry.process_identity.as_ref()?;
    if !lean_ctx::ipc::process::matches_identity(entry.pid, identity) {
        return None;
    }
    let bound_here = binds_caller(registry.held_by_this_process(agent), entry.pid);
    Some(Presence {
        agent: agent.to_owned(),
        role: entry
            .role
            .as_deref()
            .and_then(lean_ctx::core::agents::role_id),
        app_session: entry.app_session_id.clone(),
        process: Some(process_key(entry.pid, identity)),
        bound_here,
    })
}

/// The parent of process `pid` (Linux: `/proc/<pid>/stat`; other Unix:
/// `ps`); `None` when it cannot be read.
fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may hold spaces and parentheses: read after it.
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Whether the host that started the process `registered` (its parent:
/// the agent session that spawned that MCP server) is this process or one
/// of its ancestors: a CLI run from that session's own shell.
///
/// Follow-up: an MCP server orphaned to a subreaper (`systemd --user`)
/// makes that subreaper its host, and every process below it a match;
/// storing the parent's `pid@start` in the registry at registration and
/// refusing a host that differs would close that.
pub(crate) fn hosts_caller(registered: u32) -> bool {
    let Some(host) = parent_of(registered).filter(|host| *host > 1) else {
        return false;
    };
    let mut pid = std::process::id();
    for _ in 0..64 {
        if pid == host {
            return true;
        }
        match parent_of(pid) {
            Some(parent) if parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

/// Whether the calling process may act as a presence registered by the
/// process `registered`: it holds the presence itself (MCP), or it runs
/// under the session that started it (CLI, by process ancestry). The
/// environment (`LEAN_CTX_SESSION_ID`) never binds: any process can set it.
pub(crate) fn binds_caller(held_here: bool, registered: u32) -> bool {
    held_here || hosts_caller(registered)
}

/// The identity of the session acting as `agent` here: its live presence's,
/// else this process with its app session.
pub(crate) fn acting_identity(agent: &str, presence: PresenceLookup<'_>) -> SessionIdentity {
    if let Some(found) = presence(agent) {
        return found.identity();
    }
    let pid = std::process::id();
    SessionIdentity {
        agent: agent.to_owned(),
        session: crate::policy_sessions::current_session(),
        process: lean_ctx::ipc::process::identity(pid).map(|id| process_key(pid, &id)),
    }
}

fn refused(text: String) -> HiveError {
    HiveError::Refused(text)
}

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

fn missing_text(number: u64, missing: &Missing) -> String {
    match missing {
        Missing::Hive(what) => format!(
            "not supported by Hive yet: this Hive's view of process task #{number} carries no {what}, which G6 needs; nothing was sent"
        ),
        Missing::Local(what) => format!(
            "task #{number}: this Instance has no record of {what} (claim the task again from this checkout); nothing was sent"
        ),
    }
}

/// The one commit a result names (its `commit` evidence).
fn result_commit(number: u64, evidence: &[EvidenceV1]) -> Result<String, HiveError> {
    let mut commits: Vec<&str> = evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::Commit)
        .filter_map(|e| e.reference.as_deref())
        .collect();
    commits.sort_unstable();
    commits.dedup();
    match commits.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(refused(format!(
            "task #{number} is a process task: its result names the judged commit (evidence kind commit); nothing was sent"
        ))),
        _ => Err(refused(format!(
            "task #{number} is a process task: its result names exactly one commit, the one G6 judges; nothing was sent"
        ))),
    }
}

fn result_name(result: GateResult) -> &'static str {
    match result {
        GateResult::Pass => "pass",
        GateResult::Fail => "fail",
        GateResult::Blocked => "blocked",
    }
}

fn record_text(record: &G6RecordV1) -> String {
    let mut text = format!(
        "G6 {} for {}",
        result_name(record.result),
        short(&record.artifact_commit)
    );
    if let Some(rule) = &record.rule {
        text.push_str(&format!(" (rule {rule})"));
    }
    if let Some(reason) = &record.reason {
        text.push_str(": ");
        text.push_str(reason);
    }
    text
}

/// G6 before a result of `task` leaves the worktree `root`, worked out in
/// the gate's directories `dirs`: `None` for a task that is not a
/// process task (or a failed result, which G6 does not judge), the record
/// to send with it (everything the Instance checks holds), or the refusal.
pub(crate) fn submit_gate(
    task: &TaskV1,
    root: &Path,
    dirs: &gate::GateDirs,
    evidence: &[EvidenceV1],
    outcome: TaskOutcome,
    trace: Option<&WorkTrace>,
) -> Result<Option<G6RecordV1>, HiveError> {
    let Some(process) = task.process.as_ref() else {
        return Ok(None);
    };
    if outcome == TaskOutcome::Failed {
        return Ok(None);
    }
    let number = task.number;
    let artifact = result_commit(number, evidence)?;
    let artifact = gate::resolve_commit(root, &artifact)
        .map_err(|e| refused(format!("task #{number}: {e}; nothing was sent")))?;
    if let Some(candidate) = process.candidate_commit.as_deref()
        && candidate != artifact
    {
        return Err(refused(format!(
            "task #{number}: G6 judges the commit G4 passed ({}), not {}; nothing was sent",
            short(candidate),
            short(&artifact)
        )));
    }
    let start = trace.and_then(|t| t.start_commit.as_deref());
    let inputs = gate::inputs_for(process, start).map_err(|m| refused(missing_text(number, &m)))?;
    let record = gate::g6_record(root, dirs, &artifact, &inputs)
        .map_err(|e| refused(format!("task #{number}: {e}; nothing was sent")))?;
    if !record.ready_for_hive() {
        return Err(refused(format!(
            "task #{number}: {}; the result was not sent",
            record_text(&record)
        )));
    }
    Ok(Some(record))
}

/// Who checks: the live bus presence `agent` of this Instance, held by the
/// calling process or its app session, and for a process task in a
/// verifying role. The machine's shared worker id (`wkr_…`), an id no live
/// presence has, or one another process holds is no per-session identity.
pub(crate) fn checker_identity(
    agent: &str,
    presence: Option<Presence>,
    process_task: bool,
) -> Result<SessionIdentity, HiveError> {
    let agent = agent.trim();
    if agent.is_empty() || crate::bus::address::is_worker_id(agent) {
        return Err(refused(
            "no per-session identity: verify as a session's bus agent (ctx_agent action=register), not as the machine's worker; checker ≠ author cannot be shown otherwise".into(),
        ));
    }
    let Some(presence) = presence.filter(|p| p.agent == agent) else {
        return Err(refused(format!(
            "no per-session identity: {agent} is no live bus agent of this Instance (register it with ctx_agent in its own session); nothing was sent"
        )));
    };
    if !presence.bound_here {
        return Err(refused(format!(
            "no per-session identity: this process does not act for {agent} (it neither registered it nor runs in its app session); verify from that agent's own session; nothing was sent"
        )));
    }
    if process_task
        && !presence
            .role
            .as_deref()
            .is_some_and(|role| VERIFYING_ROLES.contains(&role))
    {
        return Err(refused(format!(
            "not_permitted: {agent} has role {}; a process task is verified by a tester or reviewer (§3.1); nothing was sent",
            presence.role.as_deref().unwrap_or("none")
        )));
    }
    Ok(presence.identity())
}

/// Checker ≠ author (§3.1) for a verify of `task` by `checker`: a result of
/// another Instance or a person is someone else's; a result of this
/// Instance is compared with the session that submitted it, and the
/// session that claimed the task never checks it either.
pub(crate) fn checker_not_author(
    task: &TaskV1,
    checker: &SessionIdentity,
    trace: Option<&WorkTrace>,
) -> Result<(), HiveError> {
    let number = task.number;
    let Some(result) = task.result.as_ref() else {
        return Err(refused(format!(
            "task #{number} carries no submitted result to verify"
        )));
    };
    match result.by.kind {
        TaskActorKind::Person => Ok(()),
        TaskActorKind::Instance if !result.by.mine => Ok(()),
        TaskActorKind::Instance => {
            let author = trace.and_then(|t| t.submitted_by.as_ref()).ok_or_else(|| {
                refused(format!(
                    "no per-session identity for the author of task #{number}: it was submitted on this Instance, which keeps no record of the session that did it, so checker ≠ author cannot be shown; another Instance or a person verifies it"
                ))
            })?;
            let claimed_here = trace.is_some_and(|t| {
                t.claimed_by == checker.agent
                    || t.claimed_session
                        .as_ref()
                        .is_some_and(|claimant| claimant.same_as(checker))
            });
            if author.same_as(checker) || claimed_here {
                return Err(refused(format!(
                    "self_check: task #{number} was claimed or submitted by this session; another session verifies it"
                )));
            }
            Ok(())
        }
        TaskActorKind::Unknown => Err(refused(format!(
            "task #{number}: its result's author is of a kind this engine does not know, so checker ≠ author cannot be shown"
        ))),
    }
}

/// The judged commit of a submitted process task: the one G4 passed, else
/// the result's commit evidence.
fn judged_commit(task: &TaskV1) -> Option<String> {
    let process = task.process.as_ref()?;
    if let Some(candidate) = &process.candidate_commit {
        return Some(candidate.clone());
    }
    let commits: Vec<&str> = task
        .result
        .as_ref()?
        .evidence
        .iter()
        .filter(|e| e.kind == EvidenceKind::Commit)
        .filter_map(|e| e.reference.as_deref())
        .collect();
    match commits.as_slice() {
        [one] => Some((*one).to_owned()),
        _ => None,
    }
}

fn hive_record_text(record: &HiveG6RecordV1, why: &str) -> String {
    format!(
        "G6 {} for {}: {why}",
        result_name(record.result),
        short(record.artifact_commit())
    )
}

/// G6 before a verify of a process task (E48): Hive's G6 record for the
/// judged commit is green, `ci` included, and the locked paths are still
/// unchanged in it (checked here, from a private copy of `repo` in the
/// Instance's G6 directory `work`).
pub(crate) fn verify_gate(task: &TaskV1, repo: &Path, work: &Path) -> Result<(), HiveError> {
    let Some(process) = task.process.as_ref() else {
        return Ok(());
    };
    let number = task.number;
    let record = process.g6.as_ref().ok_or_else(|| {
        refused(format!(
            "not supported by Hive yet: this Hive serves no G6 record for process task #{number}, and a verify needs a green one; nothing was sent"
        ))
    })?;
    match record.green() {
        Ok(()) => {}
        Err(NotGreen::NoCi) => {
            return Err(refused(format!(
                "not supported by Hive yet: Hive's G6 record for process task #{number} carries no ci, and a pass needs a green pipeline for the judged commit (§5.1); nothing was sent"
            )));
        }
        Err(NotGreen::Red(why)) => {
            return Err(refused(format!(
                "task #{number}: verify needs a green G6 record; Hive's is {}; nothing was sent",
                hive_record_text(record, &why)
            )));
        }
    }
    let artifact = judged_commit(task).ok_or_else(|| {
        refused(format!(
            "task #{number}: the judged commit is unknown (no candidate commit, not exactly one commit in the result); nothing was sent"
        ))
    })?;
    let artifact = gate::resolve_commit(repo, &artifact)
        .map_err(|e| refused(format!("task #{number}: {e}; nothing was sent")))?;
    if !record.judges(&artifact) {
        return Err(refused(format!(
            "task #{number}: the G6 record judged {}, not the submitted commit {}; nothing was sent",
            short(record.artifact_commit()),
            short(&artifact)
        )));
    }
    let check = gate::recheck_commit(
        repo,
        work,
        &artifact,
        &record.locked_paths.spec_commit,
        &record.locked_paths.checked,
    )
    .map_err(|e| refused(format!("task #{number}: {e}; nothing was sent")))?;
    if !check.ok {
        return Err(refused(format!(
            "task #{number}: {}; nothing was sent",
            gate::changed_reason(&check)
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "process_tests.rs"]
pub(crate) mod tests;
