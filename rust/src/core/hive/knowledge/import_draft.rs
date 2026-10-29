// SPDX-License-Identifier: Apache-2.0
//! Facts imported from the repository itself (K1: `lean-ctx import
//! docs|git`) → import drafts in the outbox (C0 1.9 import addendum).
//!
//! An import draft is a learning draft with `origin: import` and the import
//! source of each fact (`import:docs:<path>#<slug>`, `import:git:<sha>`):
//! the same planning against the last pulled base and the same screening,
//! queued only, sent by [`super::pull`]. A sealed project gets the sources
//! inside the sealed patch only ([`super::pull`] seals the draft as a
//! whole); `origin` stays in the clear. A draft carries at most 16 sources
//! and [`MAX_PATCH_OPS`] facts, so a large import becomes several drafts.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::draft::{QueuedDraft, Watermark, session_ref};
use super::learn::plan_import;
use super::screening::Screen;
use super::{BaseRevision, load_base};
use crate::core::hive::HiveError;
use crate::core::hive::capture;
use crate::core::hive::config::HiveConfig;
use crate::core::hive::outbox::{CHANNEL_KNOWLEDGE, Outbox};
use crate::core::hive::state::{HiveStore, write_private};
use crate::core::hive::vault::{VaultMap, project_identity};
use lean_ctx_protocol::hive::knowledge::{
    ContextPatchV1, DRAFT_PROPOSAL_SCHEMA, DraftOrigin, DraftProposalV1, FactV1, MAX_PATCH_OPS,
    PatchOpV1, ProposalState,
};
use lean_ctx_protocol::hive::vault::stable_identity;

/// Sources one draft may name (`DraftProposalV1::validate`).
const MAX_SOURCES: usize = 16;
/// Facts planned per run; the rest follows with the next import.
const MAX_FACTS_PER_RUN: usize = 400;

fn watermark_name(stable: &str) -> String {
    format!("{stable}.import-watermark")
}

fn load_watermark(data_dir: &Path, stable: &str) -> Watermark {
    std::fs::read(data_dir.join("drafts").join(watermark_name(stable)))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_watermark(data_dir: &Path, stable: &str, mark: &Watermark) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec(mark).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&data_dir.join("drafts"), &watermark_name(stable), &bytes)
}

/// The planned facts in drafts of at most [`MAX_SOURCES`] distinct sources
/// and [`MAX_PATCH_OPS`] facts, in plan order.
fn batches(facts: Vec<FactV1>, sources: Vec<String>) -> Vec<(Vec<FactV1>, Vec<String>)> {
    let mut out: Vec<(Vec<FactV1>, Vec<String>)> = Vec::new();
    for (fact, source) in facts.into_iter().zip(sources) {
        let fits = out.last().is_some_and(|(facts, names)| {
            facts.len() < MAX_PATCH_OPS && (names.contains(&source) || names.len() < MAX_SOURCES)
        });
        if !fits {
            out.push((Vec::new(), Vec::new()));
        }
        let (facts, names) = out.last_mut().expect("pushed above");
        if !names.contains(&source) {
            names.push(source);
        }
        facts.push(fact);
    }
    out
}

fn summary_and_body(facts: &[FactV1]) -> (String, String) {
    let summary = match facts.len() {
        1 => "Imported from the repository: 1 fact".to_owned(),
        n => format!("Imported from the repository: {n} facts"),
    };
    let mut body = String::from("Imported by lean-ctx from the repository's docs and history:\n");
    // Each fact passed the capture gate: one complete claim, listed whole.
    for fact in facts {
        let claim = fact.statement.lines().next().unwrap_or_default();
        body.push_str(&format!("- {claim}\n"));
    }
    (summary, body)
}

/// Plan, screen and queue the imported facts of the project at `root` that
/// changed since its last import draft. Returns the outbox ids (none when
/// there is nothing to propose or Hive is not set up here).
pub fn enqueue_import_drafts(
    store: &HiveStore,
    config: &HiveConfig,
    root: &Path,
    session_id: &str,
    screen: &mut Screen,
    now: DateTime<Utc>,
) -> Result<Vec<String>, HiveError> {
    if config.enabled == Some(false) || !config.auto_capture || store.instance()?.is_none() {
        return Ok(Vec::new());
    }
    let Some(identity) = project_identity(root) else {
        return Ok(Vec::new());
    };
    let stable = stable_identity(&identity);
    let data = store.data_dir();
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(&root.to_string_lossy())
    else {
        return Ok(Vec::new());
    };
    let base: BaseRevision = VaultMap::load(data)
        .vaults
        .get(&stable)
        .map(|record| load_base(data, &record.vault_id))
        .unwrap_or_default();
    let mut mark = load_watermark(data, &stable);
    let (mut run, place) = capture::open_run(data, &stable, root, screen, now.timestamp_millis());
    let plan = plan_import(
        &knowledge,
        &base.document,
        mark.since,
        MAX_FACTS_PER_RUN,
        &mut run,
    );
    capture::finish_run(data, &place, &run)?;
    let screening = run.screen.counts();
    if plan.watermark.is_some() {
        mark.since = plan.watermark;
    }
    // Every draft is built and validated before the first is queued, and a
    // queue that fails part way takes back what it queued: an aborted run
    // never leaves drafts that the next run (same watermark) queues again.
    let mut payloads = Vec::new();
    for (facts, sources) in batches(plan.added, plan.import_sources) {
        let (summary, body) = summary_and_body(&facts);
        let draft = DraftProposalV1 {
            schema: DRAFT_PROPOSAL_SCHEMA.into(),
            state: ProposalState::Draft,
            summary: Some(summary),
            body: Some(body),
            sources,
            context_patch: ContextPatchV1 {
                base_revision: base.revision,
                ops: facts
                    .into_iter()
                    .map(|fact| PatchOpV1::AddFact { fact })
                    .collect(),
                sealed: None,
            },
            session_ref: session_ref(session_id),
            engine_screening: screening,
            origin: Some(DraftOrigin::Import),
        };
        draft
            .validate()
            .map_err(|e| HiveError::Protocol(format!("import draft: {e}")))?;
        let payload = serde_json::to_value(QueuedDraft {
            stable_identity: stable.clone(),
            root: root.to_string_lossy().into_owned(),
            draft,
        })
        .map_err(|e| HiveError::Local(e.to_string()))?;
        payloads.push(payload);
    }
    let outbox = Outbox::at(data);
    let mut ids = Vec::new();
    for payload in payloads {
        match outbox.enqueue(CHANNEL_KNOWLEDGE, payload, now) {
            Ok(id) => ids.push(id),
            Err(error) => {
                for item in outbox
                    .list(CHANNEL_KNOWLEDGE)
                    .into_iter()
                    .filter(|item| ids.contains(&item.id))
                {
                    let _ = outbox.remove(&item);
                }
                return Err(error);
            }
        }
    }
    if !ids.is_empty() {
        mark.last_draft_at = Some(now);
    }
    save_watermark(data, &stable, &mark)?;
    Ok(ids)
}

#[cfg(test)]
#[path = "import_draft_tests.rs"]
mod tests;
