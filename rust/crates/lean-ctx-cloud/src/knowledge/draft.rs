// SPDX-License-Identifier: Apache-2.0
//! Captured session knowledge → one draft proposal in the outbox.
//!
//! Every fact passes the capture gate first ([`crate::capture`]);
//! what fails stays local and is only counted. Runs at session end, on `ctx_session save` (debounced 10 min per
//! project) and at server shutdown. It never touches the network: the
//! draft is planned against the last pulled base, screened, and queued;
//! [`super::pull`] / the scheduler send it later.

use std::path::Path;

use lean_ctx_time::{Duration, UtcDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::learn::plan_learn;
use super::screening::Screen;
use super::{BaseRevision, load_base};
use crate::CloudError;
use crate::capture;
use crate::config::CloudConfig;
use crate::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::state::{CloudStore, write_private};
use crate::vault::{VaultMap, project_identity};
use lean_ctx_protocol::cloud::knowledge::{
    ContextPatchV1, DRAFT_PROPOSAL_SCHEMA, DraftProposalV1, MAX_PATCH_OPS, PatchOpV1, ProposalState,
};
use lean_ctx_protocol::cloud::vault::stable_identity;

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
    pub since: Option<UtcDateTime>,
    pub last_draft_at: Option<UtcDateTime>,
}

/// The payload of a knowledge outbox item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedDraft {
    /// `stable_identity` of the project (never leaves the machine as such).
    pub stable_identity: String,
    /// Local checkout the draft came from, to resolve the project later.
    pub root: String,
    pub draft: DraftProposalV1,
    /// The draft's `session_ref` is keyed under the local receipts key
    /// ([`crate::session_ref::uploaded_ses_ref`]). Absent on a draft queued
    /// before refs were keyed: its unsalted digest is re-keyed before it is
    /// sent ([`QueuedDraft::rekey_legacy_ref`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ref_keyed: bool,
}

impl QueuedDraft {
    /// Key the `session_ref` of a draft queued before refs were keyed (the
    /// queue holds only its unsalted digest, never the session id), so no
    /// guessable digest is sent. Returns whether it changed; an error (no
    /// local key) keeps the draft queued.
    pub fn rekey_legacy_ref(&mut self, cloud_data: &Path) -> Result<bool, CloudError> {
        if self.ref_keyed {
            return Ok(false);
        }
        self.draft.session_ref =
            crate::session_ref::rekeyed_legacy_ref(cloud_data, &self.draft.session_ref)?;
        self.ref_keyed = true;
        Ok(true)
    }
}

/// 3.11.5 kept a project's watermark in `<data>/cloud/drafts/<stable>.watermark`.
pub(crate) const LEGACY_WATERMARK_DIR: &str = "drafts";
/// 3.12: `<data>/cloud/capture/<place>.watermark` (SPEC-ENGINE §3.16).
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

fn save_watermark(data_dir: &Path, stable: &str, mark: &Watermark) -> Result<(), CloudError> {
    let bytes = serde_json::to_vec(mark).map_err(|e| CloudError::Local(e.to_string()))?;
    let place = watermark_place(data_dir, stable);
    write_private(
        &data_dir.join(WATERMARK_DIR),
        &format!("{place}.{WATERMARK_EXT}"),
        &bytes,
    )
}

/// Start the capture of the project `stable` from the beginning again, as
/// [`recapture`] does: `cloud unlink` dropped its unsent drafts, so their
/// facts are captured again once the folder is shared again (rc4-6).
pub(crate) fn reset_watermark(data_dir: &Path, stable: &str) -> Result<(), CloudError> {
    save_watermark(data_dir, stable, &Watermark::default())
}

/// An opaque per-session reference: no host, user or path data. It is an
/// unsalted digest, so a guessable session id can be tested against it:
/// it names sessions locally (file names, done markers) and, uploaded, only
/// constant labels that name no session (`cloud-link`,
/// `cloud-task-proposals`). What is uploaded for a session, or for a label
/// passed in its place (`cli-import`, `emspace-import`), is
/// [`crate::session_ref::uploaded_ses_ref`].
pub fn session_ref(session_id: &str) -> String {
    let digest = Sha256::digest(format!("leanctx.cloud.session.v1\n{session_id}").as_bytes());
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
/// replica, as `cloud sync` decides: the enrolled Cloud speaks 2.0 and the
/// project is bound (or linked, and bound on its next sync). A 1.x Cloud
/// keeps the draft path.
pub fn syncs_through_replica(store: &CloudStore, stable: &str, root: &Path) -> bool {
    crate::bus::relay_v2::speaks_v2(store)
        && (VaultMap::load(store.data_dir()).vaults.contains_key(stable)
            || crate::link::has_link_file(root))
}

/// Plan, screen and queue the learnings of the project at `root` made since
/// its last draft. Returns the outbox id, or `None` when there is nothing
/// to propose (or Cloud is not set up for this machine).
pub fn enqueue_session_draft(
    store: &CloudStore,
    config: &CloudConfig,
    root: &Path,
    session_id: &str,
    trigger: Trigger,
    screen: &mut Screen,
    now: UtcDateTime,
) -> Result<Option<String>, CloudError> {
    capture_session(store, config, root, session_id, trigger, screen, now).map(|(id, _)| id)
}

/// [`enqueue_session_draft`], also returning what the capture run counted.
fn capture_session(
    store: &CloudStore,
    config: &CloudConfig,
    root: &Path,
    session_id: &str,
    trigger: Trigger,
    screen: &mut Screen,
    now: UtcDateTime,
) -> Result<(Option<String>, capture::CaptureCounts), CloudError> {
    let nothing = || Ok((None, capture::CaptureCounts::default()));
    if config.enabled == Some(false) || !config.auto_capture || store.instance()?.is_none() {
        return nothing();
    }
    // A folder the person stopped sharing (`cloud unlink`) queues no draft.
    if crate::unlinked::contains(store.data_dir(), root) {
        return nothing();
    }
    let Some(identity) = project_identity(root) else {
        return nothing();
    };
    let stable = stable_identity(&identity);
    // A C0 2.0 Cloud gets knowledge only as signed events (`cloud sync`),
    // and only into a linked or bound vault: it takes no 1.x draft.
    if crate::bus::relay_v2::speaks_v2(store) {
        return nothing();
    }
    let data = store.data_dir();
    // Without the local key nothing naming the session is uploaded: the
    // draft is not queued and the watermark stays, so a later run retries.
    let session_ref = crate::session_ref::uploaded_ses_ref(data, session_id)?;
    let mut mark = load_watermark(data, &stable);
    if trigger == Trigger::Save
        && mark
            .last_draft_at
            .is_some_and(|at| now - at < Duration::minutes(SAVE_DEBOUNCE_MINUTES))
    {
        return nothing();
    }
    let Some(knowledge) =
        lean_ctx::core::knowledge::ProjectKnowledge::load(&root.to_string_lossy())
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
        },
        session_ref,
        engine_screening: screening,
        origin: None,
    };
    draft
        .validate()
        .map_err(|e| CloudError::Protocol(format!("draft: {e}")))?;
    let payload = serde_json::to_value(QueuedDraft {
        stable_identity: stable.clone(),
        root: root.to_string_lossy().into_owned(),
        draft,
        ref_keyed: true,
    })
    .map_err(|e| CloudError::Local(e.to_string()))?;
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

/// `lean-ctx cloud capture --recapture` (SPEC-ENGINE §3.1.6): remove the
/// project's unsent session drafts (they never reached members), reset its
/// capture watermark and run the gate over the whole local knowledge.
pub fn recapture(
    store: &CloudStore,
    config: &CloudConfig,
    root: &Path,
    screen: &mut Screen,
    now: UtcDateTime,
) -> Result<Recapture, CloudError> {
    let identity = project_identity(root)
        .ok_or_else(|| CloudError::Config("not inside a git repository".into()))?;
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
/// never an error for the caller (a failure is recorded for `cloud status`).
pub fn on_session_event(project_root: &str, session_id: &str, trigger: Trigger) {
    let Ok(store) = CloudStore::open() else {
        return;
    };
    if !matches!(store.instance(), Ok(Some(_))) {
        return;
    }
    let config = lean_ctx::core::config::Config::load().cloud.clone();
    let mut screen = Screen::for_installation();
    if let Err(error) = enqueue_session_draft(
        &store,
        &config,
        Path::new(project_root),
        session_id,
        trigger,
        &mut screen,
        UtcDateTime::now(),
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
