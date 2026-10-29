// SPDX-License-Identifier: Apache-2.0
//! V1 done-when: the old "Session learnings" drafts (truncated raw text) are
//! replaced, and nothing that fails the capture gate is shared.

use chrono::Utc;

use super::*;
use crate::core::hive::capture::ReasonCode;
use crate::core::knowledge::ProjectKnowledge;

struct Place {
    _dirs: tempfile::TempDir,
    store: HiveStore,
    root: std::path::PathBuf,
}

fn git(root: &Path, args: &[&str]) {
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

fn place() -> Place {
    let dirs = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dirs.path().join("config"), dirs.path().join("data"));
    crate::core::hive::bus::control::enroll_for_tests(&store, 1);
    let root = dirs.path().join("thinkery");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/relay.rs"), "pub fn relay() {}\n").unwrap();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/thinkery.git",
        ],
    );
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let root = root.canonicalize().unwrap();
    Place {
        _dirs: dirs,
        store,
        root,
    }
}

fn queued_drafts(place: &Place) -> Vec<DraftProposalV1> {
    Outbox::at(place.store.data_dir())
        .list(CHANNEL_KNOWLEDGE)
        .into_iter()
        .filter_map(|item| queued(&item.payload))
        .map(|q| q.draft)
        .collect()
}

/// What sessions wrote into project knowledge: one clean, referenced claim
/// and the kinds of text the old drafts shipped truncated.
fn remember_session(root: &str) {
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    ProjectKnowledge::mutate_locked(root, |k| {
        k.remember(
            "architecture",
            "relay-frames",
            "The relay drops frames when the link is slower than 1 Mbit/s.",
            "s1",
            0.9,
            &policy,
        );
        let f = k.facts.last_mut().unwrap();
        f.capture.refs = vec!["file:src/relay.rs".into()];
        k.remember(
            "build-log",
            "run-17",
            "error[E0308]: mismatched types\n --> src/relay.rs:1:1\n[exit:101]",
            "s1",
            0.5,
            &policy,
        );
        k.remember(
            "architecture",
            "long",
            &format!(
                "{}.",
                "The scheduler renews leases and retries pushes ".repeat(10)
            ),
            "s1",
            0.8,
            &policy,
        );
        k.remember(
            "architecture",
            "unreferenced",
            "The relay keeps one socket per peer for its lifetime.",
            "s1",
            0.8,
            &policy,
        );
    })
    .unwrap();
}

#[test]
fn recapture_replaces_broken_session_drafts_with_gated_claims() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let place = place();
    let root = place.root.to_string_lossy().into_owned();
    remember_session(&root);

    // A draft of the old shape is waiting in the outbox.
    let stable = stable_identity(&project_identity(&place.root).unwrap());
    let old = QueuedDraft {
        stable_identity: stable,
        root: root.clone(),
        draft: DraftProposalV1 {
            schema: DRAFT_PROPOSAL_SCHEMA.into(),
            state: ProposalState::Draft,
            summary: Some("Session learnings: 3 facts".into()),
            body: Some("Learned in a lean-ctx session:\n- error[E0308]: mismatched".into()),
            sources: Vec::new(),
            context_patch: ContextPatchV1 {
                base_revision: 0,
                ops: Vec::new(),
                sealed: None,
            },
            session_ref: session_ref("old"),
            engine_screening: Screen::for_installation().counts(),
            origin: None,
        },
    };
    Outbox::at(place.store.data_dir())
        .enqueue(
            CHANNEL_KNOWLEDGE,
            serde_json::to_value(old).unwrap(),
            Utc::now(),
        )
        .unwrap();

    let mut screen = Screen::for_installation();
    let done = recapture(
        &place.store,
        &HiveConfig::default(),
        &place.root,
        &mut screen,
        Utc::now(),
    )
    .unwrap();
    assert_eq!(done.removed, 1, "the broken draft is gone");
    assert_eq!(done.queued.len(), 1);
    assert_eq!(done.counts.considered, 4);
    assert_eq!(done.counts.captured, 1);
    assert_eq!(done.counts.rejected.get(&ReasonCode::NoType), Some(&1));
    assert_eq!(done.counts.rejected.get(&ReasonCode::TooLong), Some(&1));
    assert_eq!(done.counts.rejected.get(&ReasonCode::NoReference), Some(&1));

    let drafts = queued_drafts(&place);
    assert_eq!(drafts.len(), 1);
    let draft = &drafts[0];
    assert_eq!(draft.summary.as_deref(), Some("Captured 1 fact"));
    let body = draft.body.as_deref().unwrap();
    assert!(!body.contains("Session learnings"), "{body}");
    assert!(
        body.contains("- The relay drops frames when the link is slower than 1 Mbit/s.\n"),
        "the claim is listed whole: {body}"
    );
    // No shared item fails the gate.
    for op in &draft.context_patch.ops {
        let PatchOpV1::AddFact { fact } = op else {
            panic!("only facts")
        };
        assert_eq!(
            crate::core::hive::capture::rules::check_statement(&fact.statement),
            Ok(())
        );
        assert!(
            fact.sources.iter().any(|s| s == "file:src/relay.rs"),
            "{fact:?}"
        );
    }

    // Nothing new on the next run; the counts are kept for the place.
    let again = enqueue_session_draft(
        &place.store,
        &HiveConfig::default(),
        &place.root,
        "s2",
        Trigger::SessionEnd,
        &mut screen,
        Utc::now(),
    )
    .unwrap();
    assert!(again.is_none());
    let counts = crate::core::hive::capture::load_counts(place.store.data_dir());
    assert_eq!(counts.values().map(|c| c.captured).sum::<u32>(), 1);
}

/// Live 26.10.: a fact remembered without `refs` is rejected
/// (`no_reference`) and the watermark moves past it; the agent then
/// remembers the same statement again with `--ref`. That second remember
/// must bring the fact back to the gate, which now captures it; a third,
/// identical remember changes nothing and captures nothing twice.
#[test]
fn refs_added_by_a_later_remember_bring_a_rejected_fact_back() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let place = place();
    let root = place.root.to_string_lossy().into_owned();
    let statement = "The relay keeps one socket per peer for its lifetime.";
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    ProjectKnowledge::mutate_locked(&root, |k| {
        k.remember("architecture", "socket", statement, "s1", 0.8, &policy);
    })
    .unwrap();
    let mut screen = Screen::for_installation();
    let run = |screen: &mut Screen| {
        capture_session(
            &place.store,
            &HiveConfig::default(),
            &place.root,
            "s1",
            Trigger::SessionEnd,
            screen,
            Utc::now(),
        )
        .unwrap()
    };
    let (first, counts) = run(&mut screen);
    assert!(first.is_none());
    assert_eq!(counts.rejected.get(&ReasonCode::NoReference), Some(&1));

    let with_ref = || crate::core::knowledge::FactCapture {
        refs: vec!["file:src/relay.rs".into()],
        ..Default::default()
    };
    crate::tools::ctx_knowledge::capture_remembered(
        &root,
        "architecture",
        Some("socket"),
        statement,
        with_ref(),
    )
    .unwrap();
    let (second, counts) = run(&mut screen);
    assert!(second.is_some(), "the referenced fact is captured");
    assert_eq!((counts.considered, counts.captured), (1, 1));

    crate::tools::ctx_knowledge::capture_remembered(
        &root,
        "architecture",
        Some("socket"),
        statement,
        with_ref(),
    )
    .unwrap();
    let (third, counts) = run(&mut screen);
    assert!(third.is_none(), "unchanged refs: not considered again");
    assert_eq!(counts.considered, 0);
}

/// Live 26.10. (lead decision): a vault that syncs through its C0 2.0
/// replica gets knowledge only as signed events. Session end, save and
/// shutdown queue no 1.x draft for it (that duplicated the claims and fed
/// the frozen review flow as "Sent draft #1"); against a 1.x Hive the
/// same project still drafts.
#[test]
fn a_vault_on_the_v2_replica_gets_no_v1_draft() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let place = place();
    let root = place.root.to_string_lossy().into_owned();
    remember_session(&root);
    let stable = stable_identity(&project_identity(&place.root).unwrap());
    let mut map = VaultMap::default();
    map.vaults.insert(
        stable.clone(),
        crate::core::hive::vault::VaultRecord {
            vault_id: "1bae9408a81f40368b48645fad237bd6".into(),
            ns: "thinkery".into(),
            name: "thinkery".into(),
            sealed: false,
            roots: vec![root.clone()],
        },
    );
    map.save(place.store.data_dir()).unwrap();
    let draft = |trigger| {
        enqueue_session_draft(
            &place.store,
            &HiveConfig::default(),
            &place.root,
            "s1",
            trigger,
            &mut Screen::for_installation(),
            Utc::now(),
        )
        .unwrap()
    };

    crate::core::hive::bus::relay_v2::record_protocol(&place.store, true).unwrap();
    assert!(syncs_through_replica(&place.store, &stable, &place.root));
    for trigger in [Trigger::SessionEnd, Trigger::Save, Trigger::Shutdown] {
        assert_eq!(draft(trigger), None, "{trigger:?}: no draft on the replica");
    }
    assert!(queued_drafts(&place).is_empty());

    crate::core::hive::bus::relay_v2::record_protocol(&place.store, false).unwrap();
    assert!(!syncs_through_replica(&place.store, &stable, &place.root));
    assert!(
        draft(Trigger::SessionEnd).is_some(),
        "a 1.x Hive still drafts"
    );
    assert_eq!(queued_drafts(&place).len(), 1);
}
