// SPDX-License-Identifier: Apache-2.0
//! Directives (contract §37): a person changed the plan or a team decision
//! in Code, and Cloud told this engine. Each affected local agent gets a
//! record here, first **offered**: invisible to the agent (no bus entry, no
//! pending line, no ack). Only the person's confirmed "Send to agents" in
//! LeanCTX Code releases it, through the local CLI
//! `lean-ctx cloud plan release` with the random release id (on stdin,
//! never in argv) of that one directive and agent ([`DirectiveStore::release_offered`]).
//! The person's confirmed body goes with the release: for a plan, the
//! plan's goal and this agent's own brief of exactly the released version
//! ([`ReleaseBody`]); the goal and the brief are checked against the
//! digests of the verified plan body (the slot's signed `brief_sha256`). The agent's bus entry carries that
//! and nothing else (no other part, no other version, no release id).
//! Then the agent gets one bus entry (category `directive`, priority high)
//! and the record stays pending until the agent answers with
//! `ctx_agent action=ack`. Nothing on the agent bus or a Cloud frame can
//! release a directive: issuing ([`DirectiveStore::issue`]) never touches
//! the bus, and the release id is shown only to the app's reads.
//!
//! The records live in `<agents>/directives.json`, next to `registry.json`
//! and written under its lock, not inside it: older MCP processes re-write
//! the registry with their own schema and would drop a field they do not
//! know (the same reason `ProcessIdentityIndex` has its own file).
//!
//! A directive is an instruction, never a control: nothing here pauses,
//! stops or gates local work. Free-text refusal reasons stay on this
//! machine; Cloud receives only the state and the reason code.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::AgentRegistry;
use crate::core::a2a::message::{MessagePriority, PrivacyLevel};

/// The sender every directive entry carries on the local bus.
pub const DIRECTIVE_SENDER: &str = "cloud";
/// The bus category of a directive entry.
pub const DIRECTIVE_CATEGORY: &str = "directive";
/// Reason codes an agent may refuse a directive with (§37.4).
pub const REFUSE_REASONS: [&str; 5] = [
    "conflicts_with_decision",
    "task_finished",
    "out_of_scope",
    "unverified_plan",
    "other",
];
/// Records kept; the oldest answered ones drop first.
const MAX_DIRECTIVES: usize = 500;
/// A local refusal reason is kept to this many characters.
const MAX_REASON_CHARS: usize = 500;
/// Bytes of a released plan goal (the body's 4000 characters, UTF-8).
pub const MAX_GOAL_BYTES: usize = 16_000;
/// Bytes of a released brief (the body's 8000 characters, UTF-8).
pub const MAX_BRIEF_BYTES: usize = 32_000;

const ACK_HINT: &str = "Answer with ctx_agent action=ack directive_id={id} verdict=accept, or verdict=refuse reason_code=conflicts_with_decision|task_finished|out_of_scope|unverified_plan|other.";

/// How an agent answers `directive_id` (the last line of every directive).
pub fn ack_hint(directive_id: &str) -> String {
    ACK_HINT.replace("{id}", directive_id)
}

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    Plan,
    Decision,
}

impl DirectiveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DirectiveKind::Plan => "plan",
            DirectiveKind::Decision => "decision",
        }
    }
}

/// Where the agent stands with one directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveState {
    /// Received from Cloud, waiting for the person's release in LeanCTX
    /// Code. Invisible to the agent: no bus entry, no pending line, no ack.
    Offered,
    /// Released and delivered to the agent's inbox, not answered yet.
    Pending,
    Accepted,
    Refused,
    /// A newer version of the same plan or decision replaced it unanswered.
    Superseded,
}

impl DirectiveState {
    /// The state as the plan ack route names it (§37.5); `None` for one
    /// Cloud is not told about.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            DirectiveState::Offered | DirectiveState::Superseded => None,
            DirectiveState::Pending => Some("delivered"),
            DirectiveState::Accepted => Some("acked"),
            DirectiveState::Refused => Some("refused"),
        }
    }

    /// The state as the app's local read shows it: `offered` besides the
    /// wire states; `None` for a superseded one.
    pub fn local(self) -> Option<&'static str> {
        match self {
            DirectiveState::Offered => Some("offered"),
            other => other.wire(),
        }
    }
}

/// One directive for one local agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directive {
    /// `plan_id:version:slot` for a plan, `decision_id:status` for a decision.
    pub directive_id: String,
    pub agent_id: String,
    pub project_root: String,
    pub kind: DirectiveKind,
    /// `plan_id` or `decision_id`.
    pub subject: String,
    /// Plan version; 0 for a decision.
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The Cloud vault id the change came from.
    pub vault_id: String,
    /// The plan's `content_sha256`; empty for a decision.
    #[serde(default)]
    pub digest: String,
    /// The bus entry that carried it; empty while offered.
    pub message_id: String,
    pub state: DirectiveState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// The agent's own words; local only, never sent to Cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The last state Cloud recorded for this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<DirectiveState>,
    /// The bus text the agent reads once the directive is released.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// SHA-256 (hex) of the release id: what a release is matched
    /// against. Only this digest is in `directives.json`, which an agent's
    /// read tools may open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_sha256: Option<String>,
    /// The release id itself (`rid`, 64 hex of 32 random bytes): the one
    /// handle that releases this directive for this agent. In memory only:
    /// kept in [`RELEASE_IDS_FILE`] (a secret path every agent read tool
    /// refuses) and filled by [`DirectiveStore::load_for_app`] for the
    /// app's read (`cloud.plan.updates`), while offered.
    #[serde(skip)]
    pub release_id: Option<String>,
    /// When the person released it (RFC 3339 UTC): the local audit of the
    /// release, shown with the version and agent on the app's read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    /// The signed `brief_sha256` of the agent's slot (`sha256:<64 hex>` of
    /// the brief's UTF-8 bytes); `None` for a plan-wide directive and a
    /// decision. A released brief must match it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_sha256: Option<String>,
    /// `sha256:<64 hex>` of the verified plan body's goal; `None` for a
    /// decision (and a record from before). A released goal must match it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_sha256: Option<String>,
}

/// What the person confirmed in LeanCTX Code for one release of a plan
/// directive (the JSON `cloud plan release` reads on stdin): the version,
/// the plan's goal, and the agent's own brief (`null` for an agent no part
/// of the plan names). Nothing else is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBody {
    pub version: u64,
    pub goal: String,
    #[serde(default)]
    pub brief: Option<String>,
}

/// Why [`DirectiveStore::release_offered`] released nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseError {
    /// Not the shape of a release id (a directive id, a guess).
    NotReleaseId,
    /// A release id no directive of this machine carries (unknown, or
    /// another machine's).
    Unknown,
    /// The offer was superseded by a newer version before it was released.
    Superseded,
    /// The body does not match the directive (version, brief digest) or is
    /// not well-formed.
    Body(String),
    /// The bus could not take the entry; nothing was released.
    Internal(String),
}

impl ReleaseError {
    /// The error code of the CLI's JSON line.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotReleaseId | Self::Body(_) => "invalid_argument",
            Self::Unknown => "not_found",
            Self::Superseded => "superseded",
            Self::Internal(_) => "internal",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotReleaseId => "not a release id (64 lowercase hex)".to_string(),
            Self::Unknown => UNKNOWN_RELEASE_ID.to_string(),
            Self::Superseded => {
                "superseded: a newer version was offered before this one was released".to_string()
            }
            Self::Body(why) | Self::Internal(why) => why.clone(),
        }
    }
}

/// What [`DirectiveStore::release_offered`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// False when this id was released before (the same release again):
    /// nothing changed, the earlier delivery stands.
    pub released: bool,
    pub directive_id: String,
    pub agent_id: String,
    pub version: u64,
    pub released_at: Option<String>,
}

/// The error of [`DirectiveStore::release_offered`] for a well-formed id
/// no directive carries.
pub const UNKNOWN_RELEASE_ID: &str = "unknown release id";

/// Bytes of a release id.
const RELEASE_ID_BYTES: usize = 32;

/// A fresh release id from the OS CSPRNG (256 bit, lowercase hex).
fn new_release_id() -> Result<String, String> {
    let mut bytes = [0u8; RELEASE_ID_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| format!("release id: entropy source failed: {e}"))?;
    Ok(hex::encode(bytes))
}

/// The file next to `directives.json` that holds the release ids of the
/// offered directives (`{release_sha256: release_id}`, mode 0600). Its name
/// is a secret path ([`crate::core::io_boundary::is_secret_like`]): no
/// `ctx_*` read, search or shell tool and no hooked native read opens it.
pub const RELEASE_IDS_FILE: &str = "directive-release-ids.json";

/// SHA-256 (hex) of a release id.
fn release_sha256(rid: &str) -> String {
    hex::encode(Sha256::digest(rid.as_bytes()))
}

/// Whether `rid` has the shape of a release id (64 lowercase hex). A
/// directive id (`<plan>:<v>:<slot>`) never has it.
pub fn is_release_id(rid: &str) -> bool {
    rid.len() == RELEASE_ID_BYTES * 2 && rid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A directive to issue, as the Cloud relay builds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveIssue {
    pub directive_id: String,
    pub kind: DirectiveKind,
    pub subject: String,
    pub version: u64,
    pub slot: Option<String>,
    pub worker_id: Option<String>,
    pub task_id: Option<String>,
    pub vault_id: String,
    pub digest: String,
    /// The slot's signed `brief_sha256`; `None` for a plan-wide directive
    /// and a decision.
    pub brief_sha256: Option<String>,
    /// The digest of the verified body's goal; `None` for a decision.
    pub goal_sha256: Option<String>,
    /// The bus text of a decision. A plan's entry is built at release from
    /// the confirmed [`ReleaseBody`] instead.
    pub text: String,
}

/// The agent's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    Refuse,
}

impl Verdict {
    pub fn parse(verdict: &str) -> Option<Self> {
        match verdict {
            "accept" => Some(Verdict::Accept),
            "refuse" => Some(Verdict::Refuse),
            _ => None,
        }
    }
}

/// `<agents>/directives.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectiveStore {
    #[serde(default)]
    pub directives: Vec<Directive>,
}

/// Text a person typed may carry newlines and tabs, no other control
/// characters (they could fake a bus line or a terminal sequence) and no
/// character a Cloud text field refuses (bidi and other format controls,
/// zero-width characters: [`lean_ctx_protocol::cloud::invisible`]), which
/// could hide or reorder what the agent reads.
fn plain_text(field: &str, text: &str, max: usize) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    if text.len() > max {
        return Err(format!("{field} is longer than {max} bytes"));
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
    Ok(text.to_string())
}

/// `sha256:<64 hex>` of `text`'s UTF-8 bytes.
pub fn brief_digest(text: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
}

/// The one bus entry a released directive becomes. A plan's entry is the
/// plan id and version, `body`'s goal, this agent's own brief (or, for a
/// plan-wide directive, that no part names it) and how to answer: no other
/// part, no other version, no release id. A decision keeps its text and
/// takes no body.
pub fn release_text(d: &Directive, body: Option<&ReleaseBody>) -> Result<String, String> {
    if d.kind == DirectiveKind::Decision {
        if body.is_some() {
            return Err("a decision release takes no body".to_string());
        }
        return Ok(d.text.clone());
    }
    let body = body.ok_or_else(|| {
        "a plan release needs the confirmed body on stdin ({\"release_id\",\"version\",\"goal\",\"brief\"})"
            .to_string()
    })?;
    if body.version != d.version {
        return Err(format!(
            "the body is version {}, the release id names version {}",
            body.version, d.version
        ));
    }
    if d.goal_sha256
        .as_ref()
        .is_some_and(|signed| &brief_digest(&body.goal) != signed)
    {
        return Err("goal does not match the verified plan body of this version".to_string());
    }
    let goal = plain_text("goal", &body.goal, MAX_GOAL_BYTES)?;
    let part = match (&d.brief_sha256, &body.brief) {
        (Some(signed), Some(brief)) => {
            if &brief_digest(brief) != signed {
                return Err(
                    "brief does not match the signed brief_sha256 of this agent's part".to_string(),
                );
            }
            let brief = plain_text("brief", brief, MAX_BRIEF_BYTES)?;
            let slot = d.slot.as_deref().unwrap_or_default();
            format!("Your part ({slot}):\n{brief}")
        }
        (Some(_), None) => {
            return Err("brief is required: a part of the plan names this agent".to_string());
        }
        (None, Some(_)) => {
            return Err("brief must be null: no part of the plan names this agent".to_string());
        }
        (None, None) => {
            "No part of this plan names you: check that your work still fits its goal.".to_string()
        }
    };
    Ok(format!(
        "LeanCTX Cloud: plan {plan} v{version}, sent by the person from LeanCTX Code.\nGoal:\n{goal}\n{part}\n{hint}",
        plan = d.subject,
        version = d.version,
        hint = ack_hint(&d.directive_id),
    ))
}

fn store_path() -> Result<PathBuf, String> {
    Ok(super::persistence::agents_dir()?.join("directives.json"))
}

fn release_ids_path() -> Result<PathBuf, String> {
    Ok(super::persistence::agents_dir()?.join(RELEASE_IDS_FILE))
}

type ReleaseIds = std::collections::BTreeMap<String, String>;

/// The id file as read: absent, readable, or damaged (unreadable or not
/// the expected JSON). A symlink in its place is an error, never followed.
enum IdFile {
    Absent,
    Ids(ReleaseIds),
    Damaged(String),
}

fn read_release_ids(path: &std::path::Path) -> Result<IdFile, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(IdFile::Absent),
        Err(error) => return Ok(IdFile::Damaged(error.to_string())),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "release ids {}: a symlink, not followed",
                path.display()
            ));
        }
        Ok(_) => {}
    }
    Ok(match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(ids) => IdFile::Ids(ids),
            Err(error) => IdFile::Damaged(error.to_string()),
        },
        Err(error) => IdFile::Damaged(error.to_string()),
    })
}

/// The ids for the app's read; a missing, damaged or symlinked file gives
/// none (fail closed: nothing can be released), with a warning.
fn load_release_ids() -> ReleaseIds {
    let Ok(path) = release_ids_path() else {
        return ReleaseIds::new();
    };
    match read_release_ids(&path) {
        Ok(IdFile::Ids(ids)) => ids,
        Ok(IdFile::Absent) => ReleaseIds::new(),
        Ok(IdFile::Damaged(error)) => {
            tracing::warn!("release ids {} unreadable: {error}", path.display());
            ReleaseIds::new()
        }
        Err(error) => {
            tracing::warn!("{error}");
            ReleaseIds::new()
        }
    }
}

impl DirectiveStore {
    /// The records on disk; a missing or damaged file reads as none.
    pub fn load() -> Self {
        store_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The records with the release id of each offered one: the app's
    /// read (`cloud.plan.updates`) only. Every other reader uses
    /// [`Self::load`], which never holds a release id.
    pub fn load_for_app() -> Self {
        let mut store = Self::load();
        let ids = load_release_ids();
        for d in &mut store.directives {
            if d.state == DirectiveState::Offered {
                d.release_id = d
                    .release_sha256
                    .as_deref()
                    .and_then(|sha| ids.get(sha))
                    .filter(|rid| d.release_sha256.as_deref() == Some(&release_sha256(rid)))
                    .cloned();
            }
        }
        store
    }

    /// The id file first, then the records: a crash in between leaves an
    /// id whose record is missing (dropped at the next save), never an
    /// offered record without its release id.
    fn save(&self) -> Result<(), String> {
        self.save_release_ids()?;
        let path = store_path()?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic(&path, &json)
            .map_err(|error| format!("persist directives {}: {error}", path.display()))
    }

    /// Keep the ids of the offered directives only: a new one is added, a
    /// released or superseded one is dropped.
    fn save_release_ids(&self) -> Result<(), String> {
        let dir = super::persistence::agents_dir()?;
        let path = dir.join(RELEASE_IDS_FILE);
        let mut ids = match read_release_ids(&path)? {
            IdFile::Absent => ReleaseIds::new(),
            IdFile::Ids(ids) => ids,
            // Never overwritten unseen: the damaged file is kept aside
            // (`….damaged`, also a secret path) with a warning. Its ids are
            // lost: their directives can no longer be released (fail closed).
            IdFile::Damaged(error) => {
                let aside = dir.join(format!("{RELEASE_IDS_FILE}.damaged"));
                tracing::warn!(
                    "release ids {} damaged ({error}); kept as {} and started anew: \
                     the offered directives it named can no longer be released",
                    path.display(),
                    aside.display()
                );
                std::fs::rename(&path, &aside).map_err(|e| {
                    format!("set aside damaged release ids {}: {e}", path.display())
                })?;
                ReleaseIds::new()
            }
        };
        for d in &self.directives {
            if let (Some(sha), Some(rid)) = (&d.release_sha256, &d.release_id) {
                ids.insert(sha.clone(), rid.clone());
            }
        }
        ids.retain(|sha, _| {
            self.directives.iter().any(|d| {
                d.state == DirectiveState::Offered && d.release_sha256.as_deref() == Some(sha)
            })
        });
        if ids.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("remove release ids {}: {error}", path.display()))
                }
                _ => Ok(()),
            };
        }
        let json = serde_json::to_string(&ids).map_err(|e| e.to_string())?;
        // Crash-atomic, mode 0600, and no symlink below the agents dir is
        // followed (the file itself included).
        crate::core::pathjail_write::write_below(
            &dir,
            std::path::Path::new(RELEASE_IDS_FILE),
            json.as_bytes(),
            0o600,
        )
        .map(|_| ())
        .map_err(|error| format!("persist release ids {}: {error}", path.display()))
    }

    /// Change the registry and the records under the registry lock.
    pub fn mutate<T>(
        f: impl FnOnce(&mut AgentRegistry, &mut DirectiveStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let (_, out) = AgentRegistry::mutate_locked(|registry| {
            let mut store = Self::load();
            // The registry is written even when `f` fails part-way, so the
            // records of the directives it already posted are written too:
            // a bus entry never lacks its record (issuing again is a no-op).
            let out = f(registry, &mut store);
            store.save().and(out)
        })?;
        out
    }

    /// [`Self::release_offered`] for the CLI, so that its answer matches
    /// what happened: the bus entry (registry) is written first, then the
    /// records. A failed registry write delivered nothing (`Err`, the
    /// directive stays offered). A failed record write after it does not
    /// undo the delivery: the release is reported with the error as a
    /// warning (a retry finds the keyed entry and changes nothing).
    pub fn mutate_release(
        release_id: &str,
        body: Option<&ReleaseBody>,
    ) -> Result<(Release, Option<String>), ReleaseError> {
        let dir = super::persistence::agents_dir().map_err(ReleaseError::Internal)?;
        let (_, out) = AgentRegistry::mutate_locked(|registry| {
            let mut store = Self::load();
            let release = store.release_offered(registry, release_id, body)?;
            if !release.released {
                return Ok((release, None));
            }
            registry.save_locked(&dir).map_err(ReleaseError::Internal)?;
            Ok((release, store.save().err()))
        })
        .map_err(ReleaseError::Internal)?;
        out
    }

    /// Record `issue` for `agent_id` in `root` as **offered**: nothing is
    /// posted on the bus and the agent sees nothing until the person
    /// releases it ([`Self::release_offered`]). Offering the same directive
    /// to the same agent again changes nothing (a catch-up after a frame).
    /// Offered directives of an older version of the same plan, or of the
    /// same decision, are superseded (their release ids release nothing);
    /// a released one stays as it is until the newer one is released.
    /// True when a record was added.
    pub fn issue(
        &mut self,
        root: &str,
        agent_id: &str,
        issue: &DirectiveIssue,
    ) -> Result<bool, String> {
        if self
            .directives
            .iter()
            .any(|d| d.agent_id == agent_id && d.directive_id == issue.directive_id)
        {
            return Ok(false);
        }
        let release_id = new_release_id()?;
        self.supersede_older(
            agent_id,
            issue.kind,
            &issue.subject,
            issue.version,
            &issue.directive_id,
            DirectiveState::Offered,
        );
        self.directives.push(Directive {
            directive_id: issue.directive_id.clone(),
            agent_id: agent_id.to_string(),
            project_root: root.to_string(),
            kind: issue.kind,
            subject: issue.subject.clone(),
            version: issue.version,
            slot: issue.slot.clone(),
            worker_id: issue.worker_id.clone(),
            task_id: issue.task_id.clone(),
            vault_id: issue.vault_id.clone(),
            digest: issue.digest.clone(),
            message_id: String::new(),
            state: DirectiveState::Offered,
            reason_code: None,
            reason: None,
            reported: None,
            // An offered plan directive keeps no text: `directives.json` is
            // readable by agents, and the delivered text is the person's
            // confirmed body (set on release).
            text: match issue.kind {
                DirectiveKind::Plan => String::new(),
                DirectiveKind::Decision => issue.text.clone(),
            },
            release_sha256: Some(release_sha256(&release_id)),
            release_id: Some(release_id),
            released_at: None,
            brief_sha256: issue.brief_sha256.clone(),
            goal_sha256: issue.goal_sha256.clone(),
        });
        self.prune();
        Ok(true)
    }

    /// Issue a team decision directive to `agent_id` and post it at once.
    /// Cloud announces a decision change (`decision.changed`) only after the
    /// person's confirmed "Send to agents" in LeanCTX Code, so the frame is
    /// the release; the relay calls this only from that frame. The same
    /// directive to the same agent again posts nothing (false), unless an
    /// earlier frame recorded it but could not post it: a decision directive
    /// still offered is released by the next frame. A plan directive is
    /// refused: it waits for [`Self::release_offered`].
    pub fn deliver_decision(
        &mut self,
        registry: &mut AgentRegistry,
        root: &str,
        agent_id: &str,
        issue: &DirectiveIssue,
    ) -> Result<bool, String> {
        if issue.kind != DirectiveKind::Decision {
            return Err("only a team decision is delivered from its frame".to_string());
        }
        self.issue(root, agent_id, issue)?;
        let Some(held) = self
            .directives
            .iter()
            .find(|d| d.agent_id == agent_id && d.directive_id == issue.directive_id)
        else {
            return Err("the issued decision directive is not recorded".to_string());
        };
        // Released, superseded or pruned before: nothing to post again.
        if held.kind != DirectiveKind::Decision || held.state != DirectiveState::Offered {
            return Ok(false);
        }
        let release_id = held
            .release_id
            .clone()
            .ok_or_else(|| "the issued decision directive has no release id".to_string())?;
        self.release_offered(registry, &release_id, None)
            .map(|release| release.released)
            .map_err(|error| error.message())
    }

    /// Mark `agent_id`'s directives in `state` of an older version of the
    /// same plan (any other id of the same decision) superseded.
    fn supersede_older(
        &mut self,
        agent_id: &str,
        kind: DirectiveKind,
        subject: &str,
        version: u64,
        directive_id: &str,
        state: DirectiveState,
    ) {
        for older in self.directives.iter_mut().filter(|d| {
            d.agent_id == agent_id
                && d.kind == kind
                && d.subject == subject
                && d.state == state
                && d.directive_id != directive_id
                && (kind == DirectiveKind::Decision || d.version < version)
        }) {
            older.state = DirectiveState::Superseded;
        }
    }

    /// Release the one offered directive `release_id` names, for the agent
    /// it was offered to: post its bus entry and make it pending. Called
    /// by the local CLI `lean-ctx cloud plan release` after the person's
    /// confirmed "Send to agents" in LeanCTX Code, and for a team decision by
    /// [`Self::deliver_decision`]; no bus or Cloud handler reaches it for a
    /// plan.
    ///
    /// Per agent and version: the id releases nothing else. A plan entry is
    /// exactly `body`'s goal and this agent's brief ([`release_text`]); the
    /// body must name the directive's version and its brief must match the
    /// slot's signed digest. The same id again after its release changes
    /// nothing (`released: false`); an id superseded before its release, an
    /// unknown id, or anything that is no release id is an error.
    pub fn release_offered(
        &mut self,
        registry: &mut AgentRegistry,
        release_id: &str,
        body: Option<&ReleaseBody>,
    ) -> Result<Release, ReleaseError> {
        if !is_release_id(release_id) {
            return Err(ReleaseError::NotReleaseId);
        }
        let index = self
            .directives
            .iter()
            .position(|d| d.release_sha256.as_deref() == Some(&release_sha256(release_id)))
            .ok_or(ReleaseError::Unknown)?;
        let found = &self.directives[index];
        if found.state != DirectiveState::Offered {
            if found.released_at.is_none() {
                return Err(ReleaseError::Superseded);
            }
            return Ok(Release {
                released: false,
                directive_id: found.directive_id.clone(),
                agent_id: found.agent_id.clone(),
                version: found.version,
                released_at: found.released_at.clone(),
            });
        }
        let d = found.clone();
        let text = release_text(&d, body).map_err(ReleaseError::Body)?;
        let key = format!(
            "{}directive:{}",
            super::CLOUD_INBOUND_KEY_PREFIX,
            &crate::core::hasher::hash_hex(
                format!("{}\0{}", d.agent_id, d.directive_id).as_bytes()
            )[..32]
        );
        let message_id = registry
            .post_message_keyed(
                Some(&d.project_root),
                DIRECTIVE_SENDER,
                Some(&d.agent_id),
                DIRECTIVE_CATEGORY,
                &text,
                PrivacyLevel::Team,
                MessagePriority::High,
                None,
                Some(&key),
            )
            .map_err(ReleaseError::Internal)?;
        if let Some(entry) = registry.scratchpad.iter_mut().find(|e| e.id == message_id) {
            entry.task_id.clone_from(&d.task_id);
            let (subject_key, version) = match d.kind {
                DirectiveKind::Plan => ("plan_id", Some(d.version.to_string())),
                DirectiveKind::Decision => ("decision_id", None),
            };
            entry
                .metadata
                .insert(subject_key.to_string(), d.subject.clone());
            if let Some(version) = version {
                entry.metadata.insert("plan_version".to_string(), version);
            }
            entry
                .metadata
                .insert("directive_id".to_string(), d.directive_id.clone());
        }
        self.supersede_older(
            &d.agent_id,
            d.kind,
            &d.subject,
            d.version,
            &d.directive_id,
            DirectiveState::Pending,
        );
        let released_at = lean_ctx_time::UtcDateTime::now()
            .to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true);
        let record = &mut self.directives[index];
        record.state = DirectiveState::Pending;
        record.message_id = message_id;
        record.released_at = Some(released_at.clone());
        record.text = text;
        record.release_id = None;
        Ok(Release {
            released: true,
            directive_id: d.directive_id,
            agent_id: d.agent_id,
            version: d.version,
            released_at: Some(released_at),
        })
    }

    fn prune(&mut self) {
        while self.directives.len() > MAX_DIRECTIVES {
            let index = self
                .directives
                .iter()
                .position(|d| !matches!(d.state, DirectiveState::Pending | DirectiveState::Offered))
                .unwrap_or(0);
            self.directives.remove(index);
        }
    }

    /// Record `agent_id`'s answer to `directive_id`. A refusal needs one of
    /// [`REFUSE_REASONS`]; `reason` is kept locally. Answering again
    /// replaces the earlier answer. The directive's bus entry counts as read.
    pub fn ack(
        &mut self,
        registry: &mut AgentRegistry,
        agent_id: &str,
        directive_id: &str,
        verdict: Verdict,
        reason_code: Option<&str>,
        reason: Option<&str>,
    ) -> Result<Directive, String> {
        let directive = self
            .directives
            .iter_mut()
            // An offered directive does not exist for the agent yet.
            .find(|d| {
                d.agent_id == agent_id
                    && d.directive_id == directive_id
                    && d.state != DirectiveState::Offered
            })
            .ok_or_else(|| format!("no directive {directive_id} for this agent"))?;
        if directive.state == DirectiveState::Superseded {
            return Err(format!(
                "directive {directive_id} was superseded by a newer version: answer the newer directive"
            ));
        }
        match verdict {
            Verdict::Accept => {
                directive.state = DirectiveState::Accepted;
                directive.reason_code = None;
            }
            Verdict::Refuse => {
                let code = reason_code
                    .filter(|code| REFUSE_REASONS.contains(code))
                    .ok_or_else(|| {
                        format!(
                            "verdict=refuse needs reason_code, one of: {}",
                            REFUSE_REASONS.join(", ")
                        )
                    })?;
                directive.state = DirectiveState::Refused;
                directive.reason_code = Some(code.to_string());
            }
        }
        directive.reason = reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| r.chars().take(MAX_REASON_CHARS).collect());
        if let Some(entry) = registry
            .scratchpad
            .iter_mut()
            .find(|e| e.id == directive.message_id)
            && !entry.read_by.iter().any(|reader| reader == agent_id)
        {
            entry.read_by.push(agent_id.to_string());
        }
        Ok(directive.clone())
    }

    /// `agent_id`'s unanswered directives, by id.
    pub fn pending_for(&self, agent_id: &str) -> Vec<&Directive> {
        let mut pending: Vec<&Directive> = self
            .directives
            .iter()
            .filter(|d| d.agent_id == agent_id && d.state == DirectiveState::Pending)
            .collect();
        pending.sort_by(|a, b| a.directive_id.cmp(&b.directive_id));
        pending
    }

    /// Plan directives whose state Cloud has not recorded yet.
    pub fn unreported_plans(&self) -> Vec<&Directive> {
        self.directives
            .iter()
            .filter(|d| {
                d.kind == DirectiveKind::Plan
                    && d.state.wire().is_some()
                    && d.reported != Some(d.state)
            })
            .collect()
    }

    /// Note that Cloud recorded `state` for these `(agent, directive)`s; a
    /// state that moved on since stays unreported.
    pub fn mark_reported(&mut self, done: &[(String, String, DirectiveState)]) {
        for (agent, id, state) in done {
            if let Some(d) = self
                .directives
                .iter_mut()
                .find(|d| &d.agent_id == agent && &d.directive_id == id && d.state == *state)
            {
                d.reported = Some(*state);
            }
        }
    }
}

/// The one-line reminder of a pending directive (stable text, #498).
pub fn pending_line(directive: &Directive) -> String {
    match directive.kind {
        DirectiveKind::Plan => format!(
            "[directive pending: plan {} v{}, ack with ctx_agent action=ack directive_id={}]",
            directive.subject, directive.version, directive.directive_id
        ),
        DirectiveKind::Decision => format!(
            "[directive pending: decision {}, ack with ctx_agent action=ack directive_id={}]",
            directive.subject, directive.directive_id
        ),
    }
}

/// The reminder lines for `agent_id`, sorted; `None` when nothing is pending.
pub fn pending_lines(store: &DirectiveStore, agent_id: &str) -> Option<String> {
    let lines: Vec<String> = store
        .pending_for(agent_id)
        .into_iter()
        .map(pending_line)
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// When the records last changed; `None` while there are none.
pub fn directives_modified() -> Option<SystemTime> {
    std::fs::metadata(store_path().ok()?)
        .and_then(|m| m.modified())
        .ok()
}

type SuffixCache = Option<(Option<SystemTime>, String, Option<String>)>;
static SUFFIX_CACHE: Mutex<SuffixCache> = Mutex::new(None);

/// [`pending_lines`] for `agent_id` from disk, re-read only when the file
/// changed: the tool pipeline asks on every call.
pub fn pending_suffix_for(agent_id: &str) -> Option<String> {
    let modified = directives_modified();
    modified?;
    let mut cache = SUFFIX_CACHE.lock().ok()?;
    if let Some((at, agent, lines)) = cache.as_ref()
        && *at == modified
        && agent == agent_id
    {
        return lines.clone();
    }
    let lines = pending_lines(&DirectiveStore::load(), agent_id);
    *cache = Some((modified, agent_id.to_string(), lines.clone()));
    lines
}

#[cfg(test)]
#[path = "directives_tests.rs"]
mod tests;
