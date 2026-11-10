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
use crate::HiveError;
use crate::auth::Session;
use crate::config::HiveConfig;
use crate::mapping::{MapError, map_root};
use crate::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::vault::{VaultMap, VaultRecord};
use lean_ctx_protocol::hive::knowledge::{
    KnowledgeRevisionV1, MAX_KNOWLEDGE_BODY_BYTES, ProposalAckV1, RevisionState,
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
    pub problems: Vec<String>,
}

/// `GET /org/salt`: the salt id and bytes identity digests are made with.
pub fn org_salt(session: &Session) -> Result<(String, Vec<u8>), HiveError> {
    let body = session.with_client(|c| c.get("/org/salt"))?;
    let salt: OrgSaltV1 =
        serde_json::from_value(body).map_err(|e| HiveError::Protocol(format!("org salt: {e}")))?;
    salt.validate()
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    let bytes = B64
        .decode(&salt.salt)
        .map_err(|_| HiveError::Protocol("org salt is not base64".into()))?;
    Ok((salt.org_salt_id, bytes))
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
            self.salt = Some(org_salt(self.session)?);
        }
        Ok(self.salt.as_ref().expect("set above"))
    }

    /// The project for `root`, mapped in Hive when unknown
    /// ([`crate::mapping`]: match, first-share consent, resolve).
    fn resolve(&mut self, stable: &str, root: &Path) -> Result<VaultRecord, MapError> {
        // A resolution the link file or the identity no longer backs is
        // never used: it is dropped and the checkout resolves again.
        if let Some(stale) = crate::resolution::forget_stale(&mut self.map, root) {
            tracing::info!("hive: resolving again: {}", stale.describe());
            self.map
                .save(self.session.store.data_dir())
                .map_err(|e| HiveError::Local(e.to_string()))?;
        }
        if let Some(record) = self.map.vaults.get(stable)
            && record.roots.iter().any(|r| Path::new(r) == root)
        {
            crate::mapping::close_satisfied(self.session, self.config, stable, record);
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
        // Hive keeps an identity's first binding: an answer that is not the
        // linked vault is refused, never recorded (fail closed).
        crate::resolution::refuse_unlinked(root, &project.vault)?;
        self.map.record(stable, &project, root);
        self.map
            .save(self.session.store.data_dir())
            .map_err(|e| HiveError::Local(e.to_string()))?;
        // A first-share request opened before the vault existed (another
        // engine's sync created it) is satisfied now: never left waiting.
        let mapped = self.map.vaults[stable].clone();
        crate::mapping::close_satisfied(self.session, self.config, stable, &mapped);
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
    let identity = crate::vault::project_identity(root)
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
    if let Some(held) =
        crate::throttle::holding(&session.store, crate::throttle::Scope::Sync, Utc::now())
    {
        let depth = Outbox::at(&data).depth(CHANNEL_KNOWLEDGE);
        report.drafts_pending = depth.pending;
        report.drafts_failed = depth.failed;
        report.problems.push(held.refusal().to_string());
        return report;
    }
    // Every checkout whose recorded vault its link file or identity no
    // longer backs is dropped before anything is sent or pulled.
    let map = match crate::resolution::load_current(&data) {
        Ok((map, _)) => map,
        Err(error) => {
            report.problems.push(error.to_string());
            return report;
        }
    };
    let mut resolver = Resolver {
        session,
        salt: None,
        map,
        config,
    };
    if let Some(root) = cwd_root
        && let Some(identity) = crate::vault::project_identity(root)
    {
        let stable = lean_ctx_protocol::hive::vault::stable_identity(&identity);
        if let Err(error) = resolver.resolve(&stable, root) {
            report.problems.push(format!("{}: {error}", root.display()));
        }
    }
    // A C0 2.0 Hive takes no 1.x draft: capture sends the same knowledge
    // as signed events, so a draft still queued from before is dropped.
    if crate::bus::relay_v2::speaks_v2(&session.store) {
        drop_drafts(&data);
    } else {
        send_drafts(&mut resolver, &mut report);
    }
    pull_all(&resolver, &mut report);
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

fn drop_drafts(data: &Path) {
    let outbox = Outbox::at(data);
    for item in outbox.list(CHANNEL_KNOWLEDGE) {
        let _ = outbox.remove(&item);
    }
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
        let body = match serde_json::to_value(&queued_draft.draft) {
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
                let wait = error
                    .retry_after_at(now)
                    .or(Some(crate::throttle::POLICY_HOLD_S.unsigned_abs()));
                report.problems.push(format!("draft {}: {error}", item.id));
                let _ = if quota {
                    outbox.retry_later(&mut item, &error.to_string(), wait, now)
                } else {
                    outbox.hold_for_policy(&mut item, &error.to_string(), wait, now)
                };
                if quota {
                    let _ = crate::throttle::note(
                        &resolver.session.store,
                        crate::throttle::Scope::Sync,
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

/// Rebase a queued draft onto `base` when it is newer. Returns `false` when
/// every op is already in the base (nothing left to propose). A draft whose
/// ops clash with the newer base keeps its old base.
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
    // `validate` refused a revision without its document.
    let Some(document) = revision.document else {
        return Ok(None);
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
    let describe = |items: Vec<crate::outbox::OutboxItem>| {
        items
            .into_iter()
            .map(|item| {
                let summary = queued(&item.payload).map_or_else(
                    || "(unreadable)".to_string(),
                    |q| q.draft.summary.unwrap_or_default(),
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
