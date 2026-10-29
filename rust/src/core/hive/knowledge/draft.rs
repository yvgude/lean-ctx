// SPDX-License-Identifier: Apache-2.0
//! Captured session knowledge → one draft proposal in the outbox.
//!
//! Every fact passes the capture gate first ([`crate::core::hive::capture`]);
//! what fails stays local and is only counted. Runs at session end, on `ctx_session save` (debounced 10 min per
//! project) and at server shutdown. It never touches the network: the
//! draft is planned against the last pulled base, screened, and queued;
//! [`super::pull`] / the scheduler send it later.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::learn::plan_learn;
use super::screening::Screen;
use super::{BaseRevision, load_base};
use crate::core::hive::HiveError;
use crate::core::hive::capture;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::core::hive::state::{HiveStore, write_private};
use crate::core::hive::vault::{VaultMap, project_identity};
use lean_ctx_protocol::hive::knowledge::{
    ContextPatchV1, DRAFT_PROPOSAL_SCHEMA, DraftProposalV1, MAX_PATCH_OPS, PatchOpV1, ProposalState,
};
use lean_ctx_protocol::hive::vault::stable_identity;

/// `ctx_session save` drafts at most this often per project.
const SAVE_DEBOUNCE_MINUTES: i64 = 10;

/// Why a draft is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    SessionEnd,
    Save,
    Shutdown,
}

/// Where the next draft of a project starts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watermark {
    pub since: Option<DateTime<Utc>>,
    pub last_draft_at: Option<DateTime<Utc>>,
}

/// The payload of a knowledge outbox item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedDraft {
    /// `stable_identity` of the project (never leaves the machine as such).
    pub stable_identity: String,
    /// Local checkout the draft came from, to resolve the project later.
    pub root: String,
    pub draft: DraftProposalV1,
}

/// 3.11.5 kept a project's watermark in `<data>/hive/drafts/<stable>.watermark`.
pub(crate) const LEGACY_WATERMARK_DIR: &str = "drafts";
/// 3.12: `<data>/hive/capture/<place>.watermark` (SPEC-ENGINE §3.16).
pub(crate) const WATERMARK_DIR: &str = "capture";
pub(crate) const WATERMARK_EXT: &str = "watermark";

/// The place a project's capture state is kept under: its vault once it is
/// bound, the project itself before (as [`capture::open_run`]).
pub(crate) fn watermark_place(data_dir: &Path, stable: &str) -> String {
    VaultMap::load(data_dir)
        .vaults
        .get(stable)
        .map_or_else(|| stable.to_string(), |r| r.vault_id.clone())
}

fn read_watermark(path: &Path) -> Option<Watermark> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The vault's watermark; before the upgrade moved it (or before the
/// project was bound), the one kept under the project.
fn load_watermark(data_dir: &Path, stable: &str) -> Watermark {
    let name = |place: &str| format!("{place}.{WATERMARK_EXT}");
    let place = watermark_place(data_dir, stable);
    [
        data_dir.join(WATERMARK_DIR).join(name(&place)),
        data_dir.join(WATERMARK_DIR).join(name(stable)),
        data_dir.join(LEGACY_WATERMARK_DIR).join(name(stable)),
    ]
    .iter()
    .find_map(|path| read_watermark(path))
    .unwrap_or_default()
}

fn save_watermark(data_dir: &Path, stable: &str, mark: &Watermark) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec(mark).map_err(|e| HiveError::Local(e.to_string()))?;
    let place = watermark_place(data_dir, stable);
    write_private(
        &data_dir.join(WATERMARK_DIR),
        &format!("{place}.{WATERMARK_EXT}"),
        &bytes,
    )
}

/// An opaque per-session reference: no host, user or path data.
pub fn session_ref(session_id: &str) -> String {
    let digest = Sha256::digest(format!("leanctx.hive.session.v1\n{session_id}").as_bytes());
    format!("ses_{}", hex::encode(&digest[..12]))
}

/// Summary and body of a draft: every fact passed the capture gate, so each
/// is one complete claim and is listed whole (a procedure by its title).
fn summary_and_body(ops: &[PatchOpV1]) -> (String, String) {
    let summary = match ops.len() {
        1 => "Captured 1 fact".to_string(),
        n => format!("Captured {n} facts"),
    };
    let mut body = format!(
        "Captured in a lean-ctx session (capture gate v{}):\n",
        capture::GATE_VERSION
    );
    for op in ops {
        if let PatchOpV1::AddFact { fact } = op {
            let claim = fact.statement.lines().next().unwrap_or_default();
            body.push_str(&format!("- {claim}\n"));
        }
    }
    (summary, body)
}

/// Whether the project `stable` at `root` syncs through its vault's C0 2.0
/// replica, as `hive sync` decides: the enrolled Hive speaks 2.0 and the
/// project is bound (or linked, and bound on its next sync). A 1.x Hive
/// keeps the draft path.
pub(crate) fn syncs_through_replica(store: &HiveStore, stable: &str, root: &Path) -> bool {
    crate::core::hive::bus::relay_v2::speaks_v2(store)
        && (VaultMap::load(store.data_dir()).vaults.contains_key(stable)
            || crate::core::hive::link::has_link_file(root))
}

/// Plan, screen and queue the learnings of the project at `root` made since
/// its last draft. Returns the outbox id, or `None` when there is nothing
/// to propose (or Hive is not set up for this machine).
pub fn enqueue_session_draft(
    store: &HiveStore,
    config: &HiveConfig,
    root: &Path,
    session_id: &str,
    trigger: Trigger,
    screen: &mut Screen,
    now: DateTime<Utc>,
) -> Result<Option<String>, HiveError> {
    capture_session(store, config, root, session_id, trigger, screen, now).map(|(id, _)| id)
}

/// [`enqueue_session_draft`], also returning what the capture run counted.
fn capture_session(
    store: &HiveStore,
    config: &HiveConfig,
    root: &Path,
    session_id: &str,
    trigger: Trigger,
    screen: &mut Screen,
    now: DateTime<Utc>,
) -> Result<(Option<String>, capture::CaptureCounts), HiveError> {
    let nothing = || Ok((None, capture::CaptureCounts::default()));
    if config.enabled == Some(false) || !config.auto_capture || store.instance()?.is_none() {
        return nothing();
    }
    let Some(identity) = project_identity(root) else {
        return nothing();
    };
    let stable = stable_identity(&identity);
    // A vault that syncs through its v2 replica gets knowledge only as
    // signed events (`hive sync`); a 1.x draft would duplicate it and
    // feed the frozen review flow.
    if syncs_through_replica(store, &stable, root) {
        return nothing();
    }
    let data = store.data_dir();
    let mut mark = load_watermark(data, &stable);
    if trigger == Trigger::Save
        && mark
            .last_draft_at
            .is_some_and(|at| now - at < Duration::minutes(SAVE_DEBOUNCE_MINUTES))
    {
        return nothing();
    }
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(&root.to_string_lossy())
    else {
        return nothing();
    };
    let base: BaseRevision = VaultMap::load(data)
        .vaults
        .get(&stable)
        .map(|record| load_base(data, &record.vault_id))
        .unwrap_or_default();
    let (mut run, place) = capture::open_run(data, &stable, root, screen, now.timestamp_millis());
    let plan = plan_learn(
        &knowledge,
        &base.document,
        mark.since,
        MAX_PATCH_OPS,
        &mut run,
    );
    capture::finish_run(data, &place, &run)?;
    let screening = run.screen.counts();
    let counts = run.counts.clone();
    if plan.watermark.is_some() {
        mark.since = plan.watermark;
    }
    if plan.added.is_empty() {
        save_watermark(data, &stable, &mark)?;
        return Ok((None, counts));
    }
    let ops: Vec<PatchOpV1> = plan
        .added
        .into_iter()
        .map(|fact| PatchOpV1::AddFact { fact })
        .collect();
    let (summary, body) = summary_and_body(&ops);
    let draft = DraftProposalV1 {
        schema: DRAFT_PROPOSAL_SCHEMA.into(),
        state: ProposalState::Draft,
        summary: Some(summary),
        body: Some(body),
        sources: Vec::new(),
        context_patch: ContextPatchV1 {
            base_revision: base.revision,
            ops,
            sealed: None,
        },
        session_ref: session_ref(session_id),
        engine_screening: screening,
        origin: None,
    };
    draft
        .validate()
        .map_err(|e| HiveError::Protocol(format!("draft: {e}")))?;
    let payload = serde_json::to_value(QueuedDraft {
        stable_identity: stable.clone(),
        root: root.to_string_lossy().into_owned(),
        draft,
    })
    .map_err(|e| HiveError::Local(e.to_string()))?;
    let id = Outbox::at(data).enqueue(CHANNEL_KNOWLEDGE, payload, now)?;
    mark.last_draft_at = Some(now);
    save_watermark(data, &stable, &mark)?;
    Ok((Some(id), counts))
}

/// What [`recapture`] did.
#[derive(Debug, Clone, Default)]
pub struct Recapture {
    /// Unsent session drafts of the project it removed.
    pub removed: usize,
    /// Drafts it queued instead.
    pub queued: Vec<String>,
    pub counts: capture::CaptureCounts,
}

/// `lean-ctx hive capture --recapture` (SPEC-ENGINE §3.1.6): remove the
/// project's unsent session drafts (they never reached members), reset its
/// capture watermark and run the gate over the whole local knowledge.
pub fn recapture(
    store: &HiveStore,
    config: &HiveConfig,
    root: &Path,
    screen: &mut Screen,
    now: DateTime<Utc>,
) -> Result<Recapture, HiveError> {
    let identity = project_identity(root)
        .ok_or_else(|| HiveError::Config("not inside a git repository".into()))?;
    let stable = stable_identity(&identity);
    let data = store.data_dir();
    let outbox = Outbox::at(data);
    let mut done = Recapture::default();
    for item in outbox.list(CHANNEL_KNOWLEDGE) {
        if queued(&item.payload)
            .is_some_and(|q| q.stable_identity == stable && q.draft.origin.is_none())
        {
            outbox.remove(&item)?;
            done.removed += 1;
        }
    }
    save_watermark(data, &stable, &Watermark::default())?;
    // A run queues at most one draft of `MAX_PATCH_OPS` facts; the next
    // continues after it.
    loop {
        let (id, counts) = capture_session(
            store,
            config,
            root,
            "cli-recapture",
            Trigger::SessionEnd,
            screen,
            now,
        )?;
        done.counts.absorb(&counts);
        let full = counts
            .rejected
            .contains_key(&capture::ReasonCode::BudgetBatchFull);
        done.queued.extend(id);
        if !full {
            break;
        }
    }
    // A batch that was full is continued, not rejected.
    done.counts
        .rejected
        .remove(&capture::ReasonCode::BudgetBatchFull);
    Ok(done)
}

/// Hook for session end, save and shutdown: enqueue only, never network,
/// never an error for the caller (a failure is recorded for `hive status`).
pub fn on_session_event(project_root: &str, session_id: &str, trigger: Trigger) {
    let Ok(store) = HiveStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    let config = crate::core::config::Config::load().hive.clone();
    let mut screen = Screen::for_installation();
    if let Err(error) = enqueue_session_draft(
        &store,
        &config,
        Path::new(project_root),
        session_id,
        trigger,
        &mut screen,
        Utc::now(),
    ) {
        store.record_error(&format!("draft: {error}"));
    }
}

/// The draft inside a queued item, for status listings.
pub fn queued(payload: &Value) -> Option<QueuedDraft> {
    serde_json::from_value(payload.clone()).ok()
}

#[cfg(test)]
#[path = "draft_tests.rs"]
mod tests;
