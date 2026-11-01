// SPDX-License-Identifier: Apache-2.0
//! Fact states (Hive plan v3 J2; C0 1.9 section 3).
//!
//! After every sync the engine checks the facts of each project's released
//! base revision against its own checkout and tells Hive which ones are in
//! doubt (`POST /v1/vaults/{vault_id}/facts/states`):
//!
//! - `to_verify`: an anchor no longer matches (`anchor_changed`), its symbol
//!   moved to another file (`anchor_moved`, with where it is now), or the
//!   anchored file or symbol is gone (`anchor_missing`);
//! - `contradicted`: an agent judged the fact to contradict another one
//!   (`ctx_knowledge judge … contradicts`) and nobody resolved it yet.
//!   Hive turns a contradiction nobody fixes within a day into a task.
//!
//! A report replaces this engine's earlier one for the revision, so it is
//! sent only when it changed. A sealed project reports counts only: which
//! facts are in doubt, and why, stays on the engine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::learn::{fact_id, knowledge_category};
use super::{BaseRevision, base_dir};
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::knowledge::screening::Screen;
use crate::core::hive::state::write_private;
use crate::core::hive::vault::VaultRecord;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::knowledge::anchors::{self, AnchorState};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::facts::{
    FACT_STATE_REPORT_SCHEMA, FactStateAckV1, FactStateCountsV1, FactStateKind, FactStateReason,
    FactStateReportV1, FactStateV1, MAX_CONTRADICTED_BY,
};

const SENT_FILE: &str = "fact-states.json";

/// The report last accepted by Hive for a project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Sent {
    revision: u64,
    #[serde(default)]
    states: Vec<FactStateV1>,
    #[serde(default)]
    counts: Option<FactStateCountsV1>,
}

/// One fact's doubt, without its `since`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Doubt {
    fact: String,
    reasons: BTreeSet<FactStateReason>,
    moved_to: Option<lean_ctx_protocol::hive::anchor::CodeAnchorV1>,
    contradicted_by: BTreeSet<String>,
}

impl Doubt {
    fn kind(&self) -> FactStateKind {
        if self.reasons.contains(&FactStateReason::Contradiction) {
            FactStateKind::Contradicted
        } else {
            FactStateKind::ToVerify
        }
    }
}

fn stamp(t: DateTime<Utc>) -> UtcTimestamp {
    UtcTimestamp::new(t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .expect("chrono formats a canonical UTC timestamp")
}

/// The pairs whose latest verdict is `contradicts` and whose facts are both
/// current, as Hive fact ids: `fact → facts that contradict it`.
pub(super) fn contradictions(
    knowledge: &ProjectKnowledge,
    category: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let hive_id = |spec: &str| {
        knowledge
            .facts
            .iter()
            .find(|f| f.is_current() && spec == format!("{}/{}", f.category, f.key))
            .map(|f| {
                if f.category == category {
                    f.key.clone()
                } else {
                    fact_id(&f.category, &f.key, f.value.trim())
                }
            })
    };
    let mut latest: BTreeMap<(String, String), &str> = BTreeMap::new();
    let mut pairs: Vec<_> = knowledge.judged_pairs.iter().collect();
    pairs.sort_by_key(|p| p.judged_at);
    for pair in pairs {
        let key = if pair.key_a <= pair.key_b {
            (pair.key_a.clone(), pair.key_b.clone())
        } else {
            (pair.key_b.clone(), pair.key_a.clone())
        };
        latest.insert(key, pair.verdict.as_str());
    }
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ((a, b), verdict) in latest {
        if verdict != "contradicts" {
            continue;
        }
        if let (Some(a), Some(b)) = (hive_id(&a), hive_id(&b))
            && a != b
        {
            out.entry(a.clone()).or_default().insert(b.clone());
            out.entry(b).or_default().insert(a);
        }
    }
    out
}

/// The doubts about `base`'s facts in the checkout at `root`. Where an
/// anchor moved to is outbound content: a new path or symbol the `screen`
/// refuses stays here, and the fact is reported as changed instead.
fn doubts(
    root: &Path,
    record: &VaultRecord,
    base: &BaseRevision,
    screen: &mut Screen,
) -> Vec<Doubt> {
    let category = knowledge_category(&record.ns, &record.name);
    let contradicted = ProjectKnowledge::load(&root.to_string_lossy())
        .map(|k| contradictions(&k, &category))
        .unwrap_or_default();
    // Anchors come from other engines' facts: only tracked, admitted files
    // are read, so a crafted anchor cannot probe the checkout.
    let mut guard = anchors::GuardedResolver::for_report(root);
    let mut out = Vec::new();
    for fact in &base.document.facts {
        let mut doubt = Doubt {
            fact: fact.id.clone(),
            reasons: BTreeSet::new(),
            moved_to: None,
            contradicted_by: BTreeSet::new(),
        };
        for anchor in &fact.anchors {
            let Some(state) = guard.resolve(anchor) else {
                continue;
            };
            let moved = match state {
                AnchorState::Current => continue,
                AnchorState::Changed => {
                    doubt.reasons.insert(FactStateReason::AnchorChanged);
                    continue;
                }
                AnchorState::Missing => None,
                AnchorState::Moved { path } => {
                    match anchors::capture(root, &path, anchor.symbol.as_deref()).ok() {
                        Some(to)
                            if screen.check(&to.path).is_err()
                                || to
                                    .symbol
                                    .as_deref()
                                    .is_some_and(|symbol| screen.check(symbol).is_err()) =>
                        {
                            doubt.reasons.insert(FactStateReason::AnchorChanged);
                            continue;
                        }
                        other => other,
                    }
                }
            };
            match moved {
                Some(to) if doubt.moved_to.is_none() => {
                    doubt.reasons.insert(FactStateReason::AnchorMoved);
                    doubt.moved_to = Some(to);
                }
                Some(_) => {}
                None => {
                    doubt.reasons.insert(FactStateReason::AnchorMissing);
                }
            }
        }
        if let Some(by) = contradicted.get(&fact.id) {
            doubt.reasons.insert(FactStateReason::Contradiction);
            doubt.contradicted_by = by.iter().take(MAX_CONTRADICTED_BY).cloned().collect();
        }
        if !doubt.reasons.is_empty() {
            out.push(doubt);
        }
    }
    out.sort_by(|a, b| a.fact.cmp(&b.fact));
    out.dedup_by(|a, b| a.fact == b.fact);
    out
}

/// The report for `base` (`None` before the first revision), keeping the
/// `since` of a state that did not change since the last report.
fn build(
    root: &Path,
    record: &VaultRecord,
    base: &BaseRevision,
    sent: &Sent,
    now: DateTime<Utc>,
    screen: &mut Screen,
) -> Option<FactStateReportV1> {
    if base.revision == 0 {
        return None;
    }
    let doubts = doubts(root, record, base, screen);
    let (states, counts) = if record.sealed {
        let contradicted = doubts
            .iter()
            .filter(|d| d.kind() == FactStateKind::Contradicted)
            .count() as u64;
        let counts = FactStateCountsV1 {
            to_verify: doubts.len() as u64 - contradicted,
            contradicted,
        };
        (None, Some(counts))
    } else {
        let states = doubts
            .into_iter()
            .map(|d| {
                let mut state = FactStateV1 {
                    fact: d.fact.clone(),
                    state: d.kind(),
                    reasons: d.reasons.iter().copied().collect(),
                    moved_to: d.moved_to.clone(),
                    contradicted_by: d.contradicted_by.iter().cloned().collect(),
                    since: stamp(now),
                };
                if let Some(before) = sent.states.iter().find(|s| {
                    sent.revision == base.revision
                        && s.fact == state.fact
                        && s.state == state.state
                        && s.reasons == state.reasons
                }) {
                    state.since = before.since.clone();
                }
                state
            })
            .collect();
        (Some(states), None)
    };
    Some(FactStateReportV1 {
        schema: FACT_STATE_REPORT_SCHEMA.into(),
        revision: base.revision,
        states,
        counts,
    })
}

fn load_sent(data: &Path, vault_id: &str) -> Sent {
    base_dir(data, vault_id)
        .ok()
        .and_then(|dir| std::fs::read(dir.join(SENT_FILE)).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn unchanged(sent: &Sent, report: &FactStateReportV1) -> bool {
    sent.revision == report.revision
        && report.states.as_deref().unwrap_or_default() == sent.states.as_slice()
        && report.counts == sent.counts
}

/// Report `record`'s fact states when they changed; `Some(ack)` when sent.
pub fn report(
    session: &Session,
    data: &Path,
    record: &VaultRecord,
) -> Result<Option<FactStateAckV1>, HiveError> {
    // A "no" to the sharing question at connect keeps fact states here.
    if crate::core::hive::connect::declined(&session.store, &session.lease.lease.org_id) {
        return Ok(None);
    }
    let Some(root) = record.roots.iter().map(Path::new).find(|r| r.is_dir()) else {
        return Ok(None);
    };
    let mut screen = Screen::for_installation();
    if let Some(problem) = screen.problem() {
        return Err(HiveError::Config(problem));
    }
    let base = super::load_base(data, &record.vault_id);
    let sent = load_sent(data, &record.vault_id);
    let Some(report) = build(root, record, &base, &sent, Utc::now(), &mut screen) else {
        return Ok(None);
    };
    if unchanged(&sent, &report) {
        return Ok(None);
    }
    report
        .validate()
        .map_err(|e| HiveError::Local(format!("fact states: {e}")))?;
    let body = serde_json::to_value(&report).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{}/facts/states", record.vault_id);
    let answer = session.with_client(|c| c.post(&path, &body))?;
    let ack: FactStateAckV1 = serde_json::from_value(answer)
        .map_err(|e| HiveError::Protocol(format!("fact states: {e}")))?;
    let stored = Sent {
        revision: report.revision,
        states: report.states.unwrap_or_default(),
        counts: report.counts,
    };
    let bytes = serde_json::to_vec(&stored).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&base_dir(data, &record.vault_id)?, SENT_FILE, &bytes)?;
    Ok(Some(ack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::hive::knowledge::{FactV1, ResearchContextDocumentV1};

    fn record(sealed: bool, root: &Path) -> VaultRecord {
        VaultRecord {
            vault_id: "0123456789abcdef0123456789abcdef".into(),
            ns: "acme".into(),
            name: "billing".into(),
            sealed,
            roots: vec![root.to_string_lossy().into_owned()],
        }
    }

    fn base(facts: Vec<FactV1>) -> BaseRevision {
        let document = ResearchContextDocumentV1 {
            facts,
            ..ResearchContextDocumentV1::default()
        };
        BaseRevision {
            revision: 4,
            document,
            ..BaseRevision::default()
        }
    }

    fn fact(id: &str, anchors: Vec<anchors::CodeAnchor>) -> FactV1 {
        FactV1 {
            id: id.into(),
            statement: format!("statement {id}"),
            kind: "observation".into(),
            applies_to: Vec::new(),
            sources: Vec::new(),
            from_contributions: Vec::new(),
            anchors,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn changed_moved_and_missing_anchors_make_facts_to_verify() {
        // An anchor is read only through source admission, which reads the
        // policy, configuration and data directory of the process. Unlocked,
        // the pass saw whatever another test had set: the unchanged
        // `src/b.md` can only turn to_verify through a refused admission
        // (`Missing` without a read), as it did once (to_verify 3 vs 2, 25.10.).
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.md"), "retry three times\n").unwrap();
        std::fs::write(root.join("src/b.md"), "keep\n").unwrap();
        std::fs::write(root.join("src/c.md"), "gone soon\n").unwrap();
        let a = anchors::capture(root, "src/a.md", None).unwrap();
        let b = anchors::capture(root, "src/b.md", None).unwrap();
        let c = anchors::capture(root, "src/c.md", None).unwrap();
        let base = base(vec![
            fact("f.changed", vec![a]),
            fact("f.current", vec![b]),
            fact("f.missing", vec![c]),
            fact("f.plain", vec![]),
        ]);
        std::fs::write(root.join("src/a.md"), "retry twice\n").unwrap();
        std::fs::remove_file(root.join("src/c.md")).unwrap();

        let rec = record(false, root);
        let mut screen =
            Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new());
        let report = build(root, &rec, &base, &Sent::default(), now(), &mut screen).unwrap();
        report.validate().unwrap();
        let states = report.states.as_ref().unwrap();
        assert_eq!(
            states
                .iter()
                .map(|s| (s.fact.as_str(), s.state, s.reasons.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "f.changed",
                    FactStateKind::ToVerify,
                    vec![FactStateReason::AnchorChanged]
                ),
                (
                    "f.missing",
                    FactStateKind::ToVerify,
                    vec![FactStateReason::AnchorMissing]
                ),
            ]
        );

        // An unchanged state keeps its first `since`; the report is then
        // the one already sent.
        let sent = Sent {
            revision: report.revision,
            states: states.clone(),
            counts: None,
        };
        let later = now() + chrono::Duration::hours(3);
        let again = build(root, &rec, &base, &sent, later, &mut screen).unwrap();
        assert!(unchanged(&sent, &again));

        // Sealed: counts only, no fact ids, no paths.
        let sealed = build(
            root,
            &record(true, root),
            &base,
            &Sent::default(),
            now(),
            &mut screen,
        )
        .unwrap();
        sealed.validate().unwrap();
        assert_eq!(sealed.states, None);
        assert_eq!(
            sealed.counts,
            Some(FactStateCountsV1 {
                to_verify: 2,
                contradicted: 0
            })
        );
        let wire = serde_json::to_string(&sealed).unwrap();
        assert!(!wire.contains("f.changed") && !wire.contains("src/"));
    }

    /// Where an anchor moved to is outbound content: a new path the screen
    /// refuses stays on this machine, and that fact is reported as changed
    /// (no `moved_to`); an admitted move is reported with its new place.
    #[test]
    fn a_moved_anchor_names_its_new_place_only_when_the_screen_lets_it_pass() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let retry = "pub fn retry() {\n    three();\n}\n";
        let pay = "pub fn pay() {\n    charge();\n}\n";
        std::fs::write(root.join("src/a.rs"), retry).unwrap();
        std::fs::write(root.join("src/b.rs"), pay).unwrap();
        let base = base(vec![
            fact(
                "f.moved",
                vec![anchors::capture(root, "src/a.rs", Some("retry")).unwrap()],
            ),
            fact(
                "f.withheld",
                vec![anchors::capture(root, "src/b.rs", Some("pay")).unwrap()],
            ),
        ]);
        std::fs::remove_file(root.join("src/a.rs")).unwrap();
        std::fs::remove_file(root.join("src/b.rs")).unwrap();
        std::fs::write(root.join("src/retry.rs"), retry).unwrap();
        std::fs::write(root.join("src/falcon_billing.rs"), pay).unwrap();
        let root_text = root.to_string_lossy();
        let mut index = crate::core::graph_index::ProjectIndex::new(&root_text);
        for (file, name) in [("src/retry.rs", "retry"), ("src/falcon_billing.rs", "pay")] {
            // `ProjectIndex::load` answers only for a graph with cataloged files.
            index.files.insert(
                file.into(),
                crate::core::graph_index::FileEntry {
                    path: file.into(),
                    hash: "h".into(),
                    language: "rs".into(),
                    line_count: 3,
                    token_count: 1,
                    exports: vec![name.into()],
                    summary: String::new(),
                },
            );
            index.symbols.insert(
                format!("{file}::{name}"),
                crate::core::graph_index::SymbolEntry {
                    file: file.into(),
                    name: name.into(),
                    kind: "fn".into(),
                    start_line: 1,
                    end_line: 3,
                    is_exported: true,
                },
            );
        }
        index.save().unwrap();

        let mut screen = Screen::with_rules(
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            vec![regex::Regex::new("falcon").unwrap()],
        );
        let report = build(
            root,
            &record(false, root),
            &base,
            &Sent::default(),
            now(),
            &mut screen,
        )
        .unwrap();
        report.validate().unwrap();
        let states = report.states.as_ref().unwrap();
        assert_eq!(
            states
                .iter()
                .map(|s| (s.fact.as_str(), s.reasons.clone(), s.moved_to.is_some()))
                .collect::<Vec<_>>(),
            vec![
                ("f.moved", vec![FactStateReason::AnchorMoved], true),
                ("f.withheld", vec![FactStateReason::AnchorChanged], false),
            ]
        );
        let wire = serde_json::to_string(&report).unwrap();
        assert!(wire.contains("src/retry.rs"), "{wire}");
        assert!(
            !wire.contains("falcon"),
            "the refused path stays here: {wire}"
        );
    }

    #[test]
    fn a_judged_contradiction_is_reported_until_one_supersedes() {
        use crate::core::knowledge::{JudgedPair, KnowledgeFact};
        let mut knowledge = ProjectKnowledge::new("/tmp/fact-states");
        let category = knowledge_category("acme", "billing");
        for (key, value) in [("f.one", "retry three times"), ("f.two", "never retry")] {
            let mut f: KnowledgeFact = serde_json::from_value(serde_json::json!({
                "category": category, "key": key, "value": value, "source_session": "s",
                "confidence": 0.8, "created_at": "2026-10-22T10:00:00Z",
                "last_confirmed": "2026-10-22T10:00:00Z"
            }))
            .unwrap();
            f.imported_from = Some("hive-repo:@acme/billing@r4".into());
            knowledge.facts.push(f);
        }
        let judged = |verdict: &str, minute: u32| JudgedPair {
            key_a: format!("{category}/f.one"),
            key_b: format!("{category}/f.two"),
            verdict: verdict.into(),
            judged_at: DateTime::parse_from_rfc3339(&format!("2026-10-22T11:{minute:02}:00Z"))
                .unwrap()
                .with_timezone(&Utc),
        };
        knowledge.judged_pairs.push(judged("contradicts", 1));
        let found = contradictions(&knowledge, &category);
        assert_eq!(
            found
                .get("f.one")
                .map(|s| s.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["f.two".to_string()])
        );
        assert!(found.contains_key("f.two"));

        // A later verdict on the same pair replaces it.
        knowledge.judged_pairs.push(judged("compatible", 2));
        assert!(contradictions(&knowledge, &category).is_empty());
    }
}
