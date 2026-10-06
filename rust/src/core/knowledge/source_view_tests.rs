// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::knowledge::FactOrigin;
use crate::core::memory_policy::MemoryPolicy;

fn setup() -> (
    crate::core::data_dir::IsolatedDataDir,
    tempfile::TempDir,
    ProjectKnowledge,
) {
    let data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".lean-ctx")).unwrap();
    std::fs::write(
        root.path().join(".lean-ctx/policy.toml"),
        "name='origin'\nversion='1.0.0'\ndescription='fixture'\n",
    )
    .unwrap();
    let mut knowledge = ProjectKnowledge::new(root.path().to_str().unwrap());
    knowledge.remember_with_origin(
        "finding",
        "remote",
        "withheld issue title",
        "provider-ingest-old",
        0.9,
        &MemoryPolicy::default(),
        FactOrigin::Unverified,
    );
    (data, root, knowledge)
}

#[test]
fn source_view_keeps_hidden_records_on_unrelated_atomic_write() {
    let (_data, root, knowledge) = setup();
    knowledge.save().unwrap();
    let view = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    assert!(view.facts.is_empty());
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("withheld issue title")
    );
    ProjectKnowledge::mutate_locked(root.path().to_str().unwrap(), |current| {
        current.remember(
            "decision",
            "local",
            "use a bounded parser",
            "local-session",
            0.9,
            &MemoryPolicy::default(),
        );
    })
    .unwrap();
    let raw = ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    assert_eq!(raw.facts.len(), 2);
    assert!(
        raw.facts
            .iter()
            .any(|f| same_record(f, &knowledge.facts[0]))
    );
    let view = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    assert_eq!(view.facts.len(), 1);
    assert_eq!(view.facts[0].key, "local");
}

#[test]
fn stale_view_cannot_resurrect_an_operator_removed_hidden_record() {
    let (_data, root, knowledge) = setup();
    knowledge.save().unwrap();
    let view = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    let mut raw =
        ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    raw.facts.clear();
    raw.save().unwrap();
    assert!(view.save().unwrap_err().contains("stale"));
    assert!(
        ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap())
            .unwrap()
            .facts
            .is_empty()
    );
}

#[test]
fn authored_note_does_not_adopt_unknown_fact_on_confirmation() {
    let (_data, _root, mut knowledge) = setup();
    knowledge.remember(
        "finding",
        "remote",
        "withheld issue title",
        "local-session",
        0.9,
        &MemoryPolicy::default(),
    );
    assert_eq!(knowledge.facts.len(), 2);
    assert_eq!(knowledge.facts[0].origin, FactOrigin::Unverified);
    assert_eq!(knowledge.facts[0].source_session, "provider-ingest-old");
    assert_eq!(knowledge.facts[1].origin, FactOrigin::Local);
}

#[test]
fn hidden_counter_updates_survive_a_stale_view_save() {
    let (_data, root, knowledge) = setup();
    knowledge.save().unwrap();
    let view = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    let mut latest =
        ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    latest.facts[0].confirmation_count += 1;
    latest.facts[0].valid_until = Some(chrono::Utc::now());
    latest.save().unwrap();
    view.save().unwrap();
    let saved = ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&saved.facts).unwrap(),
        serde_json::to_value(&latest.facts).unwrap()
    );
}

#[test]
fn hidden_content_changes_require_reload_without_overwriting() {
    let (_data, root, knowledge) = setup();
    knowledge.save().unwrap();
    let view = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    let mut latest =
        ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    latest.facts[0].value = "updated remote finding".into();
    latest.save().unwrap();
    assert!(view.save().unwrap_err().contains("stale"));
    let saved = ProjectKnowledge::load_for_checked_capture(root.path().to_str().unwrap()).unwrap();
    assert_eq!(saved.facts[0].value, "updated remote finding");
}

#[test]
fn derived_origin_is_not_downgraded_when_policy_is_absent() {
    let (_data, root, mut knowledge) = setup();
    std::fs::remove_file(root.path().join(".lean-ctx/policy.toml")).unwrap();
    knowledge.facts[0].origin = FactOrigin::Derived(vec![FactOrigin::Unverified]);
    let view = knowledge.admit_sources();
    assert!(view.facts.is_empty());
    assert_eq!(view.withheld.len(), 1);
}

#[test]
fn missing_or_imported_local_origin_is_not_a_source_authorization() {
    let (_data, _root, knowledge) = setup();
    let mut value = serde_json::to_value(&knowledge.facts[0]).unwrap();
    value.as_object_mut().unwrap().remove("origin");
    let old: KnowledgeFact = serde_json::from_value(value).unwrap();
    assert_eq!(old.origin, FactOrigin::Unverified);
    assert_eq!(imported_origin(&FactOrigin::Local), FactOrigin::Unverified);
}

#[test]
fn source_view_cannot_admit_foreign_local_notes_in_a_bound_protected_request() {
    let (_data, root, _) = setup();
    let foreign = tempfile::tempdir().unwrap();
    let mut knowledge = ProjectKnowledge::new(foreign.path().to_str().unwrap());
    knowledge.remember(
        "decision",
        "foreign",
        "private other project note",
        "local",
        0.9,
        &MemoryPolicy::default(),
    );
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_owned())),
        || {
            let view = knowledge.admit_sources();
            assert!(view.facts.is_empty());
            assert_eq!(view.withheld.len(), 1);
        },
    );
}
