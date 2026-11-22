// SPDX-License-Identifier: Apache-2.0
//! The plan body `leanctx.plan-body/v1` (contract §37.3) and its publish
//! (`lean-ctx cloud plan publish`, §37.10).
//!
//! Cloud keeps a plan's body encrypted at rest and serves it as plain text
//! (`body`, standard base64 with padding) next to its signed digest. An
//! engine opens the body only after the digest and the signature checked
//! out ([`super::plan_events::verify`]); the goal and the briefs an agent
//! reads come from that verified body alone, the document's `sessions`
//! only route.
//!
//! Publishing is LeanCTX Code's: the engine serializes the body in
//! schema order, computes its digest and each session's `brief_sha256`, and
//! posts it over the vault's connection. Cloud assigns the version. No
//! plan text is written to argv, a log or a local file on the way.

use std::collections::BTreeSet;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use super::plan_events::{ALL_SLOT, plan_id_ok, slot_ok, task_id_ok};

/// `$id` of the body schema.
pub const PLAN_BODY_SCHEMA: &str = "leanctx.plan-body/v1";
/// Schema of the publish request (Cloud route `POST …/plans`).
pub const PLAN_PUBLISH_SCHEMA: &str = "leanctx.cloud.plan-publish/v1";
/// Schema of Cloud's `201` answer to a publish.
pub const PLAN_PUBLISHED_SCHEMA: &str = "leanctx.cloud.plan-published/v1";
/// Decoded body bytes of a plan document (1 B–256 KiB).
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Bytes of a publish request.
const MAX_REQUEST_BYTES: usize = 400 * 1024;
/// Bytes of Cloud's answer to a publish.
const MAX_ANSWER_BYTES: usize = 64 * 1024;
const MAX_TITLE_CHARS: usize = 200;
const MAX_GOAL_CHARS: usize = 4_000;
const MAX_ROLE_CHARS: usize = 64;
const MAX_BRIEF_CHARS: usize = 8_000;
const MAX_FOLDER_CHARS: usize = 512;
const MAX_SESSIONS: usize = 32;

/// One session of the body: the slot, the role and the brief an agent of
/// that slot reads, and an optional folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodySession {
    pub slot: String,
    pub role: String,
    pub brief: String,
    #[serde(default)]
    pub folder: Option<String>,
}

/// A plan body `leanctx.plan-body/v1`, checked ([`PlanBody::parse`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBody {
    /// `leanctx.plan-body/v1`; a body without it is read as v1, a publish
    /// always writes it.
    #[serde(default = "plan_body_schema")]
    pub schema: String,
    pub title: String,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<Vec<String>>,
    pub sessions: Vec<BodySession>,
}

fn plan_body_schema() -> String {
    PLAN_BODY_SCHEMA.to_owned()
}

/// `sha256:<64 hex>` of `bytes`.
pub fn sha256_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Text an agent reads: within `max` characters, no control character but
/// newline and tab, no invisible or bidi character; `required` text is not
/// blank. The same rules as a release (`DirectiveStore::release_text`).
fn text_ok(field: &str, text: &str, max: usize, required: bool) -> Result<(), String> {
    if required && text.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    if text.chars().count() > max {
        return Err(format!("{field} is longer than {max} characters"));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(format!("{field} carries a control character"));
    }
    if text.chars().any(lean_ctx_protocol::cloud::invisible) {
        return Err(format!("{field} carries an invisible or bidi character"));
    }
    Ok(())
}

/// `^dec-[A-Za-z0-9-]{1,64}$`
fn decision_ok(id: &str) -> bool {
    id.strip_prefix("dec-").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

impl PlanBody {
    /// The body of `bytes` (UTF-8 JSON), checked against the schema: no
    /// unknown field, the length limits, unique slots (lowercase,
    /// `[a-z0-9-]`, never `all`), decision ids, readable text.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let body: Self = serde_json::from_slice(bytes).map_err(|e| format!("plan body: {e}"))?;
        body.check()?;
        Ok(body)
    }

    fn check(&self) -> Result<(), String> {
        if self.schema != PLAN_BODY_SCHEMA {
            return Err(format!("schema is not {PLAN_BODY_SCHEMA}"));
        }
        text_ok("title", &self.title, MAX_TITLE_CHARS, true)?;
        text_ok("goal", &self.goal, MAX_GOAL_CHARS, true)?;
        if let Some(decisions) = &self.decisions
            && let Some(bad) = decisions.iter().find(|d| !decision_ok(d))
        {
            return Err(format!("decisions: {bad:?} is not a decision id"));
        }
        if self.sessions.len() > MAX_SESSIONS {
            return Err(format!("more than {MAX_SESSIONS} sessions"));
        }
        let mut slots = BTreeSet::new();
        for session in &self.sessions {
            if !slot_ok(&session.slot) {
                return Err(format!(
                    "slot {:?}: 1–32 of [a-z0-9-], and not {ALL_SLOT:?}",
                    session.slot
                ));
            }
            if !slots.insert(session.slot.as_str()) {
                return Err(format!("slot {:?} is used twice", session.slot));
            }
            let at = |field: &str| format!("sessions[{}].{field}", session.slot);
            text_ok(&at("role"), &session.role, MAX_ROLE_CHARS, true)?;
            text_ok(&at("brief"), &session.brief, MAX_BRIEF_CHARS, true)?;
            if let Some(folder) = &session.folder {
                text_ok(&at("folder"), folder, MAX_FOLDER_CHARS, false)?;
            }
        }
        Ok(())
    }

    /// The session of `slot`.
    pub fn session(&self, slot: &str) -> Option<&BodySession> {
        self.sessions.iter().find(|s| s.slot == slot)
    }

    /// The bytes that are encoded, digested and published: compact JSON
    /// in schema order (`schema`, `title`, `goal`, `decisions` when given,
    /// `sessions` with `slot`, `role`, `brief`, `folder`), the same for any
    /// key order of the input (contract §37.10).
    pub fn wire_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("plan body: {e}"))
    }
}

/// The body of a document's `body` (standard base64 with padding): its
/// decoded bytes, 1 B–256 KiB.
pub fn decode_body(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() > MAX_BODY_BYTES.div_ceil(3) * 4 {
        return Err("body is larger than 256 KiB".to_owned());
    }
    let bytes = STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| "body is not base64".to_owned())?;
    if bytes.is_empty() || bytes.len() > MAX_BODY_BYTES {
        return Err("body is not 1 B to 256 KiB".to_owned());
    }
    Ok(bytes)
}

/// The routing of one session in a publish: never text, only the slot,
/// optional task and worker ids and the brief's digest.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Routing {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    worker_id: Option<String>,
}

/// `lean-ctx cloud plan publish` stdin:
/// `{"idempotency_key", "plan_id"?, "supersedes_version"?, "body"}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishInput {
    idempotency_key: String,
    #[serde(default)]
    plan_id: Option<String>,
    #[serde(default)]
    supersedes_version: Option<u64>,
    body: Value,
}

/// A checked publish, ready to post.
#[derive(Debug, Clone, PartialEq)]
pub struct Publish {
    /// The `Idempotency-Key` header (a UUID).
    pub idempotency_key: String,
    pub content_sha256: String,
    /// The `leanctx.cloud.plan-publish/v1` request.
    pub request: Value,
}

/// The publish of stdin `text`. A body session may carry its routing
/// (`task_id`, `worker_id`); it is taken out of the body before the body
/// is serialized, so the published text holds none of it.
pub fn publish_request(text: &str) -> Result<Publish, String> {
    let input: PublishInput = serde_json::from_str(text).map_err(|e| format!("publish: {e}"))?;
    let key = uuid::Uuid::parse_str(&input.idempotency_key)
        .map_err(|_| "idempotency_key is not a UUID".to_owned())?
        .hyphenated()
        .to_string();
    if let Some(plan_id) = &input.plan_id
        && !plan_id_ok(plan_id)
    {
        return Err("plan_id is not a plan id (plan_ and 8–40 letters or digits)".to_owned());
    }
    if input.supersedes_version == Some(0) {
        return Err("supersedes_version starts at 1".to_owned());
    }
    if input.supersedes_version.is_some() && input.plan_id.is_none() {
        return Err("supersedes_version needs plan_id".to_owned());
    }
    let Value::Object(mut body) = input.body else {
        return Err("body is not a JSON object".to_owned());
    };
    let mut routing = Vec::new();
    if let Some(Value::Array(sessions)) = body.get_mut("sessions") {
        for session in sessions.iter_mut() {
            let Value::Object(fields) = session else {
                return Err("a body session is not a JSON object".to_owned());
            };
            let mut route = Map::new();
            for name in ["task_id", "worker_id"] {
                if let Some(value) = fields.remove(name) {
                    route.insert(name.to_owned(), value);
                }
            }
            let route: Routing = serde_json::from_value(Value::Object(route))
                .map_err(|e| format!("session routing: {e}"))?;
            for id in [&route.task_id, &route.worker_id].into_iter().flatten() {
                if !task_id_ok(id) {
                    return Err(format!(
                        "{id:?} is not a task or worker id (1–128 of [A-Za-z0-9._:-])"
                    ));
                }
            }
            routing.push(route);
        }
    }
    let body: PlanBody =
        serde_json::from_value(Value::Object(body)).map_err(|e| format!("plan body: {e}"))?;
    body.check()?;
    let bytes = body.wire_bytes()?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err("the body is larger than 256 KiB".to_owned());
    }
    let content_sha256 = sha256_of(&bytes);
    let sessions: Vec<Value> = body
        .sessions
        .iter()
        .zip(routing.iter().chain(std::iter::repeat(&Routing::default())))
        .map(|(session, route)| {
            json!({
                "slot": session.slot,
                "task_id": route.task_id,
                "worker_id": route.worker_id,
                "brief_sha256": sha256_of(session.brief.as_bytes()),
            })
        })
        .collect();
    let request = json!({
        "schema": PLAN_PUBLISH_SCHEMA,
        "plan_id": input.plan_id,
        "supersedes_version": input.supersedes_version,
        "content_sha256": content_sha256,
        "body": STANDARD.encode(&bytes),
        "sessions": sessions,
    });
    let size = serde_json::to_vec(&request).map_or(usize::MAX, |b| b.len());
    if size > MAX_REQUEST_BYTES {
        return Err("the publish request is larger than 400 KiB".to_owned());
    }
    Ok(Publish {
        idempotency_key: key,
        content_sha256,
        request,
    })
}

/// A version Cloud took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub plan_id: String,
    pub version: u64,
    pub content_sha256: String,
}

/// A publish Cloud or the engine refused: a `[a-z_]` code, a message
/// without plan text, and the current version of a `stale_version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRefused {
    pub code: String,
    pub message: String,
    pub current_version: Option<u64>,
}

impl PublishRefused {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            current_version: None,
        }
    }
}

/// Cloud's answer `(status, body)` to `publish`.
pub fn publish_answer(
    publish: &Publish,
    status: u16,
    answer: &Value,
) -> Result<Published, PublishRefused> {
    if (200..300).contains(&status) {
        let plan_id = answer["plan_id"].as_str().filter(|id| plan_id_ok(id));
        let version = answer["version"].as_u64().filter(|v| *v >= 1);
        let (Some(plan_id), Some(version)) = (plan_id, version) else {
            return Err(PublishRefused::new(
                "protocol",
                "LeanCTX Cloud answered the publish without a plan id and version",
            ));
        };
        if answer["schema"].as_str() != Some(PLAN_PUBLISHED_SCHEMA)
            || answer["content_sha256"].as_str() != Some(publish.content_sha256.as_str())
        {
            return Err(PublishRefused::new(
                "protocol",
                "LeanCTX Cloud answered the publish with another body digest",
            ));
        }
        return Ok(Published {
            plan_id: plan_id.to_owned(),
            version,
            content_sha256: publish.content_sha256.clone(),
        });
    }
    let code = answer["error"]["code"]
        .as_str()
        .filter(|c| {
            (1..=64).contains(&c.len()) && c.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        })
        .map_or_else(
            || match status {
                401 => "unauthorized".to_owned(),
                403 => "forbidden".to_owned(),
                404 => "not_found".to_owned(),
                413 => "payload_too_large".to_owned(),
                429 => "rate_limited".to_owned(),
                _ => format!("http_{status}"),
            },
            str::to_owned,
        );
    let message = match code.as_str() {
        "stale_version" => {
            "a newer version of this plan was published; publish on top of current_version"
        }
        "plan_exists" => {
            "a plan with this id exists; pass supersedes_version to publish a new version"
        }
        "not_found" => "LeanCTX Cloud does not know this plan or vault",
        "not_ready" => "LeanCTX Cloud does not take plans for this vault yet",
        "idempotency_key_reused" => "this idempotency_key was used for another publish",
        "invalid" | "invalid_request" => "LeanCTX Cloud refused the publish as invalid",
        "forbidden" | "not_permitted" | "unauthorized" => {
            "this account may not publish plans to this vault"
        }
        "rate_limited" => "too many publishes; try again later",
        _ => "LeanCTX Cloud refused the publish",
    };
    let mut refused = PublishRefused::new(&code, format!("{message} ({status})"));
    if code == "stale_version" {
        refused.current_version = answer["error"]["current_version"]
            .as_u64()
            .or_else(|| answer["current_version"].as_u64());
    }
    Err(refused)
}

/// The refusal code when the project root is bound to no vault.
pub const NOT_BOUND: &str = "not_bound";

/// Post `publish` for the vault `project_root` is bound to (as every
/// `cloud` command resolves it). Blocking (HTTP).
pub fn publish(project_root: &Path, publish: &Publish) -> Result<Published, PublishRefused> {
    let bound = crate::state::CloudStore::open()
        .ok()
        .and_then(|store| {
            crate::resolution::vault_for_root(store.data_dir(), project_root)
                .ok()
                .flatten()
        })
        .is_some();
    if !bound {
        return Err(PublishRefused::new(
            NOT_BOUND,
            "this repository is not bound to a LeanCTX Cloud vault",
        ));
    }
    let conn = super::leases::connected(project_root).map_err(|_| {
        PublishRefused::new(
            "not_connected",
            "this machine is not connected to LeanCTX Cloud (see `lean-ctx cloud status`)",
        )
    })?;
    let path = format!("/vaults/{}/plans", conn.vault_id);
    let (status, answer) = conn
        .session
        .with_client(|c| {
            c.send_answer_with(
                "POST",
                &path,
                Some(&publish.request),
                &[("Idempotency-Key", publish.idempotency_key.as_str())],
                MAX_ANSWER_BYTES,
            )
        })
        .map_err(|e| PublishRefused::new("offline", format!("LeanCTX Cloud: {e}")))?;
    publish_answer(publish, status, &answer)
}

#[cfg(test)]
#[path = "plan_body_tests.rs"]
mod tests;
