// SPDX-License-Identifier: Apache-2.0
//! Tasks in the engine (H2, C0 1.9 §1): read a project's tasks, claim one,
//! report short summaries, submit one result with evidence, give it back.
//!
//! The rules the engine keeps:
//! - No tool call waits for Hive ([`cache`]): reads answer from the last
//!   data Hive sent and say when it is stale; changes are queued, and what
//!   Hive answered comes with the caller's next call. Each queued request
//!   ends within [`BUDGET`]; offline or a lapsed lease answers at once.
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
//! - Text arriving from Hive (titles, goals, summaries of people and other
//!   agents) reaches the agent only fenced as foreign data
//!   ([`crate::untrusted::wrap`], X5).
//! - Hive announces claims, releases and results on the bus itself; the
//!   engine sends no task frames.

pub mod cache;
pub mod claims;
mod client;
pub mod fields;
pub mod gate;
mod keeper;
pub mod local;
mod process;
pub mod propose;
pub mod reads;
mod render;

use crate::HiveConfigExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::HiveError;
use crate::TaskRefusal;
use crate::auth::Session;
use crate::config::HiveConfig;
use crate::knowledge::screening::Screen;
use crate::metrics::hive_use;
use crate::state::HiveStore;
use crate::vault::{VaultMap, VaultRecord};
use crate::worker::{self, Admission, EvidenceSummary, Refusal, TaskScope};
use claims::HeldClaim;
use client::TasksApi;
use lean_ctx_protocol::hive::lease::{HiveRight, LeaseState};
use lean_ctx_protocol::hive::tasks::{
    EvidenceKind, EvidenceV1, MAX_CLAIM_MINUTES, MAX_TASKS_PAGE, MIN_CLAIM_MINUTES,
    TASK_AMEND_SCHEMA, TASK_CLAIM_RENEW_SCHEMA, TASK_CLAIM_REQUEST_SCHEMA, TASK_RELEASE_SCHEMA,
    TASK_RESULT_SCHEMA, TASK_UPDATE_SCHEMA, TASK_VERIFY_SCHEMA, TaskAckV1, TaskActorKind,
    TaskAmendRequestV1, TaskClaimRenewRequestV1, TaskClaimRequestV1, TaskListV1, TaskOutcome,
    TaskPriority, TaskReleaseRequestV1, TaskResultRequestV1, TaskUpdateRequestV1, TaskV1,
    TaskVerdict, TaskVerifyRequestV1, UsageV1,
};

pub use client::ListFilter;
pub use render::{
    PARTIAL_NOTE, render_list, render_listing, render_result_ack, render_task, render_verify_ack,
};

/// The time one task call may take in all.
pub const BUDGET: Duration = Duration::from_secs(5);
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
    let config = lean_ctx::core::config::Config::load().hive.clone();
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
    let project =
        crate::resolution::vault_for_root(store.data_dir(), project_root)?.ok_or_else(|| {
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
            root: &self.root,
            via: Via::Session(&self.session),
            budget: BUDGET,
            deadline: std::cell::Cell::new(None),
            workers: Some(&self.store),
            presence: &process::local_presence,
            announce: Some(crate::bus::presence::announce_and_wait),
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
    root: &'a Path,
    via: Via<'a>,
    budget: Duration,
    /// One deadline for every request of a call (a claim: opt-in read,
    /// claim and its one retry); `None`: each request gets `budget`. A
    /// claim tried again after the relay announced gets a fresh one.
    deadline: std::cell::Cell<Option<Instant>>,
    /// Where a local bus agent's worker is looked up (`None`: the agent
    /// goes to Hive as given).
    workers: Option<&'a HiveStore>,
    /// Where a bus agent's live presence on this Instance is looked up
    /// (the checker and the author of a verify, §3.1).
    presence: process::PresenceLookup<'a>,
    /// Asks the running relay to announce now and waits up to the given
    /// time for its hello: what it announced, `None` when no relay
    /// answered. `None` here: a claim refused for presence or role is not
    /// tried again.
    announce: Option<fn(Duration) -> Option<String>>,
}

/// How long a claim refused for presence or role waits for the relay's
/// fresh announcement before it is tried once more (Hive task #27).
const ANNOUNCE_WAIT: Duration = Duration::from_secs(3);

impl Link<'_> {
    /// The gate's directories: private copies under the data dir, the run
    /// tree outside `$HOME`.
    fn gate_dirs(&self) -> gate::GateDirs {
        gate::GateDirs::for_instance(self.data_dir)
    }

    /// The id Hive knows `agent` by. Hive keeps presence and claims under
    /// the worker the relay announced (`wkr_…`), so a local bus agent id
    /// is sent as its worker; a worker id, or a name Hive resolves, goes
    /// as it is.
    fn hive_agent(&self, agent: &str) -> String {
        self.hive_identity(agent).0
    }

    /// [`Self::hive_agent`] and the role of the worker it names when the
    /// book knows it: whom a refusal says Hive looked up (task #27).
    fn hive_identity(&self, agent: &str) -> (String, Option<String>) {
        if crate::bus::address::is_worker_id(agent) {
            return (agent.to_owned(), None);
        }
        self.workers
            .and_then(|store| crate::bus::messages_v2::known_worker_and_role_of(store, agent))
            .map_or_else(|| (agent.to_owned(), None), |(id, role)| (id, Some(role)))
    }

    /// One request that may change the vault's tasks: what this Instance
    /// keeps of them ([`reads`]) is outdated afterwards, so the next read
    /// asks Hive.
    fn call<T>(
        &self,
        call: impl FnOnce(&TasksApi<'_>) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        let result = self.call_read(call);
        reads::outdate(self.data_dir, self.vault_id);
        result
    }

    /// The Hive the requests go to (a 429 hold is kept per Hive).
    fn hive_url(&self) -> &str {
        match &self.via {
            Via::Session(session) => &session.instance.url,
            #[cfg(test)]
            Via::Origin(origin) => origin,
        }
    }

    /// One request that only reads.
    fn call_read<T>(
        &self,
        call: impl FnOnce(&TasksApi<'_>) -> Result<T, HiveError>,
    ) -> Result<T, HiveError> {
        let deadline = self
            .deadline
            .get()
            .unwrap_or_else(|| Instant::now() + self.budget);
        let client = match &self.via {
            Via::Session(session) => session.bounded_client(deadline)?,
            #[cfg(test)]
            Via::Origin(origin) => crate::transport::HiveClient::bounded(origin, self.budget)?,
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
        use crate::throttle::{self, Scope};
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
                lean_ctx::core::agents::friction::record_for(
                    lean_ctx::core::agents::friction::CLAIM_EXPIRED,
                    agent.as_deref(),
                    self.root.to_str(),
                );
            }
            let _ = claims::remove(self.data_dir, self.vault_id, number);
        }
        answer
    }
}

/// A task as the agent reads it: the view and its local claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub task: TaskV1,
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
    pub(crate) fn into_evidence(self, root: &Path) -> Result<EvidenceV1, HiveError> {
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
    use lean_ctx::core::knowledge::anchors;
    let (path, symbol) = anchors::parse_spec(spec);
    let root_text = root.to_string_lossy();
    if lean_ctx::core::archive::bindings::sources_at(&root_text, &[path], "ctx_tasks").is_none() {
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

/// A task list as the Instance shows it.
///
/// `partial`: a Hive answer came back full ([`MAX_TASKS_PAGE`] tasks), so
/// older tasks of these filters may be missing. The list says so, never
/// silently incomplete. A list cached before 3.12.3 reads as whole.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct TaskListing {
    #[serde(flatten)]
    pub list: TaskListV1,
    #[serde(default)]
    pub partial: bool,
}

/// The project's tasks (shared and coalesced per Instance, [`reads`]).
/// Only a current list: when Hive holds reads or is not reachable this is
/// an error, not the last good list ([`list_read`] serves that with its
/// age), so no caller takes tasks since claimed or closed for current.
pub fn list(project_root: &Path, filter: &ListFilter) -> Result<TaskListing, HiveError> {
    list_read(project_root, filter).and_then(reads::current)
}

/// [`list`] and, when Hive holds reads (429) or is not reachable, the age
/// of the last good list served instead.
pub fn list_read(
    project_root: &Path,
    filter: &ListFilter,
) -> Result<reads::Read<TaskListing>, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    list_cached(&c.link(), filter)
}

fn list_cached(
    link: &Link<'_>,
    filter: &ListFilter,
) -> Result<reads::Read<TaskListing>, HiveError> {
    let key = format!("list{}", filter.query()?);
    reads::read_through(link.data_dir, link.hive_url(), link.vault_id, &key, || {
        list_via(link, filter)
    })
}

/// The listing in board order ([`fields::sort_by_priority`]).
///
/// Hive answers one page, the newest tasks by number, so an older high task
/// can fall off it: the high tasks of the same filters are fetched too and
/// merged in, each task once, before the sort. That answer is a page too, so
/// a full one marks the listing `partial`.
fn list_via(link: &Link<'_>, filter: &ListFilter) -> Result<TaskListing, HiveError> {
    let mut list = link.call_read(|api| api.list(filter))?;
    let mut partial = list.tasks.len() >= MAX_TASKS_PAGE;
    if filter.priority.is_none() {
        let high = ListFilter {
            priority: Some(TaskPriority::High),
            ..filter.clone()
        };
        let high = link.call_read(|api| api.list(&high))?;
        partial |= high.tasks.len() >= MAX_TASKS_PAGE;
        let seen: std::collections::HashSet<u64> = list.tasks.iter().map(|t| t.number).collect();
        list.tasks
            .extend(high.tasks.into_iter().filter(|t| !seen.contains(&t.number)));
    }
    fields::sort_by_priority(&mut list.tasks);
    Ok(TaskListing { list, partial })
}

/// One task (shared and coalesced per Instance, [`reads`]). Only a current
/// answer: when Hive holds reads or is not reachable this is an error, not
/// the last good answer ([`show_read`] serves that with its age).
pub fn show(project_root: &Path, number: u64) -> Result<TaskView, HiveError> {
    show_read(project_root, number).and_then(reads::current)
}

/// [`show`] and, when Hive holds reads (429) or is not reachable, the age
/// of the last good answer served instead. A task Hive does not know is
/// [`HiveError::NotFound`]; a rate limit never reads as one.
pub fn show_read(project_root: &Path, number: u64) -> Result<reads::Read<TaskView>, HiveError> {
    let c = connected(project_root, HiveRight::TasksRead)?;
    let link = c.link();
    show_cached(&link, number)
}

fn show_cached(link: &Link<'_>, number: u64) -> Result<reads::Read<TaskView>, HiveError> {
    let key = format!("task#{number}");
    reads::read_through(link.data_dir, link.hive_url(), link.vault_id, &key, || {
        link.call_read(|api| api.show(number))
    })
    .map(|read| read.map(|task| view_of(link, task)))
}

/// `task` as the caller sees it, with its local claim (no request).
fn view_of(link: &Link<'_>, task: TaskV1) -> TaskView {
    let held_until = claims::find(link.data_dir, link.vault_id, task.number).map(|held| held.until);
    TaskView { task, held_until }
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
    let local = lean_ctx::core::config::Config::load().hive.worker.clone();
    // The whole claim (opt-in read, claim, its one retry) ends within one
    // BUDGET, like every task call from a tool.
    let deadline = std::cell::Cell::new(Instant::now() + BUDGET);
    // Whether the gate let a request go to Hive: then an `Ok` is Hive's
    // grant (a claim this machine already holds never reaches the gate).
    let asked = std::cell::Cell::new(false);
    let gate = |scope: &TaskScope| {
        let unavailable = |e: HiveError| Refusal::Unavailable(e.to_string());
        let client = c
            .session
            .bounded_client(deadline.get())
            .map_err(unavailable)?;
        let optin = worker::fetch_optin_via(&client).map_err(unavailable)?;
        let admission = worker::admit_claim_for(&c.store, &local, scope, &optin)?;
        asked.set(true);
        Ok(admission)
    };
    let link = c.link();
    link.deadline.set(Some(deadline.get()));
    // A claim tried again after the relay announced gets a fresh BUDGET,
    // its opt-in read too.
    let restart = || {
        deadline.set(Instant::now() + BUDGET);
        link.deadline.set(Some(deadline.get()));
    };
    let granted = claim_via(&link, number, agent, session_id, minutes, &gate, &restart)
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
    use lean_ctx::core::agents::friction;
    if let HiveError::Task {
        refusal: TaskRefusal::NotPermitted(check),
        ..
    } = error
    {
        let signal = match check {
            crate::error::PermitCheck::Presence => friction::CLAIM_REFUSED_PRESENCE,
            crate::error::PermitCheck::Role => friction::CLAIM_REFUSED_ROLE,
            crate::error::PermitCheck::Unknown => friction::CLAIM_REFUSED_PERMIT,
        };
        friction::record_for(signal, Some(agent), root.to_str());
    }
}

/// Hive refused for presence or role: whom it found on the bus.
fn is_permit_refusal(error: &HiveError) -> bool {
    matches!(
        error,
        HiveError::Task {
            refusal: TaskRefusal::NotPermitted(
                crate::error::PermitCheck::Presence | crate::error::PermitCheck::Role
            ),
            ..
        }
    )
}

/// A presence or role refusal that names whom Hive looked up: the id the
/// claim sent, the bus agent it stands for and that worker's role, and
/// whether the relay announced before the claim was tried again (Hive
/// task #27). Any other error stays as it is.
fn looked_up(link: &Link<'_>, agent: &str, error: HiveError, announced: bool) -> HiveError {
    let HiveError::Task {
        refusal,
        number,
        detail,
    } = error
    else {
        return error;
    };
    if !matches!(refusal, TaskRefusal::NotPermitted(_)) {
        return HiveError::Task {
            refusal,
            number,
            detail,
        };
    }
    let (sent, role) = link.hive_identity(agent);
    let mut line = detail.unwrap_or_else(|| refusal.to_string());
    line.push_str(&match role {
        Some(role) => {
            format!(" Hive looked up {sent} (the worker of bus agent {agent}, role {role}).")
        }
        None if sent == agent && !crate::bus::address::is_worker_id(agent) => format!(
            " Hive looked up {sent} as given: this machine's worker book has no worker for it."
        ),
        None => format!(" Hive looked up {sent}."),
    });
    if link.announce.is_some() {
        line.push_str(if announced {
            " The relay announced again and the claim was tried once more."
        } else {
            " No relay answered the request to announce."
        });
    }
    HiveError::Task {
        refusal,
        number,
        detail: Some(line),
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
    restart: &dyn Fn(),
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
        if held.pid != std::process::id() && lean_ctx::ipc::process::is_alive(held.pid) {
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
            agent: link.hive_agent(agent),
            session_ref: crate::knowledge::draft::session_ref(session_id),
            minutes,
            optin_id: admission.optin_id,
        };
        link.call_claim(|api| api.claim(number, &request))
    };
    let answer = || match attempt() {
        Err(HiveError::Task {
            refusal: TaskRefusal::OptinChanged,
            ..
        }) => attempt(),
        answer => answer,
    };
    // Refused for presence or role (task #27): Hive's presence of the
    // worker may be stale (a relay that reconnected, a role announced
    // late). The relay announces now and the claim is tried once more
    // when it did; a refusal that stands says whom Hive looked up.
    let ack = match answer() {
        Err(error) if is_permit_refusal(&error) => {
            let announced = link.announce.and_then(|announce| announce(ANNOUNCE_WAIT));
            let retried = if announced.is_some() {
                restart();
                answer()
            } else {
                Err(error)
            };
            retried.map_err(|error| looked_up(link, agent, error, announced.is_some()))?
        }
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
    // The commit this claim starts from (G6 of an S task pins it), and the
    // session that holds it (it never checks this task's result).
    claims::trace_claim(
        link.data_dir,
        link.vault_id,
        number,
        process::acting_identity(agent, link.presence),
        gate::head_commit(link.root).ok(),
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

/// Check the submitted result of task `number` as `agent` (C0 2.2 E48):
/// `accept` makes it done, `reject` (with its reason) opens it again. The
/// agent names itself as on its claims; Hive refuses the agent that did the
/// work (`self_verify`) and a task that is not submitted.
pub fn verify(
    project_root: &Path,
    number: u64,
    agent: &str,
    verdict: TaskVerdict,
    reason: Option<&str>,
) -> Result<TaskAckV1, HiveError> {
    let c = connected(project_root, HiveRight::TasksWork)?;
    verify_via(&c.link(), number, agent, verdict, reason)
}

fn verify_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    verdict: TaskVerdict,
    reason: Option<&str>,
) -> Result<TaskAckV1, HiveError> {
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
    screened(&[("verify reason", reason)])?;
    // Checker ≠ author by the per-session identity (process v1 §3.1), and
    // for a process task a green G6 record of the judged commit with its
    // locked paths still unchanged (§5): decided here, before anything is
    // sent.
    // The checker is the live bus presence `agent`, held by this process
    // or its app session; never a free string.
    let task = link.call(|api| api.show(number))?;
    let checker = process::checker_identity(agent, (link.presence)(agent), task.process.is_some())?;
    let trace = claims::trace(link.data_dir, link.vault_id, number);
    process::checker_not_author(&task, &checker, trace.as_ref())?;
    process::verify_gate(&task, link.root, &link.gate_dirs().work)?;
    let request = TaskVerifyRequestV1 {
        schema: TASK_VERIFY_SCHEMA.into(),
        worker_id: link.hive_agent(agent),
        verdict,
        reason: reason.map(str::to_owned),
    };
    let key = blake3::hash(
        serde_json::to_string(&(link.vault_id, number, &request))
            .unwrap_or_default()
            .as_bytes(),
    )
    .to_hex()[..32]
        .to_string();
    link.call(|api| api.verify(number, &request, &key))
}

/// [`screened`] for a result: its texts, and every path or symbol name of
/// its evidence (they leave the machine too).
fn screened_result(
    texts: &[(&str, Option<&str>)],
    evidence: &[EvidenceV1],
) -> Result<(), HiveError> {
    let mut fields = texts.to_vec();
    for item in evidence {
        let anchor = item.anchor.as_ref();
        for reference in [
            item.reference.as_deref(),
            anchor.map(|a| a.path.as_str()),
            anchor.and_then(|a| a.symbol.as_deref()),
        ]
        .into_iter()
        .flatten()
        {
            fields.push(("evidence", Some(reference)));
        }
    }
    screened(&fields)
}

/// What an amendment of a result says (see [`amend`]).
#[derive(Default)]
pub struct AmendInput {
    pub summary: Option<String>,
    pub evidence: Vec<EvidenceInput>,
    /// A `done` task's merge commit (the merge record).
    pub merge_commit: Option<String>,
    pub reason: Option<String>,
    /// The caller checked this process runs for a registered Queen session
    /// (`crate::queens::this_session_is_queen`): it may record a merge.
    pub queen: bool,
}

/// The Hive route an amendment needs and no Hive serves yet.
pub const AMEND_ROUTE: &str = "POST /v1/vaults/{vault_id}/tasks/{number}/amend";

/// Amend the result of vault task `number` as `agent` (the rules of
/// [`lean_ctx_protocol::hive::tasks::amend_rule`] and `may_amend`). Fails
/// closed: it checks the request, the screening and the task (read only)
/// and then refuses, sending nothing, because no Hive serves
/// [`AMEND_ROUTE`] yet; the task stays unchanged.
pub fn amend(
    project_root: &Path,
    number: u64,
    agent: &str,
    input: AmendInput,
) -> Result<TaskAckV1, HiveError> {
    let c = connected(project_root, HiveRight::TasksWork)?;
    amend_via(&c.link(), number, agent, input)
}

fn amend_via(
    link: &Link<'_>,
    number: u64,
    agent: &str,
    input: AmendInput,
) -> Result<TaskAckV1, HiveError> {
    let input_queen = input.queen;
    let trimmed =
        |text: Option<String>| text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    let summary = trimmed(input.summary);
    let reason = trimmed(input.reason);
    let merge_commit = trimmed(input.merge_commit);
    let evidence = input
        .evidence
        .into_iter()
        .map(|item| item.into_evidence(link.root))
        .collect::<Result<Vec<_>, _>>()?;
    let worker_id = link.hive_agent(agent);
    let request = TaskAmendRequestV1 {
        schema: TASK_AMEND_SCHEMA.into(),
        worker_id: worker_id.clone(),
        summary,
        evidence,
        merge_commit,
        reason,
    };
    request
        .validate()
        .map_err(|e| HiveError::Refused(format!("amendment of task #{number}: {e}")))?;
    screened_result(
        &[
            ("summary", request.summary.as_deref()),
            ("amend reason", request.reason.as_deref()),
        ],
        &request.evidence,
    )?;
    let task = link.call(|api| api.show(number))?;
    let mine = |by: &lean_ctx_protocol::hive::tasks::TaskActorV1| {
        by.kind == TaskActorKind::Instance
            && by.mine
            && by
                .agent
                .as_deref()
                .is_some_and(|a| a == agent || a == worker_id)
    };
    let caller = lean_ctx_protocol::hive::tasks::AmendCaller {
        submitter: task.result.as_ref().is_some_and(|r| mine(&r.by)),
        reviewer: task.verified_by.as_ref().is_some_and(mine),
        // A vault write always comes from an agent of this Instance.
        person: false,
        queen: input_queen,
    };
    lean_ctx_protocol::hive::tasks::may_amend(&request, caller).map_err(|refusal| {
        HiveError::Refused(format!(
            "task #{number}: only its submitter corrects a result; a merge record also a person or a Queen session, never the reviewer ({})",
            refusal.code()
        ))
    })?;
    lean_ctx_protocol::hive::tasks::amend_rule(&task, &request).map_err(|refusal| {
        HiveError::Refused(format!(
            "task #{number} is {:?}: its result cannot be amended like this ({})",
            task.state,
            refusal.code()
        ))
    })?;
    Err(HiveError::Refused(format!(
        "not supported by Hive yet: this Hive has no route to amend a result ({AMEND_ROUTE}); task #{number} is unchanged"
    )))
}

/// Give task `number` back; `reason` is one line.
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
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
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
    let request = TaskUpdateRequestV1 {
        schema: TASK_UPDATE_SCHEMA.into(),
        claim: held.claim,
        summary: Some(summary.to_owned()),
        usage,
    };
    link.settle(number, link.call(|api| api.update(number, &request)))
}

/// Submit the result of the claimed task `number`: it ends the claim. The
/// task waits for a second agent's check (`submitted`), or ends at once
/// (`done`, or `failed` for [`TaskOutcome::Failed`]).
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
    task: &TaskV1,
    number: u64,
    outcome: TaskOutcome,
) -> Result<Option<TaskOutcome>, HiveError> {
    if outcome == TaskOutcome::Completed {
        return Ok(None);
    }
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
    screened_result(&[("summary", Some(summary))], &evidence)?;
    gate(
        &link.scope(number),
        &EvidenceSummary {
            usage_tokens: usage.map_or(0, |usage| usage.tokens),
            usage_usd_micros: usage.and_then(|usage| usage.usd_micros).unwrap_or(0),
        },
    )
    .map_err(|refusal| HiveError::Refused(refusal.to_string()))?;
    // The task as Hive has it: its v5 lifecycle (a failed result) and, for
    // a process task, what G6 runs. G6 runs here, in the Instance, on the
    // claim's worktree; only a green record goes out with the result.
    let task = link.call(|api| api.show(number))?;
    let trace = claims::trace(link.data_dir, link.vault_id, number);
    let gate_record = process::submit_gate(
        &task,
        link.root,
        &link.gate_dirs(),
        &evidence,
        outcome,
        trace.as_ref(),
    )?;
    let outcome = negotiated_outcome(&task, number, outcome)?;
    let commit = gate_record.as_ref().map_or_else(
        || {
            evidence
                .iter()
                .find(|e| e.kind == EvidenceKind::Commit)
                .and_then(|e| e.reference.clone())
        },
        |record| Some(record.artifact_commit.clone()),
    );
    let request = TaskResultRequestV1 {
        schema: TASK_RESULT_SCHEMA.into(),
        claim: held.claim,
        summary: Some(summary.to_owned()),
        evidence,
        draft: None,
        usage,
        outcome,
        gate_record,
    };
    let ack = link.settle(number, link.call(|api| api.result(number, &request)))?;
    claims::remove(link.data_dir, link.vault_id, number)?;
    // The session that submitted it: a verify on this Instance is checked
    // against it (checker ≠ author per session, §3.1). A trace that cannot
    // be written leaves the verify refused (no per-session identity),
    // never allowed.
    let _ = claims::trace_submit(
        link.data_dir,
        link.vault_id,
        number,
        process::acting_identity(agent, link.presence),
        commit,
    );
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

/// How long the MCP session this process serves has had no tool call in
/// flight; `None` while a call runs or when no session is served.
pub type IdleClock = fn() -> Option<Duration>;

static IDLE_CLOCK: std::sync::OnceLock<IdleClock> = std::sync::OnceLock::new();

/// Register the idle clock of the served MCP session (`server::mcp_idle`,
/// when it starts serving). The first registration wins. Without one the
/// process serves no session and is never idle, so claim renewal runs.
pub fn register_idle_clock(clock: IdleClock) {
    let _ = IDLE_CLOCK.set(clock);
}

/// Renew the claims of this process whose half-time has passed, unless the
/// process has been idle for [`RENEW_IDLE`] or worker mode is paused here;
/// returns how many this process still holds.
fn renew_own_due() -> usize {
    let Ok(store) = HiveStore::open() else {
        return 0;
    };
    let idle = IDLE_CLOCK
        .get()
        .and_then(|clock| clock())
        .is_some_and(|idle| idle >= RENEW_IDLE);
    if idle || crate::worker::read_state(&store).paused {
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
    if crate::worker::read_state(&store).paused {
        return 0;
    }
    let now = Utc::now();
    let working = working_agents(now);
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

/// The reason Hive records when the daemon gives an abandoned claim back.
pub const ABANDONED_REASON: &str = "the claiming session ended on its machine";

/// Give back the claims nobody works on any more (k2 E2E: an ended
/// session's claim was neither renewed nor released, so nobody else could
/// take the task): a claim that has expired, or whose holding process has
/// ended while its agent no longer works on this machine. Runs in the
/// daemon (the bus relay's process); returns how many Hive released.
pub fn release_abandoned_claims() -> usize {
    let Ok(store) = HiveStore::open() else {
        return 0;
    };
    let now = Utc::now();
    let working = working_agents(now);
    give_back_abandoned(
        store.data_dir(),
        now,
        |agent| working.contains(agent),
        |held| {
            connected(Path::new(&held.root), HiveRight::TasksWork).and_then(|c| {
                release_via(&c.link(), held.number, &held.agent, Some(ABANDONED_REASON)).map(|_| ())
            })
        },
    )
}

/// The bus agents of this machine at work (registered, not finished,
/// active within [`RENEW_IDLE`]).
fn working_agents(now: chrono::DateTime<Utc>) -> std::collections::HashSet<String> {
    let registry = lean_ctx::core::agents::AgentRegistry::load_or_create();
    let idle = chrono::Duration::from_std(RENEW_IDLE).unwrap_or(chrono::Duration::MAX);
    registry
        .list_active(None)
        .into_iter()
        .filter(|agent| now - agent.last_active < idle)
        .map(|agent| agent.agent_id.clone())
        .collect()
}

/// Whether `held` is abandoned at `now`: expired, or its holder process
/// ended (not this one) and its agent is not `working`.
fn abandoned(
    held: &HeldClaim,
    now: chrono::DateTime<Utc>,
    working: &impl Fn(&str) -> bool,
) -> bool {
    let expired = held.until().is_none_or(|until| until <= now);
    let holder_gone = held.pid != std::process::id() && !lean_ctx::ipc::process::is_alive(held.pid);
    expired || (holder_gone && !working(&held.agent))
}

/// [`release_abandoned_claims`] with the agents at work and the release
/// given. A claim Hive released is gone from the file (`release_via`); an
/// expired one Hive refused is dropped here too (it is over on this
/// machine; nothing can renew it). An unexpired one that could not be
/// released (offline) is tried again on the next pass.
fn give_back_abandoned(
    data_dir: &Path,
    now: chrono::DateTime<Utc>,
    working: impl Fn(&str) -> bool,
    release: impl Fn(&HeldClaim) -> Result<(), HiveError>,
) -> usize {
    let mut released = 0;
    for held in claims::all(data_dir)
        .into_iter()
        .filter(|held| abandoned(held, now, &working))
    {
        match release(&held) {
            Ok(()) => {
                released += 1;
                // `release_via` removes it; a fake release may not.
                let _ = claims::remove(data_dir, &held.vault_id, held.number);
            }
            Err(error) if held.until().is_none_or(|until| until <= now) => {
                tracing::info!(
                    "hive: expired claim of task #{} dropped (Hive: {error})",
                    held.number
                );
                let _ = claims::remove(data_dir, &held.vault_id, held.number);
                crate::metrics::hive_use::claim_ended(
                    data_dir,
                    &held.vault_id,
                    held.number,
                    held.until().unwrap_or(now),
                );
            }
            Err(error) => {
                tracing::debug!(
                    "hive: abandoned claim of task #{} kept: {error}",
                    held.number
                );
            }
        }
    }
    released
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
/// project, then its open tasks from the task cache (a refresh is queued
/// when they are stale; [`cache`]). `None` when Hive does not apply or
/// there is nothing to say. Never waits for Hive.
pub fn briefing(project_root: &Path) -> Option<String> {
    let c = connected(project_root, HiveRight::TasksRead).ok()?;
    let mut lines: Vec<String> = {
        let link = c.link();
        let now = link.now();
        claims::all(link.data_dir)
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
            .collect()
    };
    lines.extend(cache::open_tasks(c));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
