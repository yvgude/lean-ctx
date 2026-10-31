// SPDX-License-Identifier: Apache-2.0
//! Worker mode (Hive plan v3 L1, owner decision O1): the person's own agent
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
//! - `[hive.worker]` may only tighten these limits on this machine, never
//!   widen them.
//! - `hive worker pause` stops claiming here until `resume`, and held
//!   claims are no longer renewed (they end at their expiry); `hive worker
//!   stop` also switches the opt-in off on the server, which ends every claim
//!   of this engine at once. Only the owner turns it on again, on the web.
//!
//! Hive enforces the same rules (C0 1.9 section 4: 403 `optin_scope` /
//! `vetoed`, 409 `optin_inactive` / `optin_changed`, 429 `daily_budget`);
//! this gate makes a refusal local and content-free instead of a request
//! that would leak which task was attempted.

use std::fmt;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::HiveError;
use super::auth::Session;
use super::state::{HiveStore, write_private};
use super::transport::HiveClient;
use lean_ctx_protocol::hive::optin::{ENGINE_OPTIN_STOP_SCHEMA, EngineOptinStopV1, EngineOptinV1};

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

/// `[hive.worker]`: stricter local caps on top of the server's budget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveWorkerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tasks_per_day: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens_per_day: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_usd_micros_per_day: Option<u64>,
}

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
    /// The Hive project (repo) id.
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
    /// `hive worker pause` is in effect on this machine.
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
                f.write_str("worker mode was stopped for this engine (the owner resumes it on the web)")
            }
            Self::Paused => f.write_str(
                "worker mode is paused on this machine; the person resumes it with `lean-ctx hive worker resume` in their own terminal",
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
impl From<Refusal> for HiveError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal.to_string())
    }
}

/// Repo ids compare without their wire prefix (`prj_`), dashes and case:
/// Hive writes a project id as 32 hex digits and an opt-in repo as a dashed
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
    local: &HiveWorkerConfig,
    state: &WorkerState,
    scope: &TaskScope,
) -> Result<Admission, Refusal> {
    if state.paused {
        return Err(Refusal::Paused);
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
    // Hive task #19: no task limit on the server and none here is none.
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
    Utc::now().format("%Y-%m-%d").to_string()
}

/// Read-modify-write the worker state under its lock.
fn with_state<T>(
    store: &HiveStore,
    change: impl FnOnce(&mut WorkerState) -> T,
) -> Result<T, HiveError> {
    let dir = store.data_dir();
    std::fs::create_dir_all(dir).map_err(|e| HiveError::Local(format!("worker: {e}")))?;
    let _lock = crate::core::agents::FileLock::acquire(&dir.join(LOCK_FILE))
        .map_err(|e| HiveError::Local(format!("worker lock: {e}")))?;
    let mut state = read_state(store).today(&day());
    let out = change(&mut state);
    let bytes = serde_json::to_vec(&state).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, STATE_FILE, &bytes)?;
    Ok(out)
}

/// Write the state before the engine enrolls, so that a missing one after
/// enrollment can only mean it was deleted. A state already there, paused
/// or not, stays as it is; the web opt-in still gates every claim.
pub(crate) fn prepare_enrollment(store: &HiveStore) -> Result<(), HiveError> {
    with_state(store, |_| ())
}

/// The local state as stored (today's counters reset on a new day). Fail
/// closed: a file that cannot be read or parsed counts as paused, and so
/// does a missing one on an engine enrolled with Hive — deleting it never
/// lifts a pause or resets today's counts. Only enrollment
/// (`prepare_enrollment`) and `hive worker resume`, both of which the
/// person runs (an agent shell refuses them), write it unpaused.
pub fn read_state(store: &HiveStore) -> WorkerState {
    let paused = || WorkerState {
        paused: true,
        ..WorkerState::default()
    };
    let state = match std::fs::read(store.data_dir().join(STATE_FILE)) {
        Ok(bytes) => serde_json::from_slice::<WorkerState>(&bytes).unwrap_or_else(|_| paused()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match store.instance() {
            Ok(None) => WorkerState::default(),
            Ok(Some(_)) | Err(_) => paused(),
        },
        Err(_) => paused(),
    };
    state.today(&day())
}

/// The server's opt-in for this engine.
pub fn fetch_optin(session: &Session) -> Result<Optin, HiveError> {
    parse_optin(session.with_client(|c| c.get(OPTIN_PATH))?)
}

/// The opt-in through `client` (a bounded client of a tool call).
pub fn fetch_optin_via(client: &HiveClient) -> Result<Optin, HiveError> {
    parse_optin(client.get(OPTIN_PATH)?)
}

fn parse_optin(body: serde_json::Value) -> Result<Optin, HiveError> {
    let optin: Optin =
        serde_json::from_value(body).map_err(|e| HiveError::Protocol(format!("opt-in: {e}")))?;
    optin
        .validate()
        .map_err(|e| HiveError::Protocol(format!("opt-in: {e}")))?;
    Ok(optin)
}

/// Admit a claim: whether this machine may ask Hive for it. Nothing is
/// counted yet; [`count_claim`] counts it once Hive granted it, so a claim
/// Hive refuses costs no task of today's budget.
pub fn admit_claim(
    session: &Session,
    local: &HiveWorkerConfig,
    scope: &TaskScope,
) -> Result<Admission, Refusal> {
    let optin = fetch_optin(session).map_err(|e| Refusal::Unavailable(e.to_string()))?;
    admit_claim_for(&session.store, local, scope, &optin)
}

/// [`admit_claim`] on an opt-in the caller just read.
pub fn admit_claim_for(
    store: &HiveStore,
    local: &HiveWorkerConfig,
    scope: &TaskScope,
    optin: &Optin,
) -> Result<Admission, Refusal> {
    with_state(store, |state| decide_claim(optin, local, state, scope))
        .map_err(|e| Refusal::Unavailable(e.to_string()))?
}

/// Count a claim Hive granted (or a verification commit it took) against
/// today's tasks on this machine. A refused claim is never counted: it
/// used to be, and refusals then ate the local budget until this machine
/// refused on its own.
pub fn count_claim(store: &HiveStore) -> Result<(), HiveError> {
    with_state(store, |state| state.used.tasks += 1)
}

/// Add the usage a result reports to today's.
pub fn admit_submit(store: &HiveStore, evidence: &EvidenceSummary) -> Result<(), Refusal> {
    with_state(store, |state| {
        state.used.tokens = state.used.tokens.saturating_add(evidence.usage_tokens);
        state.used.usd_micros = state
            .used
            .usd_micros
            .saturating_add(evidence.usage_usd_micros);
    })
    .map_err(|e| Refusal::Unavailable(e.to_string()))
}

/// Stop or resume claiming on this machine.
pub fn set_paused(store: &HiveStore, paused: bool) -> Result<(), HiveError> {
    with_state(store, |state| state.paused = paused)
}

/// Stop worker mode for this engine: pause here and switch the opt-in off
/// on the server, which ends every claim of this engine at once.
pub fn stop(session: &Session) -> Result<Optin, HiveError> {
    set_paused(&session.store, true)?;
    let stop = serde_json::to_value(EngineOptinStopV1 {
        schema: ENGINE_OPTIN_STOP_SCHEMA.into(),
        active: false,
    })
    .map_err(|e| HiveError::Local(e.to_string()))?;
    let body = session.with_client(|c| {
        c.send(
            "PUT",
            OPTIN_PATH,
            Some(&stop),
            &[],
            lean_ctx_protocol::hive::MAX_BODY_BYTES,
        )
        .map(|r| r.body)
    })?;
    parse_optin(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::hive::optin::ENGINE_OPTIN_SCHEMA;
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
        let local = HiveWorkerConfig::default();
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

    /// Hive task #19: an opt-in without a task limit (C0 §4 erratum) admits
    /// on count however many tasks the day has seen; a local cap still
    /// applies.
    #[test]
    fn no_task_limit_admits_on_count_and_a_local_cap_still_holds() {
        let mut free = optin();
        free.daily_budget.tasks = None;
        free.daily_budget.tokens = None;
        free.used_today.tasks = 140;
        let state = WorkerState::default();
        let admitted = decide_claim(&free, &HiveWorkerConfig::default(), &state, &org()).unwrap();
        assert_eq!(admitted.budget_left.tasks, None, "no limit stays no limit");
        let capped = HiveWorkerConfig {
            max_tasks_per_day: Some(150),
            ..HiveWorkerConfig::default()
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
        // As Hive sends it: no `tasks` field at all.
        let wire = parse_optin(json!({
            "schema": ENGINE_OPTIN_SCHEMA, "optin_id": "opt_2", "active": true,
            "vaults": [REPO], "networks": [], "topics": [], "daily_budget": {},
            "used_today": {"day": "2026-10-22", "tasks": 140, "tokens": 0, "usd_micros": 0}
        }))
        .unwrap();
        assert!(decide_claim(&wire, &HiveWorkerConfig::default(), &state, &org()).is_ok());
    }

    #[test]
    fn the_first_limit_reached_stops_and_local_caps_only_tighten() {
        let state = WorkerState::default();
        let mut spent = optin();
        spent.used_today.tasks = 2;
        assert_eq!(
            decide_claim(&spent, &HiveWorkerConfig::default(), &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
        let mut tokens = optin();
        tokens.used_today.tokens = 1200;
        assert_eq!(
            decide_claim(&tokens, &HiveWorkerConfig::default(), &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tokens))
        );

        // A wider local cap does not widen the server's budget.
        let wide = HiveWorkerConfig {
            max_tasks_per_day: Some(50),
            max_tokens_per_day: Some(1_000_000),
            max_usd_micros_per_day: None,
        };
        assert_eq!(
            decide_claim(&spent, &wide, &state, &org()),
            Err(Refusal::BudgetExhausted(Limit::Tasks))
        );
        // A tighter one applies, also to a limit the server leaves open.
        let tight = HiveWorkerConfig {
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
            "../../../crates/lean-ctx-protocol/tests/fixtures/hive/engine_optin.response.json"
        ))
        .unwrap();
        let optin = parse_optin(fixture.clone()).unwrap();
        assert!(optin.active && optin.optin_id.is_some());

        let mut wrong = fixture.clone();
        wrong["schema"] = json!("leanctx.hive.engine-optin/v2");
        assert!(matches!(parse_optin(wrong), Err(HiveError::Protocol(_))));
        // Active without an opt-in id breaks the contract: fail closed.
        let mut broken = fixture;
        broken.as_object_mut().unwrap().remove("optin_id");
        assert!(matches!(parse_optin(broken), Err(HiveError::Protocol(_))));

        let stop = serde_json::to_value(EngineOptinStopV1 {
            schema: ENGINE_OPTIN_STOP_SCHEMA.into(),
            active: false,
        })
        .unwrap();
        assert_eq!(
            stop,
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../crates/lean-ctx-protocol/tests/fixtures/hive/engine_optin_stop.request.json"
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
            assert_eq!(HiveError::from(refusal).to_string(), text);
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

    /// c9's finding: the gate counted a claim before Hive answered, so
    /// refused claims ate today's budget until this machine refused on its
    /// own. Now only a granted claim counts.
    #[test]
    fn only_a_granted_claim_counts_against_the_local_budget() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        prepare_enrollment(&store).unwrap();
        let cap = HiveWorkerConfig {
            max_tasks_per_day: Some(2),
            ..HiveWorkerConfig::default()
        };
        // Hive refuses every one of these (say `task_claimed`): the gate
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
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        // Enrollment writes the state first: a fresh engine is not paused.
        prepare_enrollment(&store).unwrap();
        enroll(&store);
        assert!(!read_state(&store).paused, "a fresh enrollment runs");
        set_paused(&store, true).unwrap();
        prepare_enrollment(&store).unwrap();
        assert!(read_state(&store).paused, "enrolling again keeps the pause");

        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
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

    fn enroll(store: &HiveStore) {
        store
            .write_config(
                super::super::state::INSTANCE_FILE,
                &super::super::state::InstanceRecord {
                    instance_id: "inst_1".into(),
                    org_id: "org_1".into(),
                    org_name: "Acme".into(),
                    label: "laptop".into(),
                    url: "https://hive.example.com".into(),
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
    fn a_damaged_state_file_counts_as_paused() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
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
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
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
}
