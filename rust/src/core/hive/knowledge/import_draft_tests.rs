// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::knowledge::ProjectKnowledge;
use lean_ctx_protocol::hive::knowledge::import_source_ok;

fn fact(statement: &str) -> FactV1 {
    FactV1 {
        id: format!("lc-{}", statement.len()),
        statement: statement.into(),
        kind: "observation".into(),
        applies_to: Vec::new(),
        sources: Vec::new(),
        from_contributions: Vec::new(),
        anchors: Vec::new(),
    }
}

#[test]
fn drafts_hold_at_most_sixteen_sources_and_fifty_facts() {
    let facts: Vec<FactV1> = (0..60).map(|n| fact(&"x".repeat(n + 1))).collect();
    // 20 sources, 3 facts each.
    let sources: Vec<String> = (0..60)
        .map(|n| format!("import:docs:AGENTS.md#s{}", n / 3))
        .collect();
    let drafts = batches(facts, sources);
    assert_eq!(
        drafts
            .iter()
            .map(|(facts, names)| (facts.len(), names.len()))
            .collect::<Vec<_>>(),
        [(48, 16), (12, 4)]
    );
    let many: Vec<FactV1> = (0..60).map(|n| fact(&"y".repeat(n + 1))).collect();
    let one = vec!["import:git:".to_owned() + &"a".repeat(40); 60];
    let drafts = batches(many, one);
    assert_eq!(drafts[0].0.len(), MAX_PATCH_OPS);
    assert_eq!(drafts[1].0.len(), 10);
}

struct Place {
    _dirs: tempfile::TempDir,
    store: HiveStore,
    root: std::path::PathBuf,
}

fn place() -> Place {
    let dirs = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dirs.path().join("config"), dirs.path().join("data"));
    crate::core::hive::bus::control::enroll_for_tests(&store, 1);
    let root = dirs.path().join("payments");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/payments.git",
        ],
    );
    let root = root.canonicalize().unwrap();
    Place {
        _dirs: dirs,
        store,
        root,
    }
}

fn git(root: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Commit everything in the checkout: the capture gate checks a fact's
/// references against `HEAD`.
fn commit(root: &std::path::Path) {
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "docs"]);
}

fn queued(place: &Place) -> Vec<DraftProposalV1> {
    Outbox::at(place.store.data_dir())
        .list(CHANNEL_KNOWLEDGE)
        .into_iter()
        .filter_map(|item| super::super::draft::queued(&item.payload))
        .map(|queued| queued.draft)
        .collect()
}

/// An import run becomes import drafts: origin import, one valid source per
/// section, nothing learned in a session and nothing confidential; a second
/// run without change queues nothing, a changed section queues just that
/// fact. (No secret reaches the store: the import admits every value; the
/// draft screening is plan_learn's, tested there.)
#[test]
fn imported_facts_become_import_drafts_once() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let place = place();
    let root_text = place.root.to_string_lossy().into_owned();
    let mut agents = String::from("# Agents\n");
    for n in 0..20 {
        agents.push_str(&format!(
            "\n## Rule {n}\n\n- Convention number {n} applies to every module.\n"
        ));
    }
    std::fs::write(place.root.join("AGENTS.md"), &agents).unwrap();
    commit(&place.root);
    let mut imported = crate::core::import::markdown::import(&place.root, &[]);
    assert_eq!(imported.facts.len(), 20, "{:?}", imported.errors);
    let mut confidential = imported.facts[0].clone();
    confidential.key = "AGENTS.md#agents/secret/1".into();
    confidential.value = "A confidential convention that stays here.".into();
    confidential.sensitivity = crate::core::sensitivity::SensitivityLevel::Confidential;
    let mut learned = imported.facts[2].clone();
    learned.imported_from = None;
    learned.key = "learned".into();
    learned.value = "A fact the agent learned in a session.".into();
    let facts = std::mem::take(&mut imported.facts);
    ProjectKnowledge::mutate_locked(&root_text, |knowledge| {
        knowledge.facts.extend(facts);
        knowledge.facts.extend([confidential, learned]);
    })
    .unwrap();

    let config = HiveConfig::default();
    let mut screen = Screen::for_installation();
    let now = Utc::now();
    let ids =
        enqueue_import_drafts(&place.store, &config, &place.root, "s1", &mut screen, now).unwrap();
    assert_eq!(ids.len(), 2, "20 sections: 16 sources, then 4");
    let drafts = queued(&place);
    let statements: Vec<&str> = drafts
        .iter()
        .flat_map(|draft| &draft.context_patch.ops)
        .filter_map(|op| match op {
            PatchOpV1::AddFact { fact } => Some(fact.statement.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(statements.len(), 20);
    assert!(
        statements
            .iter()
            .all(|s| s.starts_with("Convention number"))
    );
    for draft in &drafts {
        assert_eq!(draft.origin, Some(DraftOrigin::Import));
        assert!(
            draft.sources.iter().all(|s| import_source_ok(s)),
            "{:?}",
            draft.sources
        );
        assert!(
            draft
                .summary
                .as_deref()
                .unwrap()
                .starts_with("Imported from the repository")
        );
    }
    // Two drafts queued in the same millisecond list in either order.
    let mut sizes: Vec<usize> = drafts.iter().map(|d| d.sources.len()).collect();
    sizes.sort_unstable();
    assert_eq!(sizes, [4, 16]);
    let first = drafts
        .iter()
        .find(|d| d.sources.len() == 16)
        .expect("the full draft");
    assert_eq!(first.sources[0], "import:docs:AGENTS.md#agents-rule-0");

    let again =
        enqueue_import_drafts(&place.store, &config, &place.root, "s2", &mut screen, now).unwrap();
    assert!(
        again.is_empty(),
        "nothing changed since the last import draft"
    );

    let later = now + chrono::Duration::seconds(5);
    ProjectKnowledge::mutate_locked(&root_text, |knowledge| {
        let fact = knowledge
            .facts
            .iter_mut()
            .find(|f| f.key == "AGENTS.md#agents/rule-3/1")
            .unwrap();
        fact.value = "Convention number 3 now applies to every crate.".into();
        fact.last_confirmed = later;
    })
    .unwrap();
    let changed =
        enqueue_import_drafts(&place.store, &config, &place.root, "s3", &mut screen, later)
            .unwrap();
    assert_eq!(changed.len(), 1);
    let last = queued(&place).pop().unwrap();
    assert_eq!(last.context_patch.ops.len(), 1);
    assert_eq!(last.sources, ["import:docs:AGENTS.md#agents-rule-3"]);

    // A fact whose section is gone or changed (to verify) is not proposed.
    let review_at = later + chrono::Duration::seconds(5);
    ProjectKnowledge::mutate_locked(&root_text, |knowledge| {
        let fact = knowledge
            .facts
            .iter_mut()
            .find(|f| f.key == "AGENTS.md#agents/rule-4/1")
            .unwrap();
        fact.value = "Convention number 4 changed while its section is gone.".into();
        fact.last_confirmed = review_at;
        fact.mark_review(
            crate::core::knowledge::FactReviewState::ToVerify,
            crate::core::knowledge::FactReviewReason::SourceRemoved,
            review_at,
        );
    })
    .unwrap();
    let under_review = enqueue_import_drafts(
        &place.store,
        &config,
        &place.root,
        "s4",
        &mut screen,
        review_at,
    )
    .unwrap();
    assert!(under_review.is_empty(), "a fact to verify stays here");
}

#[test]
fn import_sources_follow_the_contract() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let place = place();
    std::fs::write(
        place.root.join("AGENTS.md"),
        "# Agents\n\n## Rules\n\n- Keep every file below 1500 lines.\n",
    )
    .unwrap();
    let mut fact = crate::core::import::markdown::import(&place.root, &[])
        .facts
        .pop()
        .unwrap();
    let source =
        |fact: &crate::core::knowledge::KnowledgeFact| super::super::learn::import_source(fact);
    assert_eq!(
        source(&fact).as_deref(),
        Some("import:docs:AGENTS.md#agents-rules")
    );
    fact.key = "docs/guides/release.md#release/steps/2".into();
    assert_eq!(
        source(&fact).as_deref(),
        Some("import:docs:docs/guides/release.md#release-steps")
    );
    fact.key = "AGENTS.md#top/1".into();
    assert_eq!(source(&fact).as_deref(), Some("import:docs:AGENTS.md#top"));
    fact.imported_from = Some("git".into());
    fact.key = format!("git:{}", "ab".repeat(20));
    assert_eq!(
        source(&fact),
        Some(format!("import:git:{}", "ab".repeat(20)))
    );
    fact.key = "git:abab12".into();
    assert_eq!(source(&fact), None, "a short sha is no source");
    fact.imported_from = Some("hive-repo:@acme/payments@r3".into());
    assert_eq!(
        source(&fact),
        None,
        "Hive revisions are never proposed back"
    );
}
