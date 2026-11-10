// SPDX-License-Identifier: Apache-2.0
//! Freshness of a project's knowledge: which facts are not current truth
//! anymore, and the primitives a weekly clean-up runs (`lean-ctx knowledge
//! stale | verify | dedupe | outdated | conflicts --json`, contract
//! `local-read/1`).
//!
//! - Anchored facts are resolved against the checkout on disk (file, symbol,
//!   content digest) through [`GuardedResolver`]: only files this instance may
//!   read for knowledge are read, so a pulled fact's anchor never turns the
//!   checkout into an oracle. The live state is the one source of truth for
//!   anchors; the stored *to verify* mark (what `knowledge list`, AGENTS.md
//!   and Hive read) is reconciled with it whenever it is checked.
//! - [`mark_edited`] reconciles the facts anchored to a file when an agent
//!   edits it through this instance (cheap, no model): marked while the code
//!   does not hold, unmarked once a revert restores it. [`stale`] reconciles
//!   every anchored fact before it lists them.
//! - Ids and timestamps follow [`super::fact_ids`], the rules `knowledge
//!   list` uses: a current fact is `<category>/<key>` (a second one on the
//!   same key, and every superseded version, `…@<version>`).
//! - Every listing is deterministic: fixed order, ties broken by id; the only
//!   clock input is `outdated`'s cut-off, printed with the result.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;

use lean_ctx::core::knowledge::anchors::{self, AnchorState, GuardedResolver, MAX_ANCHORS};
use lean_ctx::core::knowledge::{
    FactOrigin, FactReviewReason, FactReviewState, JudgedPair, KnowledgeFact, ProjectKnowledge,
    fact_version_id_v1, string_similarity,
};

use super::fact_ids::{current_ids, rfc3339 as ts, version_id};

pub const CONTRACT: &str = "local-read/1";
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;
pub const DEFAULT_DEDUPE_THRESHOLD: f32 = 0.9;
pub const MIN_DEDUPE_THRESHOLD: f32 = 0.5;
pub const DEFAULT_OUTDATED_DAYS: u32 = 90;
/// About a hundred years: any older cut-off lists every fact anyway.
pub const MAX_OUTDATED_DAYS: u32 = 36_500;

/// A refusal with its `local-read/1` error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessError {
    pub code: &'static str,
    pub message: String,
}

impl FreshnessError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// `<category>/<key>`: how a judged pair names a fact.
fn pair_name(fact: &KnowledgeFact) -> String {
    format!("{}/{}", fact.category, fact.key)
}

/// The id of every fact of `facts`, by position: current facts as
/// [`current_ids`] names them, superseded versions by [`version_id`].
pub fn ids_of(facts: &[KnowledgeFact]) -> Vec<String> {
    let current: Vec<usize> = (0..facts.len())
        .filter(|&i| facts[i].is_current())
        .collect();
    let refs: Vec<&KnowledgeFact> = current.iter().map(|&i| &facts[i]).collect();
    let mut ids: Vec<String> = facts.iter().map(version_id).collect();
    for (i, id) in current.into_iter().zip(current_ids(&refs)) {
        ids[i] = id;
    }
    ids
}

#[derive(Debug, Clone, Serialize)]
pub struct AnchorRef {
    pub path: String,
    pub symbol: Option<String>,
}

/// The fact reference every listing carries (INTERFACE.md "Fact reference").
#[derive(Debug, Clone, Serialize)]
pub struct FactRef {
    pub id: String,
    pub category: String,
    pub key: String,
    pub text: String,
    pub confidence: f32,
    pub anchors: Vec<AnchorRef>,
    pub review: Option<&'static str>,
    pub last_confirmed: String,
    pub updated: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    pub code: &'static str,
    pub path: Option<String>,
    pub symbol: Option<String>,
    pub moved_to: Option<String>,
}

/// Where a fact stands now: its review value and the reasons for it.
struct Assessment {
    review: Option<&'static str>,
    reasons: Vec<Reason>,
}

fn reason_code(reason: FactReviewReason) -> &'static str {
    match reason {
        FactReviewReason::AnchorChanged => "anchor_changed",
        FactReviewReason::AnchorMoved => "anchor_moved",
        FactReviewReason::AnchorMissing => "anchor_missing",
        FactReviewReason::Contradiction => "contradiction",
        FactReviewReason::SourceRemoved => "source_removed",
    }
}

const fn is_anchor_reason(reason: FactReviewReason) -> bool {
    matches!(
        reason,
        FactReviewReason::AnchorChanged
            | FactReviewReason::AnchorMoved
            | FactReviewReason::AnchorMissing
    )
}

fn contradicted(fact: &KnowledgeFact) -> bool {
    fact.review
        .as_ref()
        .is_some_and(|r| r.state == FactReviewState::Contradicted)
}

/// The worst live anchor reason of `fact` over all its anchors: gone, then
/// moved, then changed; `None` when every anchor holds.
fn live_anchor_reason(
    fact: &KnowledgeFact,
    guard: &mut GuardedResolver<'_>,
) -> Option<FactReviewReason> {
    let reasons: Vec<FactReviewReason> = fact
        .anchors
        .iter()
        .filter_map(|a| guard.resolve(a))
        .filter_map(|state| FactReviewReason::of_anchor(&state))
        .collect();
    [
        FactReviewReason::AnchorMissing,
        FactReviewReason::AnchorMoved,
        FactReviewReason::AnchorChanged,
    ]
    .into_iter()
    .find(|r| reasons.contains(r))
}

/// Bring the stored anchor mark of `fact` in line with `live` (the one
/// source of truth for anchors): mark it to verify while its code does not
/// hold, clear the mark once it holds again (an edit that was reverted).
/// A contradiction or source mark needs a person and is left alone.
/// Returns whether the stored review changed.
fn reconcile(fact: &mut KnowledgeFact, live: Option<FactReviewReason>, now: DateTime<Utc>) -> bool {
    if fact
        .review
        .as_ref()
        .is_some_and(|r| !is_anchor_reason(r.reason))
    {
        return false;
    }
    let before = fact.review.clone();
    match live {
        Some(reason) => fact.mark_review(FactReviewState::ToVerify, reason, now),
        None => fact.clear_review(),
    }
    fact.review != before
}

/// Reconcile every current anchored fact `wanted` selects, under the store
/// lock; returns the store as written and how many marks changed. Nothing
/// is written when every stored mark already matches the live state.
fn reconcile_store(
    project_root: &str,
    wanted: impl Fn(&KnowledgeFact) -> bool,
) -> Result<(ProjectKnowledge, usize), String> {
    let root = Path::new(project_root);
    let selected = |f: &KnowledgeFact| f.is_current() && !f.anchors.is_empty() && wanted(f);
    let knowledge = load(project_root);
    let mut guard = GuardedResolver::local(root);
    let now = Utc::now();
    let differs = knowledge.facts.iter().filter(|f| selected(f)).any(|f| {
        let mut probe = f.clone();
        reconcile(&mut probe, live_anchor_reason(f, &mut guard), now)
    });
    if !differs {
        return Ok((knowledge, 0));
    }
    let (written, changed) = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let mut guard = GuardedResolver::local(root);
        let mut changed = 0;
        for fact in knowledge.facts.iter_mut().filter(|f| selected(f)) {
            let live = live_anchor_reason(fact, &mut guard);
            changed += usize::from(reconcile(fact, live, now));
        }
        changed
    })?;
    Ok((written, changed))
}

fn assess(fact: &KnowledgeFact, guard: &mut GuardedResolver<'_>) -> Assessment {
    let mut reasons = Vec::new();
    let (mut gone, mut to_verify) = (false, false);
    for anchor in &fact.anchors {
        let state = guard.resolve(anchor);
        let Some(reason) = state.as_ref().and_then(FactReviewReason::of_anchor) else {
            continue;
        };
        match reason {
            FactReviewReason::AnchorMissing => gone = true,
            _ => to_verify = true,
        }
        reasons.push(Reason {
            code: reason_code(reason),
            path: Some(anchor.path.clone()),
            symbol: anchor.symbol.clone(),
            moved_to: match state {
                Some(AnchorState::Moved { path }) => Some(path),
                _ => None,
            },
        });
    }
    if let Some(review) = &fact.review
        && !is_anchor_reason(review.reason)
    {
        if review.reason == FactReviewReason::SourceRemoved {
            to_verify = true;
        }
        reasons.push(Reason {
            code: reason_code(review.reason),
            path: None,
            symbol: None,
            moved_to: None,
        });
    }
    let review = if gone {
        Some("gone")
    } else if to_verify {
        Some("to_verify")
    } else if contradicted(fact) {
        Some("contradicted")
    } else {
        None
    };
    Assessment { review, reasons }
}

fn fact_ref(fact: &KnowledgeFact, id: &str, review: Option<&'static str>) -> FactRef {
    FactRef {
        id: id.to_owned(),
        category: fact.category.clone(),
        key: fact.key.clone(),
        text: fact.value.clone(),
        confidence: fact.confidence,
        anchors: fact
            .anchors
            .iter()
            .map(|a| AnchorRef {
                path: a.path.clone(),
                symbol: a.symbol.clone(),
            })
            .collect(),
        review,
        last_confirmed: ts(fact.last_confirmed),
        updated: ts(fact.created_at.max(fact.last_confirmed)),
    }
}

fn assessed_ref(fact: &KnowledgeFact, id: &str, guard: &mut GuardedResolver<'_>) -> FactRef {
    let review = assess(fact, guard).review;
    fact_ref(fact, id, review)
}

fn load(project_root: &str) -> ProjectKnowledge {
    ProjectKnowledge::load(project_root).unwrap_or_else(|| ProjectKnowledge::new(project_root))
}

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

// ---------------------------------------------------------------- stale

#[derive(Debug, Serialize)]
pub struct StaleFact {
    pub fact: FactRef,
    pub since: Option<String>,
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Serialize)]
pub struct StaleReport {
    pub contract: &'static str,
    pub kind: &'static str,
    pub project_root: String,
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
    pub facts: Vec<StaleFact>,
}

/// Current facts that are not current truth: gone, to verify, contradicted.
///
/// The live anchor check is the one source of truth: before listing, a
/// stored anchor mark that disagrees with it is written back (set for code
/// that changed outside this instance, cleared for code that was restored),
/// so `knowledge list`, AGENTS.md and Hive show what this listing shows.
/// When the store cannot be written the listing still reports the live
/// state.
pub fn stale(project_root: &str, limit: Option<usize>) -> StaleReport {
    let knowledge = reconcile_store(project_root, |_| true)
        .map_or_else(|_| load(project_root), |(knowledge, _)| knowledge);
    let ids = ids_of(&knowledge.facts);
    let mut guard = GuardedResolver::local(Path::new(project_root));
    let mut found: Vec<(u8, StaleFact)> = knowledge
        .facts
        .iter()
        .zip(&ids)
        .filter(|(f, _)| f.is_current())
        .filter_map(|(fact, id)| {
            let assessment = assess(fact, &mut guard);
            let review = assessment.review?;
            let rank = match review {
                "gone" => 0,
                "to_verify" => 1,
                _ => 2,
            };
            Some((
                rank,
                StaleFact {
                    since: fact.review.as_ref().map(|r| ts(r.since)),
                    fact: fact_ref(fact, id, Some(review)),
                    reasons: assessment.reasons,
                },
            ))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.fact.id.cmp(&b.1.fact.id)));
    let total = found.len();
    found.truncate(clamp_limit(limit));
    StaleReport {
        contract: CONTRACT,
        kind: "knowledge.stale",
        project_root: project_root.to_owned(),
        total,
        returned: found.len(),
        truncated: found.len() < total,
        facts: found.into_iter().map(|(_, f)| f).collect(),
    }
}

/// Reconcile the current facts anchored to `path` (absolute or
/// project-relative) after an agent's edit through this instance: *to
/// verify* while their anchored code no longer holds, unmarked again once
/// it does (the edit was reverted). Returns how many marks changed. A
/// contradicted fact keeps its mark (it needs a person, not a re-check).
pub fn mark_edited(project_root: &str, path: &str) -> usize {
    let root = Path::new(project_root);
    let Some(relative) = anchors::anchor_path(root, path) else {
        return 0;
    };
    reconcile_store(project_root, |fact| {
        fact.anchors.iter().any(|a| a.path == relative)
    })
    .map_or(0, |(_, changed)| changed)
}

// ---------------------------------------------------------------- verify

#[derive(Debug, Serialize)]
pub struct Reanchored {
    pub changed: usize,
    pub moved: usize,
    pub unchanged: usize,
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub contract: &'static str,
    pub kind: &'static str,
    pub project_root: String,
    pub fact: FactRef,
    pub reanchored: Reanchored,
    pub cleared: Option<&'static str>,
}

fn find_current(facts: &[KnowledgeFact], id: &str) -> Result<usize, FreshnessError> {
    ids_of(facts)
        .iter()
        .position(|candidate| candidate == id)
        .filter(|&i| facts[i].is_current())
        .ok_or_else(|| FreshnessError::new("not_found", format!("no current fact {id}")))
}

/// Re-capture every anchor of `fact` where its code is now, reading only
/// admitted files; a gone anchor refuses the whole verify.
fn reanchor(root: &Path, fact: &mut KnowledgeFact) -> Result<Reanchored, FreshnessError> {
    let mut guard = GuardedResolver::local(root);
    let mut counts = Reanchored {
        changed: 0,
        moved: 0,
        unchanged: 0,
    };
    let mut renewed = Vec::with_capacity(fact.anchors.len());
    for anchor in &fact.anchors {
        let named = match &anchor.symbol {
            Some(symbol) => format!("{}::{symbol}", anchor.path),
            None => anchor.path.clone(),
        };
        let gone = || {
            FreshnessError::new(
                "anchor_gone",
                format!("{named} is gone: rewrite or remove the fact instead"),
            )
        };
        let capture =
            |path: &str| anchors::capture(root, path, anchor.symbol.as_deref()).map_err(|_| gone());
        let current = match guard.resolve(anchor) {
            Some(AnchorState::Current) => {
                counts.unchanged += 1;
                anchor.clone()
            }
            Some(AnchorState::Changed) => {
                counts.changed += 1;
                capture(&anchor.path)?
            }
            Some(AnchorState::Moved { path }) => {
                counts.moved += 1;
                capture(&path)?
            }
            Some(AnchorState::Missing) | None => return Err(gone()),
        };
        renewed.push(current);
    }
    fact.anchors = anchors::canonical(renewed);
    Ok(counts)
}

/// "Checked, still true": re-anchor, clear an anchor or source mark, confirm.
pub fn verify(project_root: &str, id: &str) -> Result<VerifyReport, FreshnessError> {
    let root = Path::new(project_root);
    let outcome = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let index = find_current(&knowledge.facts, id)?;
        let fact = &mut knowledge.facts[index];
        let reanchored = reanchor(root, fact)?;
        let cleared = fact
            .review
            .as_ref()
            .filter(|r| r.state == FactReviewState::ToVerify)
            .map(|r| reason_code(r.reason));
        if cleared.is_some() {
            fact.clear_review();
        }
        fact.last_confirmed = Utc::now();
        fact.confirmation_count = fact.confirmation_count.saturating_add(1);
        Ok((fact.clone(), reanchored, cleared))
    });
    let (fact, reanchored, cleared) = match outcome {
        Ok((_, result)) => result?,
        Err(error) => return Err(FreshnessError::new("unavailable", error)),
    };
    let mut guard = GuardedResolver::local(root);
    Ok(VerifyReport {
        contract: CONTRACT,
        kind: "knowledge.verify",
        project_root: project_root.to_owned(),
        fact: assessed_ref(&fact, id, &mut guard),
        reanchored,
        cleared,
    })
}

// ---------------------------------------------------------------- dedupe

#[derive(Debug, Serialize)]
pub struct DedupeMember {
    pub fact: FactRef,
    #[serde(rename = "match")]
    pub kind: &'static str,
    pub similarity: f64,
}

#[derive(Debug, Serialize)]
pub struct DedupeGroup {
    pub category: String,
    pub keep: FactRef,
    pub merge: Vec<DedupeMember>,
}

#[derive(Debug, Serialize)]
pub struct DedupeReport {
    pub contract: &'static str,
    pub kind: &'static str,
    pub project_root: String,
    pub applied: bool,
    pub threshold: f64,
    pub groups: Vec<DedupeGroup>,
    pub merged: usize,
}

struct GroupPlan {
    keep: usize,
    merge: Vec<(usize, &'static str, f32)>,
}

fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn round2(value: f32) -> f64 {
    (f64::from(value) * 100.0).round() / 100.0
}

/// Words that flip or bound a claim. Two texts that differ in one of them,
/// or in a number, say different things however similar they read
/// ("never retry" / "always retry", "3 retries" / "5 retries").
const POLARITY_WORDS: &[&str] = &[
    "not",
    "no",
    "never",
    "always",
    "none",
    "nothing",
    "nobody",
    "neither",
    "nor",
    "cannot",
    "can't",
    "don't",
    "doesn't",
    "isn't",
    "aren't",
    "wasn't",
    "won't",
    "shouldn't",
    "mustn't",
    "without",
    "only",
    "all",
    "every",
    "any",
    "must",
    "may",
    "should",
    "true",
    "false",
    "yes",
    "on",
    "off",
    "enable",
    "enabled",
    "disable",
    "disabled",
    "allow",
    "allowed",
    "deny",
    "denied",
    "forbid",
    "forbidden",
    "required",
    "optional",
];

/// Whether two normalized texts differ in a polarity word or a number: a
/// near match between them is never a duplicate.
fn differ_in_meaning(a: &str, b: &str) -> bool {
    let words = |text: &str| -> BTreeSet<String> {
        text.replace('\u{2019}', "'")
            .split(|c: char| !(c.is_alphanumeric() || c == '\''))
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (left, right) = (words(a), words(b));
    left.symmetric_difference(&right)
        .any(|w| POLARITY_WORDS.contains(&w.as_str()) || w.chars().any(|c| c.is_ascii_digit()))
}

/// Pairs a person or agent judged apart (`contradicts`, `unrelated`): never
/// merged, whatever their wording.
fn judged_apart(pairs: &[JudgedPair]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .filter(|p| matches!(p.verdict.as_str(), "contradicts" | "unrelated"))
        .flat_map(|p| {
            [
                (p.key_a.clone(), p.key_b.clone()),
                (p.key_b.clone(), p.key_a.clone()),
            ]
        })
        .collect()
}

/// Duplicate groups among the current own facts of one category each.
/// Survivor order: most confirmations, longest text, oldest, smallest id.
fn plan_dedupe(
    facts: &[KnowledgeFact],
    ids: &[String],
    pairs: &[JudgedPair],
    threshold: f32,
) -> Vec<GroupPlan> {
    let apart = judged_apart(pairs);
    let mut by_category: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, fact) in facts.iter().enumerate() {
        // This instance's own facts only (`unverified`: written before
        // origins existed); provider and derived facts belong to their source.
        let own = matches!(fact.origin, FactOrigin::Local | FactOrigin::Unverified);
        if fact.is_current() && own && !contradicted(fact) {
            by_category
                .entry(fact.category.as_str())
                .or_default()
                .push(i);
        }
    }
    let mut plans = Vec::new();
    for mut members in by_category.into_values() {
        members.sort_by(|&a, &b| {
            let (fa, fb) = (&facts[a], &facts[b]);
            fb.confirmation_count
                .cmp(&fa.confirmation_count)
                .then_with(|| fb.value.len().cmp(&fa.value.len()))
                .then_with(|| fa.created_at.cmp(&fb.created_at))
                .then_with(|| ids[a].cmp(&ids[b]))
        });
        let texts: Vec<String> = members
            .iter()
            .map(|&i| normalized(&facts[i].value))
            .collect();
        let names: Vec<String> = members.iter().map(|&i| pair_name(&facts[i])).collect();
        let mut taken = vec![false; members.len()];
        for a in 0..members.len() {
            if taken[a] {
                continue;
            }
            let mut merge = Vec::new();
            for b in a + 1..members.len() {
                if taken[b] || apart.contains(&(names[a].clone(), names[b].clone())) {
                    continue;
                }
                let found = if texts[a] == texts[b] {
                    Some(("exact", 1.0))
                } else if differ_in_meaning(&texts[a], &texts[b]) {
                    None
                } else {
                    let similarity = string_similarity(&texts[a], &texts[b]);
                    (similarity >= threshold).then_some(("near", similarity))
                };
                if let Some((kind, similarity)) = found {
                    taken[b] = true;
                    merge.push((members[b], kind, similarity));
                }
            }
            if !merge.is_empty() {
                merge.sort_by(|x, y| ids[x.0].cmp(&ids[y.0]));
                plans.push(GroupPlan {
                    keep: members[a],
                    merge,
                });
            }
        }
    }
    plans.sort_by(|x, y| ids[x.keep].cmp(&ids[y.keep]));
    plans
}

/// Merge each plan into its survivor: union of anchors, confirmations
/// added; the others archived as superseded by it, so every merged text
/// stays in the history (`outdated`, `knowledge list --state superseded`).
/// The survivor's text is never rewritten: a merge loses no wording.
fn apply_dedupe(knowledge: &mut ProjectKnowledge, plans: &[GroupPlan]) -> usize {
    let now = Utc::now();
    let mut merged = 0;
    for plan in plans {
        let keep_name = pair_name(&knowledge.facts[plan.keep]);
        let mut anchor_union = knowledge.facts[plan.keep].anchors.clone();
        let mut confirmations = 0u32;
        let mut last_confirmed = knowledge.facts[plan.keep].last_confirmed;
        for &(index, _, _) in &plan.merge {
            let fact = &mut knowledge.facts[index];
            fact.valid_until = Some(now);
            fact.valid_from = fact.valid_from.or(Some(fact.created_at));
            anchor_union.extend(fact.anchors.iter().cloned());
            confirmations = confirmations.saturating_add(fact.confirmation_count.max(1));
            last_confirmed = last_confirmed.max(fact.last_confirmed);
            let merged_name = pair_name(fact);
            knowledge.judged_pairs.push(JudgedPair {
                key_a: keep_name.clone(),
                key_b: merged_name,
                verdict: "supersedes".to_owned(),
                judged_at: now,
            });
            merged += 1;
        }
        let keep = &mut knowledge.facts[plan.keep];
        let mut anchors = anchors::canonical(anchor_union);
        anchors.truncate(MAX_ANCHORS);
        keep.anchors = anchors;
        keep.confirmation_count = keep.confirmation_count.saturating_add(confirmations);
        keep.last_confirmed = last_confirmed;
    }
    if merged > 0 {
        knowledge.rebuild_index();
    }
    merged
}

fn dedupe_groups(
    facts: &[KnowledgeFact],
    ids: &[String],
    plans: &[GroupPlan],
    guard: &mut GuardedResolver<'_>,
) -> Vec<DedupeGroup> {
    plans
        .iter()
        .map(|plan| DedupeGroup {
            category: facts[plan.keep].category.clone(),
            keep: assessed_ref(&facts[plan.keep], &ids[plan.keep], guard),
            merge: plan
                .merge
                .iter()
                .map(|&(index, kind, similarity)| DedupeMember {
                    fact: assessed_ref(&facts[index], &ids[index], guard),
                    kind,
                    similarity: round2(similarity),
                })
                .collect(),
        })
        .collect()
}

/// Exact and near duplicates; merged only with `apply`. Ids name the facts
/// as they were before the merge.
pub fn dedupe(
    project_root: &str,
    threshold: Option<f32>,
    apply: bool,
) -> Result<DedupeReport, FreshnessError> {
    let threshold = threshold.unwrap_or(DEFAULT_DEDUPE_THRESHOLD);
    if !(MIN_DEDUPE_THRESHOLD..=1.0).contains(&threshold) {
        return Err(FreshnessError::new(
            "invalid_argument",
            format!("threshold must be in [{MIN_DEDUPE_THRESHOLD}, 1.0]"),
        ));
    }
    let root = Path::new(project_root);
    let (groups, merged) = if apply {
        let outcome = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
            let ids = ids_of(&knowledge.facts);
            let plans = plan_dedupe(&knowledge.facts, &ids, &knowledge.judged_pairs, threshold);
            let before = knowledge.facts.clone();
            let merged = apply_dedupe(knowledge, &plans);
            let mut guard = GuardedResolver::local(root);
            let mut groups = dedupe_groups(&before, &ids, &plans, &mut guard);
            // The survivor as it is now.
            for (group, plan) in groups.iter_mut().zip(&plans) {
                group.keep = assessed_ref(&knowledge.facts[plan.keep], &ids[plan.keep], &mut guard);
            }
            (groups, merged)
        });
        match outcome {
            Ok((_, result)) => result,
            Err(error) => return Err(FreshnessError::new("unavailable", error)),
        }
    } else {
        let knowledge = load(project_root);
        let ids = ids_of(&knowledge.facts);
        let plans = plan_dedupe(&knowledge.facts, &ids, &knowledge.judged_pairs, threshold);
        let mut guard = GuardedResolver::local(root);
        (dedupe_groups(&knowledge.facts, &ids, &plans, &mut guard), 0)
    };
    Ok(DedupeReport {
        contract: CONTRACT,
        kind: "knowledge.dedupe",
        project_root: project_root.to_owned(),
        applied: apply,
        threshold: round2(threshold),
        groups,
        merged,
    })
}

// ---------------------------------------------------------------- outdated

#[derive(Debug, Serialize)]
pub struct OutdatedFact {
    pub fact: FactRef,
    pub reason: &'static str,
    pub superseded_by: Option<String>,
    pub valid_until: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OutdatedReport {
    pub contract: &'static str,
    pub kind: &'static str,
    pub project_root: String,
    pub days: u32,
    pub cutoff: String,
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
    pub facts: Vec<OutdatedFact>,
}

/// Midnight UTC of (`now`'s day − `days`): the same all day long.
fn cutoff_of(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    let day = now.date_naive() - chrono::Duration::days(i64::from(days));
    DateTime::from_naive_utc_and_offset(day.and_hms_opt(0, 0, 0).unwrap_or_default(), Utc)
}

/// The position of the current fact that replaced `fact`: a recorded
/// `supersedes` verdict, a successor naming its version, or the current
/// fact under the same name.
fn successor_of(knowledge: &ProjectKnowledge, fact: &KnowledgeFact) -> Option<usize> {
    let name = pair_name(fact);
    let current_named = |wanted: &str| {
        knowledge
            .facts
            .iter()
            .position(|f| f.is_current() && pair_name(f) == wanted)
    };
    if let Some(index) = knowledge
        .judged_pairs
        .iter()
        .filter(|p| p.verdict == "supersedes" && p.key_b == name)
        .filter_map(|p| current_named(&p.key_a).map(|i| (p.judged_at, i)))
        .max_by_key(|(at, _)| *at)
        .map(|(_, i)| i)
    {
        return Some(index);
    }
    let version = fact_version_id_v1(fact);
    knowledge
        .facts
        .iter()
        .position(|f| f.is_current() && f.supersedes.as_deref() == Some(version.as_str()))
        .or_else(|| current_named(&name))
}

/// Superseded facts, and current facts nobody confirmed since the cut-off.
/// `days` above [`MAX_OUTDATED_DAYS`] is `invalid_argument` (a cut-off
/// before year 0 would not be a date).
pub fn outdated(
    project_root: &str,
    days: Option<u32>,
    limit: Option<usize>,
    now: DateTime<Utc>,
) -> Result<OutdatedReport, FreshnessError> {
    let days = days.unwrap_or(DEFAULT_OUTDATED_DAYS);
    if days > MAX_OUTDATED_DAYS {
        return Err(FreshnessError::new(
            "invalid_argument",
            format!("--days must be 0-{MAX_OUTDATED_DAYS}"),
        ));
    }
    let cutoff = cutoff_of(now, days);
    let knowledge = load(project_root);
    let ids = ids_of(&knowledge.facts);
    let mut guard = GuardedResolver::local(Path::new(project_root));
    let mut found: Vec<(u8, OutdatedFact)> = Vec::new();
    for (fact, id) in knowledge.facts.iter().zip(&ids) {
        let entry = if let Some(until) = fact.valid_until {
            OutdatedFact {
                fact: fact_ref(fact, id, None),
                reason: "superseded",
                superseded_by: successor_of(&knowledge, fact).map(|i| ids[i].clone()),
                valid_until: Some(ts(until)),
            }
        } else if fact.last_confirmed < cutoff {
            OutdatedFact {
                fact: assessed_ref(fact, id, &mut guard),
                reason: "unverified",
                superseded_by: None,
                valid_until: None,
            }
        } else {
            continue;
        };
        found.push((u8::from(entry.reason == "unverified"), entry));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.fact.id.cmp(&b.1.fact.id)));
    let total = found.len();
    found.truncate(clamp_limit(limit));
    Ok(OutdatedReport {
        contract: CONTRACT,
        kind: "knowledge.outdated",
        project_root: project_root.to_owned(),
        days,
        cutoff: ts(cutoff),
        total,
        returned: found.len(),
        truncated: found.len() < total,
        facts: found.into_iter().map(|(_, f)| f).collect(),
    })
}

// ---------------------------------------------------------------- conflicts

#[derive(Debug, Serialize)]
pub struct Conflict {
    pub a: FactRef,
    pub b: FactRef,
    pub source: &'static str,
    pub judged_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConflictsReport {
    pub contract: &'static str,
    pub kind: &'static str,
    pub project_root: String,
    pub conflicts: Vec<Conflict>,
    pub unpaired: Vec<FactRef>,
}

/// Contradictions to show the person: judged pairs that are both still
/// current, two current facts under one name with different text, and
/// contradicted facts whose partner is gone.
pub fn conflicts(project_root: &str) -> ConflictsReport {
    let knowledge = load(project_root);
    let facts = &knowledge.facts;
    let ids = ids_of(facts);
    let mut guard = GuardedResolver::local(Path::new(project_root));
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, fact) in facts.iter().enumerate().filter(|(_, f)| f.is_current()) {
        by_name.entry(pair_name(fact)).or_default().push(i);
    }
    let mut pairs: BTreeMap<(usize, usize), (&'static str, Option<DateTime<Utc>>)> =
        BTreeMap::new();
    for pair in knowledge
        .judged_pairs
        .iter()
        .filter(|p| p.verdict == "contradicts" && p.key_a != p.key_b)
    {
        let (Some([a]), Some([b])) = (
            by_name.get(&pair.key_a).map(Vec::as_slice),
            by_name.get(&pair.key_b).map(Vec::as_slice),
        ) else {
            continue;
        };
        let key = if ids[*a] < ids[*b] {
            (*a, *b)
        } else {
            (*b, *a)
        };
        let slot = pairs.entry(key).or_insert(("judged", Some(pair.judged_at)));
        slot.1 = slot.1.max(Some(pair.judged_at));
    }
    for members in by_name.values() {
        let mut members = members.clone();
        members.sort_by_key(|&i| facts[i].created_at);
        if let [first, second, ..] = members.as_slice()
            && normalized(&facts[*first].value) != normalized(&facts[*second].value)
        {
            let key = if ids[*first] < ids[*second] {
                (*first, *second)
            } else {
                (*second, *first)
            };
            pairs.entry(key).or_insert(("same_key", None));
        }
    }
    let paired: BTreeSet<usize> = pairs.keys().flat_map(|&(a, b)| [a, b]).collect();
    let mut out: Vec<Conflict> = pairs
        .into_iter()
        .map(|((a, b), (source, at))| Conflict {
            a: assessed_ref(&facts[a], &ids[a], &mut guard),
            b: assessed_ref(&facts[b], &ids[b], &mut guard),
            source,
            judged_at: at.map(ts),
        })
        .collect();
    out.sort_by(|x, y| x.a.id.cmp(&y.a.id).then_with(|| x.b.id.cmp(&y.b.id)));
    let mut unpaired: Vec<FactRef> = facts
        .iter()
        .enumerate()
        .filter(|(i, f)| f.is_current() && contradicted(f) && !paired.contains(i))
        .map(|(i, f)| assessed_ref(f, &ids[i], &mut guard))
        .collect();
    unpaired.sort_by(|x, y| x.id.cmp(&y.id));
    ConflictsReport {
        contract: CONTRACT,
        kind: "knowledge.conflicts",
        project_root: project_root.to_owned(),
        conflicts: out,
        unpaired,
    }
}

/// The line recall/search end with when they showed a fact that is not
/// current truth (stable text: a state-triggered suffix).
pub const NOT_TRUTH_NOTE: &str = "note: facts tagged [to verify]/[contradicted] are not current truth; check the code, then ctx_knowledge action=repin (or lean-ctx knowledge verify <id>).";

#[cfg(test)]
#[path = "freshness_tests.rs"]
mod tests;
