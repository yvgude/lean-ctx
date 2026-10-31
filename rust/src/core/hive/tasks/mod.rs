// SPDX-License-Identifier: Apache-2.0
//! Tasks in the engine (H2, C0 1.9 §1): read a project's tasks, claim one,
//! report short summaries, submit one result with evidence, give it back.
//!
//! The rules the engine keeps:
//! - Every call made from a tool call ends within [`BUDGET`]; offline or a
//!   lapsed lease answers at once, local work is never blocked.
//! - A claim passes the worker gate first ([`worker::admit_claim`], L1:
//!   the engine's opt-in for the repo, pause, today's budget) and goes out
//!   under that opt-in; a result passes [`worker::admit_submit`]. Hive
//!   checks the same opt-in again (`optin_scope`, `optin_inactive`,
//!   `daily_budget`, `optin_changed`, `vetoed`); every refusal is shown as
//!   it is, an `optin_changed` claim reads the opt-in again once, and a
//!   stopped opt-in ends this machine's claims.
//! - The claim's lease id and fencing token stay on this machine
//!   ([`claims`], 0600) and are never printed. This process renews its
//!   claims while it runs (`keeper`) and gives them back on stop.
//! - Text leaving the machine (summaries, proposals, release reasons) passes
//!   the outbox screening; a text it refuses is not sent, never rewritten.
//!   Sealed projects send it sealed (AAD `vault_id|number|epoch|schema`).
//! - Text arriving from Hive (titles, goals, summaries of people and other
//!   agents) reaches the agent only fenced as foreign data
//!   ([`crate::core::hive::untrusted::wrap`], X5).
//! - Hive announces claims, releases and results on the bus itself; the
//!   engine sends no task frames.

pub mod claims;
mod client;
mod keeper;
pub mod propose;
mod render;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::core::hive::HiveError;
use crate::core::hive::TaskRefusal;
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::metrics::hive_use;
use crate::core::hive::sealed;
use crate::core::hive::state::HiveStore;
use crate::core::hive::vault::{VaultMap, VaultRecord};
use crate::core::hive::worker::{self, Admission, EvidenceSummary, Refusal, TaskScope};
use claims::HeldClaim;
use client::TasksApi;
use lean_ctx_protocol::hive::lease::{HiveRight, LeaseState};
use lean_ctx_protocol::hive::sealed::SealedBlobV1;
use lean_ctx_protocol::hive::tasks::{
    EvidenceKind, EvidenceV1, MAX_CLAIM_MINUTES, MIN_CLAIM_MINUTES, SEALED_TASK_RESULT_SCHEMA,
    SEALED_TASK_UPDATE_SCHEMA, TASK_CLAIM_RENEW_SCHEMA, TASK_CLAIM_REQUEST_SCHEMA,
    TASK_RELEASE_SCHEMA, TASK_RESULT_SCHEMA, TASK_UPDATE_SCHEMA, TaskAckV1,
    TaskClaimRenewRequestV1, TaskClaimRequestV1, TaskListV1, TaskOutcome, TaskReleaseRequestV1,
    TaskResultRequestV1, TaskUpdateRequestV1, TaskV1, UsageV1,
};

pub use client::ListFilter;
pub use render::{render_list, render_result_ack, render_task};

/// The time one task call made from a tool call may take in all.
pub const BUDGET: Duration = Duration::from_secs(5);
/// The session-start briefing waits less: it delays the first tool call.
pub const BRIEFING_BUDGET: Duration = Duration::from_secs(2);
/// Claim length when the caller names none.
pub const DEFAULT_CLAIM_MINUTES: u32 = 10;

/// A connected machine, its lease and the Hive project of a local root.
pub struct Connected {
    pub store: HiveStore,
    pub session: Session,
    pub project: VaultRecord,
    pub root: PathBuf,
}

/// Whether tasks apply to `project_root` with `right`; `Err` says why not
/// (no request made).
pub fn connected(project_root: &Path, right: HiveRight) -> Result<Connected, HiveError> {
    let config = crate::core::config::Config::load().hive.clone();
    connected_with(&config, HiveStore::open()?, project_root, right)
}

fn connected_with(
    config: &HiveConfig,
    store: HiveStore,
    project_root: &Path,
    right: HiveRight,
) -> Result<Connected, HiveError> {
    if config.enabled == Some(false) {
        return Err(HiveError::Config(
            "Hive is off ([hive] enabled = false)".into(),
        ));
    }
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = project_root.to_string_lossy();
    let project = VaultMap::load(store.data_dir())
        .for_root(&root)
        .cloned()
        .ok_or_else(|| {
            HiveError::Config(
                "this repository is not linked to a vault (run `lean-ctx hive sync` in it)".into(),
            )
        })?;
    let session = Session::load(store.clone(), config.root_key(&instance.url)?)?;
    if !matches!(
        session.lease.state_at(Utc::now()),
        LeaseState::Valid | LeaseState::Grace
    ) {
        return Err(HiveError::LeaseLapsed);
    }
    if !session.lease.lease.grants(right) {
        return Err(HiveError::Forbidden);
    }
    Ok(Connected {
        store,
        session,
        project,
        root: project_root.to_path_buf(),
    })
}

impl Connected {
    fn link(&self) -> Link<'_> {
        Link {
            data_dir: self.store.data_dir(),
            vault_id: &self.project.vault_id,
            sealed: self.project.sealed,
            root: &self.root,
            via: Via::Session(&self.session),
            budget: BUDGET,
            deadline: None,
        }
    }
}

/// How a call reaches Hive: the enrolled session, or (tests) a local fake.
enum Via<'a> {
    Session(&'a Session),
    #[cfg(test)]
    Origin(String),
}

/// Everything one task operation needs.
struct Link<'a> {
    data_dir: &'a Path,
    vault_id: &'a str,
    sealed: bool,
    root: &'a Path,
    via: Via<'a>,
    budget: Duration,
    /// One deadline for every request of a call (a claim: opt-in read,
    /// claim and its one retry); `None`: each request gets `budget`.
    deadline: Option<Instant>,
}

impl Link<'_> {
    fn call<T>(
        &self,
        call: impl FnOnce(&TasksApi<'_>) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        let deadline = self
            .deadline
            .unwrap_or_else(|| Instant::now() + self.budget);
        let client = match &self.via {
            Via::Session(session) => session.bounded_client(deadline)?,
            #[cfg(test)]
            Via::Origin(origin) => {
                crate::core::hive::transport::HiveClient::bounded(origin, self.budget)?
            }
        };
        let result = call(&TasksApi {
            client: &client,
            vault_id: self.vault_id,
        });
        match &self.via {
            Via::Session(session) => result.map_err(|e| session.noted(e)),
            #[cfg(test)]
            Via::Origin(_) => result,
        }
    }

    /// [`Self::call`] for a claim or its renewal: while a quota or policy
    /// throttle holds task claims nothing is asked (the refusal says until
    /// when); a `quota_exceeded`/`policy_denied` answer sets one.
    fn call_claim<T>(
        &self,
        call: impl FnOnce(&TasksApi<'_>) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        use crate::core::hive::throttle::{self, Scope};
        let store = match &self.via {
            Via::Session(session) => &session.store,
            #[cfg(test)]
            Via::Origin(_) => return self.call(call),
        };
        throttle::check(store, Scope::Tasks, chrono::Utc::now())?;
        match self.call(call) {
            Ok(value) => {
                throttle::passed(store, Scope::Tasks);
                Ok(value)
            }
            Err(error) => Err(throttle::note(
                store,
                Scope::Tasks,
                error,
                chrono::Utc::now(),
            )),
        }
    }

    /// Now on the service's clock (claim ends are service time).
    fn now(&self) -> DateTime<Utc> {
        match &self.via {
            Via::Session(session) => session.lease.service_time(Utc::now()).with_timezone(&Utc),
            #[cfg(test)]
            Via::Origin(_) => Utc::now(),
        }
    }

    fn session(&self) -> Result<&Session, HiveError> {
        match &self.via {
            Via::Session(session) => Ok(session),
            #[cfg(test)]
            Via::Origin(_) => Err(HiveError::NotConnected),
        }
    }

    /// Seal `content` for task `number` (0 for a proposal).
    fn seal<T: Serialize>(
        &self,
        number: u64,
        content_schema: &str,
        content: &T,
    ) -> Result<SealedBlobV1, HiveError> {
        // Only a key this machine already holds: a tool call never fetches
        // keys or rotates the repo (that is `lean-ctx hive sync`). A key
        // Hive has moved past is refused there, and nothing leaves.
        let session = self.session()?;
        let held = sealed::newest_held_epoch(&session.store, self.vault_id)?.and_then(|epoch| {
            sealed::held_key(&session.store, self.vault_id, epoch).map(|key| (epoch, key))
        });
        let Some((epoch, key)) = held else {
            return Err(HiveError::Refused(
                "this sealed vault's key is not on this machine yet; run `lean-ctx hive sync`"
                    .into(),
            ));
        };
        let bytes = zeroize::Zeroizing::new(
            serde_json::to_vec(content).map_err(|e| HiveError::Local(e.to_string()))?,
        );
        sealed::seal(&key, self.vault_id, number, epoch, content_schema, &bytes)
    }

    /// Open the sealed `blob` of task `number`; `None` when this machine
    /// holds no key for its epoch.
    fn open<T: DeserializeOwned>(&self, number: u64, blob: &SealedBlobV1) -> Option<T> {
        let session = self.session().ok()?;
        let key = sealed::held_key(&session.store, self.vault_id, blob.epoch)?;
        let bytes = sealed::open(&key, self.vault_id, number, blob).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Task `number` of this project as the worker gate sees it.
    fn scope(&self, number: u64) -> TaskScope {
        TaskScope {
            task_id: number.to_string(),
            vault_id: self.vault_id.to_owned(),
        }
    }

    /// The claim `agent` holds on task `number`: never another agent's, so
    /// no agent writes under another's fencing token. The process that
    /// renews it does not matter: the agent's CLI and its session (or the
    /// relay that kept a CLI claim alive) act for the same agent, and the
    /// claims file is this user's alone (0600).
    fn held(&self, number: u64, agent: &str) -> Result<HeldClaim, HiveError> {
        match claims::find(self.data_dir, self.vault_id, number) {
            Some(held) if held.agent == agent => Ok(held),
            Some(_) => Err(HiveError::Refused(format!(
                "task #{number} is claimed on this machine by another agent"
            ))),
            None => Err(HiveError::Refused(format!(
                "this machine holds no claim on task #{number}"
            ))),
        }
    }

    /// Forget the local claim when Hive says it is gone for good.
    fn settle<T>(&self, number: u64, answer: Result<T, HiveError>) -> Result<T, HiveError> {
        let gone = match &answer {
            Err(HiveError::Task { refusal, .. }) => refusal.ends_claim(),
            Err(HiveError::NotFound | HiveError::Vetoed) => true,
            _ => false,
        };
        if gone {
            if matches!(
                &answer,
                Err(HiveError::Task {
                    refusal: TaskRefusal::StaleClaim,
                    ..
                })
            ) {
                let agent =
                    claims::find(self.data_dir, self.vault_id, number).map(|held| held.agent);
                crate::core::agents::friction::record_for(
                    crate::core::agents::friction::CLAIM_EXPIRED,
                    agent.as_deref(),
                    self.root.to_str(),
                );
            }
            let _ = claims::remove(self.data_dir, self.vault_id, number);
        }
        answer
    }
}

/// `{title, goal?, acceptance}` of a sealed task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContent {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub acceptance: String,
}

/// `{summary}` of a sealed update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SealedSummary {
    summary: String,
}

/// `{summary, evidence}` of a sealed result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SealedResult {
    summary: String,
    evidence: Vec<EvidenceV1>,
}

/// A task as the agent reads it: the view, its sealed parts opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub task: TaskV1,
    /// The opened content of a sealed task.
    pub content: Option<TaskContent>,
    /// The opened summary of each sealed update, in order.
    pub update_summaries: Vec<Option<String>>,
    /// The claim end when this machine holds the claim.
    pub held_until: Option<String>,
}

/// Screen every outgoing `(field, text)`; the first refusal names the field
/// and the reason, never the text.
fn screened(fields: &[(&str, Option<&str>)]) -> Result<(), HiveError> {
    let mut screen = Screen::for_installation();
    for (field, text) in fields {
        if let Some(text) = text
            && let Err(blocked) = screen.check(text)
        {
            return Err(HiveError::Refused(format!(
                "the {field} was not sent: it contains {blocked}"
            )));
        }
    }
    Ok(())
}

/// One piece of evidence as a caller names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EvidenceInput {
    pub kind: String,
    pub reference: String,
}

impl EvidenceInput {
    /// `root` is the checkout a `file_anchor` (`path` or `path::symbol`) is
    /// digested in, as it is now (J2 anchors).
    fn into_evidence(self, root: &Path) -> Result<EvidenceV1, HiveError> {
        let kind = match self.kind.as_str() {
            "receipt" => EvidenceKind::Receipt,
            "commit" => EvidenceKind::Commit,
            "ci_run" => EvidenceKind::CiRun,
            "url" => EvidenceKind::Url,
            "fact" => EvidenceKind::Fact,
            "file_anchor" => return file_anchor(root, self.reference.trim()),
            other => {
                return Err(HiveError::Refused(format!(
                    "unknown evidence kind {other:?} (receipt, commit, ci_run, file_anchor, url, fact)"
                )));
            }
        };
        let evidence = EvidenceV1 {
            kind,
            reference: Some(self.reference.trim().to_owned()),
            anchor: None,
        };
        evidence
            .validate()
            .map_err(|e| HiveError::Refused(format!("evidence: {e}")))?;
        Ok(evidence)
    }
}

/// The anchor of `spec` in `root`: the file is admitted like a source
/// binding first (a restricted, withheld or unreadable file is never
/// digested), then digested as it is now.
fn file_anchor(root: &Path, spec: &str) -> Result<EvidenceV1, HiveError> {
    use crate::core::knowledge::anchors;
    let (path, symbol) = anchors::parse_spec(spec);
    let root_text = root.to_string_lossy();
    if crate::core::archive::bindings::sources_at(&root_text, &[path], "ctx_tasks").is_none() {
        return Err(HiveError::Refused(format!(
            "{path}: not an admitted project file"
        )));
    }
    let anchor = anchors::capture(root, path, symbol).map_err(HiveError::Refused)?;
    let evidence = EvidenceV1 {
        kind: EvidenceKind::FileAnchor,
        reference: None,
        anchor: Some(anchor),
    };
    evidence
        .validate()
        .map_err(|e| HiveError::Refused(format!("evidence: {e}")))?;
    Ok(evidence)
}

/// The project's tasks.
pub fn list(project_root: &Path, filter: &ListFilter) -> Result<TaskListV1, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    list_via(&c.link(), filter)
}

fn list_via(link: &Link<'_>, filter: &ListFilter) -> Result<TaskListV1, HiveError> {
    link.call(|api| api.list(filter))
}

/// One task, its sealed parts opened where this machine holds the key.
pub fn show(project_root: &Path, number: u64) -> Result<TaskView, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    show_via(&c.link(), number)
}

fn show_via(link: &Link<'_>, number: u64) -> Result<TaskView, HiveError> {
    let task = link.call(|api| api.show(number))?;
    let content = task
        .sealed
        .as_ref()
        .and_then(|blob| link.open::<TaskContent>(number, blob));
    let update_summaries = task
        .updates
        .iter()
        .map(|update| {
            update.summary.clone().or_else(|| {
                update
                    .sealed
                    .as_ref()
                    .and_then(|blob| link.open::<SealedSummary>(number, blob))
                    .map(|sealed| sealed.summary)
            })
        })
        .collect();
    let held_until = claims::find(link.data_dir, link.vault_id, number).map(|held| held.until);
    Ok(TaskView {
        task,
        content,
        update_summaries,
        held_until,
    })
}

/// A granted claim, as the caller may see it (no lease id, no token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimGranted {
    pub number: u64,
    pub until: String,
    pub minutes: u32,
}

/// Claim task `number` for bus agent `agent` in session `session_id`.
pub fn claim(
    project_root: &Path,
    number: u64,
    agent: &str,
    session_id: &str,
    minutes: Option<u32>,
) -> Result<ClaimGranted, HiveError> {
    let c = connected(project_root, HiveRight::TasksWork)?;
    let local = crate::core::config::Config::load().hive.worker.clone();
    // The whole claim (opt-in read, claim, its one retry) ends within one
    // BUDGET, like every task call from a tool.
    let deadline = Instant::now() + BUDGET;
    // Whether the gate let a request go to Hive: then an `Ok` is Hive's
    // grant (a claim this machine already holds never reaches the gate).
    let asked = std::cell::Cell::new(false);
    let gate = |scope: &TaskScope| {
        let unavailable = |e: HiveError| Refusal::Unavailable(e.to_string());
        let client = c.session.bounded_client(deadline).map_err(unavailable)?;
        let optin = worker::fetch_optin_via(&client).map_err(unavailable)?;
        let admission = worker::admit_claim_for(&c.store, &local, scope, &optin)?;
        asked.set(true);
        Ok(admission)
    };
    let mut link = c.link();
    link.deadline = Some(deadline);
    let granted = claim_via(&link, number, agent, session_id, minutes, &gate)
        .inspect_err(|error| record_claim_friction(error, agent, project_root))?;
    // Counted only once granted: a refused claim costs no task today. The
    // claim stands either way; Hive counts it too.
    if asked.get()
        && let Err(error) = worker::count_claim(&c.store)
    {
        tracing::warn!("hive worker: a granted claim was not counted here: {error}");
    }
    keeper::ensure_running();
    Ok(granted)
}

/// Count a claim Hive refused for presence, role or consent (local
/// friction log, the retro loop's input; ids, no content).
fn record_claim_friction(error: &HiveError, agent: &str, root: &Path) {
    use crate::core::agents::friction;
    if let HiveError::Task {
        refusal: TaskRefusal::NotPermitted(check),
        ..
    } = error
    {
        let signal = match check {
            crate::core::hive::error::PermitCheck::Presence => friction::CLAIM_REFUSED_PRESENCE,
            crate::core::hive::error::PermitCheck::Role => friction::CLAIM_REFUSED_ROLE,
            crate::core::hive::error::PermitCheck::Unknown => friction::CLAIM_REFUSED_PERMIT,
        };
        friction::record_for(signal, Some(agent), root.to_str());
    }
}

/// The worker gate of a claim (L1): the opt-in it works under, or why not.
type ClaimGate<'g> = &'g dyn Fn(&TaskScope) -> Result<Admission, Refusal>;

fn claim_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    session_id: &str,
    minutes: Option<u32>,
    gate: ClaimGate<'_>,
) -> Result<ClaimGranted, HiveError> {
    let minutes = minutes
        .unwrap_or(DEFAULT_CLAIM_MINUTES)
        .clamp(MIN_CLAIM_MINUTES, MAX_CLAIM_MINUTES);
    if let Some(held) = claims::find(link.data_dir, link.vault_id, number)
        && held.until().is_some_and(|until| until > link.now())
    {
        if held.agent != agent {
            return Err(HiveError::Refused(format!(
                "task #{number} is claimed on this machine by another agent"
            )));
        }
        // The holder claiming again wants more time: renew, so the answer is
        // a fresh expiry, not the one it already had. A running session that
        // holds the claim renews it itself; it is reported as it stands.
        if held.pid != std::process::id() && crate::ipc::process::is_alive(held.pid) {
            return Ok(ClaimGranted {
                number,
                until: held.until,
                minutes: held.minutes,
            });
        }
        let renewed = renew_via(link, number, agent)?;
        return Ok(ClaimGranted {
            number,
            until: renewed.until,
            minutes: renewed.minutes,
        });
    }
    // The worker gate first (opt-in, scope, pause, today's budget); Hive
    // then checks the same opt-in (C0 1.9 §4: `optin_scope`,
    // `optin_inactive`, `daily_budget`, `optin_changed`). An opt-in that
    // changed since it was read is read again, once.
    let attempt = || {
        let admission =
            gate(&link.scope(number)).map_err(|refusal| HiveError::Refused(refusal.to_string()))?;
        let request = TaskClaimRequestV1 {
            schema: TASK_CLAIM_REQUEST_SCHEMA.into(),
            agent: agent.to_owned(),
            session_ref: crate::core::hive::knowledge::draft::session_ref(session_id),
            minutes,
            optin_id: admission.optin_id,
        };
        link.call_claim(|api| api.claim(number, &request))
    };
    let ack = match attempt() {
        Err(HiveError::Task {
            refusal: TaskRefusal::OptinChanged,
            ..
        }) => attempt()?,
        answer => answer?,
    };
    let until = ack.until.as_str().to_owned();
    claims::put(
        link.data_dir,
        HeldClaim {
            vault_id: link.vault_id.to_owned(),
            number,
            root: link.root.to_string_lossy().into_owned(),
            agent: agent.to_owned(),
            claim: ack.claim,
            until: until.clone(),
            minutes,
            pid: std::process::id(),
        },
    )?;
    Ok(ClaimGranted {
        number,
        until,
        minutes,
    })
}

/// Renew the claim this machine holds on `number`.
fn renew_via(link: &Link<'_>, number: u64, agent: &str) -> Result<HeldClaim, HiveError> {
    let mut held = link.held(number, agent)?;
    let request = TaskClaimRenewRequestV1 {
        schema: TASK_CLAIM_RENEW_SCHEMA.into(),
        claim: held.claim.clone(),
        minutes: held.minutes,
    };
    let ack = link.settle(number, link.call_claim(|api| api.renew(number, &request)))?;
    held.claim = ack.claim;
    ack.until.as_str().clone_into(&mut held.until);
    if !claims::renewed(link.data_dir, &held)? {
        return Err(HiveError::Refused(format!(
            "task #{number} was given back meanwhile; its renewal is not kept"
        )));
    }
    Ok(held)
}

/// Give task `number` back; `reason` is one line (left out in sealed
/// projects).
pub fn release(
    project_root: &Path,
    number: u64,
    agent: &str,
    reason: Option<&str>,
) -> Result<TaskAckV1, HiveError> {
    let c = connected(
        &claim_root(project_root, number, agent),
        HiveRight::TasksWork,
    )?;
    release_via(&c.link(), number, agent, reason)
}

fn release_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    reason: Option<&str>,
) -> Result<TaskAckV1, HiveError> {
    let held = link.held(number, agent)?;
    let reason = reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty() && !link.sealed);
    screened(&[("release reason", reason)])?;
    let request = TaskReleaseRequestV1 {
        schema: TASK_RELEASE_SCHEMA.into(),
        claim: held.claim,
        reason: reason.map(str::to_owned),
    };
    let ack = link.settle(number, link.call(|api| api.release(number, &request)))?;
    claims::remove(link.data_dir, link.vault_id, number)?;
    Ok(ack)
}

/// Report one short summary on the claimed task `number`. When the claim
/// lapsed while the agent worked (Hive: stale claim), the task is claimed
/// again for it and the summary sent once more; another agent's claim
/// refuses that.
pub fn update(
    project_root: &Path,
    number: u64,
    agent: &str,
    session_id: &str,
    summary: &str,
    usage: Option<UsageV1>,
) -> Result<TaskAckV1, HiveError> {
    let project_root = &claim_root(project_root, number, agent);
    let c = connected(project_root, HiveRight::TasksWork)?;
    let usage = engine_usage(&c, number, usage);
    update_or_reclaim_via(&c.link(), number, agent, summary, usage, || {
        claim(project_root, number, agent, session_id, None)
    })
}

fn update_or_reclaim_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    summary: &str,
    usage: Option<UsageV1>,
    reclaim: impl FnOnce() -> Result<ClaimGranted, HiveError>,
) -> Result<TaskAckV1, HiveError> {
    match update_via(link, number, agent, summary, usage) {
        Err(HiveError::Task {
            refusal: TaskRefusal::StaleClaim,
            ..
        }) => {
            reclaim()?;
            update_via(link, number, agent, summary, usage)
        }
        answer => answer,
    }
}

/// C9: the usage a write carries is the engine's own measure of the claim
/// (host sessions at list price), at least what the agent reported; only
/// where this machine recorded nothing does the report stand alone.
fn engine_usage(c: &Connected, number: u64, reported: Option<UsageV1>) -> Option<UsageV1> {
    let measured = dirs::home_dir().and_then(|home| {
        let others: Vec<String> = VaultMap::load(c.store.data_dir())
            .vaults
            .into_values()
            .filter(|p| p.vault_id != c.project.vault_id)
            .flat_map(|p| p.roots)
            .collect();
        let now = Utc::now();
        let usage = hive_use::host_window(&home, &c.project.roots, &others, now);
        hive_use::claim_usage(c.store.data_dir(), &c.project.vault_id, number, now, &usage)
    });
    match (measured, reported) {
        (Some(measured), reported) => Some(UsageV1 {
            tokens: measured.tokens.max(reported.map_or(0, |r| r.tokens)),
            usd_micros: measured.usd_micros,
        }),
        (None, reported) => reported,
    }
}

fn update_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    summary: &str,
    usage: Option<UsageV1>,
) -> Result<TaskAckV1, HiveError> {
    let held = link.held(number, agent)?;
    let summary = summary.trim();
    screened(&[("summary", Some(summary))])?;
    let (summary, sealed) = if link.sealed {
        let content = SealedSummary {
            summary: summary.to_owned(),
        };
        (
            None,
            Some(link.seal(number, SEALED_TASK_UPDATE_SCHEMA, &content)?),
        )
    } else {
        (Some(summary.to_owned()), None)
    };
    let request = TaskUpdateRequestV1 {
        schema: TASK_UPDATE_SCHEMA.into(),
        claim: held.claim,
        summary,
        sealed,
        usage,
    };
    link.settle(number, link.call(|api| api.update(number, &request)))
}

/// Submit the result of the claimed task `number`: it ends the claim. A
/// 1.x Hive turns it into a proposal a person (or an auto-merge threshold)
/// decides on (`submitted`); Hive v5 ends the task (`done`, or `failed`
/// for [`TaskOutcome::Failed`]).
///
/// A result never carries a draft: knowledge the work produced travels
/// through the capture gate and `hive sync` like any other (signed E5
/// events on a C0 2.0 Hive, the draft path on 1.x).
pub fn submit(
    project_root: &Path,
    number: u64,
    agent: &str,
    summary: &str,
    evidence: Vec<EvidenceInput>,
    usage: Option<UsageV1>,
    outcome: TaskOutcome,
) -> Result<TaskAckV1, HiveError> {
    let c = connected(
        &claim_root(project_root, number, agent),
        HiveRight::TasksWork,
    )?;
    let usage = engine_usage(&c, number, usage);
    // The gate decides before anything is sent; the usage counts against
    // today's budget only once Hive accepted the result (a refused or
    // repeated submit is not counted twice).
    let decided = std::cell::RefCell::new(None);
    let gate = |_: &TaskScope, evidence: &EvidenceSummary| {
        *decided.borrow_mut() = Some(evidence.clone());
        Ok(())
    };
    let ack = submit_via(
        &c.link(),
        number,
        agent,
        Submission {
            summary,
            evidence,
            usage,
            outcome,
        },
        &gate,
    )?;
    if let Some(evidence) = decided.take() {
        let _ = worker::admit_submit(&c.store, &evidence);
    }
    Ok(ack)
}

/// The worker gate of a result (L1): what it cites and costs, admitted
/// before anything is sent.
type SubmitGate<'g> = &'g dyn Fn(&TaskScope, &EvidenceSummary) -> Result<(), Refusal>;

/// What a result says.
struct Submission<'s> {
    summary: &'s str,
    evidence: Vec<EvidenceInput>,
    usage: Option<UsageV1>,
    outcome: TaskOutcome,
}

/// `outcome` as the request carries it. `completed` is never sent (absent
/// means completed on every line). `failed` goes only to a Hive whose
/// task views carry the v5 `lifecycle`: a 1.x Hive denies the unknown
/// field, so against it the failure is refused here, before anything is
/// sent, and the agent releases the task instead.
fn negotiated_outcome(
    link: &Link<'_>,
    number: u64,
    outcome: TaskOutcome,
) -> Result<Option<TaskOutcome>, HiveError> {
    if outcome == TaskOutcome::Completed {
        return Ok(None);
    }
    let task = link.call(|api| api.show(number))?;
    if task.lifecycle.is_none() {
        return Err(HiveError::Refused(format!(
            "this Hive does not take failed results yet; release task #{number} with a reason instead"
        )));
    }
    Ok(Some(outcome))
}

fn submit_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    submission: Submission<'_>,
    gate: SubmitGate<'_>,
) -> Result<TaskAckV1, HiveError> {
    let Submission {
        summary,
        evidence,
        usage,
        outcome,
    } = submission;
    let held = link.held(number, agent)?;
    let summary = summary.trim();
    let evidence = evidence
        .into_iter()
        .map(|item| item.into_evidence(link.root))
        .collect::<Result<Vec<_>, _>>()?;
    // A path or symbol name leaves the machine too: screened like text.
    let references: Vec<&str> = evidence
        .iter()
        .flat_map(|item| {
            let anchor = item.anchor.as_ref();
            [
                item.reference.as_deref(),
                anchor.map(|a| a.path.as_str()),
                anchor.and_then(|a| a.symbol.as_deref()),
            ]
        })
        .flatten()
        .collect();
    let mut fields = vec![("summary", Some(summary))];
    fields.extend(
        references
            .iter()
            .map(|reference| ("evidence", Some(*reference))),
    );
    screened(&fields)?;
    gate(
        &link.scope(number),
        &EvidenceSummary {
            usage_tokens: usage.map_or(0, |usage| usage.tokens),
            usage_usd_micros: usage.and_then(|usage| usage.usd_micros).unwrap_or(0),
        },
    )
    .map_err(|refusal| HiveError::Refused(refusal.to_string()))?;
    let (summary, evidence, sealed) = if link.sealed {
        let content = SealedResult {
            summary: summary.to_owned(),
            evidence,
        };
        (
            None,
            Vec::new(),
            Some(link.seal(number, SEALED_TASK_RESULT_SCHEMA, &content)?),
        )
    } else {
        (Some(summary.to_owned()), evidence, None)
    };
    let outcome = negotiated_outcome(link, number, outcome)?;
    let request = TaskResultRequestV1 {
        schema: TASK_RESULT_SCHEMA.into(),
        claim: held.claim,
        summary,
        evidence,
        draft: None,
        sealed,
        usage,
        outcome,
    };
    let ack = link.settle(number, link.call(|api| api.result(number, &request)))?;
    claims::remove(link.data_dir, link.vault_id, number)?;
    Ok(ack)
}

/// Give back every claim this process holds (worker stop, shutdown);
/// returns how many Hive released. A claim Hive cannot be reached for is
/// forgotten here and ends at its expiry there.
pub fn release_own_claims(reason: &str) -> usize {
    let Ok(store) = HiveStore::open() else {
        return 0;
    };
    let mut released = 0;
    for held in claims::own(store.data_dir()) {
        let answer = connected(Path::new(&held.root), HiveRight::TasksWork)
            .and_then(|c| release_via(&c.link(), held.number, &held.agent, Some(reason)));
        if answer.is_ok() {
            released += 1;
        }
        let _ = claims::remove(store.data_dir(), &held.vault_id, held.number);
    }
    released
}

/// How many claims this process holds.
fn own_claims() -> usize {
    HiveStore::open().map_or(0, |store| claims::own(store.data_dir()).len())
}

/// A session idle this long no longer keeps its claims alive: they end at
/// their expiry in Hive, so a silent agent frees its task quickly.
const RENEW_IDLE: std::time::Duration = std::time::Duration::from_mins(30);

/// Renew the claims of this process whose half-time has passed, unless the
/// process has been idle for [`RENEW_IDLE`] or worker mode is paused here;
/// returns how many this process still holds.
fn renew_own_due() -> usize {
    let Ok(store) = HiveStore::open() else {
        return 0;
    };
    let idle = crate::server::mcp_idle::idle_for_now().is_some_and(|idle| idle >= RENEW_IDLE);
    if idle || crate::core::hive::worker::read_state(&store).paused {
        return claims::own(store.data_dir()).len();
    }
    for held in claims::own(store.data_dir()) {
        let Ok(c) = connected(Path::new(&held.root), HiveRight::TasksWork) else {
            continue;
        };
        let link = c.link();
        if held.renew_due(link.now()) {
            let _ = renew_via(&link, held.number, &held.agent);
        }
    }
    claims::own(store.data_dir()).len()
}

/// Take over the unexpired claims `agent` holds on this machine (a new
/// session of the same agent) and keep them renewed.
pub fn adopt_claims(agent: &str) {
    let Ok(store) = HiveStore::open() else {
        return;
    };
    let now = Utc::now();
    if claims::adopt(store.data_dir(), agent, now).is_ok_and(|held| !held.is_empty()) {
        keeper::ensure_running();
    }
}

/// Keep CLI claims alive (lean-ctx #3): `lean-ctx hive tasks claim` ends
/// right after claiming, so nothing renewed its claim and an agent running
/// a long gate lost it before submitting. The bus relay's process renews a
/// claim whose holder has ended at half-time while the claiming agent still
/// works on this machine (registered, not finished, active within
/// `RENEW_IDLE`), without becoming its holder; once the agent stops
/// working the claim ends at its expiry in Hive. Returns how many such
/// claims it kept.
pub fn keep_cli_claims() -> usize {
    let Ok(store) = HiveStore::open() else {
        return 0;
    };
    if crate::core::hive::worker::read_state(&store).paused {
        return 0;
    }
    let now = Utc::now();
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    let idle = chrono::Duration::from_std(RENEW_IDLE).unwrap_or(chrono::Duration::MAX);
    let working: std::collections::HashSet<String> = registry
        .list_active(None)
        .into_iter()
        .filter(|agent| now - agent.last_active < idle)
        .map(|agent| agent.agent_id.clone())
        .collect();
    keep_orphaned(
        store.data_dir(),
        now,
        |agent| working.contains(agent),
        |held| {
            if let Ok(c) = connected(Path::new(&held.root), HiveRight::TasksWork) {
                let _ = renew_via(&c.link(), held.number, &held.agent);
            }
        },
    )
}

/// Renew the orphaned claims of `working` agents whose half-time has
/// passed; returns how many orphaned claims of `working` agents there are.
fn keep_orphaned(
    data_dir: &Path,
    now: chrono::DateTime<Utc>,
    working: impl Fn(&str) -> bool,
    renew: impl Fn(&HeldClaim),
) -> usize {
    let kept: Vec<HeldClaim> = claims::orphaned(data_dir, now)
        .into_iter()
        .filter(|held| working(&held.agent))
        .collect();
    for held in kept.iter().filter(|held| held.renew_due(now)) {
        renew(held);
    }
    kept.len()
}

/// The checkout to act on claim `number` from: `project_root`, unless its
/// vault holds no claim of `agent` on `number` while exactly one other
/// vault does. An agent's session often runs in another repository than the
/// one it claimed in (the claim names its checkout), and the task number
/// alone then finds the claim the agent holds.
#[must_use]
pub fn claim_root(project_root: &Path, number: u64, agent: &str) -> PathBuf {
    let Ok(store) = HiveStore::open() else {
        return project_root.to_path_buf();
    };
    claim_root_in(store.data_dir(), project_root, number, agent)
}

fn claim_root_in(data_dir: &Path, project_root: &Path, number: u64, agent: &str) -> PathBuf {
    let here = VaultMap::load(data_dir)
        .for_root(&project_root.to_string_lossy())
        .map(|vault| vault.vault_id.clone());
    let mine: Vec<HeldClaim> = claims::all(data_dir)
        .into_iter()
        .filter(|held| held.number == number && held.agent == agent)
        .collect();
    let held_here = mine
        .iter()
        .any(|held| here.as_deref() == Some(held.vault_id.as_str()));
    match mine.as_slice() {
        [only] if !held_here => PathBuf::from(&only.root),
        _ => project_root.to_path_buf(),
    }
}

/// What a session starts with (H2): the claims this machine holds in the
/// project, then its open tasks. `None` when Hive does not apply or there
/// is nothing to say. Bounded by [`BRIEFING_BUDGET`].
pub fn briefing(project_root: &Path) -> Option<String> {
    let c = connected(project_root, HiveRight::TasksRead).ok()?;
    let mut link = c.link();
    link.budget = BRIEFING_BUDGET;
    let now = link.now();
    let mut lines: Vec<String> = claims::all(link.data_dir)
        .into_iter()
        .filter(|held| {
            held.vault_id == link.vault_id && held.until().is_some_and(|until| until > now)
        })
        .map(|held| {
            format!(
                "Hive task #{} is claimed on this machine for agent {} until {}.",
                held.number, held.agent, held.until
            )
        })
        .collect();
    let open = list_via(
        &link,
        &ListFilter {
            state: Some(lean_ctx_protocol::hive::tasks::TaskState::Open),
            ..ListFilter::default()
        },
    )
    .ok()
    .filter(|list| !list.tasks.is_empty());
    if let Some(open) = open {
        lines.push(render_list(&open, 5));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
