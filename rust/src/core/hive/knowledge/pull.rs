// SPDX-License-Identifier: Apache-2.0
//! The network half of knowledge sync, run by the scheduler or `lean-ctx
//! hive sync`, never inside a tool call:
//! 1. send queued drafts (rebased onto the newest base when possible),
//! 2. pull new revisions of known projects into project knowledge.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chrono::Utc;

use super::doc::rebase_ops;
use super::draft::{QueuedDraft, queued};
use super::learn::sync_into_knowledge;
use super::{BaseRevision, load_base, save_base};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::mapping::{MapError, map_root};
use crate::core::hive::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::core::hive::vault::{VaultMap, VaultRecord};
use lean_ctx_protocol::hive::knowledge::{
    DraftProposalV1, KnowledgeRevisionV1, MAX_KNOWLEDGE_BODY_BYTES, ProposalAckV1, RevisionState,
};
use lean_ctx_protocol::hive::vault::OrgSaltV1;

/// Seconds before a draft that waits for the person's first-share yes is
/// looked at again.
const CONSENT_RECHECK_SECONDS: u64 = 300;

/// What one sync pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub drafts_sent: Vec<u64>,
    pub drafts_pending: usize,
    pub drafts_failed: usize,
    /// `(ns/name, revision, facts loaded)` per project that moved.
    pub pulled: Vec<(String, u64, usize)>,
    /// `(ns/name, revision)` per sealed project whose ready proposals this
    /// engine merged (C0 1.9 §7).
    pub merged_ready: Vec<(String, u64)>,
    pub problems: Vec<String>,
}

struct Resolver<'a> {
    session: &'a Session,
    salt: Option<(String, Vec<u8>)>,
    map: VaultMap,
    config: &'a HiveConfig,
}

impl Resolver<'_> {
    fn salt(&mut self) -> Result<&(String, Vec<u8>), HiveError> {
        if self.salt.is_none() {
            let body = self.session.with_client(|c| c.get("/org/salt"))?;
            let salt: OrgSaltV1 = serde_json::from_value(body)
                .map_err(|e| HiveError::Protocol(format!("org salt: {e}")))?;
            salt.validate()
                .map_err(|e| HiveError::Protocol(e.to_string()))?;
            let bytes = B64
                .decode(&salt.salt)
                .map_err(|_| HiveError::Protocol("org salt is not base64".into()))?;
            self.salt = Some((salt.org_salt_id, bytes));
        }
        Ok(self.salt.as_ref().expect("set above"))
    }

    /// The project for `root`, mapped in Hive when unknown
    /// ([`crate::core::hive::mapping`]: match, first-share consent, resolve).
    fn resolve(&mut self, stable: &str, root: &Path) -> Result<VaultRecord, MapError> {
        if let Some(record) = self.map.vaults.get(stable)
            && record.roots.iter().any(|r| Path::new(r) == root)
        {
            crate::core::hive::mapping::close_satisfied(self.session, self.config, stable, record);
            return Ok(record.clone());
        }
        let salt = self.salt()?.clone();
        let known = self.map.vaults.get(stable).cloned();
        let project = map_root(
            self.session,
            self.config,
            &salt,
            stable,
            root,
            known.as_ref(),
        )?;
        self.map.record(stable, &project, root);
        self.map.save(self.session.store.data_dir())?;
        // A first-share request opened before the vault existed (another
        // engine's sync created it) is satisfied now: never left waiting.
        let mapped = self.map.vaults[stable].clone();
        crate::core::hive::mapping::close_satisfied(self.session, self.config, stable, &mapped);
        Ok(mapped)
    }
}

/// The vault the checkout at `root` is linked to, resolved in Hive (and
/// remembered) when this machine has no record of it yet, e.g. on the
/// first sync after `hive link`. A first share that waits for the
/// person's yes is `MapError::Consent`.
pub fn resolve_project(
    session: &Session,
    config: &HiveConfig,
    root: &Path,
) -> Result<VaultRecord, MapError> {
    let identity = crate::core::hive::vault::project_identity(root)
        .ok_or_else(|| HiveError::Config("not inside a git repository".into()))?;
    let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
    let mut resolver = Resolver {
        session,
        salt: None,
        map: VaultMap::load(session.store.data_dir()),
        config,
    };
    resolver.resolve(&stable, root)
}

/// Send drafts, then pull. `cwd_root` adds the current project to the pull
/// even if it never drafted (for `lean-ctx hive sync` in a project).
pub fn sync(session: &Session, config: &HiveConfig, cwd_root: Option<&Path>) -> SyncReport {
    let data = session.store.data_dir().to_path_buf();
    let mut report = SyncReport::default();
    // A used-up transfer quota or a policy hold: drafts stay queued, no
    // request goes out until it ends (never a busy loop).
    if let Some(held) = crate::core::hive::throttle::holding(
        &session.store,
        crate::core::hive::throttle::Scope::Sync,
        Utc::now(),
    ) {
        let depth = Outbox::at(&data).depth(CHANNEL_KNOWLEDGE);
        report.drafts_pending = depth.pending;
        report.drafts_failed = depth.failed;
        report.problems.push(held.refusal().to_string());
        return report;
    }
    let mut resolver = Resolver {
        session,
        salt: None,
        map: VaultMap::load(&data),
        config,
    };
    if let Some(root) = cwd_root
        && let Some(identity) = crate::core::hive::vault::project_identity(root)
    {
        let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
        if let Err(error) = resolver.resolve(&stable, root) {
            report.problems.push(format!("{}: {error}", root.display()));
        }
    }
    send_drafts(&mut resolver, &mut report);
    pull_all(&resolver, &mut report);
    // 1.x proposal merges (§7) and fact states (J2) have no C0 2.0 route:
    // a 2.0 Hive decides through review items and folded maintenance events.
    if !crate::core::hive::bus::relay_v2::speaks_v2(&session.store) {
        merge_ready_all(&resolver, &mut report);
        report_states(&resolver, &mut report);
    }
    let outbox = Outbox::at(&data);
    let depth = outbox.depth(CHANNEL_KNOWLEDGE);
    report.drafts_pending = depth.pending;
    report.drafts_failed = depth.failed;
    let now = session.lease.service_time(Utc::now()).with_timezone(&Utc);
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if report.problems.is_empty() {
        session.store.record_success("knowledge", &stamp);
    } else {
        session.store.record_error(&report.problems.join("; "));
    }
    report
}

fn send_drafts(resolver: &mut Resolver<'_>, report: &mut SyncReport) {
    let data = resolver.session.store.data_dir().to_path_buf();
    let outbox = Outbox::at(&data);
    let now = Utc::now();
    for mut item in outbox.list(CHANNEL_KNOWLEDGE) {
        if !item.due(now) {
            continue;
        }
        let Some(mut queued_draft) = queued(&item.payload) else {
            let _ = outbox.fail(&mut item, "damaged draft");
            continue;
        };
        let record = match resolver
            .resolve(&queued_draft.stable_identity, Path::new(&queued_draft.root))
        {
            Ok(record) => record,
            Err(error) => {
                report.problems.push(format!("draft {}: {error}", item.id));
                // A first share waits for the person: look again soon,
                // without the growing backoff of a failure.
                let wait = matches!(error, MapError::Consent(_)).then_some(CONSENT_RECHECK_SECONDS);
                let _ = outbox.retry_later(&mut item, &error.to_string(), wait, now);
                if matches!(
                    error,
                    MapError::Hive(HiveError::Unauthorized | HiveError::LeaseLapsed)
                ) {
                    return;
                }
                continue;
            }
        };
        let base = load_base(&data, &record.vault_id);
        if !rebase_onto(&mut queued_draft, &base) {
            let _ = outbox.remove(&item);
            continue;
        }
        if let Ok(payload) = serde_json::to_value(&queued_draft) {
            item.payload = payload;
            let _ = outbox.update(&item);
        }
        // A sealed project gets the draft sealed, with nothing readable
        // outside; the queued item keeps the clear draft on this machine.
        let wire = if record.sealed {
            match sealed_draft(resolver.session, &record.vault_id, &queued_draft.draft) {
                Ok(Some(draft)) => draft,
                Ok(None) => {
                    let _ = outbox.retry_later(
                        &mut item,
                        "sealed vault: waiting for the vault key",
                        Some(3600),
                        now,
                    );
                    continue;
                }
                Err(error) => {
                    report.problems.push(format!("draft {}: {error}", item.id));
                    let _ = outbox.retry_later(&mut item, &error.to_string(), Some(900), now);
                    continue;
                }
            }
        } else {
            queued_draft.draft.clone()
        };
        let body = match serde_json::to_value(&wire) {
            Ok(body) => body,
            Err(error) => {
                let _ = outbox.fail(&mut item, &error.to_string());
                continue;
            }
        };
        let path = format!("/vaults/{}/proposals", record.vault_id);
        let result = resolver
            .session
            .with_client(|c| c.post_idempotent(&path, &body, &item.id));
        match result {
            Ok(answer) => {
                if let Ok(ack) = serde_json::from_value::<ProposalAckV1>(answer) {
                    report.drafts_sent.push(ack.number);
                }
                let _ = outbox.remove(&item);
            }
            Err(error @ (HiveError::Unauthorized | HiveError::LeaseLapsed)) => {
                report.problems.push(error.to_string());
                let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
                return;
            }
            Err(HiveError::RateLimited { retry_after }) => {
                let _ = outbox.retry_later(&mut item, "rate limited", retry_after, now);
            }
            // A used-up quota: this draft and the rest wait in the outbox and
            // the throttle holds sync until it ends. A policy refusal holds
            // this draft only, until a newer policy is in force or the
            // hold's time is up (an admin may change the policy).
            Err(error @ (HiveError::QuotaExceeded(_) | HiveError::PolicyDenied(_))) => {
                let quota = matches!(error, HiveError::QuotaExceeded(_));
                let wait = error.retry_after_at(now).or(Some(
                    crate::core::hive::throttle::POLICY_HOLD_S.unsigned_abs(),
                ));
                report.problems.push(format!("draft {}: {error}", item.id));
                let _ = if quota {
                    outbox.retry_later(&mut item, &error.to_string(), wait, now)
                } else {
                    outbox.hold_for_policy(&mut item, &error.to_string(), wait, now)
                };
                if quota {
                    let _ = crate::core::hive::throttle::note(
                        &resolver.session.store,
                        crate::core::hive::throttle::Scope::Sync,
                        error,
                        now,
                    );
                    return;
                }
            }
            Err(error) if error.is_transient() => {
                let _ = outbox.retry_later(&mut item, &error.to_string(), None, now);
            }
            Err(
                error @ (HiveError::Conflict { .. } | HiveError::Forbidden | HiveError::NotFound),
            ) => {
                // The base moved or the right/project changed: try again after the next pull.
                report.problems.push(format!("draft {}: {error}", item.id));
                let _ = outbox.retry_later(&mut item, &error.to_string(), Some(900), now);
            }
            Err(error) => {
                report.problems.push(format!("draft {}: {error}", item.id));
                let _ = outbox.fail(&mut item, &error.to_string());
            }
        }
    }
}

/// The draft as a sealed project takes it: summary, body, sources and ops
/// sealed under the current repo key; `None` while no key reached this
/// instance or a rotation is due.
fn sealed_draft(
    session: &Session,
    vault_id: &str,
    clear: &DraftProposalV1,
) -> Result<Option<DraftProposalV1>, HiveError> {
    use crate::core::hive::sealed::{SealingKey, seal, sealing_key};
    use lean_ctx_protocol::hive::knowledge::ContextPatchV1;
    use lean_ctx_protocol::hive::sealed::{SEALED_PATCH_CONTENT_SCHEMA, SealedDraftContentV1};
    let base_revision = clear.context_patch.base_revision;
    let (epoch, key) = match sealing_key(session, vault_id, base_revision)? {
        SealingKey::Ready(epoch, key) => (epoch, key),
        SealingKey::Waiting => return Ok(None),
        SealingKey::Rotating { required } => {
            return Err(HiveError::Local(format!(
                "sealed vault: the key moves to epoch {required} first (a member was removed)"
            )));
        }
    };
    let content = SealedDraftContentV1 {
        schema: SEALED_PATCH_CONTENT_SCHEMA.into(),
        summary: clear
            .summary
            .clone()
            .ok_or_else(|| HiveError::Local("a queued draft without a summary".into()))?,
        body: clear.body.clone(),
        sources: clear.sources.clone(),
        ops: clear.context_patch.ops.clone(),
    };
    content
        .validate()
        .map_err(|e| HiveError::Protocol(format!("sealed draft: {e}")))?;
    let plaintext = zeroize::Zeroizing::new(
        serde_json::to_vec(&content).map_err(|e| HiveError::Local(e.to_string()))?,
    );
    let blob = seal(
        &key,
        vault_id,
        base_revision,
        epoch,
        SEALED_PATCH_CONTENT_SCHEMA,
        &plaintext,
    )?;
    let draft = DraftProposalV1 {
        summary: None,
        body: None,
        sources: Vec::new(),
        context_patch: ContextPatchV1 {
            base_revision,
            ops: Vec::new(),
            sealed: Some(blob),
        },
        ..clear.clone()
    };
    draft
        .validate()
        .map_err(|e| HiveError::Protocol(format!("sealed draft: {e}")))?;
    Ok(Some(draft))
}

/// Rebase a queued draft onto `base` when it is newer. Returns `false` when
/// every op is already in the base (nothing left to propose). A draft whose
/// ops clash with the newer base keeps its old base; reviewers resolve it.
fn rebase_onto(queued_draft: &mut QueuedDraft, base: &BaseRevision) -> bool {
    let patch = &mut queued_draft.draft.context_patch;
    if base.revision <= patch.base_revision {
        return true;
    }
    match rebase_ops(&base.document, &patch.ops) {
        Ok(kept) if kept.is_empty() => false,
        Ok(kept) => {
            patch.ops = kept;
            patch.base_revision = base.revision;
            true
        }
        Err(_) => true,
    }
}

fn pull_all(resolver: &Resolver<'_>, report: &mut SyncReport) {
    let data = resolver.session.store.data_dir().to_path_buf();
    for record in resolver.map.vaults.values() {
        match pull_project(resolver.session, &data, record) {
            Ok(Some((revision, loaded))) => {
                report
                    .pulled
                    .push((format!("{}/{}", record.ns, record.name), revision, loaded));
            }
            Ok(None) => {}
            Err(error) => report
                .problems
                .push(format!("pull {}/{}: {error}", record.ns, record.name)),
        }
    }
}

/// §7: sealed proposals that reached their threshold wait for an engine
/// with `knowledge.merge`; this one merges them. Proposals adding a fact an
/// agent here judged contradicting get an objection.
fn merge_ready_all(resolver: &Resolver<'_>, report: &mut SyncReport) {
    for record in resolver.map.vaults.values() {
        let repo = format!("{}/{}", record.ns, record.name);
        // An objection stops a threshold: it goes out before any merge.
        if let Err(error) = super::review::object_contradicted(resolver.session, record) {
            report.problems.push(format!("objections {repo}: {error}"));
        }
        let (merged, problems) = super::review::merge_ready(resolver.session, record);
        if let Some(revision) = merged {
            report.merged_ready.push((repo.clone(), revision));
        }
        for problem in problems {
            report
                .problems
                .push(format!("ready proposals {repo}: {problem}"));
        }
    }
}

/// J2: tell Hive which facts of each base are in doubt here (sent only
/// when that changed).
fn report_states(resolver: &Resolver<'_>, report: &mut SyncReport) {
    let data = resolver.session.store.data_dir().to_path_buf();
    for record in resolver.map.vaults.values() {
        match super::states::report(resolver.session, &data, record) {
            // A lease without knowledge.read reads no knowledge to doubt.
            Ok(_) | Err(HiveError::Forbidden) => {}
            Err(error) => report.problems.push(format!(
                "fact states {}/{}: {error}",
                record.ns, record.name
            )),
        }
    }
}

/// Bring one project's base to Hive' newest revision (before a merge).
pub(crate) fn refresh_project(
    session: &Session,
    data: &Path,
    record: &VaultRecord,
) -> Result<(), HiveError> {
    pull_project(session, data, record).map(|_| ())
}

/// Pull one project; `Some((revision, facts loaded))` when it moved.
fn pull_project(
    session: &Session,
    data: &Path,
    record: &VaultRecord,
) -> Result<Option<(u64, usize)>, HiveError> {
    let base = load_base(data, &record.vault_id);
    let path = format!("/vaults/{}/knowledge", record.vault_id);
    let etag = base
        .etag
        .clone()
        .unwrap_or_else(|| format!("\"{}\"", base.revision));
    let answer = session.with_client(|c| {
        c.send(
            "GET",
            &path,
            None,
            &[("If-None-Match", &etag)],
            MAX_KNOWLEDGE_BODY_BYTES,
        )
    })?;
    if answer.status == 304 {
        return Ok(None);
    }
    let revision: KnowledgeRevisionV1 = serde_json::from_value(answer.body)
        .map_err(|e| HiveError::Protocol(format!("knowledge: {e}")))?;
    revision
        .validate()
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    if revision.vault_id != record.vault_id {
        return Err(HiveError::Protocol("knowledge for another project".into()));
    }
    // Only a released revision is ever delivered to agents (C0 1.9 §3).
    if revision.state != RevisionState::Released {
        return Err(HiveError::Protocol(
            "Hive delivered a revision that is not released".into(),
        ));
    }
    let document = match (revision.document, revision.sealed) {
        (Some(document), _) => document,
        // Sealed: opened with the key of its epoch; without that key the
        // revision waits (never a plaintext fallback).
        (None, Some(blob)) => {
            let Some(key) =
                crate::core::hive::sealed::key_for_epoch(session, &record.vault_id, blob.epoch)?
            else {
                return Ok(None);
            };
            let plaintext =
                crate::core::hive::sealed::open(&key, &record.vault_id, revision.revision, &blob)?;
            let document: lean_ctx_protocol::hive::knowledge::ResearchContextDocumentV1 =
                serde_json::from_slice(&plaintext)
                    .map_err(|e| HiveError::Protocol(format!("sealed knowledge: {e}")))?;
            // The plain document is validated with the revision; the sealed
            // one only exists once opened, and gets the same checks then.
            document
                .validate()
                .map_err(|e| HiveError::Protocol(format!("sealed knowledge: {e}")))?;
            document
        }
        (None, None) => return Ok(None),
    };
    if revision.revision == base.revision && document == base.document {
        if revision.quarantined_after != base.quarantined_after {
            save_base(
                data,
                &record.vault_id,
                &BaseRevision {
                    quarantined_after: revision.quarantined_after,
                    etag: answer.etag,
                    ..base
                },
            )?;
        }
        return Ok(None);
    }
    let mut loaded = 0;
    for root in &record.roots {
        if Path::new(root).is_dir() {
            let (count, _) =
                sync_into_knowledge(root, &record.ns, &record.name, revision.revision, &document)
                    .map_err(HiveError::Local)?;
            loaded = loaded.max(count);
        }
    }
    save_base(
        data,
        &record.vault_id,
        &BaseRevision {
            revision: revision.revision,
            etag: answer.etag,
            document,
            quarantined_after: revision.quarantined_after,
        },
    )?;
    Ok(Some((revision.revision, loaded)))
}

/// A draft as `hive status` lists it: `(outbox id, summary, last error)`.
pub type DraftLine = (String, String, Option<String>);

/// Pending and failed drafts for `hive status`.
pub fn draft_listing(data: &Path) -> (Vec<DraftLine>, Vec<DraftLine>) {
    let outbox = Outbox::at(data);
    let describe = |items: Vec<crate::core::hive::outbox::OutboxItem>| {
        items
            .into_iter()
            .map(|item| {
                let summary = queued(&item.payload).map_or_else(
                    || "(unreadable)".to_string(),
                    |q| {
                        q.draft
                            .summary
                            .unwrap_or_else(|| "(sealed draft)".to_string())
                    },
                );
                (item.id, summary, item.last_error)
            })
            .collect::<Vec<_>>()
    };
    (
        describe(outbox.list(CHANNEL_KNOWLEDGE)),
        describe(outbox.failed(CHANNEL_KNOWLEDGE)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::hive::knowledge::{
        ContextPatchV1, DRAFT_PROPOSAL_SCHEMA, DraftProposalV1, EngineScreeningV1, FactV1,
        PatchOpV1, ProposalState, ResearchContextDocumentV1,
    };
    use serde_json::Value;

    fn fact(id: &str) -> FactV1 {
        FactV1 {
            id: id.into(),
            statement: format!("statement {id}"),
            kind: "decision".into(),
            applies_to: vec![],
            sources: vec![],
            from_contributions: vec![],
            anchors: vec![],
        }
    }

    fn queued_with(ids: &[&str], base_revision: u64) -> QueuedDraft {
        QueuedDraft {
            stable_identity: "a".repeat(64),
            root: "/tmp/p".into(),
            draft: DraftProposalV1 {
                schema: DRAFT_PROPOSAL_SCHEMA.into(),
                state: ProposalState::Draft,
                summary: Some("s".into()),
                body: None,
                sources: vec![],
                context_patch: ContextPatchV1 {
                    base_revision,
                    ops: ids
                        .iter()
                        .map(|id| PatchOpV1::AddFact { fact: fact(id) })
                        .collect(),
                    sealed: None,
                },
                session_ref: "ses_1".into(),
                engine_screening: EngineScreeningV1 {
                    scanner_version: 1,
                    secrets_removed: 0,
                    pii_removed: 0,
                    items_dropped: 0,
                },
                origin: None,
            },
        }
    }

    fn base(revision: u64, ids: &[&str]) -> BaseRevision {
        BaseRevision {
            revision,
            document: ResearchContextDocumentV1 {
                facts: ids.iter().map(|id| fact(id)).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn drafts_move_onto_newer_bases_and_vanish_when_merged() {
        let mut q = queued_with(&["a", "b"], 1);
        assert!(rebase_onto(&mut q, &base(2, &["a"])));
        assert_eq!(q.draft.context_patch.base_revision, 2);
        assert_eq!(q.draft.context_patch.ops.len(), 1);
        let mut merged = queued_with(&["a"], 1);
        assert!(
            !rebase_onto(&mut merged, &base(3, &["a"])),
            "nothing left to propose"
        );
    }

    #[test]
    fn clashing_drafts_keep_their_old_base() {
        let mut q = queued_with(&["a"], 1);
        let mut clash = base(2, &[]);
        let mut changed = fact("a");
        changed.statement = "someone else wrote this".into();
        clash.document.facts.push(changed);
        assert!(rebase_onto(&mut q, &clash));
        assert_eq!(q.draft.context_patch.base_revision, 1);
        let _: Value = serde_json::to_value(&q).unwrap();
    }
}
