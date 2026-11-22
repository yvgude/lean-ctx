// SPDX-License-Identifier: Apache-2.0
//! The plan versions this instance applied, with what its agents answered,
//! for a local reader (kind `cloud.plan.updates`, contract §37.6).
//!
//! Built from the verified plan documents the relay kept
//! (`<cloud data>/plans/<plan_id>.json`) and the agents' directives
//! (`<agents>/directives.json`). Nothing is sent to Cloud. The text
//! (`title`, `goal`, a part's `role`, `brief` and `folder`, and the same
//! per agent) comes from the verified plan body only.
//!
//! This is the app's read: it is the only place that shows an offered
//! directive's release id (`release_id`) and the plan text, which LeanCTX
//! Code shows the person and passes to `lean-ctx cloud plan release` after
//! the person confirmed "Send to agents". Every other reader (the local
//! socket) gets the same document with the text and release ids `null`. No
//! agent-facing tool or bus path renders this view.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::plan_body::PlanBody;
use super::plan_events::{ALL_SLOT, PlanDoc, vault_key};
use lean_ctx::core::agents::{Directive, DirectiveKind, DirectiveState, DirectiveStore};

/// The most plan documents read from disk.
const MAX_PLANS: usize = 256;
/// The largest plan document read from disk.
const MAX_PLAN_BYTES: u64 = 1024 * 1024;

/// One applied plan version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanUpdate {
    pub id: String,
    /// The vault address `#<workspace>/<vault>` when this instance follows
    /// the vault, else the document's vault id (32 hex).
    pub vault: String,
    pub version: u64,
    /// The body's `content_sha256`.
    pub digest: String,
    /// Who published the version (Cloud's `issued_by`, covered by its
    /// signature).
    pub issued_by: String,
    /// Whether the kept body still hashes to `digest`. A document is kept
    /// only after its digest and Cloud signature checked out (37.5); `false`
    /// when the stored copy no longer matches (it is then not opened).
    pub verified: bool,
    pub published_at: Option<String>,
    /// The verified body's title; `null` for a reader without text.
    pub title: Option<String>,
    pub goal: Option<String>,
    pub parts: Vec<PlanPart>,
    pub agents: Vec<PlanAgent>,
    /// The whole verified `leanctx.plan-body/v1` (title, goal, sessions,
    /// decisions), for the app's read; `null` for a reader without text or
    /// when the body does not open.
    pub body: Option<PlanBody>,
    /// Why the body does not open (`invalid`, `digest_mismatch`); `null`
    /// when it opened or the reader gets no text.
    pub body_error: Option<&'static str>,
    /// The local audit of every release of this plan (any version still
    /// on record): which version reached which agent, and when.
    pub releases: Vec<PlanRelease>,
}

/// One release of a plan version to one local agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanRelease {
    pub version: u64,
    pub agent_id: String,
    pub directive_id: String,
    /// RFC 3339 UTC, when `lean-ctx cloud plan release` delivered it.
    pub released_at: String,
}

/// One session of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanPart {
    pub slot: String,
    pub role: Option<String>,
    pub task_id: Option<String>,
    pub worker_id: Option<String>,
    pub brief: Option<String>,
    pub folder: Option<String>,
    pub brief_sha256: String,
    pub changed: bool,
}

/// One local agent's directive for the plan version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanAgent {
    pub agent_id: String,
    pub plan_id: String,
    /// The plan version this entry answers (the `<version>` of
    /// `directive_id`); it never counts for another version.
    pub version: u64,
    pub directive_id: String,
    /// The session's slot, or `all` for an agent no session names.
    pub slot: String,
    /// `offered` (waiting for "Send to agents"), `delivered`, `acked` or
    /// `refused`.
    pub state: &'static str,
    /// The release id, only while `offered`: the one argument of
    /// `lean-ctx cloud plan release` (as `release_id` on stdin).
    pub release_id: Option<String>,
    /// When the person released it; `null` while offered.
    pub released_at: Option<String>,
    pub reason_code: Option<String>,
    /// The agent's own words (local only, never sent to Cloud).
    pub reason: Option<String>,
    /// What this agent gets on release, from the verified body: the plan's
    /// title and goal, its part's role, brief and folder (`null` for slot
    /// `all`, which no part names). `null` for a reader without text.
    pub title: Option<String>,
    pub goal: Option<String>,
    pub role: Option<String>,
    pub brief: Option<String>,
    pub folder: Option<String>,
    /// Whether the version changed this agent's part (its session's
    /// `changed`; `false` for slot `all`).
    pub changed: bool,
}

/// One offered directive waiting for "Send to agents": what LeanCTX Code
/// shows, confirms and passes to `lean-ctx cloud plan release` (§37.6
/// `offers`). Text and `release_id` are `null` for a reader without them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub release_id: Option<String>,
    pub plan_id: String,
    pub version: u64,
    pub slot: String,
    pub agent_id: String,
    pub title: Option<String>,
    pub goal: Option<String>,
    pub role: Option<String>,
    pub brief: Option<String>,
    pub folder: Option<String>,
    pub changed: bool,
}

/// The offered directives of `plans`, in the plans' order (plan id, then
/// agent id): one entry per agent and offered version.
pub fn offers(plans: &[PlanUpdate]) -> Vec<Offer> {
    plans
        .iter()
        .flat_map(|plan| plan.agents.iter())
        .filter(|a| a.state == "offered")
        .map(|a| Offer {
            release_id: a.release_id.clone(),
            plan_id: a.plan_id.clone(),
            version: a.version,
            slot: a.slot.clone(),
            agent_id: a.agent_id.clone(),
            title: a.title.clone(),
            goal: a.goal.clone(),
            role: a.role.clone(),
            brief: a.brief.clone(),
            folder: a.folder.clone(),
            changed: a.changed,
        })
        .collect()
}

/// The plan documents kept under `data_dir`, sorted by plan id; a file
/// that does not parse is left out.
pub fn kept_plans(data_dir: &Path) -> Vec<PlanDoc> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("plans")) else {
        return Vec::new();
    };
    // Sorted before the cap: the same files are kept whatever order the
    // directory lists them in.
    let mut files: Vec<std::path::PathBuf> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| {
            e.metadata()
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_PLAN_BYTES)
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut docs: Vec<PlanDoc> = files
        .into_iter()
        .take(MAX_PLANS)
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .filter_map(|raw| serde_json::from_value::<PlanDoc>(raw).ok())
        .collect();
    docs.sort_by(|a, b| a.plan_id.cmp(&b.plan_id));
    docs
}

fn agent_of(d: &Directive, doc: &PlanDoc, body: Option<&PlanBody>) -> Option<PlanAgent> {
    let slot = d.slot.clone().unwrap_or_else(|| ALL_SLOT.to_string());
    let part = body.and_then(|b| b.session(&slot));
    Some(PlanAgent {
        agent_id: d.agent_id.clone(),
        plan_id: d.subject.clone(),
        version: d.version,
        directive_id: d.directive_id.clone(),
        state: d.state.local()?,
        release_id: if d.state == DirectiveState::Offered {
            d.release_id.clone()
        } else {
            None
        },
        released_at: d.released_at.clone(),
        reason_code: d.reason_code.clone(),
        reason: d.reason.clone(),
        title: body.map(|b| b.title.clone()),
        goal: body.map(|b| b.goal.clone()),
        role: part.map(|p| p.role.clone()),
        brief: part.map(|p| p.brief.clone()),
        folder: part.and_then(|p| p.folder.clone()),
        changed: doc
            .sessions
            .iter()
            .any(|s| s.slot == slot && s.slot != ALL_SLOT && s.changed),
        slot,
    })
}

/// The view of `docs` with the directives of `store`, limited to the
/// enrolled vault `only` (a vault id) when given. `addresses` names each
/// followed vault by its id (any case, with or without `-`). `with_text`
/// fills the plan text from each verified body (the app's read only).
/// Byte-stable: plans by id, agents by id.
pub fn updates(
    docs: &[PlanDoc],
    store: &DirectiveStore,
    addresses: &BTreeMap<String, String>,
    only: Option<&str>,
    with_text: bool,
) -> Vec<PlanUpdate> {
    let addresses: BTreeMap<String, &String> =
        addresses.iter().map(|(id, a)| (vault_key(id), a)).collect();
    let only = only.map(vault_key);
    let mut out = Vec::new();
    for doc in docs {
        let key = vault_key(&doc.vault);
        if only.as_ref().is_some_and(|o| *o != key) {
            continue;
        }
        let (body, body_error) = if with_text {
            match doc.open() {
                Ok(body) => (Some(body), None),
                Err(code) => (None, Some(code)),
            }
        } else {
            (None, None)
        };
        let mut agents: Vec<PlanAgent> = store
            .directives
            .iter()
            .filter(|d| {
                d.kind == DirectiveKind::Plan
                    && d.subject == doc.plan_id
                    && d.version == doc.version
                    && d.digest == doc.content_sha256
            })
            .filter_map(|d| agent_of(d, doc, body.as_ref()))
            .collect();
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        let mut releases: Vec<PlanRelease> = store
            .directives
            .iter()
            .filter(|d| d.kind == DirectiveKind::Plan && d.subject == doc.plan_id)
            .filter_map(|d| {
                Some(PlanRelease {
                    version: d.version,
                    agent_id: d.agent_id.clone(),
                    directive_id: d.directive_id.clone(),
                    released_at: d.released_at.clone()?,
                })
            })
            .collect();
        releases.sort_by(|a, b| {
            (&a.released_at, a.version, &a.agent_id).cmp(&(&b.released_at, b.version, &b.agent_id))
        });
        out.push(PlanUpdate {
            id: doc.plan_id.clone(),
            vault: addresses
                .get(&key)
                .map_or_else(|| doc.vault.clone(), |a| (*a).clone()),
            version: doc.version,
            digest: doc.content_sha256.clone(),
            issued_by: doc.issued_by.clone(),
            verified: super::plan_body::decode_body(&doc.body)
                .is_ok_and(|bytes| super::plan_body::sha256_of(&bytes) == doc.content_sha256),
            published_at: doc.published_at.clone(),
            title: body.as_ref().map(|b| b.title.clone()),
            goal: body.as_ref().map(|b| b.goal.clone()),
            parts: doc
                .sessions
                .iter()
                .map(|s| {
                    let part = body.as_ref().and_then(|b| b.session(&s.slot));
                    PlanPart {
                        slot: s.slot.clone(),
                        role: part.map(|p| p.role.clone()),
                        task_id: s.task_id.clone(),
                        worker_id: s.worker_id.clone(),
                        brief: part.map(|p| p.brief.clone()),
                        folder: part.and_then(|p| p.folder.clone()),
                        brief_sha256: s.brief_sha256.clone(),
                        changed: s.changed,
                    }
                })
                .collect(),
            agents,
            body,
            body_error,
            releases,
        });
    }
    out
}

#[cfg(test)]
#[path = "plan_view_tests.rs"]
mod tests;
