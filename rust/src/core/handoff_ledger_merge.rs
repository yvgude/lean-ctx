// SPDX-License-Identifier: Apache-2.0
//! Merging handoff ledgers (`ctx_handoff merge`): several agents' ledgers of
//! one project become one new ledger another agent can pull.
//!
//! The merge is a pure selection over what the inputs already carry:
//!
//! - **served first**: every input passes [`load_served`] — the same whole-
//!   ledger re-authorization as `show` and `pull`. A withheld input refuses
//!   the merge, typed and without content;
//! - **subset only**: every finding, fact, excerpt, decision and step of the
//!   result is one of an input's, byte for byte. Nothing is rewritten,
//!   summarized or ranked; duplicates collapse, bounds cut the tail;
//! - **conflicts kept**: a fact key with different values keeps every value
//!   and is reported, never resolved by picking a winner;
//! - **authority carried**: a part is recorded as bound when an input that
//!   carries the same bytes recorded it as bound — the same text, and that
//!   input's sources are in the result's lineage. The lineage is re-read
//!   through the admitted reader now, against ONE budget as a read of the
//!   result will re-read it; more sources have to hold, never fewer;
//! - **no excerpt without its source**: a curated excerpt is kept only when
//!   its file is re-bound into the result's lineage. A merged ledger carries
//!   no session snapshot, so an unbound excerpt would otherwise pass for part
//!   of a complete file lineage;
//! - **deterministic** (#498): the inputs are ordered canonically and the
//!   result takes the newest input's `created_at`, so the same set of ledgers
//!   merges to the same bytes and the same file, in any argument order.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::authority::CURATED_TOOL;
use super::{
    BoundEntry, CuratedRef, HandoffLedgerV1, KnowledgeExcerpt, KnowledgeFactMini, MAX_CURATED_REFS,
    MAX_KNOWLEDGE_FACTS, MAX_LEDGER_FINDINGS, PartKind, SCHEMA_VERSION, SessionExcerpt,
    ToolCallsSummary, load_served, manifest_md5, persist_ledger,
};
use crate::core::context_admission::carrier::Withheld;
use crate::core::context_admission::stored::SourceRef;

/// Most ledgers one merge reads.
pub(crate) const MAX_MERGE_INPUTS: usize = 8;
/// Decisions and next steps a merged ledger keeps, as `create_ledger` does.
const MAX_DECISIONS: usize = 10;
const MAX_NEXT_STEPS: usize = 20;

const SOURCE_CHANGED: &str = "store.source_changed";

/// A fact key that carries more than one value across the inputs. Every
/// value stays in the merged ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactConflict {
    pub category: String,
    pub key: String,
    pub values: usize,
}

/// What a bound cut from the merged ledger: the parts beyond each bound,
/// counted after duplicates collapsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub findings: usize,
    pub facts: usize,
    pub curated_refs: usize,
    /// Curated excerpts left out because their file is not re-bound.
    pub unbound_curated: usize,
    pub decisions: usize,
    pub next_steps: usize,
}

/// The result of a merge: the stored ledger and what the reply reports.
#[derive(Debug)]
pub(crate) struct MergeOutcome {
    pub ledger: HandoffLedgerV1,
    pub path: PathBuf,
    pub conflicts: Vec<FactConflict>,
    pub distinct_tasks: usize,
    pub dropped: Dropped,
}

/// One input as it was served.
struct Input {
    ledger: HandoffLedgerV1,
    /// The content digest recomputed from the served ledger: the stored
    /// `content_md5` field is not taken on trust.
    digest: String,
    /// The elements recorded as bound at the input's creation.
    bound: Vec<BoundEntry>,
    /// `(project, [(path, tool, admitted digest)])` of the input's binding.
    files: Option<(String, Vec<(String, String, String)>)>,
}

impl Input {
    fn bound(&self, kind: PartKind, index: usize) -> bool {
        self.bound
            .iter()
            .any(|entry| entry.kind == kind && entry.index == index)
    }
}

/// Merge the stored ledgers at `paths` into a new stored ledger.
pub(crate) fn merge_ledgers(paths: &[PathBuf]) -> Result<MergeOutcome, String> {
    let unique: BTreeSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
    if unique.len() > MAX_MERGE_INPUTS {
        return Err(format!("merge reads at most {MAX_MERGE_INPUTS} ledgers"));
    }
    let mut inputs = Vec::with_capacity(unique.len());
    for path in unique {
        let served = load_served(path)?;
        inputs.push(Input {
            digest: super::ledger_content_md5(&served.ledger),
            ledger: served.ledger,
            bound: served
                .lineage
                .map(|lineage| lineage.bound)
                .unwrap_or_default(),
            files: served.files,
        });
    }
    // Canonical order, one input per content digest.
    inputs
        .sort_by(|a, b| (&a.ledger.created_at, &a.digest).cmp(&(&b.ledger.created_at, &b.digest)));
    inputs.dedup_by(|a, b| a.digest == b.digest);
    if inputs.len() < 2 {
        return Err("merge needs at least two different ledgers".to_string());
    }
    let project_root = inputs[0].ledger.project_root.clone();
    if inputs
        .iter()
        .any(|input| input.ledger.project_root != project_root)
    {
        return Err("merge refused: the ledgers belong to different projects".to_string());
    }

    let mut dropped = Dropped::default();
    let (findings, finding_bound) = merge_findings(&inputs, &mut dropped);
    let (facts, fact_bound, conflicts) = merge_facts(&inputs, &mut dropped);
    let curated = merge_curated(&inputs, &mut dropped);
    let any_bound = finding_bound.iter().chain(&fact_bound).any(|bound| *bound);
    let (curated_refs, sources) = rebind(&inputs, curated, any_bound, &mut dropped)?;
    let tasks: BTreeSet<&str> = inputs
        .iter()
        .filter_map(|input| input.ledger.session.task.as_deref())
        .collect();
    let distinct_tasks = tasks.len();
    let merged_from: Vec<String> = inputs.iter().map(|input| input.digest.clone()).collect();

    let mut ledger = HandoffLedgerV1 {
        schema_version: SCHEMA_VERSION,
        created_at: inputs
            .iter()
            .map(|input| input.ledger.created_at.clone())
            .max()
            .unwrap_or_default(),
        content_md5: String::new(),
        manifest_md5: manifest_md5(),
        project_root: project_root.clone(),
        agent_id: None,
        client_name: None,
        workflow: common(&inputs, |ledger| ledger.workflow.as_ref()),
        // Each input's snapshot describes its own session; none describes
        // the merged one.
        session_snapshot: String::new(),
        session: SessionExcerpt {
            id: format!("merge-{}", short_digest(&merged_from)),
            task: (distinct_tasks == 1)
                .then(|| tasks.first().map(|task| (*task).to_owned()))
                .flatten(),
            decisions: keep_last(
                union(&inputs, |ledger| &ledger.session.decisions),
                MAX_DECISIONS,
                &mut dropped.decisions,
            ),
            findings,
            next_steps: keep_first(
                union(&inputs, |ledger| &ledger.session.next_steps),
                MAX_NEXT_STEPS,
                &mut dropped.next_steps,
            ),
        },
        tool_calls: sum_tool_calls(&inputs),
        evidence_keys: inputs
            .iter()
            .flat_map(|input| input.ledger.evidence_keys.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        knowledge: KnowledgeExcerpt {
            project_hash: inputs
                .iter()
                .find_map(|input| input.ledger.knowledge.project_hash.clone()),
            facts,
        },
        curated_refs,
        active_overlays: merge_overlays(&inputs),
        merged_from,
        request_id: None,
        request_digest: None,
    };
    ledger.content_md5 = super::ledger_content_md5(&ledger);

    let path = persist_ledger(&ledger, sources, |kind, index| {
        let flags = match kind {
            PartKind::Finding => &finding_bound,
            PartKind::Fact => &fact_bound,
        };
        flags.get(index).copied().unwrap_or(false)
    })?;
    Ok(MergeOutcome {
        ledger,
        path,
        conflicts,
        distinct_tasks,
        dropped,
    })
}

/// Findings in canonical order, one per text; bound when any input that
/// carries the text recorded it as bound. The last ones are kept, as at
/// creation.
fn merge_findings(inputs: &[Input], dropped: &mut Dropped) -> (Vec<String>, Vec<bool>) {
    let mut order: Vec<String> = Vec::new();
    let mut bound: BTreeMap<String, bool> = BTreeMap::new();
    for input in inputs {
        for (index, finding) in input.ledger.session.findings.iter().enumerate() {
            let is_bound = input.bound(PartKind::Finding, index);
            if let Some(flag) = bound.get_mut(finding) {
                *flag |= is_bound;
            } else {
                order.push(finding.clone());
                bound.insert(finding.clone(), is_bound);
            }
        }
    }
    let kept = keep_last(order, MAX_LEDGER_FINDINGS, &mut dropped.findings);
    let flags = kept.iter().map(|finding| bound[finding]).collect();
    (kept, flags)
}

/// Facts in canonical order, one per (category, key, value) with the highest
/// confidence any input gave it; bound when any input recorded it as bound.
/// A (category, key) with several values keeps them all and is reported.
fn merge_facts(
    inputs: &[Input],
    dropped: &mut Dropped,
) -> (Vec<KnowledgeFactMini>, Vec<bool>, Vec<FactConflict>) {
    let mut facts: Vec<(KnowledgeFactMini, bool)> = Vec::new();
    for input in inputs {
        for (index, fact) in input.ledger.knowledge.facts.iter().enumerate() {
            let is_bound = input.bound(PartKind::Fact, index);
            let same = facts.iter_mut().find(|(kept, _)| {
                kept.category == fact.category && kept.key == fact.key && kept.value == fact.value
            });
            match same {
                Some((kept, flag)) => {
                    kept.confidence = kept.confidence.max(fact.confidence);
                    *flag |= is_bound;
                }
                None => facts.push((fact.clone(), is_bound)),
            }
        }
    }
    let mut values: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for (fact, _) in &facts {
        *values.entry((&fact.category, &fact.key)).or_insert(0) += 1;
    }
    let conflicts = values
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|((category, key), values)| FactConflict {
            category: category.to_owned(),
            key: key.to_owned(),
            values,
        })
        .collect();
    let kept = keep_first(facts, MAX_KNOWLEDGE_FACTS, &mut dropped.facts);
    let (facts, flags): (Vec<_>, Vec<_>) = kept.into_iter().unzip();
    (facts, flags, conflicts)
}

/// Curated excerpts, one per (path, content). The digest is taken of the
/// content itself, not of the stored `content_md5` field.
fn merge_curated(inputs: &[Input], dropped: &mut Dropped) -> Vec<CuratedRef> {
    let mut seen = BTreeSet::new();
    let refs = inputs
        .iter()
        .flat_map(|input| input.ledger.curated_refs.iter())
        .filter(|excerpt| {
            let digest = crate::core::hasher::hash_hex(excerpt.content.as_bytes());
            seen.insert((excerpt.path.clone(), digest))
        })
        .cloned()
        .collect();
    keep_first(refs, MAX_CURATED_REFS, &mut dropped.curated_refs)
}

fn merge_overlays(inputs: &[Input]) -> Vec<serde_json::Value> {
    let mut seen = BTreeSet::new();
    inputs
        .iter()
        .flat_map(|input| input.ledger.active_overlays.iter())
        .filter(|overlay| seen.insert(overlay.to_string()))
        .cloned()
        .collect()
}

fn sum_tool_calls(inputs: &[Input]) -> ToolCallsSummary {
    let mut sum = ToolCallsSummary::default();
    for calls in inputs.iter().map(|input| &input.ledger.tool_calls) {
        sum.total += calls.total;
        for (tool, count) in &calls.by_tool {
            *sum.by_tool.entry(tool.clone()).or_insert(0) += count;
        }
        for (mode, count) in &calls.by_ctx_read_mode {
            *sum.by_ctx_read_mode.entry(mode.clone()).or_insert(0) += count;
        }
    }
    sum
}

/// The value every input carries, when they all carry the same one.
fn common<T: Clone + serde::Serialize>(
    inputs: &[Input],
    value: impl Fn(&HandoffLedgerV1) -> Option<&T>,
) -> Option<T> {
    let first = value(&inputs.first()?.ledger)?;
    let first_json = serde_json::to_value(first).ok()?;
    inputs
        .iter()
        .all(|input| {
            value(&input.ledger)
                .and_then(|other| serde_json::to_value(other).ok())
                .is_some_and(|other| other == first_json)
        })
        .then(|| first.clone())
}

/// Texts of every input in canonical order, each once.
fn union(inputs: &[Input], texts: impl Fn(&HandoffLedgerV1) -> &Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    inputs
        .iter()
        .flat_map(|input| texts(&input.ledger).iter())
        .filter(|text| seen.insert(text.as_str()))
        .cloned()
        .collect()
}

fn keep_first<T>(mut items: Vec<T>, bound: usize, dropped: &mut usize) -> Vec<T> {
    *dropped = items.len().saturating_sub(bound);
    items.truncate(bound);
    items
}

fn keep_last<T>(mut items: Vec<T>, bound: usize, dropped: &mut usize) -> Vec<T> {
    let cut = items.len().saturating_sub(bound);
    *dropped = cut;
    items.drain(..cut);
    items
}

fn short_digest(inputs: &[String]) -> String {
    let joined = inputs.join("\n");
    crate::core::hasher::hash_hex(joined.as_bytes())
        .chars()
        .take(16)
        .collect()
}

/// The lineage of the merged ledger and the excerpts it keeps. Every source
/// is re-read through the admitted reader now, against one budget — the one
/// a read of the result re-reads it with — and bound only when it returns the
/// bytes the input was bound to; the re-reads are a check, not a delivery.
///
/// - an excerpt is kept with its file's source; one whose input binding names
///   no source for it (a Community-mode excerpt) is left out and counted;
/// - the sources of bound findings and facts come from the inputs that
///   recorded bound parts, and only when a bound part is kept: a lineage with
///   part sources but no recorded part would not agree with its ledger;
/// - under protection a source that no longer re-binds refuses the merge
///   before anything is written; without protection the excerpt is left out
///   and a part source is skipped (nothing is recorded as bound then).
fn rebind(
    inputs: &[Input],
    curated: Vec<CuratedRef>,
    any_bound: bool,
    dropped: &mut Dropped,
) -> Result<(Vec<CuratedRef>, Vec<SourceRef>), String> {
    let required = crate::core::context_admission::stored::source_authority_required();
    let refused = || Withheld::new(&[SOURCE_CHANGED]).message("handoff merge");
    let mut remaining = crate::core::limits::max_read_bytes();
    crate::core::archive::authority::without_capture(|| {
        let mut sources = Vec::new();
        let mut kept = Vec::new();
        for excerpt in curated {
            let named = inputs.iter().find_map(|input| {
                let (project, files) = input.files.as_ref()?;
                files
                    .iter()
                    .find(|(path, tool, _)| *path == excerpt.path && tool == CURATED_TOOL)
                    .map(|(path, tool, digest)| (project, path, tool, digest))
            });
            let Some((project, path, tool, digest)) = named else {
                dropped.unbound_curated += 1;
                continue;
            };
            match SourceRef::read_as_admitted_within(path, project, tool, digest, &mut remaining) {
                Some(source) => {
                    sources.push(source);
                    kept.push(excerpt);
                }
                None if required => return Err(refused()),
                None => dropped.unbound_curated += 1,
            }
        }
        if any_bound {
            for input in inputs.iter().filter(|input| !input.bound.is_empty()) {
                let Some((project, files)) = input.files.as_ref() else {
                    continue;
                };
                for (path, tool, digest) in files.iter().filter(|(_, tool, _)| tool != CURATED_TOOL)
                {
                    match SourceRef::read_as_admitted_within(
                        path,
                        project,
                        tool,
                        digest,
                        &mut remaining,
                    ) {
                        Some(source) => sources.push(source),
                        None if required => return Err(refused()),
                        None => {}
                    }
                }
            }
        }
        Ok((kept, sources))
    })
}
