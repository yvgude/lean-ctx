// SPDX-License-Identifier: Apache-2.0
//! Task proposals through the outbox (`tasks` channel): an agent task the
//! agent asks for, or a contradiction between two facts (lean-ctx-ae
//! queues those). A proposal is screened when it is queued; `ctx_tasks
//! propose` sends it at once off the tool call ([`send_now`]), and the
//! scheduler and `lean-ctx cloud sync` send what is still queued. Cloud
//! answers with the task's state: a member agent's task is open at once
//! (owners and maintainers may close it afterwards); a Cloud that still
//! asks for approval answers `proposed`, and that is what is reported.
//!
//! A person's proposal from LeanCTX Code (rc4-67) goes the same way with
//! origin `person`; only the app-only local routes queue one, never
//! `ctx_tasks` or `lean-ctx cloud tasks propose`. Origin `person` is sent
//! only signed by the app's task-propose key ([`prepare_person_task`] makes
//! the exact bytes, the app signs them, [`enqueue_signed_person`] queues the
//! request with that signature; this process never signs) and only to a
//! Cloud that announces [`FEATURE_TASK_PERSON_ORIGIN`]. Any other person's
//! proposal ([`propose_person_task`], an older Cloud) goes as an agent's, as
//! before.

use std::path::Path;

use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::screened;
use crate::CloudError;
use crate::auth::Session;
use crate::outbox::{CHANNEL_TASKS, Outbox};
use crate::vault::VaultMap;
use lean_ctx_protocol::cloud::lease::CloudRight;
use lean_ctx_protocol::cloud::tasks::{
    TASK_PROPOSE_SCHEMA, TaskOriginKind, TaskOriginV1, TaskProposeRequestV1,
};

/// What a proposal is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Agent,
    Contradiction,
    /// The person this machine is linked to, from LeanCTX Code (rc4-67).
    Person,
}

/// The `GET /v1/capabilities` feature of a Cloud that records an engine's
/// proposal with origin `person` as the linked person's.
pub const FEATURE_TASK_PERSON_ORIGIN: &str = "task_person_origin";

/// The session label of a person's proposal: no agent session asked for it.
const PERSON_SESSION_LABEL: &str = "leanctx-code";

/// One queued proposal (the outbox payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedProposal {
    pub kind: ProposalKind,
    /// `stable_identity` of the project (as knowledge drafts name it).
    pub stable_identity: String,
    pub root: String,
    /// The two contradicting facts; empty for an agent task.
    #[serde(default)]
    pub facts: Vec<String>,
    pub title: String,
    #[serde(default)]
    pub goal: Option<String>,
    pub acceptance: String,
    #[serde(default)]
    pub role: Option<String>,
    /// Opaque session reference. [`propose_agent_task`] keys it under the
    /// local receipts key (`session_ref::uploaded_ses_ref`) for a session id
    /// and for the fixed labels callers pass in its place alike (such as
    /// `emspace-import`), and queues nothing when that key is unavailable.
    /// Only constant labels that name no session stay unsalted: the
    /// `cloud-link` ref of a brought local task and, when absent, the
    /// `cloud-task-proposals` ref of a contradiction proposal.
    #[serde(default)]
    pub session_ref: Option<String>,
    /// The local task this proposal brings into the vault on link
    /// (`cloud link --bring-local`); part of the key, so two local tasks
    /// with the same title stay two vault tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_number: Option<u64>,
    /// `session_ref` is keyed already. Absent on a proposal queued before
    /// refs were keyed: its stored ref (an unsalted digest; the queue never
    /// kept the session id) is re-keyed before it is sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ref_keyed: bool,
    /// A person's proposal signed by LeanCTX Code's task-propose key
    /// (rc4-67): sent with origin `person` only with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<SignedPerson>,
}

/// What the app's signature on a person's proposal covers besides the body,
/// and the signature itself (public: it proves nothing without the bound key
/// the Cloud holds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPerson {
    /// RFC 3339, whole seconds, `Z`: part of the signed body.
    pub signed_at: String,
    /// The vault the signature names (the path's id); a proposal whose
    /// checkout now syncs elsewhere is never sent there.
    pub vault_id: String,
    /// The machine the signature names.
    pub instance_id: String,
    /// base64url Ed25519 over the signing input; empty while prepared.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub task_signature: String,
}

impl QueuedProposal {
    fn origin(&self) -> TaskOriginV1 {
        let mut facts = self.facts.clone();
        facts.sort();
        TaskOriginV1 {
            kind: match self.kind {
                ProposalKind::Agent => TaskOriginKind::Agent,
                ProposalKind::Contradiction => TaskOriginKind::Contradiction,
                ProposalKind::Person => TaskOriginKind::Person,
            },
            facts,
            message: None,
        }
    }

    /// Key a stored ref of a proposal queued before refs were keyed, so no
    /// guessable digest is sent; an error (no local key) keeps it queued.
    pub(crate) fn rekey_legacy_ref(&mut self, cloud_data: &Path) -> Result<(), CloudError> {
        if self.ref_keyed {
            return Ok(());
        }
        if let Some(stored) = &self.session_ref {
            self.session_ref = Some(crate::session_ref::rekeyed_legacy_ref(cloud_data, stored)?);
        }
        self.ref_keyed = true;
        Ok(())
    }

    fn session_ref(&self) -> String {
        self.session_ref
            .clone()
            .unwrap_or_else(|| crate::knowledge::draft::session_ref("cloud-task-proposals"))
    }

    /// The request in the clear (validated before it is queued and sent): a
    /// signed person's proposal with its `signed_at` and `task_signature`,
    /// an unsigned one as an agent's.
    fn plain_request(&self) -> TaskProposeRequestV1 {
        self.request_for(true)
    }

    /// The request without `task_signature`: the body the app's signature
    /// covers (with `instance_id` and `vault_id`).
    fn unsigned_body(&self) -> TaskProposeRequestV1 {
        let mut request = self.request_for(true);
        request.task_signature = None;
        request
    }

    /// The request as sent: a person's origin only signed and only to a
    /// Cloud that announced [`FEATURE_TASK_PERSON_ORIGIN`]; otherwise the
    /// agent's origin it took before, without a signature (an older Cloud
    /// refuses `person`, a newer one an unsigned `person`).
    pub(crate) fn request_for(&self, person_origin: bool) -> TaskProposeRequestV1 {
        let mut request = TaskProposeRequestV1 {
            schema: TASK_PROPOSE_SCHEMA.into(),
            title: Some(self.title.clone()),
            goal: self.goal.clone(),
            acceptance: Some(self.acceptance.clone()),
            role: self.role.clone(),
            origin: self.origin(),
            session_ref: self.session_ref(),
            signed_at: None,
            task_signature: None,
        };
        if request.origin.kind == TaskOriginKind::Person {
            match (&self.signed, person_origin) {
                (Some(signed), true) => {
                    request.signed_at = Some(signed.signed_at.clone());
                    request.task_signature =
                        Some(signed.task_signature.clone()).filter(|s| !s.is_empty());
                }
                _ => request.origin.kind = TaskOriginKind::Agent,
            }
        }
        request
    }

    /// One proposal is one task, however often it is resent or requeued:
    /// the key covers the project, the kind, the facts, (for an agent task)
    /// the title and, for a brought local task, its local number.
    #[must_use]
    pub fn idempotency_key(&self) -> String {
        let origin = self.origin();
        let title = match self.kind {
            ProposalKind::Agent | ProposalKind::Person => self.title.trim(),
            ProposalKind::Contradiction => "",
        };
        let mut text = format!(
            "leanctx.cloud.task-proposal.v1\n{}\n{:?}\n{}\n{title}",
            self.stable_identity,
            self.kind,
            origin.facts.join(",")
        );
        // Absent for every other proposal: their keys stay as they were.
        if let Some(number) = self.local_number {
            text.push_str(&format!("\nlocal:{number}"));
        }
        let digest = Sha256::digest(text.as_bytes());
        hex::encode(&digest[..16])
    }
}

/// Secrets screened out, the request valid: before a proposal is queued
/// or sent.
fn checked(proposal: &QueuedProposal) -> Result<(), CloudError> {
    screened(&[
        ("title", Some(&proposal.title)),
        ("goal", proposal.goal.as_deref()),
        ("acceptance", Some(&proposal.acceptance)),
    ])?;
    proposal
        .plain_request()
        .validate()
        .map_err(|e| CloudError::Refused(format!("invalid task proposal: {e}")))?;
    Ok(())
}

/// A person's proposal ready for the app to sign (rc4-67): the proposal as
/// it will be queued, the exact body sent (without `task_signature`) and the
/// bytes the app signs. Nothing is queued or signed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPerson {
    pub proposal: QueuedProposal,
    pub body: serde_json::Value,
    pub signing_input: Vec<u8>,
}

/// Prepare the person's proposal for `project_root` in `vault_id` through
/// `instance_id`, signed at `signed_at` (see [`PreparedPerson`]). Screened
/// and validated as one that is sent.
pub fn prepare_person_task(
    data_dir: &Path,
    project_root: &Path,
    fields: &Fields<'_>,
    instance_id: &str,
    vault_id: &str,
    signed_at: UtcDateTime,
) -> Result<PreparedPerson, CloudError> {
    let Some((instance_id, vault_id)) =
        crate::consent::task_key::canonical_person_ids(instance_id, vault_id)
    else {
        return Err(CloudError::Local(
            "this computer's or this project's Cloud id has an unexpected form".into(),
        ));
    };
    let (instance_id, vault_id) = (instance_id.as_str(), vault_id.as_str());
    let mut proposal = proposal_of(
        data_dir,
        project_root,
        ProposalKind::Person,
        fields,
        PERSON_SESSION_LABEL,
    )?;
    proposal.signed = Some(SignedPerson {
        signed_at: signed_at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true),
        vault_id: vault_id.to_owned(),
        instance_id: instance_id.to_owned(),
        task_signature: String::new(),
    });
    screened(&[
        ("title", Some(&proposal.title)),
        ("goal", proposal.goal.as_deref()),
        ("acceptance", Some(&proposal.acceptance)),
    ])?;
    let request = proposal.unsigned_body();
    let signing_input =
        crate::consent::task_key::person_task_signing_input(&request, instance_id, vault_id)
            .map_err(CloudError::Local)?;
    let body = serde_json::to_value(&request).map_err(|e| CloudError::Local(e.to_string()))?;
    Ok(PreparedPerson {
        proposal,
        body,
        signing_input,
    })
}

/// Queue a prepared person's proposal with the app's `task_signature`
/// (checked against the bound key by the caller); returns the outbox id.
pub fn enqueue_signed_person(
    data_dir: &Path,
    prepared: &PreparedPerson,
    task_signature: &str,
) -> Result<String, CloudError> {
    let mut proposal = prepared.proposal.clone();
    let Some(signed) = proposal.signed.as_mut() else {
        return Err(CloudError::Local("not a prepared person's proposal".into()));
    };
    task_signature.clone_into(&mut signed.task_signature);
    enqueue(data_dir, &proposal)
}

/// Screen, validate and queue `proposal`; returns the outbox item id.
pub fn enqueue(data_dir: &Path, proposal: &QueuedProposal) -> Result<String, CloudError> {
    checked(proposal)?;
    let payload = serde_json::to_value(proposal).map_err(|e| CloudError::Local(e.to_string()))?;
    Outbox::at(data_dir).enqueue(CHANNEL_TASKS, payload, UtcDateTime::now())
}

/// Screen, validate and send `proposal` to `vault_id` now, without the
/// outbox: for a caller that needs the task number at once (bringing local
/// tasks into a vault on link). The idempotency key is the queued one, so
/// a repeat after an interrupted run gets the same task back.
pub fn propose_now(
    session: &Session,
    vault_id: &str,
    proposal: &QueuedProposal,
) -> Result<(u64, lean_ctx_protocol::cloud::tasks::TaskState), CloudError> {
    checked(proposal)?;
    send(session, vault_id, proposal)
}

/// An agent task for the project at `project_root`: `ctx_tasks propose`,
/// `lean-ctx cloud tasks propose` and imports. Always origin `agent`.
pub fn propose_agent_task(
    data_dir: &Path,
    project_root: &Path,
    title: &str,
    goal: Option<&str>,
    acceptance: &str,
    role: Option<&str>,
    session_id: &str,
) -> Result<String, CloudError> {
    let fields = Fields {
        title,
        goal,
        acceptance,
        role,
    };
    queue_task(
        data_dir,
        project_root,
        ProposalKind::Agent,
        &fields,
        session_id,
    )
}

/// A task the person this machine is linked to proposed in LeanCTX Code
/// (rc4-67): origin `person`. Only the app-only local route calls it; the
/// Cloud names the person from the machine's link, never from the request.
pub fn propose_person_task(
    data_dir: &Path,
    project_root: &Path,
    title: &str,
    goal: Option<&str>,
    acceptance: &str,
    role: Option<&str>,
) -> Result<String, CloudError> {
    let fields = Fields {
        title,
        goal,
        acceptance,
        role,
    };
    queue_task(
        data_dir,
        project_root,
        ProposalKind::Person,
        &fields,
        PERSON_SESSION_LABEL,
    )
}

/// A proposal's words.
pub struct Fields<'a> {
    pub title: &'a str,
    pub goal: Option<&'a str>,
    pub acceptance: &'a str,
    pub role: Option<&'a str>,
}

fn queue_task(
    data_dir: &Path,
    project_root: &Path,
    kind: ProposalKind,
    fields: &Fields<'_>,
    session_id: &str,
) -> Result<String, CloudError> {
    enqueue(
        data_dir,
        &proposal_of(data_dir, project_root, kind, fields, session_id)?,
    )
}

fn proposal_of(
    data_dir: &Path,
    project_root: &Path,
    kind: ProposalKind,
    fields: &Fields<'_>,
    session_id: &str,
) -> Result<QueuedProposal, CloudError> {
    let Fields {
        title,
        goal,
        acceptance,
        role,
    } = *fields;
    let identity = crate::vault::project_identity(project_root).ok_or_else(|| {
        CloudError::Config(
            "this repository has no identity LeanCTX Cloud can link to a vault (no git remote)"
                .into(),
        )
    })?;
    Ok(QueuedProposal {
        kind,
        stable_identity: lean_ctx_protocol::cloud::vault::stable_identity(&identity),
        root: project_root.to_string_lossy().into_owned(),
        facts: Vec::new(),
        title: title.trim().to_owned(),
        goal: goal
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_owned),
        acceptance: acceptance.trim().to_owned(),
        role: role.map(str::to_owned),
        // Keyed for a label too; without the local key nothing is queued.
        session_ref: Some(crate::session_ref::uploaded_ses_ref(data_dir, session_id)?),
        local_number: None,
        ref_keyed: true,
        signed: None,
    })
}

/// What one drain pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// The tasks Cloud created, with the state it gave each.
    pub sent: Vec<(u64, lean_ctx_protocol::cloud::tasks::TaskState)>,
    pub pending: usize,
    pub failed: usize,
    pub problems: Vec<String>,
}

/// What became of one queued proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Cloud created the task.
    Sent(u64, lean_ctx_protocol::cloud::tasks::TaskState),
    /// Kept in the outbox for a later pass (offline, 429, not linked yet).
    Waiting(String),
    /// Moved to the failed outbox; never sent.
    Failed(String),
}

const NO_PROPOSE_RIGHT: &str = "your LeanCTX Cloud plan does not allow task proposals";

fn deliver(
    session: &Session,
    outbox: &Outbox,
    map: &VaultMap,
    mut item: crate::outbox::OutboxItem,
    now: lean_ctx_time::UtcDateTime,
) -> Delivery {
    let Ok(mut proposal) = serde_json::from_value::<QueuedProposal>(item.payload.clone()) else {
        let _ = outbox.fail(&mut item, "damaged task proposal");
        return Delivery::Failed("damaged task proposal".into());
    };
    // Queued before refs were keyed: keyed now, or kept queued while the
    // local key is unavailable.
    if let Err(error) = proposal.rekey_legacy_ref(session.store.data_dir()) {
        let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
        return Delivery::Waiting(error.to_string());
    }
    // A folder the person stopped sharing sends nothing, not even through
    // the record of another checkout; kept for a later `cloud link` (rc4-6).
    if crate::unlinked::contains(session.store.data_dir(), Path::new(&proposal.root)) {
        let reason = crate::unlinked::refusal(session.store.data_dir());
        let _ = outbox.retry_later(&mut item, &reason, None, now);
        return Delivery::Waiting(reason);
    }
    // The checkout's current vault first: a proposal queued under an
    // identity the checkout left goes where the checkout syncs now.
    let Some(record) = map
        .for_root(&proposal.root)
        .or_else(|| map.vaults.get(&proposal.stable_identity))
    else {
        let reason = "this repository is not linked to a vault yet";
        let _ = outbox.retry_later(&mut item, reason, None, now);
        return Delivery::Waiting(reason.into());
    };
    // A signature names its vault and machine: never sent elsewhere.
    if let Some(signed) = &proposal.signed {
        let now_ids = crate::consent::task_key::canonical_person_ids(
            &session.instance.instance_id,
            &record.vault_id,
        );
        let signed_ids = (signed.instance_id.clone(), signed.vault_id.clone());
        if now_ids.as_ref() != Some(&signed_ids) {
            let problem = "the project now syncs elsewhere; propose the task again in the app";
            let _ = outbox.fail(&mut item, problem);
            return Delivery::Failed(problem.into());
        }
    }
    match send(session, &record.vault_id, &proposal) {
        Ok((number, state)) => {
            let _ = outbox.remove(&item);
            Delivery::Sent(number, state)
        }
        Err(error) if error.is_transient() => {
            // 429: Retry-After / retry_after_s, or a quota's reset_at.
            let retry_after = error.retry_after_at(lean_ctx_time::UtcDateTime::now());
            let _ = outbox.retry_later(&mut item, &error.to_string(), retry_after, now);
            Delivery::Waiting(error.to_string())
        }
        Err(error @ CloudError::Refused(_)) => {
            // Refused for now: try again later.
            let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
            Delivery::Waiting(error.to_string())
        }
        Err(error) => {
            let _ = outbox.fail(&mut item, &error.to_string());
            Delivery::Failed(error.to_string())
        }
    }
}

/// Send the due proposals (scheduler, `lean-ctx cloud sync`; never inside
/// a tool call).
pub fn drain(session: &Session) -> DrainReport {
    let data = session.store.data_dir().to_path_buf();
    let outbox = Outbox::at(&data);
    let mut report = DrainReport::default();
    let items = outbox.list(CHANNEL_TASKS);
    if !items.is_empty() && !session.lease.lease.grants(CloudRight::TasksPropose) {
        report.pending = items.len();
        report.problems.push(NO_PROPOSE_RIGHT.into());
        return report;
    }
    // Never into a vault a checkout's link file or identity left.
    let map = match crate::resolution::load_current(&data) {
        Ok((map, _)) => map,
        Err(error) => {
            report.problems.push(error.to_string());
            VaultMap::default()
        }
    };
    let now = UtcDateTime::now();
    for item in items {
        if !item.due(now) {
            continue;
        }
        match deliver(session, &outbox, &map, item, now) {
            Delivery::Sent(number, state) => report.sent.push((number, state)),
            Delivery::Waiting(_) => {}
            Delivery::Failed(problem) => report.problems.push(problem),
        }
    }
    let depth = outbox.depth(CHANNEL_TASKS);
    report.pending = depth.pending;
    report.failed = depth.failed;
    report
}

/// Send the queued proposal `id` now instead of with the next scheduler
/// pass (`ctx_tasks propose` runs this off the tool call, as claims and
/// results go out). A proposal that cannot go out now stays queued and the
/// scheduler sends it later. `None`: the scheduler sent it meanwhile (Cloud's
/// idempotency key keeps a resend of one proposal one task).
#[must_use]
pub fn send_now(session: &Session, id: &str) -> Option<Delivery> {
    let data = session.store.data_dir().to_path_buf();
    let outbox = Outbox::at(&data);
    let item = outbox
        .list(CHANNEL_TASKS)
        .into_iter()
        .find(|item| item.id == id)?;
    if !session.lease.lease.grants(CloudRight::TasksPropose) {
        return Some(Delivery::Waiting(NO_PROPOSE_RIGHT.into()));
    }
    // Never into a vault a checkout's link file or identity left.
    let map = match crate::resolution::load_current(&data) {
        Ok((map, _)) => map,
        Err(error) => return Some(Delivery::Waiting(error.to_string())),
    };
    Some(deliver(session, &outbox, &map, item, UtcDateTime::now()))
}

/// [`send_now`] for a person's proposal (rc4-67): asks the Cloud first what
/// it accepts (a failure keeps the last answer), so the answer says whether
/// the Cloud records the task as the person's (`true`) or, an older Cloud,
/// as an agent's.
#[must_use]
pub fn send_person_now(session: &Session, id: &str) -> (Option<Delivery>, bool) {
    let person = person_origin_offered(session);
    (send_now(session, id), person)
}

/// Whether the Cloud takes a person's origin: asked now (a failure keeps
/// the last answer).
#[must_use]
pub fn person_origin_offered(session: &Session) -> bool {
    let _ = session.with_client(|client| {
        crate::session_ref::refresh(&session.store, &session.instance.url, client)
    });
    crate::session_ref::announced(&session.store, FEATURE_TASK_PERSON_ORIGIN)
}

/// The caller's line for what [`send_now`] did.
#[must_use]
pub fn delivery_line(delivery: Option<&Delivery>) -> String {
    match delivery {
        Some(Delivery::Sent(number, state)) => created_line(*number, *state),
        Some(Delivery::Waiting(reason)) => format!(
            "Task proposal not sent yet ({reason}); it stays queued and the next LeanCTX Cloud sync sends it."
        ),
        Some(Delivery::Failed(problem)) => format!("Task proposal not sent: {problem}."),
        None => "Task proposal went out with the last LeanCTX Cloud sync.".into(),
    }
}

fn send(
    session: &Session,
    vault_id: &str,
    proposal: &QueuedProposal,
) -> Result<(u64, lean_ctx_protocol::cloud::tasks::TaskState), CloudError> {
    let person_origin = crate::session_ref::announced(&session.store, FEATURE_TASK_PERSON_ORIGIN);
    let request = proposal.request_for(person_origin);
    let key = proposal.idempotency_key();
    let sent = session
        .with_client(|client| super::client::TasksApi { client, vault_id }.propose(&request, &key))
        .map(|ack| (ack.number, ack.state));
    super::reads::outdate(session.store.data_dir(), vault_id);
    sent
}

/// What `lean-ctx cloud sync` prints for a task Cloud created from a
/// proposal: its real state, so an agent knows whether it can claim it.
#[must_use]
pub fn created_line(number: u64, state: lean_ctx_protocol::cloud::tasks::TaskState) -> String {
    use lean_ctx_protocol::cloud::tasks::TaskState;
    match state {
        TaskState::Open => format!("Created task #{number}, open now (any agent may claim it)."),
        other => format!(
            "Created task #{number} ({}).",
            format!("{other:?}").to_lowercase()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// !406 follow-up: `deliver()` of a proposal queued before refs were
    /// keyed (no `ref_keyed`, the unsalted digest) while the local key is
    /// unavailable. It waits in the outbox with its stored ref and is never
    /// moved to the failed outbox; with the key back the key gate lets it
    /// through to the next step.
    #[test]
    fn a_legacy_proposal_without_a_key_waits_and_keeps_its_ref() {
        let (_dir, session) = crate::auth::fixture_session();
        let data = session.store.data_dir().to_path_buf();
        let unsalted = crate::knowledge::draft::session_ref("20261116-091500u123456p4242s0");
        let legacy = serde_json::json!({
            "kind": "agent",
            "stable_identity": "a".repeat(64),
            "root": "/tmp/p",
            "title": "Import the emspace pages",
            "acceptance": "Every page is a fact",
            "session_ref": unsalted,
        });
        let parsed: QueuedProposal = serde_json::from_value(legacy.clone()).unwrap();
        assert!(!parsed.ref_keyed, "a pre-!406 payload");
        let outbox = Outbox::at(&data);
        let id = outbox
            .enqueue(CHANNEL_TASKS, legacy.clone(), UtcDateTime::now())
            .unwrap();
        // No vault is mapped: had the key gate let it pass, the answer
        // would be "not linked" and nothing would be sent either.
        let map = VaultMap::default();

        let keyless = data.join("receipts.hmac-key");
        crate::session_ref::make_keyless(&data);
        let item = outbox.list(CHANNEL_TASKS).remove(0);
        let delivery = deliver(&session, &outbox, &map, item, UtcDateTime::now());
        let Delivery::Waiting(reason) = &delivery else {
            panic!("{delivery:?}");
        };
        assert!(reason.contains("receipts.hmac-key"), "{reason}");
        let items = outbox.list(CHANNEL_TASKS);
        assert_eq!(items.len(), 1, "still queued");
        assert_eq!(items[0].id, id);
        assert_eq!(items[0].attempts, 1);
        assert!(items[0].next_attempt_at.is_some(), "waits with a backoff");
        assert_eq!(
            items[0].payload, legacy,
            "the stored ref is kept, never marked keyed"
        );
        assert_eq!(outbox.depth(CHANNEL_TASKS).failed, 0, "never failed");

        // The key is readable again: the key gate passes, and the proposal
        // waits for its vault, its payload still the stored one (it is keyed
        // again, the same way, when it is sent).
        std::fs::remove_dir(&keyless).unwrap();
        let mut item = outbox.list(CHANNEL_TASKS).remove(0);
        item.next_attempt_at = None;
        let delivery = deliver(&session, &outbox, &map, item, UtcDateTime::now());
        let Delivery::Waiting(reason) = &delivery else {
            panic!("{delivery:?}");
        };
        assert!(reason.contains("not linked"), "{reason}");
        assert!(!reason.contains("receipts.hmac-key"), "{reason}");
        assert_eq!(outbox.list(CHANNEL_TASKS)[0].attempts, 2);
    }

    /// bd 39657 S-a: a proposal from a folder the person unlinked is not
    /// sent through the record of its identity; it waits for a link.
    #[test]
    fn a_proposal_after_unlink_waits_and_is_not_sent() {
        let (_dir, session) = crate::auth::fixture_session();
        let data = session.store.data_dir().to_path_buf();
        let root = tempfile::tempdir().unwrap();
        let root_text = root.path().to_string_lossy().into_owned();
        let stable = "a".repeat(64);
        let outbox = Outbox::at(&data);
        outbox
            .enqueue(
                CHANNEL_TASKS,
                serde_json::json!({
                    "kind": "agent",
                    "stable_identity": stable,
                    "root": root_text,
                    "title": "Import the emspace pages",
                    "acceptance": "Every page is a fact",
                }),
                UtcDateTime::now(),
            )
            .unwrap();
        let mut map = VaultMap::default();
        map.vaults.insert(
            stable,
            crate::vault::VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec!["/elsewhere/checkout".into()],
            },
        );
        crate::unlinked::add(&data, root.path()).unwrap();

        let item = outbox.list(CHANNEL_TASKS).remove(0);
        let delivery = deliver(&session, &outbox, &map, item, UtcDateTime::now());
        assert_eq!(delivery, Delivery::Waiting(crate::unlinked::REFUSAL.into()));
        assert_eq!(outbox.list(CHANNEL_TASKS).len(), 1, "still queued");
        assert_eq!(outbox.depth(CHANNEL_TASKS).failed, 0);
    }

    const INSTANCE: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c09";
    const VAULT: &str = "9d2e4f6a1b3c4d5e8f7a6b5c4d3e2f1a";

    /// A signed person's proposal for `vault`/`instance`, queued; the map
    /// syncs its identity to `now_vault`.
    fn queued_signed(
        session: &Session,
        vault: &str,
        instance: &str,
        now_vault: &str,
    ) -> (Outbox, VaultMap) {
        let proposal = QueuedProposal {
            kind: ProposalKind::Person,
            stable_identity: "b".repeat(64),
            root: "/tmp/rc467-p".into(),
            facts: Vec::new(),
            title: "Show the retry budget in settings".into(),
            goal: None,
            acceptance: "The settings page names it".into(),
            role: None,
            session_ref: Some(PERSON_SESSION_LABEL.into()),
            local_number: None,
            ref_keyed: true,
            signed: Some(SignedPerson {
                signed_at: "2026-11-24T09:30:00Z".into(),
                vault_id: vault.into(),
                instance_id: instance.into(),
                task_signature: "A".repeat(86),
            }),
        };
        let outbox = Outbox::at(session.store.data_dir());
        outbox
            .enqueue(
                CHANNEL_TASKS,
                serde_json::to_value(&proposal).unwrap(),
                UtcDateTime::now(),
            )
            .unwrap();
        let mut map = VaultMap::default();
        map.vaults.insert(
            "b".repeat(64),
            crate::vault::VaultRecord {
                vault_id: now_vault.into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: Vec::new(),
            },
        );
        (outbox, map)
    }

    /// rc4-67 (bd S3): a signature names its vault and machine. When the
    /// project now syncs to another vault, or the machine is another one,
    /// the proposal fails for good (never retried, never sent elsewhere),
    /// before any request.
    #[test]
    fn a_signed_proposal_is_never_sent_to_another_vault_or_from_another_machine() {
        let (_dir, mut session) = crate::auth::fixture_session();
        // INSTANCE in upper case: the same machine.
        session.instance.instance_id = INSTANCE.to_ascii_uppercase();
        let other_vault = "0123456789abcdef0123456789abcdef";
        let other_instance = "0a80ace1-52f1-49c2-85f7-6991bada0099";
        for (vault, instance, now_vault) in [
            (VAULT, INSTANCE, other_vault),
            (VAULT, other_instance, VAULT),
        ] {
            let (outbox, map) = queued_signed(&session, vault, instance, now_vault);
            let failed_before = outbox.failed(CHANNEL_TASKS).len();
            let item = outbox.list(CHANNEL_TASKS).remove(0);
            let delivery = deliver(&session, &outbox, &map, item, UtcDateTime::now());
            let Delivery::Failed(problem) = &delivery else {
                panic!("{delivery:?}");
            };
            assert!(problem.contains("syncs elsewhere"), "{problem}");
            assert!(outbox.list(CHANNEL_TASKS).is_empty(), "not retried");
            assert_eq!(outbox.failed(CHANNEL_TASKS).len(), failed_before + 1);
        }
    }

    /// rc4-67 (bd S1): the ids are signed as the Cloud checks them.
    #[test]
    fn person_ids_are_signed_in_the_clouds_spelling() {
        use crate::consent::task_key::canonical_person_ids;
        let canonical = Some((INSTANCE.to_owned(), VAULT.to_owned()));
        for instance in [INSTANCE, "3F1C2B6A-8D4E-4A7B-9C10-5E2F6A7B8C09"] {
            assert_eq!(
                canonical_person_ids(instance, VAULT),
                canonical,
                "{instance}"
            );
        }
        assert_eq!(
            canonical_person_ids(INSTANCE, &VAULT.to_ascii_uppercase()),
            canonical
        );
        for (instance, vault) in [
            ("inst-1", VAULT),
            ("inst_01jb8z3k4m5n6p7q8r9s0t1v2w", VAULT),
            ("inst_3f1c2b6a8d4e4a7b9c105e2f6a7b8c09", VAULT),
            ("3f1c2b6a8d4e4a7b9c105e2f6a7b8c09", VAULT),
            (INSTANCE, "p-1"),
            (INSTANCE, "9d2e4f6a-1b3c-4d5e-8f7a-6b5c4d3e2f1a"),
            (INSTANCE, ""),
        ] {
            assert_eq!(
                canonical_person_ids(instance, vault),
                None,
                "{instance} {vault}"
            );
        }
        // prepare refuses what it cannot sign so.
        let (_dir, session) = crate::auth::fixture_session();
        let fields = Fields {
            title: "Show the retry budget in settings",
            goal: None,
            acceptance: "The settings page names it",
            role: None,
        };
        let refused = prepare_person_task(
            session.store.data_dir(),
            Path::new("/tmp/rc467-p"),
            &fields,
            "inst-1",
            VAULT,
            UtcDateTime::now(),
        );
        assert!(refused.is_err());
    }

    /// rc4-67 (bd S3): the Cloud refusing a person's signature
    /// (`person_unproven`, `signature_expired`) is final: neither transient
    /// nor `Refused`, so `deliver()` fails it instead of retrying.
    #[test]
    fn an_unproven_or_expired_person_signature_is_never_retried() {
        use crate::error::TaskRefusal;
        for (reason, refusal) in [
            ("person_unproven", TaskRefusal::PersonUnproven),
            ("signature_expired", TaskRefusal::SignatureExpired),
        ] {
            let body = serde_json::json!({"error": {"code": "forbidden_right", "message": "x", "reason": reason}});
            let error = CloudError::from_response(403, &body);
            assert!(
                matches!(&error, CloudError::Task { refusal: r, .. } if *r == refusal),
                "{error:?}"
            );
            assert!(!error.is_transient(), "{reason}");
            assert!(!matches!(error, CloudError::Refused(_)), "{reason}");
        }
    }
}
