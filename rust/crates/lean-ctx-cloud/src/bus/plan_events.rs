// SPDX-License-Identifier: Apache-2.0
//! Plan updates to running agents (contract §37). A person changes the plan
//! or a team decision in LeanCTX Code; Cloud tells every engine of the vault
//! with a text-free frame:
//!
//! - `plan.advanced {vault, plan_id, version}`: the engine pulls the plan
//!   document (`GET /v1/vaults/{id}/plans/{plan_id}?version=n`), checks its
//!   `content_sha256` and its Cloud `control` signature, keeps the last good
//!   version in `<cloud data>/plans/<plan_id>.json`, and gives each local
//!   agent the plan names a directive (`core::agents::DirectiveStore`);
//!   every other active agent of the checkout gets a plan-wide one.
//! - `decision.changed {vault, decision_id, status, superseded_by?}`:
//!   Cloud sends it only after the person's confirmed "Send to agents" on a
//!   team decision in LeanCTX Code, so the frame is the release: each active
//!   agent of the checkout gets the decision directive on the bus at once
//!   (`deliver_decision`). The same decision, status and successor again (a
//!   frame delivered twice) posts nothing. Only the frame delivers one: no
//!   catch-up, sync or list does.
//!
//! Agents get a plan update only after a person clicked "Send to agents" in
//! LeanCTX Code and confirmed it (owner decision). A plan frame or a pulled
//! version only **offers** directives: they are recorded, invisible to the
//! agents (no bus entry, no pending line). Code releases each one with the
//! local CLI `lean-ctx cloud plan release` (release id on stdin); nothing in
//! this module, the bus or a Cloud frame can release one (`apply` gets the
//! registry read-only). Delivering means posting a `cloud` directive on the
//! local agent bus, which an agent reads with `ctx_agent`: nothing is typed
//! into an agent's terminal and no agent is started or restarted. Both
//! frames are taken only when `hello.ack` names [`PLAN_UPDATES`].
//!
//! A version that fails the checks is never shown as an instruction; the
//! engine refuses it to Cloud with a reason code. Plan versions only move
//! forward ([`PlansSeen`]); a frame missed while offline is caught up after
//! each hello from `GET …/plans?state=active`. The agents' answers go back
//! per version (`POST …/plans/{plan_id}/ack`), with an HMAC of the local
//! agent id, never the id or any free text.
//!
//! Cloud keeps the plan body encrypted at rest and serves it as plain text
//! (`body`). The engine opens it only after its digest and the signature
//! checked out, and checks that every session the document routes names a
//! slot of the body whose brief has the session's `brief_sha256`
//! ([`super::plan_body`]). The goal and the brief an agent reads come from
//! that verified body alone; the offered directive keeps their digests, and
//! the person's release from Code is checked against them.

use std::collections::BTreeMap;
#[cfg(feature = "http-server")]
use std::path::Path;

use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::plan_body::PlanBody;
use super::v2::ControlKeys;
#[cfg(feature = "http-server")]
use lean_ctx::core::agents::{AgentEntry, AgentRegistry, DirectiveStore};
use lean_ctx::core::agents::{Directive, DirectiveIssue, DirectiveKind, DirectiveState};

#[cfg(feature = "http-server")]
use super::address::Address;
#[cfg(feature = "http-server")]
use crate::vault::{VaultMap, VaultRecord};

/// The `hello.ack` feature of a Cloud that serves the plan routes.
pub const PLAN_UPDATES: &str = "plan_updates";
/// Signature domain of a plan version (`domain ‖ canonical_json(statement)`,
/// see [`plan_statement`]), as controls are signed.
pub const PLAN_DOMAIN: &str = "leanctx.cloud.plan.v1\n";
pub const PLAN_SCHEMA: &str = "leanctx.cloud.plan/v1";
/// Schema of the statement Cloud signs for a plan version.
pub const PLAN_STATEMENT_SCHEMA: &str = "leanctx.cloud.plan-statement/v1";
/// Agents per ack request (the route's `maxItems`).
const MAX_ACK_AGENTS: usize = 64;
/// Sessions per plan.
const MAX_SESSIONS: usize = 32;
/// Plans remembered per vault.
#[cfg(any(test, feature = "http-server"))]
const MAX_PLANS: usize = 200;

fn chars_ok(text: &str, min: usize, max: usize, ok: impl Fn(char) -> bool) -> bool {
    (min..=max).contains(&text.chars().count()) && text.chars().all(ok)
}

pub(crate) fn plan_id_ok(id: &str) -> bool {
    id.strip_prefix("plan_")
        .is_some_and(|rest| chars_ok(rest, 8, 40, |c| c.is_ascii_alphanumeric()))
}

/// Cloud's decision id: `dec-` and lowercase kebab segments (`[a-z0-9]+`
/// joined by single `-`), at most 80 characters in all. The engine checks
/// an id it sends to the Cloud by the same rule.
pub fn decision_id_ok(id: &str) -> bool {
    id.len() <= 80
        && id.strip_prefix("dec-").is_some_and(|rest| {
            rest.split('-').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
        })
}

pub(crate) fn slot_ok(slot: &str) -> bool {
    slot != ALL_SLOT
        && chars_ok(slot, 1, 32, |c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
        })
}

pub(crate) fn task_id_ok(id: &str) -> bool {
    chars_ok(id, 1, 128, |c| {
        c.is_ascii_alphanumeric() || "._:-".contains(c)
    })
}

fn sha_ok(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

/// One `plan.advanced` frame (or a plan a catch-up found newer).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanAdvanced {
    #[serde(default)]
    t: Option<String>,
    pub vault: String,
    pub plan_id: String,
    pub version: u64,
}

impl PlanAdvanced {
    pub fn new(vault: &str, plan_id: &str, version: u64) -> Self {
        Self {
            t: None,
            vault: vault.to_string(),
            plan_id: plan_id.to_string(),
            version,
        }
    }

    pub fn parse(frame: &Value) -> Option<Self> {
        serde_json::from_value(frame.clone())
            .ok()
            .filter(|e: &Self| e.version >= 1 && plan_id_ok(&e.plan_id))
            .map(|e| Self { t: None, ..e })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Active,
    Superseded,
    Revoked,
}

impl DecisionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionStatus::Active => "active",
            DecisionStatus::Superseded => "superseded",
            DecisionStatus::Revoked => "revoked",
        }
    }
}

/// One `decision.changed` frame.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionChanged {
    #[serde(default)]
    t: Option<String>,
    pub vault: String,
    pub decision_id: String,
    pub status: DecisionStatus,
    #[serde(default)]
    pub superseded_by: Option<String>,
}

impl DecisionChanged {
    pub fn parse(frame: &Value) -> Option<Self> {
        serde_json::from_value(frame.clone())
            .ok()
            .filter(|e: &Self| {
                decision_id_ok(&e.decision_id)
                    && e.superseded_by.as_deref().is_none_or(decision_id_ok)
            })
            .map(|e| Self { t: None, ..e })
    }
}

// The pulled document tolerates fields it does not know (a later Cloud
// may add some); they are not kept, signed or shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSignature {
    pub key_id: String,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSession {
    pub slot: String,
    pub task_id: Option<String>,
    #[serde(default)]
    pub worker_id: Option<String>,
    pub brief_sha256: String,
    #[serde(default)]
    pub changed: bool,
}

/// A plan document `leanctx.cloud.plan/v1` (§37.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDoc {
    pub schema: String,
    pub plan_id: String,
    pub vault: String,
    pub version: u64,
    #[serde(default)]
    pub supersedes_version: Option<u64>,
    pub content_sha256: String,
    pub issued_by: String,
    /// When the person published this version (RFC 3339), as Cloud sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default)]
    pub signature: Option<PlanSignature>,
    /// The plan body `leanctx.plan-body/v1` as standard base64 with
    /// padding (1 B–256 KiB decoded); `content_sha256` is its digest.
    pub body: String,
    pub sessions: Vec<PlanSession>,
}

/// Why the engine did not take a version (the ack's `refused.reason_code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRefusal {
    DigestMismatch,
    SignatureInvalid,
    OlderThanApplied,
    /// Reserved (the ack route's enum): Cloud serves the body as plain
    /// text, so nothing is decrypted and this code is never sent.
    DecryptFailed,
}

impl PlanRefusal {
    pub fn code(self) -> &'static str {
        match self {
            PlanRefusal::DigestMismatch => "digest_mismatch",
            PlanRefusal::SignatureInvalid => "signature_invalid",
            PlanRefusal::OlderThanApplied => "older_than_applied",
            PlanRefusal::DecryptFailed => "decrypt_failed",
        }
    }
}

/// A plan document that was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// Refused to Cloud (the document is a plan, but not this one's).
    Refuse {
        refusal: PlanRefusal,
        content_sha256: String,
    },
    /// Not a plan document of the requested version; recorded locally.
    Invalid(String),
}

fn invalid(what: &str) -> Rejected {
    Rejected::Invalid(format!("plan document: {what}"))
}

/// A vault id compared by its 32 hex digits: lowercase, without `-`.
pub fn vault_key(id: &str) -> String {
    id.chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Check one pulled document against the frame that announced it, the
/// enrolled vault it was read from (`vault_id`, the id of the GET), its
/// digest and its signature. The frame's `vault` is the subscription
/// address and only routes; the document's `vault` is the vault id Cloud
/// signed (32 hex), bound to the enrolled one.
pub fn verify(
    raw: &Value,
    expect: &PlanAdvanced,
    vault_id: &str,
    keys: &ControlKeys,
) -> Result<PlanDoc, Rejected> {
    let doc: PlanDoc =
        serde_json::from_value(raw.clone()).map_err(|e| invalid(&format!("shape: {e}")))?;
    if doc.schema != PLAN_SCHEMA {
        return Err(invalid("schema"));
    }
    if doc.plan_id != expect.plan_id || doc.version != expect.version {
        return Err(invalid("not the announced plan version"));
    }
    let enrolled = vault_key(vault_id);
    let is_id = enrolled.len() == 32
        && enrolled
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !is_id || vault_key(&doc.vault) != enrolled {
        return Err(invalid("not the enrolled vault"));
    }
    let shape_ok = plan_id_ok(&doc.plan_id)
        && doc.version >= 1
        && sha_ok(&doc.content_sha256)
        && !doc.issued_by.trim().is_empty()
        && doc.sessions.len() <= MAX_SESSIONS
        && doc.sessions.iter().all(|s| {
            (s.slot == ALL_SLOT || slot_ok(&s.slot))
                && sha_ok(&s.brief_sha256)
                && s.worker_id.as_deref().is_none_or(task_id_ok)
                && s.task_id.as_deref().is_none_or(task_id_ok)
        });
    if !shape_ok {
        return Err(invalid("fields"));
    }
    let refuse = |refusal| Rejected::Refuse {
        refusal,
        content_sha256: doc.content_sha256.clone(),
    };
    let body = super::plan_body::decode_body(&doc.body).map_err(|e| invalid(&e))?;
    if super::plan_body::sha256_of(&body) != doc.content_sha256 {
        return Err(refuse(PlanRefusal::DigestMismatch));
    }
    let signature = doc
        .signature
        .as_ref()
        .ok_or_else(|| refuse(PlanRefusal::SignatureInvalid))?;
    let key = keys
        .get(&signature.key_id)
        .ok_or_else(|| refuse(PlanRefusal::SignatureInvalid))?;
    let mut sig = [0u8; 64];
    hex::decode_to_slice(&signature.sig, &mut sig)
        .map_err(|_| refuse(PlanRefusal::SignatureInvalid))?;
    let input = lean_ctx_protocol::cloud::signing_input_without_signature(
        PLAN_DOMAIN,
        &plan_statement(&doc, &signature.key_id),
    )
    .map_err(|_| invalid("not signable"))?;
    key.verify_strict(&input, &Signature::from_bytes(&sig))
        .map_err(|_| refuse(PlanRefusal::SignatureInvalid))?;
    // Signed, so Cloud vouches for the bytes; a body that is no plan body
    // is a publisher's fault, noted locally.
    let opened = PlanBody::parse(&body).map_err(|e| invalid(&e))?;
    // The routing must match the verified text: every routed slot is a
    // slot of the body (or `all`) with that brief. Otherwise nothing is
    // offered (Cloud does not check this).
    let routed_ok = doc.sessions.iter().all(|s| {
        s.slot == ALL_SLOT
            || opened
                .session(&s.slot)
                .is_some_and(|b| super::plan_body::sha256_of(b.brief.as_bytes()) == s.brief_sha256)
    });
    if !routed_ok {
        return Err(refuse(PlanRefusal::DigestMismatch));
    }
    Ok(doc)
}

impl PlanDoc {
    /// The body of a verified document ([`verify`]); `None` for a document
    /// whose body does not open (never one that verified).
    pub fn opened(&self) -> Option<PlanBody> {
        self.open().ok()
    }

    /// The verified body, or why it does not open: `invalid` (no base64 of
    /// 1 B–256 KiB, or no `leanctx.plan-body/v1`) or `digest_mismatch`.
    /// The signature was checked when the version was taken (§37.3); only
    /// verified versions are kept.
    pub fn open(&self) -> Result<PlanBody, &'static str> {
        let bytes = super::plan_body::decode_body(&self.body).map_err(|_| "invalid")?;
        if super::plan_body::sha256_of(&bytes) != self.content_sha256 {
            return Err(PlanRefusal::DigestMismatch.code());
        }
        PlanBody::parse(&bytes).map_err(|_| "invalid")
    }
}

/// The statement Cloud signs for a version (§37.3): ids, version, body
/// digest, issuer and signing key, never the body or the sessions. Rebuilt
/// from the received document, so a changed vault, version, digest or issuer
/// no longer verifies.
pub fn plan_statement(doc: &PlanDoc, key_id: &str) -> Value {
    json!({
        "schema": PLAN_STATEMENT_SCHEMA,
        "vault": doc.vault,
        "plan_id": doc.plan_id,
        "version": doc.version,
        "content_sha256": doc.content_sha256,
        "issued_by": doc.issued_by,
        "key_id": key_id,
    })
}

/// Vault address (`#ws/vault`) → `plan_id` → the version applied.
pub type PlansSeen = BTreeMap<String, BTreeMap<String, u64>>;

/// The version of `plan_id` applied in `vault`, if any.
pub fn applied(seen: &PlansSeen, vault: &str, plan_id: &str) -> Option<u64> {
    seen.get(vault)
        .and_then(|plans| plans.get(plan_id))
        .copied()
}

/// Record `version` as applied; false when it or a newer one already is.
#[cfg(any(test, feature = "http-server"))]
fn admit(seen: &mut PlansSeen, vault: &str, plan_id: &str, version: u64) -> bool {
    let plans = seen.entry(vault.to_string()).or_default();
    if plans.get(plan_id).is_some_and(|known| *known >= version) {
        return false;
    }
    plans.insert(plan_id.to_string(), version);
    while plans.len() > MAX_PLANS {
        let Some(first) = plans.keys().next().cloned() else {
            break;
        };
        plans.remove(&first);
    }
    true
}

fn ack_hint(directive_id: &str) -> String {
    lean_ctx::core::agents::ack_hint(directive_id)
}

/// The slot of the directive for an agent no session of the plan names
/// (`<plan_id>:<version>:all`), and of a session routed to every agent
/// (`slot: "all"`): no part of the body names it, so it has no brief.
pub const ALL_SLOT: &str = "all";

/// The directive each local agent gets for a verified plan: per named
/// session (its worker id is the session's `worker_id`), or one plan-wide
/// `…:all` directive for an agent no session names. `local` are the
/// project's active agents with their Cloud worker ids. Each directive
/// keeps the digest of the verified body's goal; a document whose body
/// does not open offers nothing.
pub fn plan_directives(
    doc: &PlanDoc,
    vault_id: &str,
    local: &[(String, Option<String>)],
) -> Vec<(String, DirectiveIssue)> {
    let mut out = Vec::new();
    let Some(body) = doc.opened() else {
        return out;
    };
    let goal_sha256 = super::plan_body::sha256_of(body.goal.as_bytes());
    for (agent, worker) in local {
        let session = worker.as_deref().and_then(|w| {
            doc.sessions
                .iter()
                .find(|s| s.worker_id.as_deref() == Some(w))
        });
        let brief_sha256 = session
            .filter(|s| s.slot != ALL_SLOT)
            .map(|s| s.brief_sha256.clone());
        let (slot, task_id) = match session {
            Some(session) => (session.slot.clone(), session.task_id.clone()),
            None => (ALL_SLOT.to_string(), None),
        };
        let directive_id = format!("{}:{}:{slot}", doc.plan_id, doc.version);
        out.push((
            agent.clone(),
            DirectiveIssue {
                directive_id,
                kind: DirectiveKind::Plan,
                subject: doc.plan_id.clone(),
                version: doc.version,
                slot: Some(slot),
                worker_id: session.and_then(|s| s.worker_id.clone()),
                task_id,
                vault_id: vault_id.to_string(),
                digest: doc.content_sha256.clone(),
                brief_sha256,
                goal_sha256: Some(goal_sha256.clone()),
                // No plan text before the person's release (37.4).
                text: String::new(),
            },
        ));
    }
    out
}

/// The directive id of a decision change, `<decision_id>:<status>:<n>`: `n`
/// numbers the decision's changes this Instance saw (`known`, the kept
/// records). The same status and successor again (a frame delivered twice)
/// keeps the last id; a change back (superseded, then active again) or
/// another successor gets a new one, so it is never taken for the earlier
/// directive.
pub fn decision_directive_id(known: &[Directive], event: &DecisionChanged) -> String {
    let prefix = format!("{}:", event.decision_id);
    let last = known
        .iter()
        .filter(|d| d.kind == DirectiveKind::Decision && d.subject == event.decision_id)
        .filter_map(|d| {
            let (status, n) = d.directive_id.strip_prefix(&prefix)?.split_once(':')?;
            Some((n.parse::<u64>().ok()?, status.to_string(), d.digest.clone()))
        })
        .max();
    match last {
        Some((n, status, successor))
            if status == event.status.as_str() && successor == decision_successor(event) =>
        {
            format!("{prefix}{status}:{n}")
        }
        Some((n, _, _)) => format!("{prefix}{}:{}", event.status.as_str(), n + 1),
        None => format!("{prefix}{}:1", event.status.as_str()),
    }
}

/// What a decision directive keeps as its `digest`: the decision that
/// superseded it, empty without one.
fn decision_successor(event: &DecisionChanged) -> String {
    event.superseded_by.clone().unwrap_or_default()
}

/// The directive a decision change asks of every local agent of its vault.
/// `known` are the kept records ([`decision_directive_id`]).
pub fn decision_directive(
    event: &DecisionChanged,
    vault_id: &str,
    known: &[Directive],
) -> DirectiveIssue {
    let directive_id = decision_directive_id(known, event);
    let id = &event.decision_id;
    let what = match (event.status, event.superseded_by.as_deref()) {
        (DecisionStatus::Superseded, Some(by)) => {
            format!("was superseded by {by}; work that relies on {id} may no longer apply")
        }
        (DecisionStatus::Superseded, None) => {
            format!("was superseded; work that relies on {id} may no longer apply")
        }
        (DecisionStatus::Revoked, _) => {
            format!("was revoked; work that relies on {id} may no longer apply")
        }
        (DecisionStatus::Active, _) => format!("is now active; follow {id} from here on"),
    };
    let text = format!(
        "LeanCTX Cloud: team decision {id} {what}. Re-read the project's decisions before you go on. {hint}",
        hint = ack_hint(&directive_id),
    );
    DirectiveIssue {
        directive_id,
        kind: DirectiveKind::Decision,
        subject: event.decision_id.clone(),
        version: 0,
        slot: None,
        worker_id: None,
        task_id: None,
        vault_id: vault_id.to_string(),
        digest: decision_successor(event),
        brief_sha256: None,
        goal_sha256: None,
        text,
    }
}

/// Deliver one `decision.changed` frame: post its directive to every active
/// local agent of the vault's checkout. Called only from the frame (Cloud
/// sends it only after the person's confirmed "Send to agents"); a frame
/// delivered twice posts nothing. Returns how many agents got it. An agent
/// that cannot take it does not stop the others: their errors are collected
/// and reported together once every agent was tried.
#[cfg(feature = "http-server")]
pub(crate) fn deliver_decision(
    registry: &mut AgentRegistry,
    store: &mut DirectiveStore,
    map: &VaultMap,
    event: &DecisionChanged,
) -> Result<usize, String> {
    let Some((record, root)) = record_of(map, &event.vault) else {
        return Ok(0);
    };
    let issue = decision_directive(event, &record.vault_id, &store.directives);
    let agents: Vec<String> = registry
        .list_active(Some(root))
        .into_iter()
        .map(|agent| agent.agent_id.clone())
        .collect();
    let mut delivered = 0;
    let mut errors = Vec::new();
    for agent in agents {
        match store.deliver_decision(registry, root, &agent, &issue) {
            Ok(true) => delivered += 1,
            Ok(false) => {}
            Err(error) => errors.push(format!("agent {agent}: {error}")),
        }
    }
    if errors.is_empty() {
        Ok(delivered)
    } else {
        Err(format!(
            "decision {} reached {delivered} agent(s); {}",
            event.decision_id,
            errors.join("; ")
        ))
    }
}

/// The ack route's `agent_type` for a local agent type.
pub fn agent_type(local: &str) -> &'static str {
    let local = local.to_ascii_lowercase();
    if local.contains("claude") {
        "claude"
    } else if local.contains("codex") {
        "codex"
    } else if local.contains("cursor") {
        "cursor"
    } else {
        "other"
    }
}

/// One `POST …/plans/{plan_id}/ack` and what it reports.
#[derive(Debug, Clone, PartialEq)]
pub struct AckRequest {
    pub vault_id: String,
    pub plan_id: String,
    pub body: Value,
    /// The idempotency key: changes whenever a reported state does.
    pub key: String,
    pub covers: Vec<(String, String, DirectiveState)>,
}

/// The ack requests for `due` directives, one per vault, plan, version and
/// 64 agents, in a stable order. `agent_ref` hides the local agent id;
/// `agent_type_of` names its host.
pub fn ack_requests(
    due: &[&Directive],
    agent_ref: &dyn Fn(&str) -> String,
    agent_type_of: &dyn Fn(&str) -> &'static str,
) -> Vec<AckRequest> {
    let mut groups: BTreeMap<(&str, &str, u64, &str), Vec<&Directive>> = BTreeMap::new();
    for d in due.iter().filter(|d| d.kind == DirectiveKind::Plan) {
        groups
            .entry((&d.vault_id, &d.subject, d.version, &d.digest))
            .or_default()
            .push(d);
    }
    let mut out = Vec::new();
    for ((vault_id, plan_id, version, digest), mut members) in groups {
        members.sort_by(|a, b| (&a.agent_id, &a.directive_id).cmp(&(&b.agent_id, &b.directive_id)));
        for chunk in members.chunks(MAX_ACK_AGENTS) {
            let agents: Vec<Value> = chunk
                .iter()
                .filter_map(|d| {
                    Some(json!({
                        "agent_ref": agent_ref(&d.agent_id),
                        "worker_id": d.worker_id,
                        "slot": d.slot,
                        "agent_type": agent_type_of(&d.agent_id),
                        "state": d.state.wire()?,
                        "reason_code": d.reason_code,
                    }))
                })
                .collect();
            let body = json!({
                "version": version,
                "content_sha256": digest,
                "agents": agents,
                "refused": null,
            });
            out.push(AckRequest {
                vault_id: vault_id.to_string(),
                plan_id: plan_id.to_string(),
                key: ack_key(plan_id, version, &body),
                body,
                covers: chunk
                    .iter()
                    .map(|d| (d.agent_id.clone(), d.directive_id.clone(), d.state))
                    .collect(),
            });
        }
    }
    out
}

/// The engine-level refusal of one version.
pub fn refusal_request(
    vault_id: &str,
    plan_id: &str,
    version: u64,
    content_sha256: &str,
    refusal: PlanRefusal,
    applied_version: Option<u64>,
) -> AckRequest {
    let body = json!({
        "version": version,
        "content_sha256": content_sha256,
        "agents": [],
        "refused": {"reason_code": refusal.code(), "applied_version": applied_version},
    });
    AckRequest {
        vault_id: vault_id.to_string(),
        plan_id: plan_id.to_string(),
        key: ack_key(plan_id, version, &body),
        body,
        covers: Vec::new(),
    }
}

fn ack_key(plan_id: &str, version: u64, body: &Value) -> String {
    let digest = hex::encode(Sha256::digest(body.to_string().as_bytes()));
    format!("plan-ack:{plan_id}:{version}:{}", &digest[..32])
}

// ----- the relay's side: HTTP and the local bus (feature `http-server`) -----

/// Cloud has no control key yet, so it publishes and serves no plans
/// (`409 not_ready`, §37.2): plan updates are not available. Not an error
/// to retry; nothing is applied.
pub const PLANS_NOT_READY: &str =
    "plan updates not available (LeanCTX Cloud has no signing key yet)";

/// Whether a `hello.ack` names [`PLAN_UPDATES`]: without it the relay
/// ignores `plan.advanced` and `decision.changed` and pulls nothing (§37.1).
pub fn frames_taken(ack: &Value) -> bool {
    super::relay_v2::acks_feature(ack, PLAN_UPDATES)
}

/// Whether `error` is Cloud's `409 not_ready` for plans.
pub fn plans_not_ready(error: &crate::CloudError) -> bool {
    matches!(error, crate::CloudError::Server { status: 409, code } if code == crate::error::NOT_READY)
}

/// What one pull gave.
#[cfg(feature = "http-server")]
#[derive(Debug)]
pub(crate) enum PlanOutcome {
    /// A verified version, to apply on the relay's loop.
    Verified {
        vault: String,
        vault_id: String,
        root: String,
        doc: Box<PlanDoc>,
    },
    /// Nothing to apply; a note for the error record.
    Note(String),
}

#[cfg(feature = "http-server")]
fn record_of<'a>(map: &'a VaultMap, address: &str) -> Option<(&'a VaultRecord, &'a str)> {
    let Ok(Address::Vault { workspace, vault }) = Address::parse(Some(address)) else {
        return None;
    };
    let record = map
        .vaults
        .values()
        .find(|r| r.ns == workspace && r.name == vault)?;
    let root = record.roots.iter().find(|root| Path::new(root).is_dir())?;
    Some((record, root.as_str()))
}

#[cfg(feature = "http-server")]
fn post_ack(root: &str, request: &AckRequest) -> Result<(), String> {
    let conn = super::leases::connected(Path::new(root))?;
    let path = format!("/vaults/{}/plans/{}/ack", conn.vault_id, request.plan_id);
    match conn
        .session
        .with_client(|c| c.post_idempotent(&path, &request.body, &request.key))
    {
        // `409` (`stale_version` or the key's earlier answer): a newer
        // version exists or Cloud has this answer; it counts as delivered.
        Ok(_)
        | Err(crate::CloudError::Conflict { .. } | crate::CloudError::Server { status: 409, .. }) => {
            Ok(())
        }
        Err(error) => Err(format!("plan ack {}: {error}", request.plan_id)),
    }
}

/// Pull, verify and keep the announced versions. Blocking (HTTP); the relay
/// runs it off its session loop. A version equal to the applied one is not
/// pulled; an older one, or one that fails its checks, is refused to Cloud
/// and never applied.
#[cfg(feature = "http-server")]
pub(crate) fn fetch(
    map: &VaultMap,
    data_dir: &Path,
    seen: &PlansSeen,
    keys: &ControlKeys,
    events: &[PlanAdvanced],
) -> Vec<PlanOutcome> {
    let mut out = Vec::new();
    for event in events {
        let applied_version = applied(seen, &event.vault, &event.plan_id);
        // An equal version is a no-op; an older one announced as new is
        // pulled, checked and refused `older_than_applied` (§37.3 step 4).
        if applied_version == Some(event.version) {
            continue;
        }
        let older = applied_version.is_some_and(|v| v > event.version);
        let Some((record, root)) = record_of(map, &event.vault) else {
            continue;
        };
        let note = |what: String| {
            PlanOutcome::Note(format!("{} v{}: {what}", event.plan_id, event.version))
        };
        let conn = match super::leases::connected(Path::new(root)) {
            Ok(conn) => conn,
            Err(error) => {
                out.push(note(error));
                continue;
            }
        };
        let path = format!(
            "/vaults/{}/plans/{}?version={}",
            conn.vault_id, event.plan_id, event.version
        );
        let raw = match conn.session.with_client(|c| c.get(&path)) {
            Ok(raw) => raw,
            Err(error) if plans_not_ready(&error) => {
                out.push(note(PLANS_NOT_READY.to_string()));
                continue;
            }
            Err(error) => {
                out.push(note(error.to_string()));
                continue;
            }
        };
        match verify(&raw, event, &conn.vault_id, keys) {
            Ok(doc) if older => {
                let request = refusal_request(
                    &record.vault_id,
                    &event.plan_id,
                    event.version,
                    &doc.content_sha256,
                    PlanRefusal::OlderThanApplied,
                    applied_version,
                );
                let sent = post_ack(root, &request).err().map(|e| format!("; {e}"));
                out.push(note(format!(
                    "refused ({}){}",
                    PlanRefusal::OlderThanApplied.code(),
                    sent.unwrap_or_default()
                )));
            }
            Ok(doc) => {
                let dir = data_dir.join("plans");
                let saved = std::fs::create_dir_all(&dir)
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        lean_ctx::config_io::write_atomic(
                            &dir.join(format!("{}.json", doc.plan_id)),
                            &raw.to_string(),
                        )
                    });
                if let Err(error) = saved {
                    out.push(note(format!("keep: {error}")));
                }
                out.push(PlanOutcome::Verified {
                    vault: event.vault.clone(),
                    vault_id: record.vault_id.clone(),
                    root: root.to_string(),
                    doc: Box::new(doc),
                });
            }
            Err(Rejected::Refuse {
                refusal,
                content_sha256,
            }) => {
                let request = refusal_request(
                    &record.vault_id,
                    &event.plan_id,
                    event.version,
                    &content_sha256,
                    refusal,
                    applied_version,
                );
                let sent = post_ack(root, &request).err().map(|e| format!("; {e}"));
                out.push(note(format!(
                    "refused ({}){}",
                    refusal.code(),
                    sent.unwrap_or_default()
                )));
            }
            Err(Rejected::Invalid(why)) => out.push(note(why)),
        }
    }
    out
}

/// The active plans of the `subscribed` vaults newer than applied, for
/// [`fetch`], and what could not be read. Blocking (HTTP).
#[cfg(feature = "http-server")]
pub(crate) fn catch_up(
    map: &VaultMap,
    subscribed: &[String],
    seen: &PlansSeen,
) -> (Vec<PlanAdvanced>, Vec<String>) {
    let (mut events, mut errors) = (Vec::new(), Vec::new());
    for record in map.vaults.values() {
        let vault = super::relay_v2::address_of(record);
        if !subscribed.contains(&vault) {
            continue;
        }
        let Some(root) = record.roots.iter().find(|root| Path::new(root).is_dir()) else {
            continue;
        };
        let listed = super::leases::connected(Path::new(root)).and_then(|conn| {
            let path = format!("/vaults/{}/plans?state=active", conn.vault_id);
            conn.session.with_client(|c| c.get(&path)).map_err(|e| {
                if plans_not_ready(&e) {
                    PLANS_NOT_READY.to_string()
                } else {
                    e.to_string()
                }
            })
        });
        let listed = match listed {
            Ok(listed) => listed,
            Err(error) => {
                errors.push(format!("{vault}: {error}"));
                continue;
            }
        };
        events.extend(active_newer(&listed, &vault, seen));
    }
    (events, errors)
}

/// The plans of a `GET …/plans?state=active` answer newer than applied, by
/// plan id.
pub fn active_newer(listed: &Value, vault: &str, seen: &PlansSeen) -> Vec<PlanAdvanced> {
    // Cloud answers `items`; `plans` is the earlier contract name.
    let mut events: Vec<PlanAdvanced> = listed["items"]
        .as_array()
        .or_else(|| listed["plans"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|plan| {
            let plan_id = plan["plan_id"].as_str()?;
            let version = plan["version"].as_u64()?;
            (plan_id_ok(plan_id)
                && version >= 1
                && applied(seen, vault, plan_id).is_none_or(|v| v < version))
            .then(|| PlanAdvanced::new(vault, plan_id, version))
        })
        .collect();
    events.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
    events
}

/// Apply one verified version: record it and offer its directives (no bus
/// entry; the person releases them from Code). False when it or a newer one
/// already was.
/// `worker_of` names a local agent's Cloud worker id.
#[cfg(feature = "http-server")]
pub(crate) fn apply(
    registry: &AgentRegistry,
    store: &mut DirectiveStore,
    seen: &mut PlansSeen,
    outcome: (&str, &str, &str, &PlanDoc),
    worker_of: &dyn Fn(&AgentEntry) -> Option<String>,
) -> Result<bool, String> {
    let (vault, vault_id, root, doc) = outcome;
    if !admit(seen, vault, &doc.plan_id, doc.version) {
        return Ok(false);
    }
    let local: Vec<(String, Option<String>)> = registry
        .list_active(Some(root))
        .into_iter()
        .map(|agent| (agent.agent_id.clone(), worker_of(agent)))
        .collect();
    for (agent, issue) in plan_directives(doc, vault_id, &local) {
        store.issue(root, &agent, &issue)?;
    }
    Ok(true)
}

/// Send the agents' answers Cloud has not recorded yet and note what it
/// recorded. Blocking (HTTP). Returns the errors; an unsent state stays
/// due for the next pass.
#[cfg(feature = "http-server")]
pub(crate) fn report(map: &VaultMap, data_dir: &Path) -> Vec<String> {
    let store = DirectiveStore::load();
    let due = store.unreported_plans();
    if due.is_empty() {
        return Vec::new();
    }
    let Some(key) = crate::receipts::ReceiptKey::load_or_create(data_dir) else {
        return vec!["plan ack: no local HMAC key".to_string()];
    };
    let registry = AgentRegistry::load_or_create();
    let agent_ref =
        |agent: &str| format!("ag-{}", &key.mac_hex("leanctx.plan-ack.agent", agent)[..32]);
    let agent_type_of = |agent: &str| {
        registry
            .agents
            .iter()
            .find(|a| a.agent_id == agent)
            .map_or("other", |a| agent_type(&a.agent_type))
    };
    let mut errors = Vec::new();
    let mut done = Vec::new();
    for request in ack_requests(&due, &agent_ref, &agent_type_of) {
        let Some(root) = map
            .vaults
            .values()
            .find(|r| r.vault_id == request.vault_id)
            .and_then(|r| r.roots.iter().find(|root| Path::new(root).is_dir()))
        else {
            continue;
        };
        match post_ack(root, &request) {
            Ok(()) => done.extend(request.covers),
            Err(error) => errors.push(error),
        }
    }
    if !done.is_empty()
        && let Err(error) = DirectiveStore::mutate(|_, store| {
            store.mark_reported(&done);
            Ok(())
        })
    {
        errors.push(format!("plan ack record: {error}"));
    }
    errors
}

#[cfg(test)]
#[path = "plan_events_tests.rs"]
mod tests;
