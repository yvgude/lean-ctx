// SPDX-License-Identifier: Apache-2.0
//! Task pickup switch for LeanCTX Code (rc4-16): whether this computer's
//! agents pull tasks from the person's vaults, turned on and off from the
//! app.
//!
//! The opt-in lives on the Cloud (`GET|PUT /v1/me/optin`). Turning it off
//! needs nothing but the instance token. Turning it on needs the person:
//! LeanCTX Code signs the turn-on with its own opt-in key (Keychain, no
//! prompt per click, domain `leanctx.cloud.optin.v1\n`), and the engine
//! checks that signature and forwards the request unchanged. The engine
//! never holds the key's private half
//! ([`lean_ctx_cloud::consent::optin_key`]).
//!
//! Default on (owner decision rc3-72 part 4): the Cloud turns task pickup on
//! for the person's own vaults when the computer is approved in the
//! browser, with 3 tasks and 1M tokens a day and no cost limit, and keeps
//! an earlier stop. Here the document says so: `suggest_on` is true while
//! there was never an opt-in and nothing was turned off; it only preselects
//! "on" in the app, which signs `turn_on` only after the person's click.
//! An explicit off (a stop on the Cloud, a pause here, or the off kept in
//! the worker state) is never turned on again by the engine: `cloud worker
//! resume` lifts only the pause, and only a turn-on the app signed lifts
//! the off.
//!
//! All three kinds are app-only (`app_key`): the socket answers them only
//! with the app key, and the CLI has no verb for them.
//!
//! - `cloud.pickup`: `GET /v1/local/cloud/pickup[?vault=…&daily_budget=…]`.
//! - `cloud.pickup.set`: `POST /v1/local/cloud/pickup/set`, body
//!   `{"state": "off"}` or `{"state": "on", "turn_on": <signed body>}`.
//! - `cloud.pickup.key`: `POST /v1/local/cloud/pickup/key`, body
//!   `{"optin_key": {alg, public_key, key_id, bind_signature}}`. The key is
//!   checked and kept as pending (`"bind": "pending_connect"`): the Cloud
//!   takes the bind on `POST /v1/device/token`, and `worker_on` is true only
//!   after that and while the Cloud announces `instance_optin_key`.
//!   The bind travels with `cloud connect --json --app-keys -`
//!   (`optin_key`), which marks the key accepted after the 2xx answer; that
//!   offer comes only from the app on stdin, never rebuilt from
//!   `optin_key.json` (agents can write it).
//!
//! `lean-ctx cloud worker on --json` forwards a signed turn-on read from
//! stdin, for the app's CLI path ([`run_worker_on`]); like `cloud connect
//! --chat-key`, it needs the app key and is refused before stdin is read
//! without it.

use std::io::Read as _;

use serde::Serialize;
use serde_json::{Value, json};

use lean_ctx_cloud::auth::Session;
use lean_ctx_cloud::consent::optin_key::{self, BoundOptinKey, OptinKeyOffer};
use lean_ctx_cloud::worker;
use lean_ctx_cloud::{CloudError, CloudStore};
use lean_ctx_protocol::cloud::optin::{
    DailyBudgetV1, ENGINE_OPTIN_ON_SCHEMA, EngineOptinOnV1, EngineOptinV1, MAX_OPTIN_VAULTS,
    OPTIN_SIGNING_DOMAIN,
};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The kind of the task pickup state.
pub(crate) const STATUS: &str = "cloud.pickup";
/// The kind of a switch turned on or off.
pub(crate) const SET: &str = "cloud.pickup.set";
/// The kind of the opt-in key handed over.
pub(crate) const KEY: &str = "cloud.pickup.key";

const TOOL: &str = "ctx_agent";
const READ_TOOL: &str = "ctx_knowledge";

/// The largest Cloud answer and signed request read.
const MAX_BYTES: usize = 64 * 1024;

/// This part's routes. App-only: no CLI words; listed as socket verbs.
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: STATUS,
        method: Method::Get,
        path: "/v1/local/cloud/pickup",
        cli: &[],
        action: &["status"],
        params: &[
            Param {
                name: "vault",
                flag: "--vault",
                shape: Shape::Repeat,
            },
            Param {
                name: "daily_budget",
                flag: "--daily-budget",
                shape: Shape::Json,
            },
        ],
        project: Project::None,
        names: Some(("cloud.pickup", "status")),
    },
    Route {
        kind: SET,
        method: Method::Post,
        path: "/v1/local/cloud/pickup/set",
        cli: &[],
        action: &["set"],
        params: &[
            Param {
                name: "state",
                flag: "--state",
                shape: Shape::Value,
            },
            Param {
                name: "turn_on",
                flag: "--turn-on",
                shape: Shape::Json,
            },
        ],
        project: Project::None,
        names: Some(("cloud.pickup", "set")),
    },
    Route {
        kind: KEY,
        method: Method::Post,
        path: "/v1/local/cloud/pickup/key",
        cli: &[],
        action: &["key"],
        params: &[Param {
            name: "optin_key",
            flag: "--optin-key",
            shape: Shape::Json,
        }],
        project: Project::None,
        names: Some(("cloud.pickup", "key")),
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[STATUS, SET, KEY];

/// No CLI verbs: all three are socket verbs only.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[];

/// A vault the turn-on names, for the app to show at the click.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct NamedVault {
    pub vault_id: String,
    /// The vault's name when this computer knows it.
    pub name: Option<String>,
}

/// What LeanCTX Code signs to turn task pickup on.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct TurnOn {
    /// The request without `presence_signature` (`EngineOptinOnV1`).
    pub body: Value,
    /// `leanctx.cloud.optin.v1\n`.
    pub domain: &'static str,
    /// The exact UTF-8 text the opt-in key signs: `domain` ‖ canonical
    /// JSON of `body` plus `instance_id`. The app signs these bytes as
    /// they are and never rebuilds them.
    pub signing_input: String,
    /// The vaults `body` names, with their names when known.
    pub vaults: Vec<NamedVault>,
    /// The daily budget `body` sets.
    pub daily_budget: DailyBudgetV1,
}

/// The task pickup state of this computer.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct Pickup {
    /// `on` | `off` | `paused_here` | `no_vaults`.
    pub state: &'static str,
    /// The person turned it off: a stop on the Cloud, a pause here, or an
    /// off kept on this computer (`cloud worker resume` never lifts it).
    pub explicit_off: bool,
    /// Preselect "on" in the app: there was never an opt-in and nothing was
    /// turned off. Never a turn-on by itself: the app signs `turn_on` only
    /// after the person's click.
    pub suggest_on: bool,
    pub paused_here: bool,
    /// The Cloud took the opt-in key and announces `instance_optin_key`,
    /// so the app can turn task pickup on.
    pub worker_on: bool,
    /// The opt-in key kept here, accepted or still pending.
    pub optin_key_id: Option<String>,
    /// The kept key waits for `cloud connect` to bind it at the Cloud.
    pub optin_key_pending: bool,
    /// The Instance UUID the signature covers.
    pub instance_id: String,
    /// Vaults tasks are pulled from (the Cloud's `vault_count`).
    pub vault_count: u64,
    /// The budget a first turn-on starts with: 3 tasks, 1M tokens a day.
    pub default_budget: DailyBudgetV1,
    /// The turn-on to sign, `null` when there is no vault to name.
    pub turn_on: Option<TurnOn>,
    /// The opt-in as the Cloud answered it.
    pub optin: Value,
}

/// The state words of an opt-in (pure). `local` is this computer's worker
/// state: its pause, the off kept here
/// ([`worker::WorkerState::explicit_off`]), and whether enrollment wrote it
/// (a first turn-on is suggested only then).
pub(crate) fn derive(optin: &EngineOptinV1, raw: &Value, local: &worker::WorkerState) -> Derived {
    let (paused_here, local_off) = (local.paused, local.explicit_off);
    let vault_count = raw["vault_count"]
        .as_u64()
        .unwrap_or(optin.vaults.len() as u64);
    let stopped = optin.optin_id.is_some() && !optin.active;
    let state = if !optin.active || local_off {
        "off"
    } else if vault_count == 0 {
        "no_vaults"
    } else if paused_here {
        "paused_here"
    } else {
        "on"
    };
    let explicit_off = stopped || paused_here || local_off;
    Derived {
        state,
        explicit_off,
        // S1: a state rebuilt after its file went missing never suggests on.
        suggest_on: optin.optin_id.is_none() && !explicit_off && local.enrolled_fresh,
        vault_count,
    }
}

/// See [`derive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Derived {
    pub state: &'static str,
    pub explicit_off: bool,
    pub suggest_on: bool,
    pub vault_count: u64,
}

/// The turn-on to sign for `optin` (pure): `vaults` named by the app, else
/// the opt-in's; `budget` named, else the opt-in's, else the default of a
/// first turn-on. `None` without any vault. `name_of` names a vault for the
/// app to show.
pub(crate) fn proposal(
    optin: &EngineOptinV1,
    vaults: Vec<String>,
    budget: Option<DailyBudgetV1>,
    instance_id: &str,
    name_of: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<TurnOn>, Failure> {
    let mut vaults = if vaults.is_empty() {
        optin.vaults.clone()
    } else {
        vaults
    };
    vaults.sort_unstable();
    vaults.dedup();
    if vaults.is_empty() {
        return Ok(None);
    }
    let budget = budget.unwrap_or(if optin.optin_id.is_some() {
        optin.daily_budget
    } else {
        DailyBudgetV1::AUTO
    });
    let named = vaults
        .iter()
        .map(|vault_id| NamedVault {
            vault_id: vault_id.clone(),
            name: name_of(vault_id),
        })
        .collect();
    let typed = EngineOptinOnV1 {
        schema: ENGINE_OPTIN_ON_SCHEMA.into(),
        active: true,
        prev_optin_id: optin.optin_id.clone(),
        vaults: Some(vaults),
        daily_budget: budget,
        presence_signature: None,
    };
    typed
        .validate()
        .map_err(|e| Failure::invalid(e.to_string()))?;
    let body = serde_json::to_value(&typed).map_err(|e| Failure::invalid(e.to_string()))?;
    let input = lean_ctx_cloud::consent::person_sign::signing_input(
        OPTIN_SIGNING_DOMAIN,
        &body,
        instance_id,
    )
    .map_err(|e| Failure::new(Code::Internal, e))?;
    Ok(Some(TurnOn {
        body,
        domain: OPTIN_SIGNING_DOMAIN,
        signing_input: String::from_utf8(input)
            .map_err(|_| Failure::new(Code::Internal, "the signing input is not text"))?,
        vaults: named,
        daily_budget: budget,
    }))
}

/// The name of `vault_id` (pure): this computer's vault map first, else
/// the Cloud's `vault_names` beside its `vaults`.
pub(crate) fn vault_name(
    map: &lean_ctx::core::vault_map::VaultMap,
    raw: &Value,
    vault_id: &str,
) -> Option<String> {
    let same = |a: &str| {
        a.replace('-', "")
            .eq_ignore_ascii_case(&vault_id.replace('-', ""))
    };
    map.vaults
        .values()
        .find(|record| same(&record.vault_id))
        .map(|record| record.name.clone())
        .or_else(|| {
            let ids = raw["vaults"].as_array()?;
            let at = ids.iter().position(|v| v.as_str().is_some_and(same))?;
            raw["vault_names"][at].as_str().map(str::to_owned)
        })
}

/// The connected session of this computer.
fn connected() -> Result<Session, Failure> {
    let store = CloudStore::open()?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    Ok(lean_ctx_cloud::team_stop::session(&store, &config)?)
}

/// The task pickup state, read fresh from the Cloud.
pub(crate) fn status_of(
    session: &Session,
    vaults: Vec<String>,
    budget: Option<DailyBudgetV1>,
) -> Result<Pickup, Failure> {
    let raw = session.with_client(|client| client.get(worker::OPTIN_PATH))?;
    let optin = worker::parse_optin(raw.clone())?;
    // A turn-on on the web lifts the off kept here (rc4-16 M3).
    let local = worker::observe(&session.store, &optin)?;
    let paused_here = local.paused;
    let worker_on = optin_key::bound_optin_key(&session.store).is_some();
    let kept = optin_key::pending_optin_key(&session.store);
    let derived = derive(&optin, &raw, &local);
    let instance_id = session.instance.instance_id.clone();
    let map = lean_ctx::core::vault_map::VaultMap::load(session.store.data_dir());
    let name_of = |vault_id: &str| vault_name(&map, &raw, vault_id);
    Ok(Pickup {
        state: derived.state,
        explicit_off: derived.explicit_off,
        suggest_on: derived.suggest_on,
        paused_here,
        worker_on,
        optin_key_pending: kept.as_ref().is_some_and(|k| !k.accepted),
        optin_key_id: kept.map(|k| k.key_id),
        turn_on: offer(worker_on, &optin, vaults, budget, &instance_id, &name_of)?,
        instance_id,
        vault_count: derived.vault_count,
        default_budget: DailyBudgetV1::AUTO,
        optin: raw,
    })
}

/// The turn-on to sign, only with the opt-in key bound and the Cloud
/// announcing `instance_optin_key` (`worker_on`): without it there is no
/// switch to offer, and no presence-key fallback (pure).
pub(crate) fn offer(
    worker_on: bool,
    optin: &EngineOptinV1,
    vaults: Vec<String>,
    budget: Option<DailyBudgetV1>,
    instance_id: &str,
    name_of: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<TurnOn>, Failure> {
    if !worker_on {
        return Ok(None);
    }
    proposal(optin, vaults, budget, instance_id, name_of)
}

/// Why a turn-on did not go through, in the words of `cloud worker on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnCode {
    AppOnly,
    NoOptinKey,
    PersonUnproven,
    NotConnected,
    VaultNotInOrg,
    OptinKeyBound,
    OptinKeyCleared,
    TokenEnrollment,
    OptinChanged,
    TeamStopped,
    Failed,
}

impl OnCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AppOnly => "app_only",
            Self::NoOptinKey => "no_optin_key",
            Self::PersonUnproven => "person_unproven",
            Self::NotConnected => "not_connected",
            Self::VaultNotInOrg => "vault_not_in_org",
            Self::OptinKeyBound => "optin_key_bound",
            Self::OptinKeyCleared => "optin_key_cleared",
            Self::TokenEnrollment => "token_enrollment",
            Self::OptinChanged => "optin_changed",
            Self::TeamStopped => "team_stopped",
            Self::Failed => "failed",
        }
    }
}

/// A refused turn-on: the CLI's code and the Local Read failure.
#[derive(Debug)]
pub(crate) struct OnRefusal {
    pub code: OnCode,
    pub failure: Failure,
}

impl From<Failure> for OnRefusal {
    fn from(failure: Failure) -> Self {
        let code = if failure.code == Code::NotConnected {
            OnCode::NotConnected
        } else {
            OnCode::Failed
        };
        Self { code, failure }
    }
}

impl From<CloudError> for OnRefusal {
    fn from(error: CloudError) -> Self {
        Failure::from(error).into()
    }
}

fn refusal(code: OnCode, failure: Failure) -> OnRefusal {
    OnRefusal { code, failure }
}

fn no_key() -> OnRefusal {
    refusal(
        OnCode::NoOptinKey,
        Failure::new(
            Code::Forbidden,
            "Pair LeanCTX Code with this computer first to turn on task pickup here.",
        )
        .with_reason("no_optin_key"),
    )
}

fn unproven() -> OnRefusal {
    refusal(
        OnCode::PersonUnproven,
        Failure::new(
            Code::Forbidden,
            "the turn-on is not signed by this computer's opt-in key",
        )
        .with_reason("person_unproven"),
    )
}

/// The Cloud's refusal of a turn-on (pure).
pub(crate) fn refused(status: u16, body: &Value) -> OnRefusal {
    let error = &body["error"];
    let code = error["code"].as_str().unwrap_or_default();
    let reason = error["reason"].as_str().unwrap_or_default();
    let field = error["field"]
        .as_str()
        .or_else(|| error["details"]["field"].as_str())
        .unwrap_or_default();
    let is = |word: &str| code == word || reason == word;
    match (status, code, reason) {
        (403, _, "person_unproven") => unproven(),
        (400, "invalid_field", _) if field == "vaults" => refusal(
            OnCode::VaultNotInOrg,
            Failure::new(
                Code::InvalidField,
                "a vault named is not one of this computer's team",
            )
            .with_reason("vault_not_in_org"),
        ),
        (409, _, _) if is("optin_key_bound") => refusal(
            OnCode::OptinKeyBound,
            Failure::new(
                Code::Conflict,
                "Another opt-in key is bound to this computer; remove it on the computer's page, then pair again.",
            )
            .with_reason("optin_key_bound"),
        ),
        (409, _, _) if is("optin_key_cleared") => refusal(
            OnCode::OptinKeyCleared,
            Failure::new(
                Code::Conflict,
                "This opt-in key was removed; LeanCTX Code makes a new one and pairs again.",
            )
            .with_reason("optin_key_cleared"),
        ),
        (400, _, _) if is("token_enrollment") => refusal(
            OnCode::TokenEnrollment,
            Failure::new(
                Code::InvalidArgument,
                "This computer was connected with an enrollment token; pairing the opt-in key needs a connect from LeanCTX Code.",
            )
            .with_reason("token_enrollment"),
        ),
        (409, _, "optin_changed") => refusal(
            OnCode::OptinChanged,
            Failure::new(
                Code::InvalidState,
                "task pickup changed since it was signed; read it again and sign again",
            )
            .with_reason("optin_changed"),
        ),
        (423, _, _) => refusal(
            OnCode::TeamStopped,
            Failure::new(Code::Conflict, "the team is stopped").with_reason("team_stopped"),
        ),
        (403 | 402, _, _) => {
            Failure::new(Code::Forbidden, "the Cloud refused task pickup here").into()
        }
        (404, _, _) => Failure::new(Code::NotFound, "the Cloud does not offer task pickup").into(),
        (429, _, _) => Failure::new(Code::RateLimited, "the Cloud asks to wait a moment").into(),
        (408 | 500.., _, _) => {
            Failure::new(Code::Unavailable, "the Cloud cannot be reached").into()
        }
        _ => Failure::new(Code::Internal, format!("the Cloud answered {status}")).into(),
    }
}

/// The checked turn-on of `signed` (pure): the request, the signature and
/// the request without it.
fn signed_parts(signed: &Value) -> Result<(Value, String), OnRefusal> {
    let Value::Object(fields) = signed else {
        return Err(Failure::invalid("turn_on is a JSON object").into());
    };
    let Some(signature) = fields
        .get(lean_ctx_cloud::consent::person_sign::SIGNATURE_FIELD)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Err(unproven());
    };
    let typed: EngineOptinOnV1 = serde_json::from_value(signed.clone())
        .map_err(|e| OnRefusal::from(Failure::invalid(format!("turn_on: {e}"))))?;
    typed
        .validate()
        .map_err(|e| OnRefusal::from(Failure::invalid(format!("turn_on: {e}"))))?;
    if typed.vaults.as_ref().is_none_or(Vec::is_empty) {
        return Err(Failure::invalid("turn_on names no vault").into());
    }
    let mut unsigned = fields.clone();
    unsigned.remove(lean_ctx_cloud::consent::person_sign::SIGNATURE_FIELD);
    Ok((Value::Object(unsigned), signature.to_owned()))
}

/// Check a signed turn-on against `key` and this Instance (pure): nothing
/// is sent for one the opt-in key did not sign.
pub(crate) fn check_turn_on(
    signed: &Value,
    key: Option<&BoundOptinKey>,
    instance_id: &str,
) -> Result<(), OnRefusal> {
    // No presence-key fallback: without the bound opt-in key nothing is sent.
    let key = key.ok_or_else(no_key)?;
    let (unsigned, signature) = signed_parts(signed)?;
    if !key.admits_turn_on(&unsigned, instance_id, &signature) {
        return Err(unproven());
    }
    Ok(())
}

/// Turn task pickup on with the app-signed `signed` request: checked here,
/// forwarded unchanged, and the pause on this computer lifted once the
/// Cloud took it.
/// With `folder` (the CLI's project folder), the request must name the
/// vault that folder is linked to ([`folder_vault`], [`names_vault`]).
pub(crate) fn turn_on(
    signed: &Value,
    folder: Option<&std::path::Path>,
) -> Result<Pickup, OnRefusal> {
    let session = connected()?;
    let key = optin_key::bound_optin_key(&session.store);
    if key.is_none() {
        return Err(no_key());
    }
    if let Some(root) = folder {
        names_vault(signed, &folder_vault(session.store.data_dir(), root)?)?;
    }
    check_turn_on(signed, key.as_ref(), &session.instance.instance_id)?;
    // Only a 2xx lifts the kept off and the pause (`worker::send_turn_on`).
    if let Err((status, answer)) = worker::send_turn_on(&session, signed)? {
        let refusal = refused(status, &answer);
        if refusal.failure.reason == Some("optin_key_cleared") {
            // The Cloud dropped the key: back to pending until a new bind.
            optin_key::clear_accepted(&session.store)?;
        }
        return Err(refusal);
    }
    Ok(status_of(&session, Vec::new(), None)?)
}

fn folder_refusal(message: &str) -> OnRefusal {
    refusal(
        OnCode::VaultNotInOrg,
        Failure::new(Code::InvalidField, message).with_reason("vault_not_in_org"),
    )
}

/// The vault the project folder `root` is linked to (`projects.json` in
/// `data_dir`): `vault_not_in_org` when it is not linked, or its link
/// names another vault than the one recorded.
pub(crate) fn folder_vault(
    data_dir: &std::path::Path,
    root: &std::path::Path,
) -> Result<String, OnRefusal> {
    match lean_ctx_cloud::resolution::vault_for_root(data_dir, root) {
        Ok(Some(record)) => Ok(record.vault_id),
        Ok(None) => Err(folder_refusal("this folder is not linked to a vault")),
        Err(_) => Err(folder_refusal(
            "this folder's link names another vault; run `lean-ctx cloud sync` in it",
        )),
    }
}

/// The folder's vault must be among the signed request's vaults (pure).
pub(crate) fn names_vault(signed: &Value, vault: &str) -> Result<(), OnRefusal> {
    let named = signed["vaults"]
        .as_array()
        .is_some_and(|vaults| vaults.iter().any(|v| v.as_str() == Some(vault)));
    if named {
        Ok(())
    } else {
        Err(folder_refusal(
            "the signed request does not name this folder's vault; read `cloud worker status --json` in this folder and sign its turn_on",
        ))
    }
}

/// The project folder of the current directory, as other `cloud` commands
/// resolve it, with symbolic links resolved. Refused when the current
/// folder cannot be read (the vault check is never skipped), and when the
/// project folder resolves outside the current one's path (a link out of
/// the project).
pub(crate) fn current_folder() -> Result<std::path::PathBuf, OnRefusal> {
    let unreadable = || {
        OnRefusal::from(Failure::invalid(
            "the current folder cannot be read; run this in the project folder",
        ))
    };
    let cwd = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .map_err(|_| unreadable())?;
    let root = std::fs::canonicalize(lean_ctx::core::protocol::detect_project_root_or_cwd(
        &cwd.to_string_lossy(),
    ))
    .map_err(|_| unreadable())?;
    project_holds(&root, &cwd)?;
    Ok(root)
}

/// The resolved project folder `root` must hold the resolved current
/// folder `cwd` (pure).
pub(crate) fn project_holds(
    root: &std::path::Path,
    cwd: &std::path::Path,
) -> Result<(), OnRefusal> {
    if cwd.starts_with(root) {
        Ok(())
    } else {
        Err(folder_refusal(
            "this folder's project is reached through a symbolic link; run this in the project folder itself",
        ))
    }
}

/// Turn task pickup off: paused here and stopped on the Cloud; held claims
/// end.
pub(crate) fn turn_off() -> Result<Pickup, Failure> {
    let session = connected()?;
    let stopped = worker::stop(&session);
    lean_ctx_cloud::tasks::claims::clear(session.store.data_dir())?;
    stopped?;
    status_of(&session, Vec::new(), None)
}

/// The opt-in key handed over.
#[derive(Debug, Serialize)]
pub(crate) struct KeyBound {
    pub key_id: String,
    /// True only once the Cloud took the key and announces
    /// `instance_optin_key`.
    pub worker_on: bool,
    /// `pending_connect` until the Cloud took the bind; absent after.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind: Option<&'static str>,
}

/// Check the app's opt-in key as the Cloud will (its own signature over
/// this computer's `instance_public_key`) and keep the public part as
/// pending. The Cloud takes the bind on `POST /v1/device/token`, so this
/// never claims `worker_on` by itself.
/// `cloud connect --json --app-keys -` with `optin_key` sends the bind and
/// marks the key accepted; the offer comes only from the app on stdin,
/// never rebuilt from `optin_key.json` (agents can write that file).
fn bind_key(offer: &OptinKeyOffer) -> Result<KeyBound, Failure> {
    let store = CloudStore::open()?;
    store.instance()?.ok_or(CloudError::NotConnected)?;
    let instance_public_key =
        lean_ctx_cloud::instance_key::InstanceKeys::load(&store)?.public_key_b64();
    offer
        .check(&instance_public_key)
        .map_err(Failure::invalid)?;
    optin_key::store_pending(&store, offer)?;
    let worker_on = optin_key::bound_optin_key(&store).is_some_and(|k| k.key_id == offer.key_id);
    Ok(KeyBound {
        key_id: offer.key_id.clone(),
        worker_on,
        bind: (!worker_on).then_some("pending_connect"),
    })
}

fn json_arg(args: &[String], flag: &str, name: &str) -> Result<Option<Value>, Failure> {
    super::flag(args, flag)?
        .map(|raw| {
            serde_json::from_str::<Value>(raw)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| Failure::invalid(format!("{name} is a JSON object")))
        })
        .transpose()
}

fn status_args(args: &[String]) -> Result<(Vec<String>, Option<DailyBudgetV1>), Failure> {
    let vaults: Vec<String> = super::flags(args, "--vault")?
        .into_iter()
        .map(str::to_owned)
        .collect();
    if vaults.len() > MAX_OPTIN_VAULTS {
        return Err(Failure::invalid(format!(
            "at most {MAX_OPTIN_VAULTS} vaults"
        )));
    }
    for vault in &vaults {
        lean_ctx_protocol::cloud::validate_vault_id(vault)
            .map_err(|_| Failure::invalid("a vault is 32 lowercase hexadecimal characters"))?;
    }
    let budget = json_arg(args, "--daily-budget", "daily_budget")?
        .map(|raw| {
            let budget: DailyBudgetV1 = serde_json::from_value(raw)
                .map_err(|_| Failure::invalid("daily_budget is {tasks?, tokens?, usd_micros?}"))?;
            budget
                .validate()
                .map_err(|e| Failure::invalid(e.to_string()))?;
            Ok::<_, Failure>(budget)
        })
        .transpose()?;
    Ok((vaults, budget))
}

/// A switch request.
enum Set {
    Off,
    On(Value),
}

fn set_args(args: &[String]) -> Result<Set, Failure> {
    let turn_on = json_arg(args, "--turn-on", "turn_on")?;
    match (super::flag(args, "--state")?, turn_on) {
        (Some("off"), None) => Ok(Set::Off),
        (Some("off"), Some(_)) => Err(Failure::invalid("turn_on is only for state on")),
        (Some("on"), Some(body)) => Ok(Set::On(body)),
        (Some("on"), None) => Err(Failure::invalid(
            "state on needs turn_on, signed by LeanCTX Code",
        )),
        _ => Err(Failure::invalid("state is on or off")),
    }
}

/// The released document of `kind`, for the socket only.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    // App-only: the socket checked the app key before this; the CLI never
    // switches.
    if via == Via::Cli {
        return Answer::failure(&super::app_key::refusal());
    }
    match kind {
        STATUS => {
            let (vaults, budget) = match status_args(args) {
                Ok(parts) => parts,
                Err(failure) => return Answer::failure(&failure),
            };
            super::answer_released(READ_TOOL, "", kind, via, || {
                status_of(&connected()?, vaults, budget)
            })
        }
        SET => {
            let set = match set_args(args) {
                Ok(set) => set,
                Err(failure) => return Answer::failure(&failure),
            };
            let preview = match &set {
                Set::Off => json!({"state": "off"}),
                Set::On(_) => json!({"state": "on"}),
            };
            super::answer_released_write(TOOL, "", kind, via, &preview, || match set {
                Set::Off => turn_off(),
                Set::On(body) => turn_on(&body, None).map_err(|refusal| refusal.failure),
            })
        }
        KEY => {
            let offer = match json_arg(args, "--optin-key", "optin_key").and_then(|raw| {
                let raw = raw.ok_or_else(|| Failure::invalid("optin_key is required"))?;
                serde_json::from_value::<OptinKeyOffer>(raw).map_err(|_| {
                    Failure::invalid("optin_key is {alg, public_key, key_id, bind_signature}")
                })
            }) {
                Ok(offer) => offer,
                Err(failure) => return Answer::failure(&failure),
            };
            let preview = json!({"key_id": offer.key_id});
            super::answer_released_write(TOOL, "", kind, via, &preview, || bind_key(&offer))
        }
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// The JSON `cloud worker on --json` prints.
pub(crate) fn on_document(outcome: &Result<Pickup, OnRefusal>) -> Value {
    match outcome {
        Ok(pickup) => json!({"ok": true, "state": pickup.state, "optin": pickup.optin}),
        Err(refusal) => json!({
            "ok": false,
            "code": refusal.code.as_str(),
            "error": refusal.failure.message,
        }),
    }
}

/// LeanCTX Code's launch key, which it passes in this variable when it
/// runs `cloud worker on` (the same variable as `cloud connect --chat-key`).
/// Removed from the environment once read, so no child inherits it.
const APP_KEY_ENV: &str = "LEAN_CTX_APP_KEY";

/// The app key of this run, removed from the environment.
fn take_app_key() -> Option<Vec<u8>> {
    let given = std::env::var_os(APP_KEY_ENV)?;
    // SAFETY: removed on the CLI's main thread before this command spawns
    // anything.
    unsafe { std::env::remove_var(APP_KEY_ENV) };
    Some(given.to_string_lossy().into_owned().into_bytes())
}

/// `cloud worker on` only for LeanCTX Code (pure): refused before stdin is
/// read unless `given` is the app key (`admits` checks it).
pub(crate) fn app_gate(
    given: Option<&[u8]>,
    admits: impl Fn(Option<&[u8]>) -> bool,
) -> Result<(), OnRefusal> {
    if admits(given) {
        Ok(())
    } else {
        Err(refusal(OnCode::AppOnly, super::app_key::refusal()))
    }
}

/// `lean-ctx cloud worker on [--json]`: the signed turn-on from stdin,
/// forwarded. Exit code 0 when it went through, 1 otherwise. Only with the
/// app key in [`APP_KEY_ENV`]; refused before stdin is read otherwise.
pub(crate) fn run_worker_on(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let given = take_app_key();
    let outcome =
        app_gate(given.as_deref(), super::app_key::admits).and_then(|()| read_and_turn_on());
    let ok = outcome.is_ok();
    if json {
        println!("{}", on_document(&outcome));
    } else {
        match &outcome {
            Ok(pickup) => println!("Task pickup: {}", pickup.state),
            Err(refusal) => eprintln!("cloud worker on: {}", refusal.failure.message),
        }
    }
    i32::from(!ok)
}

/// The signed turn-on from stdin, sent for the current project folder.
fn read_and_turn_on() -> Result<Pickup, OnRefusal> {
    let mut text = String::new();
    let read = std::io::stdin()
        .lock()
        .take(MAX_BYTES as u64 + 1)
        .read_to_string(&mut text);
    match read {
        Ok(n) if n <= MAX_BYTES => match serde_json::from_str::<Value>(text.trim()) {
            // rc4-16 (af): no `--vault`; the vault is the one the current
            // project folder is linked to.
            Ok(signed) => current_folder().and_then(|folder| turn_on(&signed, Some(&folder))),
            Err(_) => Err(OnRefusal::from(Failure::invalid(
                "stdin holds the signed turn-on as JSON",
            ))),
        },
        _ => Err(OnRefusal::from(Failure::invalid(
            "stdin holds the signed turn-on as JSON, at most 64 KiB",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    use ed25519_dalek::{Signer as _, SigningKey};

    const INSTANCE: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
    const VAULT: &str = "0f3c2b6a8d4e4a7b9c105e2f6a7b8c01";

    fn optin(raw: Value) -> EngineOptinV1 {
        worker::parse_optin(raw).unwrap()
    }

    fn none() -> Value {
        json!({"schema": "leanctx.cloud.instance-optin/v1", "active": false, "vaults": [],
               "vault_names": [], "vault_count": 0, "daily_budget": {"tasks": 5},
               "used_today": {"day": "2026-11-24", "tasks": 0, "tokens": 0, "usd_micros": 0}})
    }

    fn active() -> Value {
        json!({"schema": "leanctx.cloud.instance-optin/v1", "optin_id": "opt_1", "active": true,
               "source": "auto_pair", "vaults": [VAULT], "vault_names": ["notes"], "vault_count": 1,
               "daily_budget": {"tasks": 3, "tokens": 1_000_000},
               "used_today": {"day": "2026-11-24", "tasks": 0, "tokens": 0, "usd_micros": 0}})
    }

    fn stopped() -> Value {
        let mut raw = active();
        raw["active"] = json!(false);
        raw["stopped_at"] = json!("2026-11-24T08:00:00Z");
        raw
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn bound(key: &SigningKey) -> BoundOptinKey {
        let public = key.verifying_key().to_bytes();
        BoundOptinKey {
            alg: "ed25519".into(),
            key_id: optin_key::key_id_of(&public),
            public_key: B64URL.encode(public),
            accepted: true,
        }
    }

    fn no_names(_: &str) -> Option<String> {
        None
    }

    fn signed(key: &SigningKey, turn_on: &TurnOn) -> Value {
        let signature = B64URL.encode(key.sign(turn_on.signing_input.as_bytes()).to_bytes());
        let mut body = turn_on.body.clone();
        body["presence_signature"] = json!(signature);
        body
    }

    #[test]
    fn pickup_kinds_are_app_only_and_refused_on_the_cli() {
        for kind in KINDS {
            assert!(super::super::app_key::guards(kind), "{kind}");
            let answer = answer(kind, &[], Via::Cli);
            assert!(format!("{answer:?}").contains("app_only"), "{kind}");
        }
        assert!(ROUTES.iter().all(|r| r.cli.is_empty() && r.names.is_some()));
    }

    #[test]
    fn pickup_default_on_only_without_any_opt_in_and_never_after_an_off() {
        let fresh = derive(&optin(none()), &none(), &here(false, false));
        assert_eq!(
            (fresh.state, fresh.suggest_on, fresh.explicit_off),
            ("off", true, false)
        );
        let paused = derive(&optin(none()), &none(), &here(true, false));
        assert!(!paused.suggest_on && paused.explicit_off);
        let off = derive(&optin(stopped()), &stopped(), &here(false, false));
        assert_eq!(
            (off.state, off.suggest_on, off.explicit_off),
            ("off", false, true)
        );
        let on = derive(&optin(active()), &active(), &here(false, false));
        assert_eq!((on.state, on.suggest_on, on.vault_count), ("on", false, 1));
        assert_eq!(
            derive(&optin(active()), &active(), &here(true, false)).state,
            "paused_here"
        );
        let mut empty = active();
        empty["vault_count"] = json!(0);
        assert_eq!(
            derive(&optin(active()), &empty, &here(false, false)).state,
            "no_vaults"
        );
        // M1: an off kept here (no opt-in row on the Cloud, pause lifted by
        // `cloud worker resume`) is still off and never suggests on.
        let kept = derive(&optin(none()), &none(), &here(false, true));
        assert_eq!(
            (kept.state, kept.suggest_on, kept.explicit_off),
            ("off", false, true)
        );
        let kept_on_cloud = derive(&optin(active()), &active(), &here(false, true));
        assert_eq!(
            (kept_on_cloud.state, kept_on_cloud.suggest_on),
            ("off", false)
        );
        // S1: deleted state file, then `cloud worker resume`: the state is
        // written again without enrollment's mark, so no suggestion.
        let rebuilt = worker::WorkerState::default();
        let rebuilt = derive(&optin(none()), &none(), &rebuilt);
        assert_eq!(
            (rebuilt.state, rebuilt.suggest_on, rebuilt.explicit_off),
            ("off", false, false)
        );
    }

    /// This computer's worker state as enrollment wrote it, with `paused`
    /// and the kept off.
    fn here(paused: bool, explicit_off: bool) -> worker::WorkerState {
        worker::WorkerState {
            paused,
            explicit_off,
            enrolled_fresh: true,
            ..worker::WorkerState::default()
        }
    }

    #[test]
    fn pickup_worker_on_without_the_app_key_is_refused_before_stdin() {
        let refused = app_gate(Some(b"not the key".as_slice()), |_| false).unwrap_err();
        assert_eq!(refused.code, OnCode::AppOnly);
        assert_eq!(on_document(&Err(refused))["code"], "app_only");
        assert_eq!(
            app_gate(None, |given| given.is_some()).unwrap_err().code,
            OnCode::AppOnly
        );
        app_gate(Some(b"key".as_slice()), |given| {
            given == Some(b"key".as_slice())
        })
        .unwrap();
    }

    #[test]
    fn pickup_turn_on_shows_vault_names_and_budget_for_the_click() {
        use lean_ctx::core::vault_map::{VaultMap, VaultRecord};
        let mut map = VaultMap::default();
        let raw = active();
        assert_eq!(vault_name(&map, &raw, VAULT).as_deref(), Some("notes"));
        map.vaults.insert(
            "stable".into(),
            VaultRecord {
                vault_id: VAULT.into(),
                ns: "me".into(),
                name: "my notes".into(),
                sealed: false,
                roots: Vec::new(),
            },
        );
        let name_of = |vault_id: &str| vault_name(&map, &raw, vault_id);
        let turn_on = proposal(&optin(raw.clone()), Vec::new(), None, INSTANCE, &name_of)
            .unwrap()
            .unwrap();
        assert_eq!(
            turn_on.vaults,
            vec![NamedVault {
                vault_id: VAULT.into(),
                name: Some("my notes".into()),
            }]
        );
        assert_eq!(
            serde_json::to_value(turn_on.daily_budget).unwrap(),
            turn_on.body["daily_budget"]
        );
        assert_eq!(vault_name(&VaultMap::default(), &none(), VAULT), None);
    }

    #[test]
    fn pickup_proposal_uses_the_default_budget_first_and_the_persons_after() {
        let first = proposal(
            &optin(none()),
            vec![VAULT.into()],
            None,
            INSTANCE,
            &no_names,
        )
        .unwrap()
        .unwrap();
        assert_eq!(first.body["prev_optin_id"], Value::Null);
        assert_eq!(
            first.body["daily_budget"],
            json!({"tasks": 3, "tokens": 1_000_000})
        );
        assert!(
            first
                .signing_input
                .starts_with("leanctx.cloud.optin.v1\n{\"active\":true")
        );
        assert!(first.signing_input.contains(INSTANCE));
        assert!(
            proposal(&optin(none()), Vec::new(), None, INSTANCE, &no_names)
                .unwrap()
                .is_none()
        );
        let mut own = stopped();
        own["daily_budget"] = json!({"tasks": 7});
        let again = proposal(&optin(own), Vec::new(), None, INSTANCE, &no_names)
            .unwrap()
            .unwrap();
        assert_eq!(again.body["prev_optin_id"], "opt_1");
        assert_eq!(again.body["vaults"], json!([VAULT]));
        assert_eq!(again.body["daily_budget"], json!({"tasks": 7}));
    }

    /// No presence-key fallback (p37 !307): without the bound opt-in key a
    /// turn-on is refused before anything is read or sent, signed or not,
    /// and without `worker_on` (key accepted + `instance_optin_key`) there
    /// is no turn-on to offer.
    #[test]
    fn pickup_without_the_opt_in_key_there_is_no_switch_and_nothing_is_sent() {
        let key = key();
        let turn_on = proposal(&optin(active()), Vec::new(), None, INSTANCE, &no_names)
            .unwrap()
            .unwrap();
        for body in [signed(&key, &turn_on), turn_on.body.clone(), json!("junk")] {
            let refusal = check_turn_on(&body, None, INSTANCE).unwrap_err();
            assert_eq!(refusal.code, OnCode::NoOptinKey);
            assert_eq!(refusal.failure.reason, Some("no_optin_key"));
            assert_eq!(
                refusal.failure.message,
                "Pair LeanCTX Code with this computer first to turn on task pickup here."
            );
        }
        let offered = |on| offer(on, &optin(active()), Vec::new(), None, INSTANCE, &no_names);
        assert!(offered(false).unwrap().is_none());
        assert!(offered(true).unwrap().is_some());
    }

    #[test]
    fn pickup_turn_on_is_sent_only_when_the_opt_in_key_signed_it() {
        let key = key();
        let turn_on = proposal(&optin(active()), Vec::new(), None, INSTANCE, &no_names)
            .unwrap()
            .unwrap();
        let good = signed(&key, &turn_on);
        check_turn_on(&good, Some(&bound(&key)), INSTANCE).unwrap();
        let code = |r: Result<(), OnRefusal>| r.unwrap_err().code;
        assert_eq!(
            code(check_turn_on(&good, None, INSTANCE)),
            OnCode::NoOptinKey
        );
        assert_eq!(
            code(check_turn_on(&turn_on.body, Some(&bound(&key)), INSTANCE)),
            OnCode::PersonUnproven
        );
        let other = SigningKey::from_bytes(&[3u8; 32]);
        assert_eq!(
            code(check_turn_on(&good, Some(&bound(&other)), INSTANCE)),
            OnCode::PersonUnproven
        );
        let mut changed = good.clone();
        changed["daily_budget"] = json!({"tasks": 100});
        assert_eq!(
            code(check_turn_on(&changed, Some(&bound(&key)), INSTANCE)),
            OnCode::PersonUnproven
        );
        let mut extra = good;
        extra["networks"] = json!([]);
        assert_eq!(
            code(check_turn_on(&extra, Some(&bound(&key)), INSTANCE)),
            OnCode::Failed
        );
    }

    #[test]
    fn pickup_worker_on_takes_the_vault_from_the_linked_folder() {
        use lean_ctx::core::vault_map::{VaultMap, VaultRecord};
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let linked = dir.path().join("linked");
        let other = dir.path().join("other");
        for d in [&data, &linked, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mut map = VaultMap::default();
        map.vaults.insert(
            "stable".into(),
            VaultRecord {
                vault_id: VAULT.into(),
                ns: "me".into(),
                name: "notes".into(),
                sealed: false,
                roots: vec![linked.to_string_lossy().into_owned()],
            },
        );
        map.save(&data).unwrap();
        assert_eq!(folder_vault(&data, &linked).unwrap(), VAULT);
        let unlinked = folder_vault(&data, &other).unwrap_err();
        assert_eq!(unlinked.code, OnCode::VaultNotInOrg);
        let turn_on = proposal(
            &optin(none()),
            vec![VAULT.into()],
            None,
            INSTANCE,
            &no_names,
        )
        .unwrap()
        .unwrap();
        names_vault(&turn_on.body, VAULT).unwrap();
        let elsewhere = names_vault(&turn_on.body, "ffffffffffffffffffffffffffffffff").unwrap_err();
        assert_eq!(elsewhere.code, OnCode::VaultNotInOrg);
    }

    #[test]
    fn pickup_cloud_refusals_map_to_the_apps_codes() {
        let error = |status, body: Value| refused(status, &json!({"error": body}));
        let unproven = error(
            403,
            json!({"code": "forbidden_right", "reason": "person_unproven"}),
        );
        assert_eq!(unproven.code, OnCode::PersonUnproven);
        let vault = error(400, json!({"code": "invalid_field", "field": "vaults"}));
        assert_eq!(vault.code, OnCode::VaultNotInOrg);
        assert_eq!(vault.failure.reason, Some("vault_not_in_org"));
        let changed = error(
            409,
            json!({"code": "invalid_state", "reason": "optin_changed"}),
        );
        assert_eq!(
            (changed.code, changed.failure.code),
            (OnCode::OptinChanged, Code::InvalidState)
        );
        assert_eq!(
            error(423, json!({"code": "team_stopped"})).code,
            OnCode::TeamStopped
        );
        let bound = error(409, json!({"code": "optin_key_bound"}));
        assert_eq!(
            (bound.code, bound.failure.reason),
            (OnCode::OptinKeyBound, Some("optin_key_bound"))
        );
        let cleared = error(
            409,
            json!({"code": "conflict", "reason": "optin_key_cleared"}),
        );
        assert_eq!(
            (cleared.code, cleared.failure.reason),
            (OnCode::OptinKeyCleared, Some("optin_key_cleared"))
        );
        let enrollment = error(400, json!({"code": "token_enrollment"}));
        assert_eq!(
            (enrollment.code, enrollment.failure.reason),
            (OnCode::TokenEnrollment, Some("token_enrollment"))
        );
        let other_field = error(
            400,
            json!({"code": "invalid_field", "field": "daily_budget"}),
        );
        assert_eq!(other_field.code, OnCode::Failed);
        let doc = on_document(&Err(unproven));
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["code"], "person_unproven");
        assert!(doc["error"].is_string());
    }
}
