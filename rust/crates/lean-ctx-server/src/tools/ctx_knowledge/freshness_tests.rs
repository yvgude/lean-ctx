// SPDX-License-Identifier: Apache-2.0
//! Freshness primitives against a real store in an isolated data dir.

use lean_ctx::core::context_admission::stored::lineage_support::{project, scoped};
use lean_ctx::core::knowledge::{FactReviewReason, FactReviewState, JudgedPair, KnowledgeFact};

use super::*;

fn fact(category: &str, key: &str, value: &str, confirmations: u32) -> KnowledgeFact {
    let mut fact: KnowledgeFact = serde_json::from_value(serde_json::json!({
        "category": category, "key": key, "value": value,
        "source_session": "s", "confidence": 0.8,
        "created_at": "2026-10-01T09:00:00Z", "last_confirmed": "2026-10-01T09:00:00Z",
    }))
    .expect("fact");
    fact.confirmation_count = confirmations;
    fact
}

fn store(root: &str, facts: Vec<KnowledgeFact>, pairs: Vec<JudgedPair>) {
    scoped(root, || {
        ProjectKnowledge::mutate_locked(root, |knowledge| {
            knowledge.facts.extend(facts);
            knowledge.judged_pairs.extend(pairs);
            knowledge.rebuild_index();
        })
    })
    .expect("store saved");
}

fn isolated() -> (
    lean_ctx::core::data_dir::IsolatedDataDir,
    lean_ctx::core::policy::runtime::TestPolicyOverride,
) {
    (
        lean_ctx::core::data_dir::isolated_data_dir(),
        lean_ctx::core::policy::runtime::TestPolicyOverride::set(None),
    )
}

/// An edit through the instance marks the anchored fact to verify; `stale`
/// lists it with the reason; `verify` re-anchors and clears it; a deleted
/// file makes it gone and refuses verify.
#[test]
fn an_edit_marks_anchored_facts_to_verify_until_verified() {
    let _guards = isolated();
    let (_directory, root) = project();
    let file = format!("{root}/notes.md");
    std::fs::write(&file, "Webhooks retry three times.\n").expect("source");
    let mut anchored = fact(
        "architecture",
        "retries",
        "Webhook retries are capped at three",
        1,
    );
    anchored.anchors = vec![anchors::capture(Path::new(&root), "notes.md", None).expect("anchor")];
    store(&root, vec![anchored], Vec::new());

    assert_eq!(
        scoped(&root, || mark_edited(&root, &file)),
        0,
        "unchanged code"
    );
    assert_eq!(stale(&root, None).total, 0);

    std::fs::write(&file, "Webhooks retry five times.\n").expect("edit");
    assert_eq!(scoped(&root, || mark_edited(&root, &file)), 1);
    let report = stale(&root, None);
    assert_eq!(report.total, 1);
    let listed = &report.facts[0];
    assert_eq!(listed.fact.id, "architecture/retries");
    assert_eq!(listed.fact.review, Some("to_verify"));
    assert!(listed.since.is_some(), "the edit marked it in the store");
    assert_eq!(listed.reasons[0].code, "anchor_changed");
    assert_eq!(listed.reasons[0].path.as_deref(), Some("notes.md"));
    let json = serde_json::to_string(&report).expect("json");
    assert!(
        json.starts_with(r#"{"contract":"local-read/1","kind":"knowledge.stale""#),
        "{json}"
    );

    let verified = scoped(&root, || verify(&root, "architecture/retries")).expect("verified");
    assert_eq!(verified.cleared, Some("anchor_changed"));
    assert_eq!(verified.reanchored.changed, 1);
    assert_eq!(verified.fact.review, None);
    assert_eq!(stale(&root, None).total, 0);

    // One source of truth: a revert clears the stored mark, whether the
    // revert went through this instance (`mark_edited`) or not (`stale`).
    let stored = || {
        ProjectKnowledge::load(&root).unwrap().facts[0]
            .review
            .as_ref()
            .map(|r| r.state)
    };
    for through_instance in [true, false] {
        std::fs::write(&file, "Webhooks retry seven times.\n").expect("edit");
        assert_eq!(scoped(&root, || mark_edited(&root, &file)), 1);
        assert_eq!(stored(), Some(FactReviewState::ToVerify));
        std::fs::write(&file, "Webhooks retry five times.\n").expect("revert");
        if through_instance {
            assert_eq!(scoped(&root, || mark_edited(&root, &file)), 1);
        }
        assert_eq!(scoped(&root, || stale(&root, None)).total, 0);
        assert_eq!(
            stored(),
            None,
            "reverted (through instance: {through_instance})"
        );
    }

    std::fs::remove_file(&file).expect("delete");
    assert_eq!(stale(&root, None).facts[0].fact.review, Some("gone"));
    let refused = scoped(&root, || verify(&root, "architecture/retries")).unwrap_err();
    assert_eq!(refused.code, "anchor_gone");
    assert_eq!(
        scoped(&root, || verify(&root, "architecture/nope"))
            .unwrap_err()
            .code,
        "not_found"
    );
}

/// Exact and near duplicates group under the most confirmed fact; a dry run
/// writes nothing, `apply` archives the others as superseded by it and never
/// rewrites the survivor's text; a pair judged unrelated, or texts that
/// differ in a polarity word or a number, are never merged.
#[test]
fn dedupe_groups_duplicates_and_merges_only_on_apply() {
    let _guards = isolated();
    let (_directory, root) = project();
    store(
        &root,
        vec![
            fact("ops", "deploy-day", "Deploys run on Fridays only", 3),
            fact("ops", "fridays", "deploys  run on FRIDAYS only", 1),
            fact(
                "ops",
                "deploy-review",
                "Deploys run on Fridays only after review",
                1,
            ),
            fact(
                "ops",
                "deploy-window",
                "Deploys run on Fridays only per window",
                1,
            ),
            fact("ops", "backups", "Backups are encrypted at rest", 1),
            fact("jobs", "retry-never", "Failed jobs are never retried", 1),
            fact("jobs", "retry-always", "Failed jobs are always retried", 1),
            fact("jobs", "retry-count", "Failed jobs are retried 3 times", 1),
            fact("jobs", "retry-five", "Failed jobs are retried 5 times", 1),
        ],
        vec![JudgedPair {
            key_a: "ops/deploy-day".into(),
            key_b: "ops/deploy-window".into(),
            verdict: "unrelated".into(),
            judged_at: chrono::Utc::now(),
        }],
    );

    let dry = dedupe(&root, Some(0.7), false).expect("dry run");
    assert!(!dry.applied);
    assert_eq!(dry.merged, 0);
    assert_eq!(dry.groups.len(), 1);
    let group = &dry.groups[0];
    assert_eq!(group.keep.id, "ops/deploy-day");
    let merged: Vec<(&str, &str)> = group
        .merge
        .iter()
        .map(|m| (m.fact.id.as_str(), m.kind))
        .collect();
    assert_eq!(
        merged,
        vec![("ops/deploy-review", "near"), ("ops/fridays", "exact")]
    );
    assert_eq!(
        serde_json::to_string(&dry).unwrap(),
        serde_json::to_string(&dedupe(&root, Some(0.7), false).unwrap()).unwrap(),
        "deterministic"
    );
    assert_eq!(
        ProjectKnowledge::load(&root)
            .unwrap()
            .facts
            .iter()
            .filter(|f| f.is_current())
            .count(),
        9,
        "a dry run writes nothing"
    );

    let applied = scoped(&root, || dedupe(&root, Some(0.7), true)).expect("applied");
    assert_eq!(applied.merged, 2);
    assert_eq!(applied.groups[0].keep.text, "Deploys run on Fridays only");
    let knowledge = ProjectKnowledge::load(&root).unwrap();
    let current = knowledge.facts.iter().filter(|f| f.is_current()).count();
    assert_eq!(current, 7);
    assert!(
        knowledge
            .facts
            .iter()
            .any(|f| !f.is_current() && f.value == "Deploys run on Fridays only after review"),
        "the merged wording stays in the history"
    );
    let now = chrono::Utc::now();
    assert!(outdated(&root, Some(MAX_OUTDATED_DAYS), None, now).is_ok());
    assert_eq!(
        outdated(&root, Some(u32::MAX), None, now).unwrap_err().code,
        "invalid_argument"
    );
    let outdated = outdated(&root, None, None, now).expect("outdated");
    let superseded: Vec<(&str, bool, Option<&str>)> = outdated
        .facts
        .iter()
        .filter(|f| f.reason == "superseded")
        .map(|f| {
            let versioned = f.fact.id.starts_with(&format!("ops/{}@", f.fact.key));
            (f.fact.key.as_str(), versioned, f.superseded_by.as_deref())
        })
        .collect();
    assert_eq!(
        superseded,
        vec![
            ("deploy-review", true, Some("ops/deploy-day")),
            ("fridays", true, Some("ops/deploy-day"))
        ],
        "a superseded version has a version id, its successor the plain one"
    );
}

/// A judged contradiction between two current facts is one conflict; a
/// contradicted fact whose partner is gone is listed unpaired.
#[test]
fn conflicts_list_judged_contradictions_and_unpaired_marks() {
    let _guards = isolated();
    let (_directory, root) = project();
    let now = chrono::Utc::now();
    let contradicted = |mut f: KnowledgeFact| {
        f.mark_review(
            FactReviewState::Contradicted,
            FactReviewReason::Contradiction,
            now,
        );
        f
    };
    store(
        &root,
        vec![
            contradicted(fact("db", "primary", "The primary database is Postgres", 1)),
            contradicted(fact("db", "engine", "The primary database is MySQL", 1)),
            contradicted(fact("api", "timeout", "Requests time out after 30 s", 1)),
            fact("api", "auth", "Tokens are JWTs", 1),
        ],
        vec![JudgedPair {
            key_a: "db/primary".into(),
            key_b: "db/engine".into(),
            verdict: "contradicts".into(),
            judged_at: now,
        }],
    );

    let report = conflicts(&root);
    assert_eq!(report.conflicts.len(), 1);
    let conflict = &report.conflicts[0];
    assert_eq!(
        (conflict.a.id.as_str(), conflict.b.id.as_str()),
        ("db/engine", "db/primary")
    );
    assert_eq!(conflict.source, "judged");
    assert_eq!(conflict.a.review, Some("contradicted"));
    let unpaired: Vec<&str> = report.unpaired.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(unpaired, vec!["api/timeout"]);
    assert_eq!(
        stale(&root, None).total,
        3,
        "contradicted facts are not current truth"
    );
}
