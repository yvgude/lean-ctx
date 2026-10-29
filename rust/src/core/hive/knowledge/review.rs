// SPDX-License-Identifier: Apache-2.0
//! Engine-side review: list a project's open proposals and merge them into
//! a new revision. For a sealed project this is the only place a proposal
//! is ever readable: Hive holds ciphertext, so the merging engine fetches
//! each proposal (`GET /proposals/{n}`), opens it with its epoch's key,
//! applies the ops to the newest revision and writes the result sealed for
//! exactly the revision Hive will assign (`expected_revision + 1`). Plain
//! projects merge the same way, in the clear.

use std::path::Path;

use super::doc::{apply_patch, rebase_ops};
use super::learn::sync_into_knowledge;
use super::{BaseRevision, load_base, save_base};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::sealed::{self, SealingKey};
use crate::core::hive::vault::VaultRecord;
use lean_ctx_protocol::hive::automerge::{
    ConfirmVerdict, PROPOSAL_CONFIRM_SCHEMA, ProposalConfirmAckV1, ProposalConfirmV1,
};
use lean_ctx_protocol::hive::knowledge::{
    DraftProposalV1, KNOWLEDGE_PUT_REQUEST_SCHEMA, KnowledgePutRequestV1, KnowledgeRevisionV1,
    MAX_KNOWLEDGE_BODY_BYTES, MAX_MERGED_PROPOSALS, PatchOpV1, ProposalListV1, ProposalState,
    ProposalV1, RESEARCH_CONTEXT_FORMAT, ResearchContextDocumentV1, RevisionState,
};
use lean_ctx_protocol::hive::lease::HiveRight;
use lean_ctx_protocol::hive::sealed::SealedDraftContentV1;

/// A proposal as a reviewer sees it, opened when sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub number: u64,
    pub state: ProposalState,
    pub mine: bool,
    pub sealed: bool,
    pub summary: String,
    pub base_revision: u64,
    pub ops: Vec<PatchOpV1>,
    /// The session that wrote it (opaque, `draft::session_ref`).
    pub session_ref: String,
}

/// The readable content of a draft: summary and ops.
fn open_draft(
    session: &Session,
    vault_id: &str,
    draft: &DraftProposalV1,
) -> Result<(String, Vec<PatchOpV1>), HiveError> {
    let base_revision = draft.context_patch.base_revision;
    let Some(blob) = &draft.context_patch.sealed else {
        return Ok((
            draft.summary.clone().unwrap_or_default(),
            draft.context_patch.ops.clone(),
        ));
    };
    let key = sealed::key_for_epoch(session, vault_id, blob.epoch)?.ok_or_else(|| {
        HiveError::Config(format!(
            "this instance holds no key for epoch {} of the sealed vault",
            blob.epoch
        ))
    })?;
    let plaintext = sealed::open(&key, vault_id, base_revision, blob)?;
    let content: SealedDraftContentV1 = serde_json::from_slice(&plaintext)
        .map_err(|e| HiveError::Protocol(format!("sealed draft: {e}")))?;
    content
        .validate()
        .map_err(|e| HiveError::Protocol(format!("sealed draft: {e}")))?;
    Ok((content.summary, content.ops))
}

fn fetch(session: &Session, vault_id: &str, number: u64) -> Result<ReviewItem, HiveError> {
    let body = session.with_client(|c| c.get(&format!("/vaults/{vault_id}/proposals/{number}")))?;
    let view: ProposalV1 =
        serde_json::from_value(body).map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    view.validate()
        .map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    if view.number != number {
        return Err(HiveError::Protocol(
            "Hive answered with another proposal".into(),
        ));
    }
    let (summary, ops) = open_draft(session, vault_id, &view.draft)?;
    Ok(ReviewItem {
        number,
        state: view.state,
        mine: view.mine,
        sealed: view.draft.context_patch.sealed.is_some(),
        summary,
        base_revision: view.draft.context_patch.base_revision,
        ops,
        session_ref: view.draft.session_ref,
    })
}

/// The open proposals (draft, open, or sealed and ready to merge) of one
/// project, readable.
pub fn list(session: &Session, record: &VaultRecord) -> Result<Vec<ReviewItem>, HiveError> {
    list_in(
        session,
        record,
        &[
            ProposalState::Draft,
            ProposalState::Open,
            ProposalState::Ready,
        ],
    )
}

/// The proposals of `states`. The states are the contract's own enum, so a
/// listing never asks for a state C0 does not define.
fn list_in(
    session: &Session,
    record: &VaultRecord,
    states: &[ProposalState],
) -> Result<Vec<ReviewItem>, HiveError> {
    let mut items = Vec::new();
    for state in states {
        let state = serde_json::to_value(state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| HiveError::Local("proposal state".into()))?;
        let body = session.with_client(|c| {
            c.get(&format!(
                "/vaults/{}/proposals?state={state}",
                record.vault_id
            ))
        })?;
        let listing: ProposalListV1 = serde_json::from_value(body)
            .map_err(|e| HiveError::Protocol(format!("proposals: {e}")))?;
        for summary in listing.proposals {
            items.push(fetch(session, &record.vault_id, summary.number)?);
        }
    }
    items.sort_by_key(|item| item.number);
    Ok(items)
}

/// After a key rotation: write the newest revision again, sealed under
/// `epoch`, so a removed member's old key opens nothing new. A repo without
/// content has nothing to re-seal.
pub(crate) fn reseal_head(
    session: &Session,
    vault_id: &str,
    epoch: u32,
    key: &sealed::RepoKey,
) -> Result<(), HiveError> {
    let data = session.store.data_dir().to_path_buf();
    let map = crate::core::hive::vault::VaultMap::load(&data);
    let Some(record) = map.vaults.values().find(|r| r.vault_id == vault_id) else {
        return Ok(());
    };
    super::pull::refresh_project(session, &data, record)?;
    let base = load_base(&data, vault_id);
    if base.head() == 0 {
        return Ok(());
    }
    // The newest merged revision is re-sealed, quarantined or not: sealing
    // the delivered one again would silently undo the quarantined ones.
    let head = merge_head(session, record, &base)?;
    let next = head.revision + 1;
    let plaintext = zeroize::Zeroizing::new(
        serde_json::to_vec(&head.document).map_err(|e| HiveError::Local(e.to_string()))?,
    );
    let request = KnowledgePutRequestV1 {
        schema: KNOWLEDGE_PUT_REQUEST_SCHEMA.into(),
        expected_revision: head.revision,
        document: None,
        sealed: Some(sealed::seal(
            key,
            vault_id,
            next,
            epoch,
            RESEARCH_CONTEXT_FORMAT,
            &plaintext,
        )?),
        merged_proposals: Vec::new(),
        rolls_back: None,
    };
    let body = serde_json::to_value(&request).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{vault_id}/knowledge");
    let answer = session
        .with_client(|c| c.send("PUT", &path, Some(&body), &[], MAX_KNOWLEDGE_BODY_BYTES))?;
    store_written(record, &data, &base, answer, next, head.document).map(|_| ())
}

/// The newest merged revision of `record` to write on top of: the delivered
/// base, or, while revisions wait in quarantine, the newest of them, read
/// with `knowledge.merge` and never loaded into a checkout (C0 1.9 §3:
/// merging continues on top of a quarantined revision).
fn merge_head(
    session: &Session,
    record: &VaultRecord,
    base: &BaseRevision,
) -> Result<BaseRevision, HiveError> {
    if base.quarantined_after == 0 {
        return Ok(base.clone());
    }
    let (_, document) = read_revision(session, record, base.head())?;
    Ok(BaseRevision {
        revision: base.head(),
        document,
        ..BaseRevision::default()
    })
}

/// Revision `n` of `record` with its state, opened with the key of its
/// epoch when sealed and validated either way.
fn read_revision(
    session: &Session,
    record: &VaultRecord,
    n: u64,
) -> Result<(RevisionState, ResearchContextDocumentV1), HiveError> {
    let path = format!("/vaults/{}/knowledge?revision={n}", record.vault_id);
    let answer =
        session.with_client(|c| c.send("GET", &path, None, &[], MAX_KNOWLEDGE_BODY_BYTES))?;
    let revision: KnowledgeRevisionV1 = serde_json::from_value(answer.body)
        .map_err(|e| HiveError::Protocol(format!("knowledge: {e}")))?;
    revision
        .validate()
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    if revision.vault_id != record.vault_id || revision.revision != n {
        return Err(HiveError::Protocol(
            "Hive sent another revision than asked for".into(),
        ));
    }
    let document: ResearchContextDocumentV1 = match (revision.document, revision.sealed) {
        (Some(document), None) if !record.sealed => document,
        (None, Some(blob)) if record.sealed => {
            let key = sealed::key_for_epoch(session, &record.vault_id, blob.epoch)?
                .ok_or_else(|| {
                    HiveError::Config(format!(
                        "this machine holds no key of epoch {} that revision {n} is sealed with; \
                         run this on a machine that holds it, or have a member grant it (`lean-ctx hive grant`)",
                        blob.epoch
                    ))
                })?;
            let plaintext = sealed::open(&key, &record.vault_id, n, &blob)?;
            serde_json::from_slice(&plaintext)
                .map_err(|e| HiveError::Protocol(format!("sealed knowledge: {e}")))?
        }
        _ => {
            return Err(HiveError::Protocol(
                "a revision that does not match the vault's sealing".into(),
            ));
        }
    };
    document
        .validate()
        .map_err(|e| HiveError::Protocol(format!("revision {n}: {e}")))?;
    Ok((revision.state, document))
}

/// Record what Hive stored as revision `next` on top of `delivered`: a
/// released revision is loaded into the project's checkouts and becomes
/// the base; a quarantined one only moves the head (C0 1.9 §3: it never
/// reaches an agent). Returns `next`.
fn store_written(
    record: &VaultRecord,
    data: &Path,
    delivered: &BaseRevision,
    answer: crate::core::hive::transport::HiveResponse,
    next: u64,
    document: ResearchContextDocumentV1,
) -> Result<u64, HiveError> {
    let revision: KnowledgeRevisionV1 = serde_json::from_value(answer.body)
        .map_err(|e| HiveError::Protocol(format!("knowledge: {e}")))?;
    if revision.revision != next {
        return Err(HiveError::Protocol(format!(
            "Hive stored revision {} instead of {next}",
            revision.revision
        )));
    }
    let base = match revision.state {
        RevisionState::Released => {
            for root in &record.roots {
                if Path::new(root).is_dir() {
                    sync_into_knowledge(root, &record.ns, &record.name, next, &document)
                        .map_err(HiveError::Local)?;
                }
            }
            BaseRevision {
                revision: next,
                etag: answer.etag,
                document,
                quarantined_after: 0,
            }
        }
        RevisionState::Quarantined => BaseRevision {
            quarantined_after: next - delivered.revision,
            ..delivered.clone()
        },
        state => {
            return Err(HiveError::Protocol(format!(
                "Hive stored revision {next} as {state:?}"
            )));
        }
    };
    save_base(data, &record.vault_id, &base)?;
    Ok(next)
}

/// Merge `numbers` into a new revision of `record`; returns it.
pub fn merge(session: &Session, record: &VaultRecord, numbers: &[u64]) -> Result<u64, HiveError> {
    if !session.lease.lease.grants(HiveRight::KnowledgeMerge) {
        return Err(HiveError::Forbidden);
    }
    if numbers.is_empty() || numbers.len() > MAX_MERGED_PROPOSALS {
        return Err(HiveError::Config(format!(
            "merge 1-{MAX_MERGED_PROPOSALS} proposals at a time"
        )));
    }
    let data = session.store.data_dir().to_path_buf();
    // Merge onto the newest revision, never onto a stale copy.
    super::pull::refresh_project(session, &data, record)?;
    let base = load_base(&data, &record.vault_id);
    let head = merge_head(session, record, &base)?;
    let mut document = head.document;
    for number in numbers {
        let item = fetch(session, &record.vault_id, *number)?;
        // A sealed vault's proposals are sealed: a plaintext one was never
        // written by a member's engine, and sealing it now would launder it.
        if record.sealed && !item.sealed {
            return Err(HiveError::Refused(format!(
                "proposal #{number} is not sealed in a sealed vault; a person decides on it"
            )));
        }
        if !matches!(
            item.state,
            ProposalState::Draft | ProposalState::Open | ProposalState::Ready
        ) {
            return Err(HiveError::Config(format!("proposal #{number} is not open")));
        }
        let ops = if item.base_revision < head.revision {
            rebase_ops(&document, &item.ops)
                .map_err(|e| HiveError::Config(format!("proposal #{number}: {e}")))?
        } else {
            item.ops
        };
        document = apply_patch(&document, &ops)
            .map_err(|e| HiveError::Config(format!("proposal #{number}: {e}")))?;
    }
    write_revision(
        session,
        record,
        &base,
        head.revision,
        document,
        numbers.to_vec(),
        None,
    )
}

/// Write `document` as revision `head + 1` of `record`, whose delivered
/// base is `base`: sealed under the current repo key for exactly that
/// number in a sealed project, in the clear otherwise; then
/// [`store_written`]. Returns the new revision.
#[allow(clippy::too_many_arguments)]
fn write_revision(
    session: &Session,
    record: &VaultRecord,
    base: &BaseRevision,
    head: u64,
    document: ResearchContextDocumentV1,
    merged_proposals: Vec<u64>,
    rolls_back: Option<u64>,
) -> Result<u64, HiveError> {
    let data = session.store.data_dir().to_path_buf();
    let mut delivered = base.clone();
    let mut next = head + 1;
    let mut expected = head;
    let mut request = KnowledgePutRequestV1 {
        schema: KNOWLEDGE_PUT_REQUEST_SCHEMA.into(),
        expected_revision: expected,
        document: None,
        sealed: None,
        merged_proposals,
        rolls_back,
    };
    if record.sealed {
        let key = sealed::sealing_key(session, &record.vault_id, head)?;
        // A due rotation re-sealed the head as head + 1 (same document):
        // write on top of that one instead of conflicting with it.
        let current = load_base(&data, &record.vault_id);
        if current.head() != head {
            if current.document != base.document
                || current.quarantined_after != base.quarantined_after
            {
                return Err(HiveError::Conflict {
                    expected: Some(head),
                    actual: Some(current.head()),
                });
            }
            expected = current.head();
            next = expected + 1;
            request.expected_revision = expected;
            delivered = current;
        }
        let (epoch, key) = match key {
            SealingKey::Ready(epoch, key) => (epoch, key),
            SealingKey::Waiting => {
                return Err(HiveError::Config(
                    "this instance holds no key of the sealed vault yet".into(),
                ));
            }
            SealingKey::Rotating { required } => {
                return Err(HiveError::Config(format!(
                    "the sealed vault key moves to epoch {required} first; run `lean-ctx hive sync`"
                )));
            }
        };
        let plaintext = zeroize::Zeroizing::new(
            serde_json::to_vec(&document).map_err(|e| HiveError::Local(e.to_string()))?,
        );
        request.sealed = Some(sealed::seal(
            &key,
            &record.vault_id,
            next,
            epoch,
            RESEARCH_CONTEXT_FORMAT,
            &plaintext,
        )?);
    } else {
        request.document = Some(document.clone());
    }
    request
        .validate()
        .map_err(|e| HiveError::Protocol(format!("knowledge: {e}")))?;
    let body = serde_json::to_value(&request).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{}/knowledge", record.vault_id);
    let answer = session
        .with_client(|c| c.send("PUT", &path, Some(&body), &[], MAX_KNOWLEDGE_BODY_BYTES))?;
    store_written(record, &data, &delivered, answer, next, document)
}

/// Roll `record` back to the document of released revision `target`: a new
/// revision with that document and `rolls_back: target` (C0 1.9 §3), always
/// released. While revisions wait in quarantine, `target` may be the
/// delivered head itself (undoing just them). In a sealed project the old
/// revision is opened with the key of its epoch and sealed again under the
/// current key for the new number; its old ciphertext is never sent.
/// Returns the new revision.
pub fn rollback(session: &Session, record: &VaultRecord, target: u64) -> Result<u64, HiveError> {
    if !session.lease.lease.grants(HiveRight::KnowledgeMerge) {
        return Err(HiveError::Forbidden);
    }
    let data = session.store.data_dir().to_path_buf();
    super::pull::refresh_project(session, &data, record)?;
    let base = load_base(&data, &record.vault_id);
    if record.sealed && base.revision == 0 {
        return Err(HiveError::Config(
            "this machine holds no key of the sealed vault yet; roll back on a machine that holds it, \
             or have a member grant it (`lean-ctx hive grant`)"
                .into(),
        ));
    }
    let newest = if base.quarantined_after > 0 {
        base.revision
    } else {
        base.revision.saturating_sub(1)
    };
    if target == 0 || target > newest {
        return Err(HiveError::Config(format!(
            "revision {target} is not an earlier released revision (the delivered one is {})",
            base.revision
        )));
    }
    let (state, document) = read_revision(session, record, target)?;
    if state != RevisionState::Released {
        return Err(HiveError::Config(format!(
            "revision {target} is not released; roll back to a released revision"
        )));
    }
    write_revision(
        session,
        record,
        &base,
        base.head(),
        document,
        Vec::new(),
        Some(target),
    )
}

/// What a reviewing session says about a proposal (C0 1.9 §7). Either
/// names its reason (for a confirmation: what was checked); it stays on
/// this machine in a sealed project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Confirm(String),
    /// Something is wrong; a person decides.
    Object(String),
}

/// How long a session's read of a proposal backs its confirmation.
const READ_BACKS_CONFIRM: chrono::Duration = chrono::Duration::minutes(30);

/// One proposal a session read (C15): `<data>/hive/proposal-reads.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ProposalRead {
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    vault_id: String,
    number: u64,
    session_ref: String,
    /// SHA-256 of the draft as read: a confirmation backs that content only.
    draft: String,
    at: chrono::DateTime<chrono::Utc>,
}

const READS_FILE: &str = "proposal-reads.json";

fn draft_digest(draft: &DraftProposalV1) -> String {
    use sha2::Digest as _;
    let bytes = serde_json::to_vec(draft).unwrap_or_default();
    hex::encode(sha2::Sha256::digest(bytes))
}

fn load_reads(data: &Path) -> Vec<ProposalRead> {
    std::fs::read(data.join(READS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Record that `session_id` read `draft` (proposal `number`) at `now`.
fn record_read(
    data: &Path,
    vault_id: &str,
    number: u64,
    session_id: &str,
    draft: &DraftProposalV1,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), HiveError> {
    let session_ref = super::draft::session_ref(session_id);
    let mut reads = load_reads(data);
    reads.retain(|read| {
        now - read.at < READ_BACKS_CONFIRM
            && !(read.vault_id == vault_id
                && read.number == number
                && read.session_ref == session_ref)
    });
    reads.push(ProposalRead {
        vault_id: vault_id.to_owned(),
        number,
        session_ref,
        draft: draft_digest(draft),
        at: now,
    });
    let bytes = serde_json::to_vec(&reads).map_err(|e| HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(data, READS_FILE, &bytes)
}

/// Whether `session_id` read exactly `draft` within [`READ_BACKS_CONFIRM`].
fn read_recently(
    data: &Path,
    vault_id: &str,
    number: u64,
    session_id: &str,
    draft: &DraftProposalV1,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let session_ref = super::draft::session_ref(session_id);
    let digest = draft_digest(draft);
    load_reads(data).iter().any(|read| {
        read.vault_id == vault_id
            && read.number == number
            && read.session_ref == session_ref
            && read.draft == digest
            && now - read.at < READ_BACKS_CONFIRM
    })
}

/// Proposal `number` of `record` for an agent's review (C15): its summary
/// and ops, opened when sealed and wrapped as data; the read is recorded
/// so that this session may confirm it within 30 minutes.
pub fn read_for_review(
    session: &Session,
    record: &VaultRecord,
    number: u64,
    session_id: &str,
) -> Result<String, HiveError> {
    if !session.lease.lease.grants(HiveRight::KnowledgeDraft) {
        return Err(HiveError::Forbidden);
    }
    let body = session
        .with_client(|c| c.get(&format!("/vaults/{}/proposals/{number}", record.vault_id)))?;
    let view: ProposalV1 =
        serde_json::from_value(body).map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    view.validate()
        .map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    if view.number != number {
        return Err(HiveError::Protocol(
            "Hive answered with another proposal".into(),
        ));
    }
    let (summary, ops) = open_draft(session, &record.vault_id, &view.draft)?;
    let mut text = format!("Summary: {summary}\n");
    for op in &ops {
        match op {
            PatchOpV1::AddFact { fact } => {
                text.push_str(&format!("+ fact {}: {}\n", fact.id, fact.statement));
            }
            other => text.push_str(&format!(
                "{}\n",
                serde_json::to_string(other).unwrap_or_default()
            )),
        }
    }
    record_read(
        session.store.data_dir(),
        &record.vault_id,
        number,
        session_id,
        &view.draft,
        chrono::Utc::now(),
    )?;
    Ok(format!(
        "Knowledge proposal #{number} ({:?}){}:\n{}",
        view.state,
        if view.mine { ", from this machine" } else { "" },
        crate::core::hive::untrusted::wrap(
            crate::core::hive::untrusted::UntrustedKind::Knowledge,
            &format!("hive proposal #{number}"),
            &text,
        )
    ))
}

/// Confirm or object to proposal `number` of `record` as session
/// `session_id`. A session never confirms what it wrote; Hive refuses a
/// confirmation that may not count (`reviewer_not_independent`). An
/// Confirm or object to proposal `number` of `record` as session
/// `session_id` (an agent's own session; people review on the web). A
/// session never confirms what it wrote; Hive counts a session only when it
/// knows it from this owner's instance (`reviewer_not_independent`
/// otherwise). An objection's reason passes the outbox screening. With a
/// `deadline` (a tool call) every request ends by then.
///
/// 1.x only: Hive v4/v5 removed auto-merge (`410 feature_removed`,
/// `feature: auto_merge`) and decides knowledge through review items. Against
/// a Hive that speaks C0 2.0 nothing is sent; a Hive that answers 410
/// is told apart as [`confirmations_retired`].
pub fn confirm(
    session: &Session,
    record: &VaultRecord,
    number: u64,
    session_id: &str,
    verdict: &Verdict,
    deadline: Option<std::time::Instant>,
) -> Result<ProposalConfirmAckV1, HiveError> {
    if crate::core::hive::bus::relay_v2::speaks_v2(&session.store) {
        return Err(HiveError::Refused(CONFIRMATIONS_RETIRED.into()));
    }
    if !session.lease.lease.grants(HiveRight::KnowledgeDraft) {
        return Err(HiveError::Forbidden);
    }
    let client = match deadline {
        Some(deadline) => session.bounded_client(deadline)?,
        None => session.client()?,
    };
    // The state and the author's session are in the clear, sealed or not.
    let body = client
        .get(&format!("/vaults/{}/proposals/{number}", record.vault_id))
        .map_err(retired_on_gone)?;
    let view: ProposalV1 =
        serde_json::from_value(body).map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    view.validate()
        .map_err(|e| HiveError::Protocol(format!("proposal: {e}")))?;
    if view.number != number {
        return Err(HiveError::Protocol(
            "Hive answered with another proposal".into(),
        ));
    }
    if !matches!(view.state, ProposalState::Draft | ProposalState::Open) {
        return Err(HiveError::Config(format!("proposal #{number} is not open")));
    }
    // C15: an agent confirms only what this session read, as it is now.
    if matches!(verdict, Verdict::Confirm(_))
        && !read_recently(
            session.store.data_dir(),
            &record.vault_id,
            number,
            session_id,
            &view.draft,
            chrono::Utc::now(),
        )
    {
        return Err(HiveError::Refused(format!(
            "read proposal #{number} in this session first (ctx_tasks action=read proposal={number}); a confirmation backs only what was read in the last 30 minutes"
        )));
    }
    let request = confirmation(
        record.sealed,
        session_id,
        &view.draft.session_ref,
        view.mine,
        number,
        verdict,
    )?;
    let body = serde_json::to_value(&request).map_err(|e| HiveError::Local(e.to_string()))?;
    let answer = client
        .post(
            &format!(
                "/vaults/{}/proposals/{number}/confirmations",
                record.vault_id
            ),
            &body,
        )
        .map_err(retired_on_gone)?;
    let ack: ProposalConfirmAckV1 = serde_json::from_value(answer)
        .map_err(|e| HiveError::Protocol(format!("confirmation: {e}")))?;
    ack.validate()
        .map_err(|e| HiveError::Protocol(format!("confirmation: {e}")))?;
    if ack.number != number {
        return Err(HiveError::Protocol(
            "Hive confirmed another proposal".into(),
        ));
    }
    Ok(ack)
}

/// What a confirmation or objection answers once Hive has no auto-merge.
pub const CONFIRMATIONS_RETIRED: &str = "proposal confirmations are retired on this Hive (auto-merge was removed); a person decides review items on the web";

/// A `410` (`feature_removed`) of the confirmation route as
/// [`CONFIRMATIONS_RETIRED`]; any other error unchanged.
fn retired_on_gone(error: HiveError) -> HiveError {
    match error {
        HiveError::Server { status: 410, .. } => HiveError::Refused(CONFIRMATIONS_RETIRED.into()),
        other => other,
    }
}

/// Whether `error` says that this Hive retired confirmations.
#[must_use]
pub fn confirmations_retired(error: &HiveError) -> bool {
    matches!(error, HiveError::Refused(why) if why == CONFIRMATIONS_RETIRED)
}

/// The confirmation request of session `session_id` for proposal `number`
/// written by `author` (its draft's `session_ref`): refused for the
/// author's own session, an objection without reason or with a reason the
/// screening refuses; a sealed project's reason stays here.
fn confirmation(
    sealed_project: bool,
    session_id: &str,
    author: &str,
    mine: bool,
    number: u64,
    verdict: &Verdict,
) -> Result<ProposalConfirmV1, HiveError> {
    let own = super::draft::session_ref(session_id);
    if author == own {
        return Err(HiveError::Refused(format!(
            "proposal #{number} was written in this session; another session or person confirms it"
        )));
    }
    // C0 1.9 §7 (M9): the instance that wrote a proposal, or claimed the
    // task and submitted its result, never counts toward it, whichever of
    // its sessions confirms. An objection from it still stops a threshold.
    if mine && matches!(verdict, Verdict::Confirm(_)) {
        return Err(HiveError::Refused(format!(
            "proposal #{number} comes from this machine; confirm it from your other machine or on the web"
        )));
    }
    let (verdict, reason) = match verdict {
        Verdict::Confirm(reason) => (ConfirmVerdict::Confirm, reason.trim()),
        Verdict::Object(reason) => (ConfirmVerdict::Object, reason.trim()),
    };
    if reason.is_empty() {
        return Err(HiveError::Config(
            "a confirmation names what was checked, an objection what is wrong".into(),
        ));
    }
    if let Err(blocked) = super::screening::Screen::for_installation().check(reason) {
        return Err(HiveError::Refused(format!(
            "the reason was not sent: it contains {blocked}"
        )));
    }
    let reason = (!sealed_project).then(|| reason.to_owned());
    let request = ProposalConfirmV1 {
        schema: PROPOSAL_CONFIRM_SCHEMA.into(),
        session_ref: own,
        verdict,
        reason,
    };
    request
        .validate()
        .map_err(|e| HiveError::Config(format!("confirmation: {e}")))?;
    Ok(request)
}

/// The proposals of `record` this engine already objected to (on its own,
/// for a contradiction): `<data>/hive/objected/<vault_id>.json`.
fn objected_file(data: &Path, vault_id: &str) -> std::path::PathBuf {
    data.join("objected").join(format!("{vault_id}.json"))
}

/// Object to every open proposal of `record` that adds a fact an agent here
/// judged contradicting a current one (`ctx_knowledge judge … contradicts`),
/// once per proposal: an objection stops the auto-merge threshold and a
/// person decides (C0 1.9 §7). Returns how many objections were sent.
pub fn object_contradicted(session: &Session, record: &VaultRecord) -> Result<usize, HiveError> {
    if crate::core::hive::bus::relay_v2::speaks_v2(&session.store)
        || !session.lease.lease.grants(HiveRight::KnowledgeDraft)
    {
        return Ok(0);
    }
    let contradicted = contradicted_facts(record);
    if contradicted.is_empty() {
        return Ok(0);
    }
    let data = session.store.data_dir().to_path_buf();
    let file = objected_file(&data, &record.vault_id);
    let mut done: std::collections::BTreeSet<u64> = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut sent = 0;
    let mut refused = Vec::new();
    for item in list_in(
        session,
        record,
        &[ProposalState::Draft, ProposalState::Open],
    )? {
        if done.contains(&item.number) {
            continue;
        }
        let Some((fact, other)) = clash(&item, &contradicted) else {
            continue;
        };
        let verdict = Verdict::Object(format!(
            "Fact {fact} contradicts fact {other} (judged by an agent of this engine)."
        ));
        // Only an objection Hive took counts as sent; a refused one is
        // reported on every sync until it goes through or a person decides.
        match confirm(session, record, item.number, "hive-engine", &verdict, None) {
            Ok(_) => {
                done.insert(item.number);
                sent += 1;
            }
            // No auto-merge on this Hive: no threshold to stop, nothing to
            // report; a person decides every proposal.
            Err(error) if confirmations_retired(&error) => break,
            Err(error @ (HiveError::Refused(_) | HiveError::Task { .. })) => {
                refused.push(format!("#{}: {error}", item.number));
            }
            Err(error) => return Err(error),
        }
    }
    let bytes = serde_json::to_vec(&done).map_err(|e| HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(
        &data.join("objected"),
        &format!("{}.json", record.vault_id),
        &bytes,
    )?;
    if refused.is_empty() {
        Ok(sent)
    } else {
        Err(HiveError::Refused(format!(
            "objection not taken for {}",
            refused.join("; ")
        )))
    }
}

/// Fact id → the current facts an agent here judged it to contradict
/// (`ctx_knowledge judge … contradicts`), over every checkout of `record`.
fn contradicted_facts(
    record: &VaultRecord,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let category = super::learn::knowledge_category(&record.ns, &record.name);
    let mut contradicted: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for root in record.roots.iter().filter(|root| Path::new(root).is_dir()) {
        if let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(root) {
            for (fact, others) in super::states::contradictions(&knowledge, &category) {
                contradicted.entry(fact).or_default().extend(others);
            }
        }
    }
    contradicted
}

/// The first fact `item` adds that is judged contradicting here, with the
/// fact it contradicts.
fn clash(
    item: &ReviewItem,
    contradicted: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Option<(String, String)> {
    item.ops.iter().find_map(|op| match op {
        PatchOpV1::AddFact { fact } => contradicted
            .get(&fact.id)
            .and_then(|others| others.iter().next())
            .map(|other| (fact.id.clone(), other.clone())),
        _ => None,
    })
}

/// Merge the sealed proposals of `record` that reached their auto-merge
/// threshold (`ready`): Hive cannot read them, so an engine with
/// `knowledge.merge` merges them, one by one. Returns the newest revision
/// written and what could not be merged.
pub fn merge_ready(session: &Session, record: &VaultRecord) -> (Option<u64>, Vec<String>) {
    if !record.sealed || !session.lease.lease.grants(HiveRight::KnowledgeMerge) {
        return (None, Vec::new());
    }
    let items = match list_in(session, record, &[ProposalState::Ready]) {
        Ok(items) => items,
        Err(error) => return (None, vec![error.to_string()]),
    };
    // One at a time: a proposal that no longer applies waits for a person
    // and does not hold back the others.
    let mut merged = None;
    let mut problems = Vec::new();
    let contradicted = contradicted_facts(record);
    for item in items.into_iter().take(MAX_MERGED_PROPOSALS) {
        // Whatever became of the objection, a proposal judged contradicting
        // here is never merged by this engine.
        if let Some((fact, other)) = clash(&item, &contradicted) {
            problems.push(format!(
                "#{}: fact {fact} contradicts fact {other} here; a person decides",
                item.number
            ));
            continue;
        }
        match merge(session, record, &[item.number]) {
            Ok(revision) => merged = Some(revision),
            Err(error) => problems.push(format!("#{}: {error}", item.number)),
        }
    }
    (merged, problems)
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `ProposalRead` of the engine before the VA rename.
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct PreVaProposalRead {
        project_id: String,
        number: u64,
        session_ref: String,
        draft: String,
        at: chrono::DateTime<chrono::Utc>,
    }

    #[test]
    fn proposal_reads_keep_pre_va_names() {
        let old = r#"{"project_id":"v1","number":3,"session_ref":"s1","draft":"d","at":"2026-10-22T10:00:00Z"}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<
            ProposalRead,
            PreVaProposalRead,
        >(old, "project_id", "vault_id");
        assert_eq!((read.vault_id.as_str(), read.number), ("v1", 3));
    }
}

#[cfg(test)]
mod confirmation_tests {
    use super::*;

    const AWS_KEY: &str = concat!("AKIA", "IOSFODNN7EXAMPLE");

    /// Hive v4/v5 answers the confirmation route with `410
    /// feature_removed` (auto-merge removed): one quiet "retired" refusal,
    /// which an automatic objection run treats as nothing to report.
    #[test]
    fn a_gone_confirmation_route_is_retired_quietly() {
        let gone = HiveError::from_response(
            410,
            &serde_json::json!({"error": {
                "code": "feature_removed",
                "message": "auto-merge was removed",
                "feature": "auto_merge"
            }}),
        );
        assert!(
            matches!(gone, HiveError::Server { status: 410, .. }),
            "{gone:?}"
        );
        let retired = retired_on_gone(gone);
        assert!(confirmations_retired(&retired));
        assert_eq!(retired.to_string(), CONFIRMATIONS_RETIRED);
        // Every other answer stays what it was (prod 1.x keeps working).
        for other in [
            HiveError::NotFound,
            HiveError::Server {
                status: 409,
                code: "invalid_state".into(),
            },
            HiveError::Refused("objection not taken".into()),
        ] {
            let kept = retired_on_gone(other.clone());
            assert_eq!(kept, other);
            assert!(!confirmations_retired(&kept));
        }
    }

    #[test]
    fn a_session_never_confirms_its_own_proposal_and_objections_say_why() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let author = super::super::draft::session_ref("s-1");
        assert!(matches!(
            confirmation(false, "s-1", &author, false, 4, &Verdict::Confirm("Checked against the code.".into())),
            Err(HiveError::Refused(why)) if why.contains("written in this session")
        ));
        let other = confirmation(
            false,
            "s-2",
            &author,
            false,
            4,
            &Verdict::Confirm("Checked against the code.".into()),
        )
        .unwrap();
        // M9: another session of the instance that wrote it does not confirm
        // it either; it may still object.
        assert!(matches!(
            confirmation(false, "s-2", &author, true, 4, &Verdict::Confirm("Checked against the code.".into())),
            Err(HiveError::Refused(why)) if why.contains("from your other machine")
        ));
        assert!(
            confirmation(
                false,
                "s-2",
                &author,
                true,
                4,
                &Verdict::Object("Wrong.".into())
            )
            .is_ok()
        );
        assert_eq!(other.verdict, ConfirmVerdict::Confirm);
        assert_eq!(other.session_ref, super::super::draft::session_ref("s-2"));
        assert_eq!(other.reason.as_deref(), Some("Checked against the code."));

        let object = Verdict::Object("Contradicts the retry budget fact.".into());
        let clear = confirmation(false, "s-2", &author, false, 4, &object).unwrap();
        assert_eq!(clear.verdict, ConfirmVerdict::Object);
        assert_eq!(
            clear.reason.as_deref(),
            Some("Contradicts the retry budget fact.")
        );
        let sealed = confirmation(true, "s-2", &author, false, 4, &object).unwrap();
        assert_eq!(sealed.reason, None, "a sealed project's reason stays here");

        assert!(matches!(
            confirmation(
                false,
                "s-2",
                &author,
                false,
                4,
                &Verdict::Object("  ".into())
            ),
            Err(HiveError::Config(_))
        ));
        let secret = Verdict::Object(format!("Uses {AWS_KEY}"));
        assert!(matches!(
            confirmation(false, "s-2", &author, false, 4, &secret),
            Err(HiveError::Refused(why)) if !why.contains(AWS_KEY)
        ));
    }
}
