// SPDX-License-Identifier: Apache-2.0
//! Worker mode (Cloud plan v3 L1, owner decision O1): the person's own agent
//! pulls tasks through MCP; the engine never starts an agent by itself.
//!
//! Every claim and every result passes this gate first:
//!
//! - The engine's opt-in lives on the server (`GET /v1/me/optin`): which
//!   vaults its agents may work on, whether it is active, and the daily
//!   budget. It is read fresh for every claim; when it cannot be read,
//!   nothing is claimed (fail closed).
//! - The daily budget has three limits and the first one reached stops the
//!   work: tasks per day (checked before a claim), and optionally tokens and
//!   USD per day (counted from the usage each result reports; the result
//!   that crosses a limit still goes through, the next claim does not).
//! - `[cloud.worker]` may only tighten these limits on this machine, never
//!   widen them.
//! - `cloud worker pause` stops claiming here until `resume`, and held
//!   claims are no longer renewed (they end at their expiry); `cloud worker
//!   stop` also switches the opt-in off on the server, which ends every claim
//!   of this engine at once. The owner turns it on again on the web, or the
//!   person at this computer in LeanCTX Code signing with its own opt-in key
//!   (rc4-16, `local_read::pickup_part`).
//!
//! Cloud enforces the same rules (C0 1.9 section 4: 403 `optin_scope` /
//! `vetoed`, 409 `optin_inactive` / `optin_changed`, 429 `daily_budget`);
//! this gate makes a refusal local and content-free instead of a request
//! that would leak which task was attempted.

use std::fmt;

use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};

use super::CloudError;
use super::auth::Session;
use super::state::{CloudStore, write_private};
use super::transport::CloudClient;
use lean_ctx_protocol::cloud::optin::{ENGINE_OPTIN_STOP_SCHEMA, EngineOptinStopV1, EngineOptinV1};

/// The engine's own opt-in record (engine auth, own instance only).
pub const OPTIN_PATH: &str = "/me/optin";
/// The provider-terms note every worker opt-in shows (plan X1; the web
/// opt-in form carries the same text). LeanCTX never handles subscription
/// credentials: the agent is one the person started and signed in to.
pub const PROVIDER_TERMS_NOTE: &str = "\
You use your own subscription; follow your provider's terms. LeanCTX never sees,
stores or routes your provider login: your agent runs in a session you started
and signed in to yourself. Not allowed by the providers: OpenCode or Pi signed in
with a Claude Pro or Max login, and third-party tools using a Gemini CLI login
(OAuth). Use the provider's own agent or an API key instead.";

const STATE_FILE: &str = "worker.json";
const LOCK_FILE: &str = "worker.lock";

pub use lean_ctx::core::config::cloud::CloudWorkerConfig;

/// What a day has used on this machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub tasks: u64,
    pub tokens: u64,
    pub usd_micros: u64,
}

/// The engine's opt-in (C0 1.9 section 4).
pub type Optin = EngineOptinV1;

/// The local worker state: pause flag and what this machine used today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerState {
    pub paused: bool,
    /// The person turned task pickup off (rc4-16). Durable: `cloud worker
    /// resume` lifts only `paused`, never this; only a turn-on LeanCTX Code
    /// signed with its opt-in key clears it ([`clear_off`]), and so does an
    /// opt-in the person turned on on the web ([`reconcile`]).
    pub explicit_off: bool,
    /// The opt-in id that was current when this computer turned task pickup
    /// off; empty when none was active then, `None` when it is not known
    /// yet. The id rotates on every stop and turn-on, so an active opt-in
    /// with another id was turned on after this off, on the web.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_optin_id: Option<String>,
    /// Set by enrollment only: a first turn-on may be preselected
    /// (`suggest_on`). A state written again after its file went missing
    /// lacks it, so deleting the file never brings the suggestion back.
    pub enrolled_fresh: bool,
    /// UTC day the counters belong to (`YYYY-MM-DD`).
    pub day: String,
    pub used: Usage,
}

impl WorkerState {
    fn today(mut self, day: &str) -> Self {
        if self.day != day {
            day.clone_into(&mut self.day);
            self.used = Usage::default();
        }
        self
    }
}

/// The task a claim or result is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScope {
    pub task_id: String,
    /// The Cloud project (repo) id.
    pub vault_id: String,
}

/// What a result reports it cost, for today's budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidenceSummary {
    pub usage_tokens: u64,
    pub usage_usd_micros: u64,
}

/// What is left of today's budget; `None` is no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLeft {
    pub tasks: Option<u64>,
    pub tokens: Option<u64>,
    pub usd_micros: Option<u64>,
}

/// An admitted claim: the opt-in it works under and what is left after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub optin_id: Option<String>,
    pub budget_left: BudgetLeft,
}

/// The limit a budget refusal hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Tasks,
    Tokens,
    UsdMicros,
}

/// Why the gate refused; its text never names the task or its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This engine has no opt-in, or it does not cover the task.
    NotOptedIn,
    /// The opt-in was stopped; only the owner turns it on again.
    Stopped,
    /// `cloud worker pause` is in effect on this machine.
    Paused,
    BudgetExhausted(Limit),
    /// The opt-in could not be read; nothing is claimed without it.
    Unavailable(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOptedIn => f.write_str(
                "worker mode is not enabled for this task (opt in for its vault on the web)",
            ),
            Self::Stopped => {
                f.write_str("worker mode was stopped for this computer (the owner turns it on again on the web or in LeanCTX Code)")
            }
            Self::Paused => f.write_str(
                "worker mode is paused on this machine; the person resumes it with `lean-ctx cloud worker resume` in their own terminal",
            ),
            Self::BudgetExhausted(limit) => write!(
                f,
                "today's worker budget is used up ({})",
                match limit {
                    Limit::Tasks => "tasks",
                    Limit::Tokens => "tokens",
                    Limit::UsdMicros => "USD",
                }
            ),
            Self::Unavailable(why) => write!(f, "the worker opt-in cannot be read ({why})"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A refusal reaches the caller with its own (content-free) words.
impl From<Refusal> for CloudError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal.to_string())
    }
}

/// Repo ids compare without their wire prefix (`prj_`), dashes and case:
/// Cloud writes a project id as 32 hex digits and an opt-in repo as a dashed
/// UUID.
fn same_id(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let bare = match s.split_once('_') {
            Some((prefix, rest))
                if !prefix.is_empty()
                    && prefix.len() <= 8
                    && prefix.bytes().all(|b| b.is_ascii_lowercase()) =>
            {
                rest
            }
            _ => s,
        };
        bare.chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    !a.is_empty() && norm(a) == norm(b)
}

fn left(limit: Option<u64>, local: Option<u64>, used: u64) -> Option<u64> {
    let limit = match (limit, local) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    limit.map(|l| l.saturating_sub(used))
}

/// The claim decision on a read opt-in and the local state (no I/O).
pub fn decide_claim(
    optin: &Optin,
    local: &CloudWorkerConfig,
    state: &WorkerState,
    scope: &TaskScope,
) -> Result<Admission, Refusal> {
    if state.paused {
        return Err(Refusal::Paused);
    }
    if state.explicit_off {
        return Err(Refusal::Stopped);
    }
    if optin.optin_id.is_none() {
        return Err(Refusal::NotOptedIn);
    }
    if !optin.active {
        return Err(Refusal::Stopped);
    }
    let budget = optin.daily_budget;
    if !optin.vaults.iter().any(|r| same_id(r, &scope.vault_id)) {
        return Err(Refusal::NotOptedIn);
    }
    // The server counts the engine, this machine counts itself: the higher
    // of the two is today's use.
    let used = Usage {
        tasks: u64::from(optin.used_today.tasks).max(state.used.tasks),
        tokens: optin.used_today.tokens.max(state.used.tokens),
        usd_micros: optin.used_today.usd_micros.max(state.used.usd_micros),
    };
    // Cloud task #19: no task limit on the server and none here is none.
    let tasks = left(
        budget.tasks.map(u64::from),
        local.max_tasks_per_day.map(u64::from),
        used.tasks,
    );
    let tokens = left(budget.tokens, local.max_tokens_per_day, used.tokens);
    let usd_micros = left(
        budget.usd_micros,
        local.max_usd_micros_per_day,
        used.usd_micros,
    );
    for (rest, limit) in [
        (tasks, Limit::Tasks),
        (tokens, Limit::Tokens),
        (usd_micros, Limit::UsdMicros),
    ] {
        if rest == Some(0) {
            return Err(Refusal::BudgetExhausted(limit));
        }
    }
    Ok(Admission {
        optin_id: optin.optin_id.clone(),
        budget_left: BudgetLeft {
            // The claim this admits counts one.
            tasks: tasks.map(|t| t - 1),
            tokens,
            usd_micros,
        },
    })
}

fn day() -> String {
    UtcDateTime::now().format("%Y-%m-%d").to_string()
}

/// Read-modify-write the worker state under its lock.
fn with_state<T>(
    store: &CloudStore,
    change: impl FnOnce(&mut WorkerState) -> T,
) -> Result<T, CloudError> {
    let dir = store.data_dir();
    std::fs::create_dir_all(dir).map_err(|e| CloudError::Local(format!("worker: {e}")))?;
    let _lock = lean_ctx::core::agents::FileLock::acquire(&dir.join(LOCK_FILE))
        .map_err(|e| CloudError::Local(format!("worker lock: {e}")))?;
    let mut state = read_state(store).today(&day());
    let out = change(&mut state);
    let bytes = serde_json::to_vec(&state).map_err(|e| CloudError::Local(e.to_string()))?;
    write_private(dir, STATE_FILE, &bytes)?;
    Ok(out)
}

/// Write the state before the engine enrolls, so that a missing one after
/// enrollment can only mean it was deleted. A state already there, paused
/// or not, stays as it is; the web opt-in still gates every claim.
pub(crate) fn prepare_enrollment(store: &CloudStore) -> Result<(), CloudError> {
    with_state(store, |state| state.enrolled_fresh = true)
}

/// The local state as stored (today's counters reset on a new day). Fail
/// closed: a file that cannot be read or parsed counts as paused, and so
/// does a missing one on an engine enrolled with Cloud — deleting it never
/// lifts a pause or resets today's counts. Only enrollment
/// (`prepare_enrollment`) and `cloud worker resume`, both of which the
/// person runs (an agent shell refuses them), write it unpaused.
pub fn read_state(store: &CloudStore) -> WorkerState {
    let paused = || WorkerState {
        paused: true,
        ..WorkerState::default()
    };
    // A damaged or unreadable file also counts as an explicit off: only the
    // app's signed turn-on lifts it. A missing file does not (rc4-16): a
    // state never written (a fresh or older engine) cannot be told apart
    // from a deleted one, and an update must not stop a worker the Cloud
    // has on.
    let damaged = || WorkerState {
        explicit_off: true,
        ..paused()
    };
    let state = match std::fs::read(store.data_dir().join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<WorkerState>(&bytes).unwrap_or_else(|_| damaged()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match store.instance() {
            Ok(None) => WorkerState::default(),
            Ok(Some(_)) | Err(_) => paused(),
        },
        Err(_) => damaged(),
    };
    state.today(&day())
}

/// The server's opt-in for this engine.
pub fn fetch_optin(session: &Session) -> Result<Optin, CloudError> {
    parse_optin(session.with_client(|c| c.get(OPTIN_PATH))?)
}

/// The opt-in through `client` (a bounded client of a tool call).
pub fn fetch_optin_via(client: &CloudClient) -> Result<Optin, CloudError> {
    parse_optin(client.get(OPTIN_PATH)?)
}

/// An opt-in as Cloud answers it, checked.
pub fn parse_optin(body: serde_json::Value) -> Result<Optin, CloudError> {
    let optin: Optin =
        serde_json::from_value(body).map_err(|e| CloudError::Protocol(format!("opt-in: {e}")))?;
    optin
        .validate()
        .map_err(|e| CloudError::Protocol(format!("opt-in: {e}")))?;
    Ok(optin)
}

/// Admit a claim: whether this machine may ask Cloud for it. Nothing is
/// counted yet; [`count_claim`] counts it once Cloud granted it, so a claim
/// Cloud refuses costs no task of today's budget.
pub fn admit_claim(
    session: &Session,
    local: &CloudWorkerConfig,
    scope: &TaskScope,
) -> Result<Admission, Refusal> {
    let optin = fetch_optin(session).map_err(|e| Refusal::Unavailable(e.to_string()))?;
    admit_claim_for(&session.store, local, scope, &optin)
}

/// [`admit_claim`] on an opt-in the caller just read.
pub fn admit_claim_for(
    store: &CloudStore,
    local: &CloudWorkerConfig,
    scope: &TaskScope,
    optin: &Optin,
) -> Result<Admission, Refusal> {
    with_state(store, |state| {
        reconcile(state, optin);
        decide_claim(optin, local, state, scope)
    })
    .map_err(|e| Refusal::Unavailable(e.to_string()))?
}

/// Bring the kept off in line with the Cloud's opt-in (pure; rc4-16 M3):
/// an active opt-in with another id than the one this computer stopped
/// was turned on by the person on the web, and lifts the off and the
/// pause the stop set. The same id keeps the off. An off whose id is not
/// known yet (the stop did not reach the Cloud, or the file was damaged)
/// takes the id seen now as the stopped one: it stays off. Returns whether
/// `state` changed.
pub fn reconcile(state: &mut WorkerState, optin: &Optin) -> bool {
    if !state.explicit_off {
        return false;
    }
    let Some(id) = optin.optin_id.as_deref() else {
        return false;
    };
    match state.stopped_optin_id.as_deref() {
        Some(stopped) if stopped == id => false,
        Some(_) if optin.active => {
            state.paused = false;
            state.explicit_off = false;
            state.stopped_optin_id = None;
            true
        }
        // Not known yet, or a later stop (on the web): this one stays off.
        _ => {
            state.stopped_optin_id = Some(id.to_owned());
            true
        }
    }
}

/// The local state after [`reconcile`] with `optin` the Cloud just
/// answered: written only when it changed.
pub fn observe(store: &CloudStore, optin: &Optin) -> Result<WorkerState, CloudError> {
    let state = read_state(store);
    if !reconcile(&mut state.clone(), optin) {
        return Ok(state);
    }
    with_state(store, |state| {
        reconcile(state, optin);
        state.clone()
    })
}

/// Count a claim Cloud granted (or a verification commit it took) against
/// today's tasks on this machine. A refused claim is never counted: it
/// used to be, and refusals then ate the local budget until this machine
/// refused on its own.
pub fn count_claim(store: &CloudStore) -> Result<(), CloudError> {
    with_state(store, |state| state.used.tasks += 1)
}

/// Add the usage a result reports to today's.
pub fn admit_submit(store: &CloudStore, evidence: &EvidenceSummary) -> Result<(), Refusal> {
    with_state(store, |state| {
        state.used.tokens = state.used.tokens.saturating_add(evidence.usage_tokens);
        state.used.usd_micros = state
            .used
            .usd_micros
            .saturating_add(evidence.usage_usd_micros);
    })
    .map_err(|e| Refusal::Unavailable(e.to_string()))
}

/// Stop or resume claiming on this machine. Resuming never lifts an
/// explicit off.
pub fn set_paused(store: &CloudStore, paused: bool) -> Result<(), CloudError> {
    with_state(store, |state| state.paused = paused)
}

/// Lift the pause and the explicit off: only after the Cloud took a turn-on
/// LeanCTX Code signed with its opt-in key (rc4-16).
pub fn clear_off(store: &CloudStore) -> Result<(), CloudError> {
    with_state(store, |state| {
        state.paused = false;
        state.explicit_off = false;
        state.stopped_optin_id = None;
    })
}

/// A turn-on the Cloud did not take: its status and answer.
pub type TurnOnRefused = (u16, serde_json::Value);

/// Send the app-signed turn-on `signed` (the caller checked its
/// signature) unchanged. Only a 2xx answer carrying a valid opt-in lifts
/// the off and the pause here ([`clear_off`]); any other answer, or none,
/// leaves them as they were. A refusal comes back with its status and
/// answer for the caller's words.
pub fn send_turn_on(
    session: &Session,
    signed: &serde_json::Value,
) -> Result<Result<Optin, TurnOnRefused>, CloudError> {
    let (status, answer) = session.with_client(|client| {
        let (status, answer) = client.send_answer(
            "PUT",
            OPTIN_PATH,
            Some(signed),
            lean_ctx_protocol::cloud::MAX_BODY_BYTES,
        )?;
        if status == 401 {
            return Err(CloudError::Unauthorized);
        }
        Ok((status, answer))
    })?;
    if !(200..300).contains(&status) {
        return Ok(Err((status, answer)));
    }
    let optin = parse_optin(answer)?;
    // The person's signed click lifts the off kept here.
    clear_off(&session.store)?;
    Ok(Ok(optin))
}

/// Stop worker mode for this engine: pause here and switch the opt-in off
/// on the server, which ends every claim of this engine at once. The off
/// is kept here first; the stopped opt-in's id is kept with it
/// ([`reconcile`]). A Cloud without an active opt-in (409
/// `optin_inactive`) is already off: that is a success, kept with an
/// empty id, so any opt-in turned on later lifts the off.
pub fn stop(session: &Session) -> Result<(), CloudError> {
    with_state(&session.store, |state| {
        state.paused = true;
        state.explicit_off = true;
    })?;
    let stop = serde_json::to_value(EngineOptinStopV1 {
        schema: ENGINE_OPTIN_STOP_SCHEMA.into(),
        active: false,
    })
    .map_err(|e| CloudError::Local(e.to_string()))?;
    let answer = session.with_client(|c| {
        c.send(
            "PUT",
            OPTIN_PATH,
            Some(&stop),
            &[],
            lean_ctx_protocol::cloud::MAX_BODY_BYTES,
        )
        .map(|r| r.body)
    });
    let stopped_id = match answer {
        Ok(body) => parse_optin(body)?.optin_id.unwrap_or_default(),
        Err(CloudError::Task {
            refusal: crate::TaskRefusal::OptinInactive,
            ..
        }) => String::new(),
        Err(e) => return Err(e),
    };
    note_stopped(&session.store, stopped_id)
}

/// Keep `id` as the opt-in this computer stopped, while the off holds.
fn note_stopped(store: &CloudStore, id: String) -> Result<(), CloudError> {
    with_state(store, |state| {
        if state.explicit_off {
            state.stopped_optin_id = Some(id);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::cloud::optin::ENGINE_OPTIN_SCHEMA;
    use serde_json::Value;
    use serde_json::json;

    const REPO: &str = "0b7c3c1e5d2a4f7e9a312c4d5e6f7a8b";

    fn optin() -> Optin {
        parse_optin(json!({
            "schema": ENGINE_OPTIN_SCHEMA,
            "optin_id": "opt_1",
            "active": true,
            "vaults": [REPO],
            "networks": [],
            "topics": [],
            "daily_budget": {"tasks": 2, "tokens": 1000},
            "used_today": {"day": "2026-10-22", "tasks": 0, "tokens": 0, "usd_micros": 0},
            "updated_at": "2026-10-22T10:00:00Z"
        }))
        .unwrap()
    }

    fn org() -> TaskScope {
        TaskScope {
            task_id: "7".into(),
            vault_id: REPO.into(),
        }
    }

    #[test]
    fn a_claim_needs_an_active_opt_in_that_covers_the_task() {
        let local = CloudWorkerConfig::default();
        let state = WorkerState::default();
        let admitted = decide_claim(&optin(), &local, &state, &org()).unwrap();
        assert_eq!(admitted.optin_id.as_deref(), Some("opt_1"));
        assert_eq!(admitted.budget_left.tasks, Some(1));
        assert_eq!(admitted.budget_left.tokens, Some(1000));
        assert_eq!(admitted.budget_left.usd_micros, None);

        let other = TaskScope {
            vault_id: "ffffffffffffffffffffffffffffffff".into(),
            ..org()
        };
        assert_eq!(
            decide_claim(&optin(), &local, &state, &other),
            Err(Refusal::NotOptedIn)
        );
        let none = parse_optin(json!({
            "schema": ENGINE_OPTIN_SCHEMA, "active": false, "vaults": [], "networks": [], "topics": [],
            "daily_budget": {"tasks": 5},
            "used_today": {"day": "2026-10-22", "tasks": 0, "tokens": 0, "usd_micros": 0}
        }))
        .unwrap();
        assert_eq!(
            decide_claim(&none, &local, &state, &org()),
            Err(Refusal::NotOptedIn)
        );
        let stopped = Optin {
            active: false,
            ..optin()
        };
        assert_eq!(
            decide_claim(&stopped, &local, &state, &org()),
            Err(Refusal::Stopped)
        );
        let paused = WorkerState {
            paused: true,
            ..WorkerState::default()
        };
        assert_eq!(
            decide_claim(&optin(), &local, &paused, &org()),
            Err(Refusal::Paused)
        );
    }

    /// Cloud task #19: an opt-in without a task limit (C0 §4 erratum) admits
    /// on count however many tasks the day has seen; a local cap still
    /// applies.
    #[test]
    fn no_task_limit_admits_on_count_and_a_local_cap_still_holds() {
        let mut free = optin();
        free.daily_budget.tasks = None;
        free.daily_budget.tokens = None;
        free.used_today.tasks = 140;
        let state = WorkerState::default();
        let admitted = decide_claim(&free, &CloudWorkerConfig::default(), &state, &org()).unwrap();
        assert_eq!(admitted.budget_left.tasks, None, "no limit stays no limit");
        let capped = CloudWorkerConfig {
            max_tasks_per_day: Some(150),
            ..CloudWorkerConfig::default()
        };
        assert_eq!(
            decide_claim(&free, &capped, &state, &org())
                .unwrap()
                .budget_left
                .tasks,
            Some(9)
        );
        free.used_today.tasks = 150;
        assert_eq!(
            decide_claim(&free, &capped, &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
        // As Cloud sends it: no `tasks` field at all.
        let wire = parse_optin(json!({
            "schema": ENGINE_OPTIN_SCHEMA, "optin_id": "opt_2", "active": true,
            "vaults": [REPO], "networks": [], "topics": [], "daily_budget": {},
            "used_today": {"day": "2026-10-22", "tasks": 140, "tokens": 0, "usd_micros": 0}
        }))
        .unwrap();
        assert!(decide_claim(&wire, &CloudWorkerConfig::default(), &state, &org()).is_ok());
    }

    #[test]
    fn the_first_limit_reached_stops_and_local_caps_only_tighten() {
        let state = WorkerState::default();
        let mut spent = optin();
        spent.used_today.tasks = 2;
        assert_eq!(
            decide_claim(&spent, &CloudWorkerConfig::default(), &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
        let mut tokens = optin();
        tokens.used_today.tokens = 1200;
        assert_eq!(
            decide_claim(&tokens, &CloudWorkerConfig::default(), &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tokens))
        );

        // A wider local cap does not widen the server's budget.
        let wide = CloudWorkerConfig {
            max_tasks_per_day: Some(50),
            max_tokens_per_day: Some(1_000_000),
            max_usd_micros_per_day: None,
        };
        assert_eq!(
            decide_claim(&spent, &wide, &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
        // A tighter one applies, also to a limit the server leaves open.
        let tight = CloudWorkerConfig {
            max_tasks_per_day: Some(1),
            max_tokens_per_day: None,
            max_usd_micros_per_day: Some(500),
        };
        let one_done = WorkerState {
            used: Usage {
                tasks: 1,
                ..Usage::default()
            },
            ..WorkerState::default()
        };
        assert_eq!(
            decide_claim(&optin(), &tight, &one_done, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks)),
            "the machine's own count holds when the server's is behind"
        );
        let admitted = decide_claim(&optin(), &tight, &state, &org()).unwrap();
        assert_eq!(admitted.budget_left.usd_micros, Some(500));
        let usd_spent = WorkerState {
            used: Usage {
                usd_micros: 500,
                ..Usage::default()
            },
            ..WorkerState::default()
        };
        assert_eq!(
            decide_claim(&optin(), &tight, &usd_spent, &org()),
            Err(Refusal::BudgetExhausted(Limit::UsdMicros))
        );
    }

    #[test]
    fn opt_ins_follow_the_section_4_contract() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../crates/lean-ctx-protocol/tests/fixtures/cloud/instance_optin.response.json"
        ))
        .unwrap();
        let optin = parse_optin(fixture.clone()).unwrap();
        assert!(optin.active && optin.optin_id.is_some());

        let mut wrong = fixture.clone();
        wrong["schema"] = json!("leanctx.cloud.instance-optin/v2");
        assert!(matches!(parse_optin(wrong), Err(CloudError::Protocol(_))));
        // Active without an opt-in id breaks the contract: fail closed.
        let mut broken = fixture;
        broken.as_object_mut().unwrap().remove("optin_id");
        assert!(matches!(parse_optin(broken), Err(CloudError::Protocol(_))));

        let stop = serde_json::to_value(EngineOptinStopV1 {
            schema: ENGINE_OPTIN_STOP_SCHEMA.into(),
            active: false,
        })
        .unwrap();
        assert_eq!(
            stop,
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../crates/lean-ctx-protocol/tests/fixtures/cloud/instance_optin_stop.request.json"
            ))
            .unwrap()
        );
    }

    #[test]
    fn refusals_name_no_task_and_reach_the_caller_in_their_own_words() {
        for refusal in [
            Refusal::NotOptedIn,
            Refusal::Stopped,
            Refusal::Paused,
            Refusal::BudgetExhausted(Limit::Tokens),
        ] {
            let text = refusal.to_string();
            assert!(!text.contains(REPO), "{text}");
            assert_eq!(CloudError::from(refusal).to_string(), text);
        }
    }

    #[test]
    fn ids_match_across_wire_prefix_dashes_and_case() {
        let uuid = "558f6ef0-1c2d-4e5f-8a9b-0c1d2e3f4a5b";
        assert!(same_id(uuid, "558f6ef01c2d4e5f8a9b0c1d2e3f4a5b"));
        assert!(same_id("prj_558F6EF01C2D4E5F8A9B0C1D2E3F4A5B", uuid));
        assert!(!same_id(uuid, "prj_658f6ef01c2d4e5f8a9b0c1d2e3f4a5b"));
        assert!(!same_id("", ""));
    }

    /// c9's finding: the gate counted a claim before Cloud answered, so
    /// refused claims ate today's budget until this machine refused on its
    /// own. Now only a granted claim counts.
    #[test]
    fn only_a_granted_claim_counts_against_the_local_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        prepare_enrollment(&store).unwrap();
        let cap = CloudWorkerConfig {
            max_tasks_per_day: Some(2),
            ..CloudWorkerConfig::default()
        };
        // Cloud refuses every one of these (say `task_claimed`): the gate
        // admitted them, nothing was granted, nothing counts.
        for _ in 0..5 {
            assert!(admit_claim_for(&store, &cap, &org(), &optin()).is_ok());
        }
        assert_eq!(read_state(&store).used.tasks, 0);
        // Two granted claims use the local cap of two.
        count_claim(&store).unwrap();
        count_claim(&store).unwrap();
        assert_eq!(read_state(&store).used.tasks, 2);
        assert_eq!(
            admit_claim_for(&store, &cap, &org(), &optin()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
    }

    #[test]
    fn an_agent_deleting_the_state_file_leaves_the_worker_paused() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        // Enrollment writes the state first: a fresh engine is not paused.
        prepare_enrollment(&store).unwrap();
        enroll(&store);
        assert!(!read_state(&store).paused, "a fresh enrollment runs");
        set_paused(&store, true).unwrap();
        prepare_enrollment(&store).unwrap();
        assert!(read_state(&store).paused, "enrolling again keeps the pause");

        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        enroll(&store);
        assert!(
            read_state(&store).paused,
            "an enrolled engine without its state counts as paused"
        );
        set_paused(&store, false).unwrap();
        assert!(!read_state(&store).paused, "the person resumed it");
        set_paused(&store, true).unwrap();
        std::fs::remove_file(store.data_dir().join(STATE_FILE)).unwrap();
        assert!(
            read_state(&store).paused,
            "a deleted file never lifts the pause"
        );
    }

    fn enroll(store: &CloudStore) {
        store
            .write_config(
                super::super::state::INSTANCE_FILE,
                &super::super::state::InstanceRecord {
                    instance_id: "inst_1".into(),
                    org_id: "org_1".into(),
                    org_name: "Acme".into(),
                    label: "laptop".into(),
                    url: "https://cloud.example.com".into(),
                    org_salt_id: "salt_1".into(),
                    enrolled_at: "2026-10-22T12:00:00Z".into(),
                    web_url: None,
                    org_kind: None,
                    company_name: None,
                },
            )
            .unwrap();
    }

    #[test]
    fn pickup_resume_never_lifts_an_explicit_off() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        prepare_enrollment(&store).unwrap();
        enroll(&store);
        // The person turned task pickup off (what `stop` writes first).
        with_state(&store, |state| {
            state.paused = true;
            state.explicit_off = true;
        })
        .unwrap();
        // `cloud worker resume` (an agent can run it) lifts only the pause.
        set_paused(&store, false).unwrap();
        let state = read_state(&store);
        assert!(!state.paused && state.explicit_off, "the off stays");
        assert_eq!(
            decide_claim(&optin(), &CloudWorkerConfig::default(), &state, &org()),
            Err(Refusal::Stopped)
        );
        // A damaged file fails closed on the off too.
        write_private(store.data_dir(), STATE_FILE, b"{not json").unwrap();
        set_paused(&store, false).unwrap();
        assert!(read_state(&store).explicit_off);
        // Only the app's signed turn-on lifts it.
        clear_off(&store).unwrap();
        let state = read_state(&store);
        assert!(!state.paused && !state.explicit_off);
    }

    #[test]
    fn pickup_an_update_without_a_state_file_keeps_an_active_worker_on() {
        let local = CloudWorkerConfig::default();
        // An engine that never wrote the state (fresh, or older than it):
        // with an active opt-in on the Cloud it claims as before.
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let state = read_state(&store);
        assert!(!state.explicit_off);
        assert!(decide_claim(&optin(), &local, &state, &org()).is_ok());
        // A state written before `explicit_off` existed reads as not off.
        write_private(
            store.data_dir(),
            STATE_FILE,
            br#"{"paused":false,"day":"2000-01-01","used":{}}"#,
        )
        .unwrap();
        let state = read_state(&store);
        assert!(!state.paused && !state.explicit_off);
        assert!(decide_claim(&optin(), &local, &state, &org()).is_ok());
        // A missing file never adds an explicit off on an enrolled engine.
        std::fs::remove_file(store.data_dir().join(STATE_FILE)).unwrap();
        enroll(&store);
        assert!(!read_state(&store).explicit_off);
    }

    #[test]
    fn a_damaged_state_file_counts_as_paused() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        assert!(!read_state(&store).paused, "no file yet: never paused");
        write_private(store.data_dir(), STATE_FILE, b"{not json").unwrap();
        assert!(read_state(&store).paused, "a broken file fails closed");
        // Lifting it takes an explicit resume, which writes a sound file.
        set_paused(&store, false).unwrap();
        assert!(!read_state(&store).paused);
    }

    #[test]
    fn usage_counts_per_day_and_pause_survives_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        admit_submit(
            &store,
            &EvidenceSummary {
                usage_tokens: 300,
                usage_usd_micros: 40,
            },
        )
        .unwrap();
        set_paused(&store, true).unwrap();
        let state = read_state(&store);
        assert!(state.paused);
        assert_eq!(state.used.tokens, 300);
        assert_eq!(state.used.usd_micros, 40);

        // A new day starts with empty counters; the pause stays.
        let mut stale = state.clone();
        stale.day = "2000-01-01".into();
        write_private(
            store.data_dir(),
            STATE_FILE,
            &serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();
        let state = read_state(&store);
        assert!(state.paused);
        assert_eq!(state.used, Usage::default());
        set_paused(&store, false).unwrap();
        assert!(!read_state(&store).paused);
    }

    fn optin_as(id: Option<&str>, active: bool) -> Optin {
        let mut optin = optin();
        optin.optin_id = id.map(str::to_owned);
        optin.active = active;
        optin
    }

    fn kept_off(stopped: Option<&str>) -> WorkerState {
        WorkerState {
            paused: true,
            explicit_off: true,
            stopped_optin_id: stopped.map(str::to_owned),
            ..WorkerState::default()
        }
    }

    #[test]
    fn pickup_a_turn_on_on_the_web_lifts_the_kept_off_and_the_same_id_keeps_it() {
        // The same id as the stopped one keeps the off, active or not.
        for active in [false, true] {
            let mut state = kept_off(Some("opt_2"));
            assert!(!reconcile(&mut state, &optin_as(Some("opt_2"), active)));
            assert_eq!(state, kept_off(Some("opt_2")));
        }
        // A new active id: the person turned it on on the web.
        let mut state = kept_off(Some("opt_2"));
        assert!(reconcile(&mut state, &optin_as(Some("opt_3"), true)));
        assert!(!state.paused && !state.explicit_off && state.stopped_optin_id.is_none());
        let scope = org();
        assert!(decide_claim(&optin(), &CloudWorkerConfig::default(), &state, &scope).is_ok());
        // Stopped without an active opt-in (empty id): any later one is new.
        let mut state = kept_off(Some(""));
        assert!(reconcile(&mut state, &optin_as(Some("opt_1"), true)));
        assert!(!state.explicit_off);
        // A new stopped id (stopped on the web too) is kept, still off.
        let mut state = kept_off(Some("opt_2"));
        assert!(reconcile(&mut state, &optin_as(Some("opt_4"), false)));
        assert_eq!(state, kept_off(Some("opt_4")));
        // Not known yet (the stop did not reach the Cloud, or a damaged
        // state): the id read now counts as the stopped one.
        let mut state = kept_off(None);
        assert!(reconcile(&mut state, &optin_as(Some("opt_1"), true)));
        assert_eq!(state, kept_off(Some("opt_1")));
        assert!(!reconcile(&mut state, &optin_as(Some("opt_1"), true)));
        // No opt-in row, or no off: nothing changes.
        let mut state = kept_off(Some("opt_2"));
        assert!(!reconcile(&mut state, &optin_as(None, false)));
        let mut on = WorkerState::default();
        assert!(!reconcile(&mut on, &optin_as(Some("opt_9"), true)));
        assert_eq!(on, WorkerState::default());
    }

    #[test]
    fn pickup_only_enrollment_marks_a_state_that_may_suggest_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        prepare_enrollment(&store).unwrap();
        enroll(&store);
        assert!(read_state(&store).enrolled_fresh);
        // Deleted, then `cloud worker resume`: written again without it.
        std::fs::remove_file(store.data_dir().join(STATE_FILE)).unwrap();
        set_paused(&store, false).unwrap();
        let state = read_state(&store);
        assert!(!state.paused && !state.explicit_off && !state.enrolled_fresh);
    }

    type Script = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(u16, Value)>>>;
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

    /// A Cloud on 127.0.0.1 that mints tokens and answers every other
    /// request with the next of `answers`; those requests are recorded as
    /// method and path, with their body.
    fn cloud(answers: Vec<(u16, Value)>) -> (String, Seen) {
        use std::io::{BufRead as _, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let script: Script = std::sync::Arc::new(std::sync::Mutex::new(answers.into()));
        let seen = Seen::default();
        let recorded = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (script, recorded) = (
                    std::sync::Arc::clone(&script),
                    std::sync::Arc::clone(&recorded),
                );
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = std::io::BufReader::new(stream);
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut words = line.split_whitespace();
                        let method = words.next().unwrap_or_default().to_owned();
                        let path = words.next().unwrap_or_default().to_owned();
                        let mut length = 0usize;
                        loop {
                            let mut header = String::new();
                            reader.read_line(&mut header).unwrap();
                            let header = header.trim_end();
                            if header.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = header.split_once(':')
                                && name.eq_ignore_ascii_case("content-length")
                            {
                                length = value.trim().parse().unwrap();
                            }
                        }
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).unwrap();
                        let (code, reply) = if path.ends_with("/token") {
                            let token = json!({"schema": "leanctx.cloud.access-token/v1",
                                "access_token": "test-access", "expires_in": 3600});
                            (200, token)
                        } else {
                            let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
                            recorded
                                .lock()
                                .unwrap()
                                .push((format!("{method} {path}"), body));
                            script.lock().unwrap().pop_front().unwrap_or((
                                500,
                                json!({"error": {"code": "internal", "message": "x"}}),
                            ))
                        };
                        let text = reply.to_string();
                        let _ = write!(
                            writer,
                            "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
                            text.len()
                        );
                    }
                });
            }
        });
        (origin, seen)
    }

    fn optin_json(id: &str, active: bool) -> Value {
        let mut optin = serde_json::to_value(optin()).unwrap();
        optin["optin_id"] = json!(id);
        optin["active"] = json!(active);
        // The Cloud sets `stopped_at` exactly while an opt-in is inactive.
        optin["stopped_at"] = if active {
            Value::Null
        } else {
            json!("2026-10-20T12:00:00Z")
        };
        optin
    }

    fn refusal(reason: &str) -> Value {
        json!({"error": {"code": "invalid_state", "message": "x", "reason": reason}})
    }

    /// rc4-16 S2: turn off, failed turn-ons, the signed turn-on, a stop
    /// without an opt-in, then a turn-on on the web, against a Cloud.
    #[test]
    fn pickup_turn_on_and_off_against_a_cloud_lift_the_off_only_after_a_2xx() {
        let signed = json!({"schema": "leanctx.cloud.instance-optin-on/v1", "active": true,
            "prev_optin_id": "opt_2", "vaults": [REPO], "daily_budget": {"tasks": 2},
            "presence_signature": "sig"});
        let (origin, seen) = cloud(vec![
            (200, optin_json("opt_2", false)), // stop
            (409, refusal("optin_changed")),   // turn-on refused
            (
                403,
                json!({"error": {"code": "forbidden", "message": "x",
                "reason": "person_unproven"}}),
            ), // turn-on refused
            (200, json!({"not": "an opt-in"})), // 2xx without an opt-in
            (200, optin_json("opt_3", true)),  // the signed turn-on
            (409, refusal("optin_inactive")),  // stop, nothing active
        ]);
        let (_dir, session) = crate::auth::fixture_session_at(&origin);
        crate::instance_key::InstanceKeys::load_or_create(&session.store).unwrap();
        let store = &session.store;
        prepare_enrollment(store).unwrap();
        let off = |state: &WorkerState| state.paused && state.explicit_off;

        stop(&session).unwrap();
        let state = read_state(store);
        assert!(off(&state));
        assert_eq!(state.stopped_optin_id.as_deref(), Some("opt_2"));
        // The stopped opt-in read again keeps the off.
        assert!(off(
            &observe(store, &optin_as(Some("opt_2"), false)).unwrap()
        ));

        // Refused and broken answers never lift it.
        for status in [409, 403] {
            let refused = send_turn_on(&session, &signed).unwrap().unwrap_err();
            assert_eq!(refused.0, status);
            assert!(off(&read_state(store)), "{status} keeps the off");
        }
        assert!(send_turn_on(&session, &signed).is_err());
        assert!(off(&read_state(store)), "a 2xx without an opt-in keeps it");

        // The signed turn-on the Cloud took lifts it.
        let on = send_turn_on(&session, &signed).unwrap().unwrap();
        assert_eq!(on.optin_id.as_deref(), Some("opt_3"));
        let state = read_state(store);
        assert!(!state.paused && !state.explicit_off && state.stopped_optin_id.is_none());

        // A stop the Cloud answers `optin_inactive` is a success, kept off
        // with an empty id; a later turn-on on the web lifts it.
        stop(&session).unwrap();
        let state = read_state(store);
        assert!(off(&state));
        assert_eq!(state.stopped_optin_id.as_deref(), Some(""));
        let state = observe(store, &optin_as(Some("opt_5"), true)).unwrap();
        assert!(!state.paused && !state.explicit_off);
        assert_eq!(read_state(store), state);

        let seen = seen.lock().unwrap().clone();
        let lines: Vec<&str> = seen.iter().map(|(line, _)| line.as_str()).collect();
        let put = format!("PUT /v1{OPTIN_PATH}");
        assert_eq!(lines, [put.as_str(); 6]);
        assert_eq!(seen[0].1["active"], false);
        assert_eq!(seen[1].1, signed);
    }
}
