// SPDX-License-Identifier: Apache-2.0
//! `decisions.list` (3.12.3): the team's decisions that live in LeanCTX
//! Cloud, for a project linked to a vault, as a Local Read kind.
//!
//! The repo's own records (`.leanctx/decisions/*.toml`) are read by the app
//! from the worktree; this kind adds what only the Cloud holds. The engine
//! sends `GET /v1/vaults/{vault_id}/decisions` with its instance token (the
//! Cloud admits it with `knowledge.read`) and passes the Cloud's `items`
//! through unchanged. Nothing is written.
//!
//! - `decisions.list`: `GET /v1/local/cloud/decisions?project_root=P`, CLI
//!   `lean-ctx cloud decisions list --json --project-root P`.
//!
//! `decisions.create` (3.12.3) records a team decision in LeanCTX Cloud as
//! the person the instance is linked to: the engine sends
//! `POST /v1/decisions` (Cloud API §17.2) with its instance token, in the
//! engine's own team, with that person's role, plan and rate budgets. A
//! project linked to a vault names that vault; without a project the
//! decision is the team's. The Cloud's recorded decision is the document.
//!
//! - `decisions.create`: `POST /v1/local/cloud/decisions/create`, CLI
//!   `lean-ctx cloud decisions create --json --decision '<JSON>'
//!   [--project-root P]`.
//!
//! `decisions.send` ("Send to agents") asks LeanCTX Cloud to send one team
//! decision to the team's agents after the person's confirmed click in
//! LeanCTX Code: the engine sends `POST /v1/decisions/{id}/send` with
//! `{"confirm": true}`. App-only (`app_key`): the socket route answers only
//! a request with the app key, and the CLI has no verb for it.
//!
//! - `decisions.send`: `POST /v1/local/cloud/decisions/send`, body
//!   `{"decision_id": …, "project_root"?: …}`.
//!
//! Wired as one `local_read::Part`: its routes, kinds and commands are
//! chained into the route table and the capabilities builder.

use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use lean_ctx_cloud::transport::CloudClient;
use lean_ctx_cloud::{CloudError, CloudStore};

use super::routes::{Method, Param, Project, Route, Shape};
use super::{Answer, Code, Failure, Via};

/// The kind of the Cloud decisions' document.
pub(crate) const KIND: &str = "decisions.list";

/// The kind of a decision recorded in the Cloud.
pub(crate) const CREATE: &str = "decisions.create";

/// The kind of a decision sent to the team's agents.
pub(crate) const SEND: &str = "decisions.send";

const PROJECT_ROOT: Param = Param {
    name: "project_root",
    flag: "--project-root",
    shape: Shape::Value,
};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: KIND,
        method: Method::Get,
        path: "/v1/local/cloud/decisions",
        cli: &["cloud", "decisions", "list", "--json"],
        action: &["list"],
        params: &[PROJECT_ROOT],
        project: Project::Required,
        names: None,
    },
    Route {
        kind: CREATE,
        method: Method::Post,
        path: "/v1/local/cloud/decisions/create",
        cli: &["cloud", "decisions", "create", "--json"],
        action: &["create"],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "decision",
                flag: "--decision",
                shape: Shape::Json,
            },
        ],
        project: Project::Optional,
        names: None,
    },
    // App-only (`app_key`): no CLI words; listed as a socket verb.
    Route {
        kind: SEND,
        method: Method::Post,
        path: "/v1/local/cloud/decisions/send",
        cli: &[],
        action: &["send"],
        params: &[
            PROJECT_ROOT,
            Param {
                name: "decision_id",
                flag: "--decision-id",
                shape: Shape::Value,
            },
        ],
        project: Project::Optional,
        names: Some(("cloud.decisions", "send")),
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &[KIND, CREATE, SEND];

/// Command path → CLI verbs, as `capabilities::COMMANDS` (`send` is a
/// socket verb only).
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[("cloud.decisions", &["create", "list"])];

/// The largest decision accepted from a reader, and Cloud answer read.
const MAX_DECISION_BYTES: usize = 64 * 1024;
const MAX_ANSWER_BYTES: usize = 256 * 1024;

/// The decision fields the Cloud records (`NewDecision`, Cloud API §16),
/// besides `vault`, which the engine sets from the project.
const DECISION_FIELDS: &[&str] = &[
    "id",
    "title",
    "statement",
    "rationale",
    "decided_by",
    "decided_at",
    "source",
    "scope",
    "check",
];

/// The Cloud's decisions of the project's vault.
#[derive(Debug, Serialize)]
pub(crate) struct DecisionsList {
    pub project_root: String,
    /// `"cloud"`: the repo's own records are not in this document.
    pub source: &'static str,
    pub vault: VaultRef,
    /// The Cloud's records as it sent them (`leanctx.cloud.decisions/v1`).
    pub items: Vec<Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct VaultRef {
    /// `<ws>/<vault>`, what `cloud link --vault` takes.
    pub address: String,
    pub vault_id: String,
}

/// The document of the Cloud's answer `body` for `root` in `vault`.
pub(crate) fn decisions_doc(
    root: &str,
    vault: VaultRef,
    body: &Value,
) -> Result<DecisionsList, Failure> {
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            Failure::from(CloudError::Protocol(
                "the decisions answer has no items".to_owned(),
            ))
        })?;
    Ok(DecisionsList {
        project_root: root.to_owned(),
        source: "cloud",
        vault,
        items,
    })
}

/// Ask the connected instance's Cloud for the decisions of `root`'s vault.
fn list(root: &str) -> Result<DecisionsList, Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let record = lean_ctx_cloud::resolution::vault_for_root(store.data_dir(), Path::new(root))?
        .ok_or_else(|| {
            Failure::new(
                Code::NoVault,
                "this project is not linked to a LeanCTX Cloud vault".to_owned(),
            )
        })?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust)?;
    let path = format!("/vaults/{}/decisions", record.vault_id);
    let (status, answer) = session.with_client(|client| send_list(client, &path))?;
    let body = listed(status, answer)?;
    decisions_doc(
        root,
        VaultRef {
            address: format!("{}/{}", record.ns, record.name),
            vault_id: record.vault_id.clone(),
        },
        &body,
    )
}

/// Ask for the decisions at `path` with `client`, answering the status and
/// body even when that is an error. A `401` is an error, so the session
/// mints a new token once and then reports it.
pub(crate) fn send_list(client: &CloudClient, path: &str) -> Result<(u16, Value), CloudError> {
    let (status, answer) = client.send_answer("GET", path, None, MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, answer))
}

/// The refusal of a Cloud `upgrade_required` answer, naming its
/// `min_instance` only when that is a plain version.
pub(crate) fn upgrade_refusal(body: &Value) -> Failure {
    let min = body["error"]["min_instance"].as_str().filter(|v| {
        !v.is_empty()
            && v.len() <= 32
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
    });
    Failure::new(
        Code::Internal,
        match min {
            Some(min) => {
                format!("LeanCTX Cloud needs lean-ctx {min} or newer (run `lean-ctx update`)")
            }
            None => "LeanCTX Cloud needs a newer lean-ctx (run `lean-ctx update`)".to_owned(),
        },
    )
}

/// The body of a decisions answer, or its refusal: a policy refusal is
/// `policy_denied` with Cloud's fields; every other one keeps its meaning
/// (§26: `not_connected`, `forbidden`, `not_found`, `rate_limited`,
/// `unavailable`, `internal`).
pub(crate) fn listed(status: u16, body: Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return Ok(body);
    }
    if let Some(refusal) = policy_refusal::from_answer(&body) {
        return Err(refusal);
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    Err(match code {
        "unauthorized" | "lease_expired" | "instance_revoked" => Failure::new(
            Code::NotConnected,
            "LeanCTX Cloud no longer accepts this machine's connection; run `lean-ctx cloud connect`",
        ),
        "not_found" => Failure::new(
            Code::NotFound,
            "LeanCTX Cloud does not know this project's vault",
        ),
        "forbidden" | "forbidden_right" | "plan_required" => Failure::new(
            Code::Forbidden,
            "the person this machine is linked to may not read this vault's decisions",
        ),
        _ if status == 429 || code == "rate_limited" => {
            let message = match body["error"]["retry_after_s"].as_u64() {
                Some(s) => format!(
                    "LeanCTX Cloud asks to retry in {} s",
                    s.min(lean_ctx_cloud::error::RETRY_AFTER_CAP_S)
                ),
                None => "LeanCTX Cloud asks to retry later".to_owned(),
            };
            Failure::new(Code::RateLimited, message)
        }
        "upgrade_required" => upgrade_refusal(&body),
        _ if status == 402 || status == 403 => Failure::new(
            Code::Forbidden,
            "the person this machine is linked to may not read this vault's decisions",
        ),
        _ if status == 404 => Failure::new(Code::NotFound, "LeanCTX Cloud does not know this"),
        _ if status == 408 || status >= 500 => Failure::new(
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status})"),
        ),
        _ => Failure::new(Code::Internal, format!("LeanCTX Cloud answered {status}")),
    })
}

/// The Cloud's body for a reader's `decision` (the app's draft record):
/// its fields the Cloud records, empty ones left out. `status` may only be
/// `active` and `supersedes` only empty (a new decision supersedes nothing
/// from here); any other field is refused before anything is sent.
pub(crate) fn create_body(decision: &Value) -> Result<Map<String, Value>, Failure> {
    let record = decision
        .as_object()
        .ok_or_else(|| Failure::invalid("decision is a JSON object"))?;
    let mut body = Map::new();
    for (key, value) in record {
        match key.as_str() {
            "status" => {
                if !(value.is_null() || value.as_str() == Some("active")) {
                    return Err(Failure::invalid("a new decision's status is active"));
                }
            }
            "supersedes" => {
                let none = value.is_null() || value.as_array().is_some_and(Vec::is_empty);
                if !none {
                    return Err(Failure::invalid(
                        "a decision recorded here supersedes nothing",
                    ));
                }
            }
            field if DECISION_FIELDS.contains(&field) => {
                if !value.is_null() {
                    body.insert(key.clone(), value.clone());
                }
            }
            other => {
                let shown: String = other.chars().take(40).collect();
                return Err(Failure::invalid(format!("decision has no field {shown:?}")));
            }
        }
    }
    for field in ["title", "statement"] {
        let given = body
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
        if !given {
            return Err(Failure::invalid(format!("decision {field} is required")));
        }
    }
    Ok(body)
}

/// The reader's `--decision` and optional `--project-root`, checked.
fn create_args(args: &[String]) -> Result<(Option<String>, Map<String, Value>), Failure> {
    let root = super::flag(args, "--project-root")?
        .filter(|raw| !raw.is_empty())
        .map(super::routes::project_dir)
        .transpose()?;
    let raw =
        super::flag(args, "--decision")?.ok_or_else(|| Failure::invalid("decision is required"))?;
    if raw.len() > MAX_DECISION_BYTES {
        return Err(Failure::invalid("decision is larger than 64 KiB"));
    }
    let decision: Value =
        serde_json::from_str(raw).map_err(|_| Failure::invalid("decision is not JSON"))?;
    Ok((root, create_body(&decision)?))
}

/// The recorded decision of a Cloud answer, or its refusal with the Cloud's
/// words kept.
pub(crate) fn created(status: u16, body: &Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return match body.get("decision") {
            Some(decision @ Value::Object(_)) => Ok(decision.clone()),
            _ => Err(Failure::new(
                Code::Internal,
                "LeanCTX Cloud answered without the decision",
            )),
        };
    }
    if let Some(refusal) = policy_refusal::from_answer(body) {
        return Err(refusal);
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let said = body["error"]["message"].as_str().and_then(|message| {
        let text: String = message
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(300)
            .collect();
        let text = text.trim().to_owned();
        (!text.is_empty()).then_some(text)
    });
    let (local, fallback) = match code {
        "decision_exists" | "invalid_state" => (
            Code::Conflict,
            "the team already has this decision (or holds as many as it may)".to_owned(),
        ),
        "invalid_field" => (
            Code::InvalidArgument,
            "LeanCTX Cloud refused a field of the decision".to_owned(),
        ),
        "forbidden" | "plan_required" => (
            Code::Forbidden,
            "the person this machine is linked to may not record team decisions".to_owned(),
        ),
        "not_found" => (
            Code::NotFound,
            "LeanCTX Cloud does not know this project's vault".to_owned(),
        ),
        _ if status == 409 => (
            Code::Conflict,
            "the team already has this decision".to_owned(),
        ),
        _ if status == 400 || status == 422 => (
            Code::InvalidArgument,
            "LeanCTX Cloud refused the decision".to_owned(),
        ),
        _ if status == 402 || status == 403 => (
            Code::Forbidden,
            "the person this machine is linked to may not record team decisions".to_owned(),
        ),
        _ if status == 404 => (
            Code::NotFound,
            "LeanCTX Cloud does not know this".to_owned(),
        ),
        _ if status == 429 => (
            Code::RateLimited,
            "LeanCTX Cloud asks to retry later".to_owned(),
        ),
        _ if status >= 500 => (
            Code::Unavailable,
            format!("LeanCTX Cloud could not answer ({status})"),
        ),
        _ => (Code::Internal, format!("LeanCTX Cloud answered {status}")),
    };
    let message = match local {
        Code::RateLimited | Code::Unavailable | Code::Internal => fallback,
        _ => said.unwrap_or(fallback),
    };
    Err(Failure::new(local, message))
}

/// Send the decision `body` with `client`. A `401` is an error, so the
/// session mints a new token once and then reports it.
pub(crate) fn send_create(
    client: &CloudClient,
    body: &Map<String, Value>,
) -> Result<(u16, Value), CloudError> {
    let body = Value::Object(body.clone());
    let (status, answer) =
        client.send_answer("POST", "/decisions", Some(&body), MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, answer))
}

/// A decision recorded in the Cloud.
#[derive(Debug, Serialize)]
pub(crate) struct DecisionCreated {
    /// The project named, `None` for a decision of the team.
    pub project_root: Option<String>,
    /// The project's vault, `None` for a decision of the team.
    pub vault: Option<VaultRef>,
    /// The Cloud's record as it answered it (`origin: "cloud"`).
    pub decision: Value,
}

/// Record `body` in the connected instance's Cloud, in `root`'s vault when
/// a project is named (it must be linked to one).
fn create(root: Option<&str>, mut body: Map<String, Value>) -> Result<DecisionCreated, Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let vault = match root {
        None => None,
        Some(root) => {
            let record =
                lean_ctx_cloud::resolution::vault_for_root(store.data_dir(), Path::new(root))?
                    .ok_or_else(|| {
                        Failure::new(
                            Code::NoVault,
                            "this project is not linked to a LeanCTX Cloud vault".to_owned(),
                        )
                    })?;
            body.insert("vault".into(), Value::String(record.name.clone()));
            Some(VaultRef {
                address: format!("{}/{}", record.ns, record.name),
                vault_id: record.vault_id.clone(),
            })
        }
    };
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust)?;
    let (status, answer) = session.with_client(|client| send_create(client, &body))?;
    Ok(DecisionCreated {
        project_root: root.map(str::to_owned),
        vault,
        decision: created(status, &answer)?,
    })
}

/// The reader's `--decision-id` and optional `--project-root`, checked: a
/// decision id is 1..=128 of `[A-Za-z0-9_.-]`, so it is one path segment.
fn send_args(args: &[String]) -> Result<(Option<String>, String), Failure> {
    let root = super::flag(args, "--project-root")?
        .filter(|raw| !raw.is_empty())
        .map(super::routes::project_dir)
        .transpose()?;
    let id = super::flag(args, "--decision-id")?
        .ok_or_else(|| Failure::invalid("decision_id is required"))?;
    // The Cloud's own rule for a decision id: an id it would not know is
    // never sent.
    if !lean_ctx_cloud::bus::plan_events::decision_id_ok(id) {
        return Err(Failure::invalid(
            "decision_id is `dec-` and lowercase letters or digits joined by single '-', at most 80 characters",
        ));
    }
    Ok((root, id.to_owned()))
}

/// Ask the Cloud to send decision `id` to the team's agents with `client`:
/// always the person's confirmation `{"confirm": true}`. A `401` is an
/// error, so the session mints a new token once and then reports it.
pub(crate) fn send_send(client: &CloudClient, id: &str) -> Result<(u16, Value), CloudError> {
    let body = serde_json::json!({"confirm": true});
    let path = format!("/decisions/{id}/send");
    let (status, answer) = client.send_answer("POST", &path, Some(&body), MAX_ANSWER_BYTES)?;
    if status == 401 {
        return Err(CloudError::Unauthorized);
    }
    Ok((status, answer))
}

/// A decision sent to the team's agents.
#[derive(Debug, Serialize)]
pub(crate) struct DecisionSent {
    /// The project named, `None` without one.
    pub project_root: Option<String>,
    pub decision_id: String,
    /// The Cloud's answer as it sent it.
    pub sent: Value,
}

/// The Cloud's answer to a send, or its refusal (as `created`'s).
pub(crate) fn sent(status: u16, body: &Value) -> Result<Value, Failure> {
    if (200..300).contains(&status) {
        return match body {
            Value::Object(_) => Ok(body.clone()),
            _ => Err(Failure::new(
                Code::Internal,
                "LeanCTX Cloud answered without a document",
            )),
        };
    }
    if body["error"]["code"].as_str() == Some("step_up_required") {
        return Err(Failure::new(
            Code::Forbidden,
            "LeanCTX Cloud asks the person to confirm this on the LeanCTX Cloud web",
        )
        .with_reason("step_up_required"));
    }
    Err(match created(status, body) {
        Err(failure) => failure,
        Ok(_) => Failure::new(Code::Internal, format!("LeanCTX Cloud answered {status}")),
    })
}

/// Send decision `id` with the connected instance's Cloud.
fn send(root: Option<String>, id: String) -> Result<DecisionSent, Failure> {
    let store = CloudStore::open()?;
    let instance = store.instance()?.ok_or(CloudError::NotConnected)?;
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let trust = lean_ctx_cloud::root_cert::trust_root(&store, &config, &instance.url)?;
    let session = lean_ctx_cloud::auth::Session::load(store, trust)?;
    let (status, answer) = session.with_client(|client| send_send(client, &id))?;
    Ok(DecisionSent {
        project_root: root,
        decision_id: id,
        sent: sent(status, &answer)?,
    })
}

/// The released document of `kind`, for the CLI or the socket. A new
/// decision goes through the release point before anything is sent.
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    match kind {
        KIND => {
            let root = match super::project_root(args) {
                Ok(root) => root,
                Err(failure) => return Answer::failure(&failure),
            };
            super::answer_released("ctx_knowledge", &root, kind, via, || list(&root))
        }
        CREATE => {
            let (root, body) = match create_args(args) {
                Ok(parts) => parts,
                Err(failure) => return Answer::failure(&failure),
            };
            let preview = Value::Object(body.clone());
            let tool_root = root.clone().unwrap_or_default();
            super::answer_released_write("ctx_knowledge", &tool_root, kind, via, &preview, || {
                create(root.as_deref(), body)
            })
        }
        // App-only: the socket checked the app key before this; the CLI
        // never sends.
        SEND if via == Via::Cli => Answer::failure(&super::app_key::refusal()),
        SEND => {
            let (root, id) = match send_args(args) {
                Ok(parts) => parts,
                Err(failure) => return Answer::failure(&failure),
            };
            let preview = serde_json::json!({"decision_id": id, "confirm": true});
            let tool_root = root.clone().unwrap_or_default();
            super::answer_released_write("ctx_knowledge", &tool_root, kind, via, &preview, || {
                send(root, id)
            })
        }
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx cloud decisions list|create --json …`: the Local Read
/// document; `false` for every other command line.
pub(crate) fn try_run(command: &str, rest: &[String]) -> bool {
    let words: Vec<&str> = rest.iter().map(String::as_str).take(2).collect();
    let kind = match words.as_slice() {
        ["decisions", "list"] => KIND,
        ["decisions", "create"] => CREATE,
        _ => return false,
    };
    if command != "cloud" || !super::wants_json(rest) {
        return false;
    }
    super::print(&answer(kind, &rest[1..], Via::Cli));
    true
}

/// LeanCTX Cloud's policy refusals as the Local Read error `policy_denied`.
///
/// Cloud enforces its sharing policy and answers a refused request with
/// `403 {"error":{"code":"policy_denied","message":…,"member":…,"level":…}}`
/// (`member` and `level` in the error object itself, `action` where a person
/// must confirm), `409 non_mergeable` when the policy chain does not merge
/// (`field` names the member), or `403 sharing_not_confirmed`. Each is a
/// refusal by policy, never "forbidden" in general, "no rules" or an internal
/// error: the error document's code is `policy_denied`, and its error object
/// carries `cloud_code`, `member`, `level` and `action` as Cloud sent them
/// (each only when it is a plain token). The message is LeanCTX's own text.
pub(crate) mod policy_refusal {
    use serde_json::{Map, Value};

    use super::super::{Code, Failure};

    /// Cloud's code of a policy refusal (`403`).
    pub(crate) const POLICY_DENIED: &str = "policy_denied";
    /// Cloud's code when the policy chain does not merge (`409`).
    pub(crate) const NON_MERGEABLE: &str = "non_mergeable";
    /// Cloud's code when sharing is not confirmed yet (`403`).
    pub(crate) const SHARING_NOT_CONFIRMED: &str = "sharing_not_confirmed";

    /// A value of Cloud kept as data: `[a-z0-9_.-]`, 1..=`max` bytes.
    fn token(value: Option<&str>, max: usize) -> Option<&str> {
        value.filter(|s| {
            (1..=max).contains(&s.len())
                && s.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
                })
        })
    }

    /// The policy refusal of a Cloud answer, `None` for every other answer.
    pub(crate) fn from_answer(body: &Value) -> Option<Failure> {
        let error = body.get("error")?;
        let code = error.get("code")?.as_str()?;
        // Top level in the error object; an older answer may nest them.
        let field = |name: &str| {
            error
                .get(name)
                .and_then(Value::as_str)
                .or_else(|| error.get("details")?.get(name)?.as_str())
        };
        match code {
            POLICY_DENIED | SHARING_NOT_CONFIRMED => Some(denied(
                code,
                field("member").or_else(|| field("field")),
                field("level"),
                field("action"),
            )),
            NON_MERGEABLE => Some(denied(
                code,
                field("field").or_else(|| field("member")),
                field("level"),
                None,
            )),
            _ => None,
        }
    }

    /// What a policy member refuses, in LeanCTX's words.
    fn reason(member: &str) -> Option<&'static str> {
        Some(match member {
            "sharing.allowed_vaults" => "this vault is not among the vaults the policy allows",
            "sharing.task_claim_min_role" => "the policy requires a higher role for this",
            "sharing.consent_required_for" => "the policy requires a person to confirm this",
            "sharing.cross_team_sharing" => "the policy does not allow sharing across teams",
            "sharing.personal_workspace" => "the policy does not allow this for a personal space",
            _ => return None,
        })
    }

    /// Who set the policy, in LeanCTX's words (Cloud's `level` stays data).
    fn level_word(level: &str) -> Option<&'static str> {
        Some(match level {
            "org" => "organization",
            "workspace" => "team",
            "vault" => "vault",
            "instance" => "machine",
            _ => return None,
        })
    }

    /// The `policy_denied` failure of Cloud's `cloud_code` with its fields.
    pub(crate) fn denied(
        cloud_code: &str,
        member: Option<&str>,
        level: Option<&str>,
        action: Option<&str>,
    ) -> Failure {
        let member = token(member, 80);
        let level = token(level, 16);
        let action = token(action, 64);
        let mut message = match cloud_code {
            NON_MERGEABLE => {
                "LeanCTX Cloud refuses this: its policy layers do not merge into one policy"
                    .to_owned()
            }
            SHARING_NOT_CONFIRMED => {
                "LeanCTX Cloud refuses this: sharing is not confirmed for this project yet"
                    .to_owned()
            }
            _ => match member.and_then(reason) {
                Some(why) => format!("LeanCTX Cloud policy refuses this: {why}"),
                None => "LeanCTX Cloud policy refuses this".to_owned(),
            },
        };
        if let Some(member) = member {
            message.push_str(&format!(" (policy {member}"));
            if let Some(word) = level.and_then(level_word) {
                message.push_str(&format!(", set at the {word} level"));
            }
            message.push(')');
        }
        match action {
            Some(action) => message.push_str(&format!(
                "; a person confirms it on the LeanCTX Cloud web ({})",
                action.replace('_', " ")
            )),
            None if cloud_code == SHARING_NOT_CONFIRMED => {
                message.push_str("; a person confirms sharing on the LeanCTX Cloud web");
            }
            None => message
                .push_str("; it is not retried until an admin changes the policy in LeanCTX Cloud"),
        }
        let mut detail = Map::new();
        detail.insert("cloud_code".into(), cloud_code.to_owned().into());
        for (key, value) in [("member", member), ("level", level), ("action", action)] {
            if let Some(value) = value {
                detail.insert(key.into(), value.to_owned().into());
            }
        }
        Failure::new(Code::PolicyDenied, message).with_detail(detail)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        fn document(failure: &Failure) -> Value {
            serde_json::from_str(&super::super::super::error_document(failure)).unwrap()
        }

        #[test]
        fn policy_denied_keeps_member_and_level_as_data() {
            let body = json!({"error": {
                "code": "policy_denied",
                "message": "vault not allowed by workspace policy",
                "member": "sharing.allowed_vaults",
                "level": "workspace"
            }});
            let failure = from_answer(&body).expect("a policy refusal");
            assert_eq!(failure.code, Code::PolicyDenied);
            assert_eq!(
                failure.message,
                "LeanCTX Cloud policy refuses this: this vault is not among the vaults the policy allows (policy sharing.allowed_vaults, set at the team level); it is not retried until an admin changes the policy in LeanCTX Cloud"
            );
            assert!(
                !failure.message.contains("workspace"),
                "{}",
                failure.message
            );
            let doc = document(&failure);
            assert_eq!(
                doc,
                json!({"contract": "local-read/1", "kind": "error", "error": {
                    "code": "policy_denied",
                    "message": failure.message,
                    "cloud_code": "policy_denied",
                    "member": "sharing.allowed_vaults",
                    "level": "workspace"
                }})
            );
            // Byte-stable (#498): the same answer gives the same document.
            assert_eq!(
                super::super::super::error_document(&failure),
                super::super::super::error_document(&from_answer(&body).unwrap())
            );
        }

        #[test]
        fn every_policy_member_and_level_is_named() {
            for member in [
                "sharing.allowed_vaults",
                "sharing.task_claim_min_role",
                "sharing.consent_required_for",
                "sharing.cross_team_sharing",
                "sharing.personal_workspace",
            ] {
                assert!(reason(member).is_some(), "{member}");
                for level in ["org", "workspace", "vault", "instance"] {
                    let failure = denied(POLICY_DENIED, Some(member), Some(level), None);
                    let doc = document(&failure);
                    assert_eq!(doc["error"]["member"], member);
                    assert_eq!(doc["error"]["level"], level);
                    // Our words never say "workspace"; the member path is data.
                    let ours = failure.message.replace(member, "");
                    assert!(!ours.contains("workspace"), "{}", failure.message);
                    assert!(
                        !ours.to_lowercase().contains("instance"),
                        "{}",
                        failure.message
                    );
                }
            }
        }

        #[test]
        fn a_consent_refusal_names_the_action() {
            let failure = from_answer(&json!({"error": {
                "code": "policy_denied",
                "member": "sharing.consent_required_for",
                "level": "org",
                "action": "first_share"
            }}))
            .unwrap();
            assert!(
                failure
                    .message
                    .ends_with("; a person confirms it on the LeanCTX Cloud web (first share)"),
                "{}",
                failure.message
            );
            assert_eq!(document(&failure)["error"]["action"], "first_share");
        }

        #[test]
        fn non_mergeable_and_sharing_not_confirmed_are_policy_refusals() {
            let merge = from_answer(&json!({"error": {
                "code": "non_mergeable",
                "message": "policy chain does not merge",
                "field": "sharing.allowed_vaults"
            }}))
            .unwrap();
            assert_eq!(merge.code, Code::PolicyDenied);
            assert!(merge.message.contains("do not merge"), "{}", merge.message);
            assert!(!merge.message.contains("no rules"), "{}", merge.message);
            let doc = document(&merge);
            assert_eq!(doc["error"]["cloud_code"], "non_mergeable");
            assert_eq!(doc["error"]["member"], "sharing.allowed_vaults");

            let unconfirmed = from_answer(&json!({"error": {
                "code": "sharing_not_confirmed",
                "message": "sharing not confirmed"
            }}))
            .unwrap();
            assert_eq!(unconfirmed.code, Code::PolicyDenied);
            assert_eq!(
                unconfirmed.message,
                "LeanCTX Cloud refuses this: sharing is not confirmed for this project yet; a person confirms sharing on the LeanCTX Cloud web"
            );
            assert_eq!(
                document(&unconfirmed)["error"]["cloud_code"],
                "sharing_not_confirmed"
            );
        }

        #[test]
        fn fields_that_are_not_plain_tokens_are_dropped() {
            let failure = from_answer(&json!({"error": {
                "code": "policy_denied",
                "member": "sharing.allowed_vaults\u{1b}[31m",
                "level": "Bearer abc"
            }}))
            .unwrap();
            let doc = document(&failure);
            assert!(doc["error"].get("member").is_none(), "{doc}");
            assert!(doc["error"].get("level").is_none(), "{doc}");
            assert_eq!(
                failure.message,
                "LeanCTX Cloud policy refuses this; it is not retried until an admin changes the policy in LeanCTX Cloud"
            );
        }

        #[test]
        fn other_answers_are_not_policy_refusals() {
            assert!(from_answer(&json!({"error": {"code": "forbidden"}})).is_none());
            assert!(from_answer(&json!("<html>")).is_none());
            assert!(from_answer(&json!({"items": []})).is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_owned()).collect()
    }

    fn vault() -> VaultRef {
        VaultRef {
            address: "acme/platform".to_owned(),
            vault_id: "vlt_1".to_owned(),
        }
    }

    #[test]
    fn decisions_are_dispatched_routed_and_need_a_project() {
        assert!(super::super::part_of(KIND).is_some(), "{KIND} has no part");
        let text = super::super::routes::table_json().to_string();
        assert!(
            text.contains("/v1/local/cloud/decisions"),
            "the route is listed: {text}"
        );
        let route = super::super::routes::all()
            .find(|r| r.kind == KIND)
            .expect("routed");
        assert!(matches!(route.method, Method::Get));
        assert!(matches!(route.project, Project::Required));
    }

    #[test]
    fn the_document_passes_the_clouds_items_through() {
        let body = json!({
            "schema": "leanctx.cloud.decisions/v1",
            "vault_id": "vlt_1",
            "items": [{"id": "dec-use-postgres", "title": "Use Postgres", "status": "accepted"}]
        });
        let doc = decisions_doc("/work/app", vault(), &body).expect("doc");
        let text = super::super::document(KIND, &doc);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["project_root"], "/work/app");
        assert_eq!(value["source"], "cloud");
        assert_eq!(value["vault"]["address"], "acme/platform");
        assert_eq!(value["items"][0]["id"], "dec-use-postgres");
    }

    #[test]
    fn an_answer_without_items_is_refused() {
        let failure =
            decisions_doc("/work/app", vault(), &json!({"schema": "x"})).expect_err("no items");
        assert_eq!(failure.code, Code::Internal);
    }

    #[test]
    fn without_a_connection_nothing_is_sent_and_the_answer_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let failure = list("/work/app").expect_err("no instance here");
        assert_eq!(failure.code, Code::NotConnected);
    }

    #[test]
    fn only_the_decisions_list_command_line_is_taken() {
        assert!(
            !try_run("cloud", &words(&["decisions", "list"])),
            "needs --json"
        );
        assert!(!try_run("cloud", &words(&["docs", "list", "--json"])));
        assert!(!try_run("decisions", &words(&["list", "--json"])));
        assert!(
            !try_run("cloud", &words(&["decisions", "create"])),
            "needs --json"
        );
        assert!(!try_run("decisions", &words(&["create", "--json"])));
    }

    /// The draft LeanCTX Code posts (`draftJson` of the app's decisions).
    fn draft() -> Value {
        json!({
            "id": "use-postgres",
            "title": "Use Postgres",
            "statement": "Every service stores its data in Postgres.",
            "rationale": null,
            "status": "active",
            "supersedes": [],
            "decided_by": "team",
            "decided_at": "2026-11-16",
            "source": null,
            "scope": {"repos": ["acme/app"], "paths": ["src/db/**"], "symbols": []}
        })
    }

    /// One HTTP exchange on a loopback port: the request as text, and the
    /// answer `status` with `body` (as LeanCTX Cloud sends it).
    fn serve_once(status: u16, body: &str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nCloud-Protocol: 2.2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        });
        (origin, handle)
    }

    #[test]
    fn create_is_dispatched_routed_and_takes_an_optional_project() {
        assert!(
            super::super::part_of(CREATE).is_some(),
            "{CREATE} has no part"
        );
        let route = super::super::routes::all()
            .find(|r| r.kind == CREATE)
            .expect("routed");
        assert!(matches!(route.method, Method::Post));
        assert!(matches!(route.project, Project::Optional));
        let text = super::super::routes::table_json().to_string();
        assert!(text.contains("/v1/local/cloud/decisions/create"), "{text}");
    }

    #[test]
    fn the_apps_draft_becomes_the_clouds_body() {
        let route = super::super::routes::all()
            .find(|r| r.kind == CREATE)
            .unwrap();
        // As the app posts it (`create_cloud`): no project is `""`.
        let inputs: Vec<(String, Value)> = vec![
            ("project_root".to_owned(), json!("")),
            ("decision".to_owned(), draft()),
        ];
        let args = super::super::routes::args_of(route, None, &inputs).unwrap();
        let (root, body) = create_args(&args).unwrap();
        assert_eq!(root, None);
        assert_eq!(
            Value::Object(body),
            json!({
                "id": "use-postgres",
                "title": "Use Postgres",
                "statement": "Every service stores its data in Postgres.",
                "decided_by": "team",
                "decided_at": "2026-11-16",
                "scope": {"repos": ["acme/app"], "paths": ["src/db/**"], "symbols": []}
            })
        );
    }

    #[test]
    fn a_draft_the_cloud_would_not_record_is_refused_before_anything_is_sent() {
        let refused = |change: Value| {
            let mut d = draft();
            for (k, v) in change.as_object().unwrap() {
                d[k] = v.clone();
            }
            create_body(&d).expect_err("refused").code
        };
        assert_eq!(refused(json!({"status": "revoked"})), Code::InvalidArgument);
        assert_eq!(
            refused(json!({"supersedes": ["old"]})),
            Code::InvalidArgument
        );
        assert_eq!(refused(json!({"vault": "other"})), Code::InvalidArgument);
        assert_eq!(refused(json!({"title": " "})), Code::InvalidArgument);
        assert_eq!(refused(json!({"statement": null})), Code::InvalidArgument);
        assert_eq!(
            create_body(&json!("x")).expect_err("no object").code,
            Code::InvalidArgument
        );
        let bad_json = create_args(&words(&["create", "--json", "--decision", "{"]));
        assert_eq!(bad_json.expect_err("not json").code, Code::InvalidArgument);
        let missing = create_args(&words(&["create", "--json"]));
        assert_eq!(missing.expect_err("required").code, Code::InvalidArgument);
    }

    #[test]
    fn a_new_decision_is_posted_and_the_clouds_record_is_the_document() {
        let (origin, server) = serve_once(
            201,
            r#"{"decision":{"id":"use-postgres","title":"Use Postgres","origin":"cloud","status":"active"}}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let mut body = create_body(&draft()).unwrap();
        body.insert("vault".into(), json!("platform"));
        let (status, answer) = send_create(&client, &body).expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/decisions HTTP/1.1\r\n"),
            "{request}"
        );
        let sent: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(sent["vault"], "platform");
        assert_eq!(sent["title"], "Use Postgres");
        assert!(sent.get("status").is_none() && sent.get("supersedes").is_none());
        let doc = DecisionCreated {
            project_root: Some("/work/app".to_owned()),
            vault: Some(vault()),
            decision: created(status, &answer).unwrap(),
        };
        let value: Value = serde_json::from_str(&super::super::document(CREATE, &doc)).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], CREATE);
        assert_eq!(value["vault"]["address"], "acme/platform");
        assert_eq!(value["decision"]["origin"], "cloud");
    }

    #[test]
    fn cloud_refusals_keep_their_reason() {
        let refused = |status: u16, error: Value| {
            created(status, &json!({ "error": error })).expect_err("refused")
        };
        let taken = refused(
            409,
            json!({"code": "decision_exists", "message": "the team already has a decision use-postgres"}),
        );
        assert_eq!(taken.code, Code::Conflict);
        assert_eq!(
            taken.message,
            "the team already has a decision use-postgres"
        );
        assert_eq!(
            refused(
                409,
                json!({"code": "invalid_state", "state": "decision_limit"})
            )
            .code,
            Code::Conflict
        );
        assert_eq!(
            refused(400, json!({"code": "invalid_field", "field": "decided_at"})).code,
            Code::InvalidArgument
        );
        assert_eq!(
            refused(403, json!({"code": "forbidden"})).code,
            Code::Forbidden
        );
        assert_eq!(
            refused(402, json!({"code": "plan_required"})).code,
            Code::Forbidden
        );
        assert_eq!(
            refused(404, json!({"code": "not_found"})).code,
            Code::NotFound
        );
        assert_eq!(
            refused(429, json!({"code": "rate_limited"})).code,
            Code::RateLimited
        );
        assert_eq!(
            created(502, &json!("<html>")).expect_err("down").code,
            Code::Unavailable
        );
        assert_eq!(
            created(201, &json!({})).expect_err("empty").code,
            Code::Internal
        );
    }

    /// The policy refusal p37 sends (`policy_enforce.rs::denied`).
    const POLICY_DENIED_BODY: &str = r#"{"error":{"code":"policy_denied","message":"vault is not allowed by the workspace policy","member":"sharing.allowed_vaults","level":"workspace"}}"#;

    fn error_doc(failure: &Failure) -> Value {
        serde_json::from_str(&super::super::error_document(failure)).unwrap()
    }

    #[test]
    fn a_policy_refused_create_is_policy_denied_with_its_fields() {
        let (origin, server) = serve_once(403, POLICY_DENIED_BODY);
        let client = CloudClient::new(&origin).unwrap();
        let mut body = create_body(&draft()).unwrap();
        body.insert("vault".into(), json!("platform"));
        let (status, answer) = send_create(&client, &body).expect("answered");
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /v1/decisions HTTP/1.1\r\n"));
        let failure = created(status, &answer).expect_err("refused by policy");
        assert_eq!(failure.code, Code::PolicyDenied, "never plain forbidden");
        let doc = error_doc(&failure);
        assert_eq!(doc["error"]["code"], "policy_denied");
        assert_eq!(doc["error"]["cloud_code"], "policy_denied");
        assert_eq!(doc["error"]["member"], "sharing.allowed_vaults");
        assert_eq!(doc["error"]["level"], "workspace");
        assert!(
            !failure.message.contains("workspace"),
            "{}",
            failure.message
        );
        // A plain `forbidden` stays what it was.
        assert_eq!(
            created(403, &json!({"error": {"code": "forbidden"}}))
                .expect_err("forbidden")
                .code,
            Code::Forbidden
        );
    }

    #[test]
    fn a_policy_refused_list_is_policy_denied_never_internal() {
        let (origin, server) = serve_once(403, POLICY_DENIED_BODY);
        let client = CloudClient::new(&origin).unwrap();
        let (status, answer) = send_list(&client, "/vaults/vlt_1/decisions").expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /v1/vaults/vlt_1/decisions HTTP/1.1\r\n"),
            "{request}"
        );
        let failure = listed(status, answer).expect_err("refused by policy");
        assert_eq!(failure.code, Code::PolicyDenied);
        let doc = error_doc(&failure);
        assert_eq!(doc["error"]["member"], "sharing.allowed_vaults");
        assert_eq!(doc["error"]["level"], "workspace");
    }

    #[test]
    fn non_mergeable_and_unconfirmed_sharing_are_refusals_on_list_and_create() {
        for (status, body, cloud_code) in [
            (
                409,
                r#"{"error":{"code":"non_mergeable","message":"the policy chain does not merge","field":"sharing.cross_team_sharing"}}"#,
                "non_mergeable",
            ),
            (
                403,
                r#"{"error":{"code":"sharing_not_confirmed","message":"sharing is not confirmed"}}"#,
                "sharing_not_confirmed",
            ),
        ] {
            let (origin, server) = serve_once(status, body);
            let client = CloudClient::new(&origin).unwrap();
            let (got, answer) = send_list(&client, "/vaults/vlt_1/decisions").expect("answered");
            server.join().unwrap();
            let list = listed(got, answer.clone()).expect_err("refused");
            let create = created(got, &answer).expect_err("refused");
            for failure in [&list, &create] {
                assert_eq!(failure.code, Code::PolicyDenied, "{cloud_code}");
                assert_eq!(error_doc(failure)["error"]["cloud_code"], cloud_code);
                assert!(!failure.message.contains("no rules"), "{}", failure.message);
            }
            assert_eq!(list, create, "one refusal, one document");
        }
    }

    #[test]
    fn a_listed_answer_and_other_refusals_keep_their_meaning() {
        let (origin, server) = serve_once(
            200,
            r#"{"schema":"leanctx.cloud.decisions/v1","vault_id":"vlt_1","items":[{"id":"dec-a"}]}"#,
        );
        let client = CloudClient::new(&origin).unwrap();
        let (status, answer) = send_list(&client, "/vaults/vlt_1/decisions").expect("answered");
        server.join().unwrap();
        let body = listed(status, answer).expect("listed");
        let doc = decisions_doc("/work/app", vault(), &body).expect("doc");
        assert_eq!(doc.items[0]["id"], "dec-a");
        assert_eq!(
            listed(404, json!({"error": {"code": "not_found"}}))
                .expect_err("gone")
                .code,
            Code::NotFound
        );
        assert_eq!(
            listed(503, json!({"error": {"code": "unavailable"}}))
                .expect_err("down")
                .code,
            Code::Unavailable
        );
        for (status, body, code) in [
            (
                403,
                json!({"error": {"code": "forbidden_right"}}),
                Code::Forbidden,
            ),
            (
                403,
                json!({"error": {"code": "instance_revoked"}}),
                Code::NotConnected,
            ),
            (
                429,
                json!({"error": {"code": "server_busy"}}),
                Code::RateLimited,
            ),
            (408, json!({}), Code::Unavailable),
            (400, json!({}), Code::Internal),
        ] {
            let failure = listed(status, body).expect_err("refused");
            assert_eq!(failure.code, code, "{status}");
            assert!(
                !failure.message.to_lowercase().contains("instance"),
                "{}",
                failure.message
            );
        }
        // A rate limit names its seconds (§ errors), capped like CloudError.
        let held = listed(
            429,
            json!({"error": {"code": "rate_limited", "retry_after_s": 30}}),
        )
        .expect_err("held");
        assert_eq!(held.code, Code::RateLimited);
        assert_eq!(held.message, "LeanCTX Cloud asks to retry in 30 s");
        let capped = listed(
            429,
            json!({"error": {"code": "rate_limited", "retry_after_s": u64::MAX}}),
        )
        .expect_err("held");
        assert_eq!(
            capped.message,
            format!(
                "LeanCTX Cloud asks to retry in {} s",
                lean_ctx_cloud::error::RETRY_AFTER_CAP_S
            )
        );
        let old = listed(
            426,
            json!({"error": {"code": "upgrade_required", "min_instance": "3.12.8"}}),
        )
        .expect_err("old");
        assert_eq!(
            old.message,
            "LeanCTX Cloud needs lean-ctx 3.12.8 or newer (run `lean-ctx update`)"
        );
        let shady = listed(
            426,
            json!({"error": {"code": "upgrade_required", "min_instance": "1\u{1b}[2J"}}),
        )
        .expect_err("old");
        assert_eq!(
            shady.message,
            "LeanCTX Cloud needs a newer lean-ctx (run `lean-ctx update`)"
        );
    }

    #[test]
    fn send_is_a_socket_verb_only() {
        let route = super::super::routes::all()
            .find(|r| r.kind == SEND)
            .expect("routed");
        assert!(matches!(route.method, Method::Post));
        assert!(route.cli.is_empty(), "no CLI words");
        assert_eq!(route.names, Some(("cloud.decisions", "send")));
        assert_eq!(COMMANDS, &[("cloud.decisions", &["create", "list"][..])]);
        assert!(!try_run(
            "cloud",
            &words(&["decisions", "send", "--json", "--decision-id", "dec-1"])
        ));
        let answer = super::super::kinds::answer(
            SEND,
            &words(&["send", "--json", "--decision-id", "dec-1"]),
            Via::Cli,
        );
        assert_eq!(answer.failed, Some(Code::AppOnly));
    }

    #[test]
    fn a_decision_id_follows_the_clouds_rule() {
        let route = super::super::routes::all()
            .find(|r| r.kind == SEND)
            .unwrap();
        let args_for = |id: Value| {
            super::super::routes::args_of(route, None, &[("decision_id".to_owned(), id)])
        };
        let (root, id) = send_args(&args_for(json!("dec-0042")).unwrap()).unwrap();
        assert_eq!((root, id.as_str()), (None, "dec-0042"));
        let longest = format!("dec-{}", "a".repeat(76));
        for good in ["dec-1", "dec-use-postgres-2", longest.as_str()] {
            assert!(send_args(&args_for(json!(good)).unwrap()).is_ok(), "{good}");
        }
        // The Cloud's id rule: what it would answer 404 for is never sent.
        let long = format!("dec-{}", "a".repeat(77));
        for bad in [
            "",
            "..",
            "a/b",
            "a?b",
            "dec 1",
            "dec-",
            "dec--1",
            "dec-1-",
            "DEC-1",
            "dec-A1",
            "dec_1",
            "dec-1.2",
            "decision-1",
            &long,
        ] {
            let refused = args_for(json!(bad)).and_then(|args| send_args(&args));
            assert_eq!(
                refused.expect_err("refused").code,
                Code::InvalidArgument,
                "{bad:?}"
            );
        }
        assert_eq!(
            send_args(&words(&["send", "--json"]))
                .expect_err("required")
                .code,
            Code::InvalidArgument
        );
    }

    #[test]
    fn send_always_confirms_and_names_the_decision() {
        let (origin, server) = serve_once(200, r#"{"decision_id":"dec-0042","sent":true}"#);
        let client = CloudClient::new(&origin).unwrap();
        let (status, answer) = send_send(&client, "dec-0042").expect("answered");
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/decisions/dec-0042/send HTTP/1.1\r\n"),
            "{request}"
        );
        let body: Value =
            serde_json::from_str(&request[request.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body, json!({"confirm": true}));
        assert_eq!(sent(status, &answer).unwrap()["sent"], true);
        let step_up = sent(403, &json!({"error": {"code": "step_up_required"}})).unwrap_err();
        assert_eq!(step_up.reason, Some("step_up_required"));
        assert_eq!(
            sent(404, &json!({"error": {"code": "not_found"}}))
                .unwrap_err()
                .code,
            Code::NotFound
        );
        assert_eq!(sent(429, &json!({})).unwrap_err().code, Code::RateLimited);
    }

    #[test]
    fn create_without_a_connection_sends_nothing_and_is_not_connected() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let body = create_body(&draft()).unwrap();
        assert_eq!(
            create(None, body).expect_err("no instance").code,
            Code::NotConnected
        );
    }
}
