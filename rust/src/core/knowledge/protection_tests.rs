// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::memory_archive::{self, ArchiveConfig, MemoryStore};
use crate::core::memory_policy::MemoryPolicy;

const BASE: &str = "name='knowledge'\nversion='1.0.0'\ndescription='fixture'\n";
const MASK: &str = "[redaction]\ncustomer='CUS-[0-9]{4}'\n";

fn policy(root: &Path, rules: &str) {
    std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
    std::fs::write(root.join(".lean-ctx/policy.toml"), format!("{BASE}{rules}")).unwrap();
}

fn knowledge(root: &Path, value: &str) -> ProjectKnowledge {
    let mut k = ProjectKnowledge::new(root.to_str().unwrap());
    k.remember(
        "finding",
        "account",
        value,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    k
}

fn store(base: &Path, k: &ProjectKnowledge) -> std::path::PathBuf {
    base.join("knowledge")
        .join(&k.project_hash)
        .join("knowledge.json")
}

fn gateway_config(value: &str) {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config directory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.toml"), value).unwrap();
}

fn scope<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    runtime::REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), f)
}

/// A value the PII detector does mask: the standard Luhn-valid test number in
/// groups of four. Assembled at run time, so the source carries no card-shaped
/// literal.
fn card_like() -> String {
    let group = "1".repeat(4);
    format!("4{} {group} {group} {group}", "1".repeat(3))
}

#[test]
fn direct_and_locked_writes_mask_before_publication() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), MASK);
    let k = knowledge(root.path(), "Customer CUS-1234");
    k.save().unwrap();
    let path = store(iso.path(), &k);
    assert!(!std::fs::read_to_string(&path).unwrap().contains("CUS-1234"));
    let (saved, ()) = ProjectKnowledge::mutate_locked(root.path().to_str().unwrap(), |k| {
        k.remember(
            "finding",
            "second",
            "Customer CUS-9999",
            "fixture",
            0.9,
            &MemoryPolicy::default(),
        );
    })
    .unwrap();
    let serialized = serde_json::to_string(&saved).unwrap();
    assert!(serialized.contains("REDACTED") && !serialized.contains("CUS-9999"));
    assert!(!std::fs::read_to_string(path).unwrap().contains("CUS-9999"));
}

#[test]
fn changed_rules_filter_reads_without_overwriting_legacy_values() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let k = knowledge(root.path(), "Customer CUS-1234");
    k.save().unwrap();
    let path = store(iso.path(), &k);
    let before = std::fs::read(&path).unwrap();
    policy(root.path(), MASK);
    let safe = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    assert!(safe.facts.is_empty());
    assert!(matches!(
        safe.withheld.as_slice(),
        [WithheldKnowledge::Fact(fact)] if fact.value == "Customer CUS-1234"
    ));
    assert!(safe.save().is_err());
    let mut called = false;
    assert!(
        ProjectKnowledge::mutate_locked(root.path().to_str().unwrap(), |_| called = true).is_err()
    );
    assert!(!called);
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn revoked_or_invalid_policy_and_foreign_project_cannot_write() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let k = knowledge(root.path(), "safe value");
    k.save().unwrap();
    let path = store(iso.path(), &k);
    let before = std::fs::read(&path).unwrap();
    policy(root.path(), "[context]\ndeny_tools=['ctx_knowledge']\n");
    assert!(ProjectKnowledge::load(root.path().to_str().unwrap()).is_none());
    assert!(k.save().is_err());
    policy(root.path(), "[invalid");
    assert!(k.save().is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
    policy(root.path(), MASK);
    let other = tempfile::tempdir().unwrap();
    let foreign = knowledge(other.path(), "safe value");
    assert!(scope(root.path(), || foreign.save()).is_err());
    assert!(!store(iso.path(), &foreign).exists());
}

#[test]
fn block_and_structural_redaction_preserve_existing_store() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    policy(
        root.path(),
        &format!("{MASK}[filters]\nclassification='block'\n"),
    );
    let mut k = knowledge(root.path(), "safe original");
    k.save().unwrap();
    let path = store(iso.path(), &k);
    let before = std::fs::read(&path).unwrap();
    k.facts[0].value = "safe preview\nCONFIDENTIAL\nCUS-1234".into();
    assert!(k.save().is_err());
    k.facts[0].value = "safe original".into();
    k.facts[0].key = "CUS-1234".into();
    assert!(k.save().is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn archives_preserve_exact_records_and_filter_them_on_restore() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    policy(root.path(), "[redaction]\ncustomer='NO-MATCH-[0-9]{4}'\n");
    policy(other.path(), MASK);
    let path = scope(root.path(), || {
        memory_archive::archive_items(
            MemoryStore::Facts,
            None,
            &[serde_json::json!({"key":"account","value":"CUS-1234 ACC-9876"})],
            &ArchiveConfig::default(),
        )
        .unwrap()
        .unwrap()
    });
    let original = std::fs::read_to_string(&path).unwrap();
    assert!(original.contains("CUS-1234 ACC-9876"));
    policy(
        root.path(),
        "[redaction]\ncustomer='CUS-[0-9]{4}|ACC-[0-9]{4}'\n",
    );
    let restored: Vec<serde_json::Value> =
        scope(root.path(), || memory_archive::restore_items(&path)).unwrap();
    assert!(restored.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(
        scope(other.path(), || memory_archive::restore_items::<
            serde_json::Value,
        >(&path))
        .is_err()
    );
    policy(root.path(), "[context]\ndeny_tools=['ctx_knowledge']\n");
    assert!(
        scope(root.path(), || memory_archive::restore_items::<
            serde_json::Value,
        >(&path))
        .is_err()
    );
}

#[test]
fn failed_archival_restores_the_active_fact_collection() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let mut k = ProjectKnowledge::new(root.path().to_str().unwrap());
    let mut memory = MemoryPolicy::default();
    for key in ["a", "b", "c", "d"] {
        k.remember(
            "finding",
            key,
            "CONFIDENTIAL\nrepeated low value observation",
            "fixture",
            0.1,
            &memory,
        );
    }
    // Model legacy in-memory facts admitted before a stricter project policy.
    // New writes under that policy are now rejected at ingestion itself.
    policy(root.path(), "[filters]\nclassification='block'\n");
    let before = serde_json::to_value(&k.facts).unwrap();
    assert!(!k.facts.is_empty(), "fixture must hold facts to archive");
    let active = current(root.path().to_str().unwrap()).unwrap().unwrap();
    assert!(crate::core::policy::content::evaluate_text(&k.facts[0].value, &active).blocked);
    // The object itself supplies project authority even without an MCP scope.
    memory.lifecycle.low_confidence_threshold = 0.9;
    assert!(k.run_memory_lifecycle(&memory).is_err());
    assert_eq!(serde_json::to_value(&k.facts).unwrap(), before);
    {
        let found = &scope(root.path(), || {
            memory_archive::list_archives(MemoryStore::Facts, None)
        });
        assert!(found.is_empty(), "{found:?}");
    }
}

#[test]
fn explicit_empty_root_recovery_remains_available_only_without_protection() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let memory = MemoryPolicy::default();
    let mut legacy = ProjectKnowledge::new(root.path().to_str().unwrap());
    legacy.remember(
        "finding",
        "legacy",
        "Customer CUS-1234",
        "old",
        0.9,
        &memory,
    );
    // Seed valid serialized legacy data first: current admission correctly
    // rejects constructing new state against the empty, nonexistent root.
    legacy.project_root.clear();
    legacy.project_hash = crate::core::project_hash::hash_path_only("");
    let project = root.path().to_str().unwrap();
    let dir = iso
        .path()
        .join("knowledge")
        .join(crate::core::project_hash::hash_path_only(""));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("knowledge.json");
    let original = serde_json::to_vec(&legacy).unwrap();
    std::fs::write(&path, &original).unwrap();
    policy(root.path(), MASK);
    assert!(ProjectKnowledge::migrate_legacy_empty_root(project, &memory).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::remove_file(root.path().join(".lean-ctx/policy.toml")).unwrap();
    assert!(ProjectKnowledge::migrate_legacy_empty_root(project, &memory).unwrap());
    assert!(
        ProjectKnowledge::load(project)
            .unwrap()
            .facts
            .iter()
            .any(|fact| fact.key == "legacy")
    );
    assert!(!path.exists());
    let backups: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("knowledge.legacy-empty-root.")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(backups[0].path()).unwrap(), original);
}

#[test]
fn tightened_global_gateway_hides_whole_identity_bound_knowledge_records() {
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
    let iso = crate::core::data_dir::isolated_data_dir();
    gateway_config("[context_gateway]\nenabled = false\n");
    let root = tempfile::tempdir().unwrap();
    let mut k = ProjectKnowledge::new(root.path().to_str().unwrap());
    k.remember(
        "finding",
        "secret",
        AWS_KEY,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    let now = chrono::Utc::now();
    k.patterns.push(crate::core::knowledge::ProjectPattern {
        pattern_type: "example".into(),
        description: format!("pattern {AWS_KEY}"),
        examples: vec!["safe example".into()],
        source_session: "fixture".into(),
        created_at: now,
    });
    k.history.push(crate::core::knowledge::ConsolidatedInsight {
        summary: format!("history {AWS_KEY}"),
        from_sessions: vec!["fixture".into()],
        timestamp: now,
    });
    k.save().unwrap();
    let path = store(iso.path(), &k);
    let original = std::fs::read(&path).unwrap();

    gateway_config("[context_gateway]\nmode = \"governed\"\n");
    let view = ProjectKnowledge::load(root.path().to_str().unwrap())
        .expect("the safe view remains readable");
    assert!(view.facts.is_empty());
    assert!(view.patterns.is_empty());
    assert!(view.history.is_empty());
    assert_eq!(view.withheld.len(), 3);
    assert!(view.save().is_err(), "unsafe legacy rows are not rewritten");
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn canonical_project_alias_keeps_source_authority_and_foreign_root_is_denied() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    policy(project.path(), MASK);
    let alias_parent = tempfile::tempdir().unwrap();
    let alias = alias_parent.path().join("project-alias");
    std::os::unix::fs::symlink(project.path(), &alias).unwrap();
    let alias_root = alias.to_string_lossy().to_string();
    let canonical_root = project.path().to_string_lossy().to_string();

    let result = runtime::with_explicit_project_source_view(project.path(), |_| {
        ProjectKnowledge::mutate_locked(&alias_root, |knowledge| {
            knowledge.remember(
                "finding",
                "alias-write",
                "safe through canonical alias",
                "fixture",
                0.9,
                &MemoryPolicy::default(),
            );
        })
    });
    assert!(result.is_ok_and(|inner| inner.is_ok()));

    let foreign = tempfile::tempdir().unwrap();
    let foreign_root = foreign.path().to_string_lossy().to_string();
    let denied = runtime::with_explicit_project_source_view(project.path(), |_| {
        ProjectKnowledge::mutate_locked(&foreign_root, |_| ())
    });
    assert!(matches!(denied, Ok(Err(_))));
    assert!(
        ProjectKnowledge::load(&canonical_root)
            .unwrap()
            .facts
            .iter()
            .any(|fact| fact.key == "alias-write")
    );
    assert!(ProjectKnowledge::load(&foreign_root).is_none());
}

#[test]
fn safe_new_knowledge_is_admitted_under_the_global_gateway() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    gateway_config("[context_gateway]\nmode = \"governed\"\n");
    let root = tempfile::tempdir().unwrap();
    let k = knowledge(root.path(), "ordinary meeting notes");
    assert_eq!(k.facts.len(), 1);
    k.save().unwrap();
    let loaded = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    assert_eq!(loaded.facts.len(), 1);
    assert_eq!(loaded.facts[0].value, "ordinary meeting notes");
}

#[test]
fn deleted_project_root_withholds_new_facts_and_publication() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().to_string_lossy().to_string();
    drop(project);

    let mut knowledge = ProjectKnowledge::new(&project_root);
    knowledge.remember(
        "finding",
        "deleted-root",
        "ordinary value",
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    assert!(
        knowledge.facts.is_empty(),
        "missing project context cannot admit a fact"
    );
    assert!(KnowledgeAdmission::capture(&project_root).is_err());
    assert!(
        knowledge.save().is_err(),
        "publication must fail closed without a project root"
    );
    assert!(!store(iso.path(), &knowledge).exists());
}

// Formerly `strict_engine_ids_round_trip_only_in_typed_knowledge_metadata`.
#[test]
fn engine_ids_round_trip_in_typed_knowledge_metadata_and_other_positions_stay_generic() {
    const ENGINE_ID: &str = "20261004-123406-123456p43210s0";
    let _iso = crate::core::data_dir::isolated_data_dir();
    gateway_config("[context_gateway]\nmode = \"developer\"\npii = \"redact\"\n");
    let generic = crate::core::context_admission::stores::StoreAdmission::current();
    let card = card_like();
    let card_text = format!("paid with {card}");
    assert_ne!(
        generic.admit_text(&card_text).as_deref(),
        Some(card_text.as_str()),
        "generic admission still masks a card number in free text"
    );
    // Changed expectation: this asserted that generic admission masks the id.
    // Since the 3.11 detector rule (`pii::card_layout_plausible`: a separated
    // digit run is a card only in an issuer layout) its `YYYYmmdd-HHMMSS` run
    // is no card, so the id passes generic admission as well.
    assert_eq!(generic.admit_text(ENGINE_ID).as_deref(), Some(ENGINE_ID));

    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_str().unwrap();
    policy(root.path(), "[filters]\npii = \"redact\"\n");
    let mut knowledge = ProjectKnowledge::new(project);
    knowledge.remember(
        "finding",
        "runtime",
        "ordinary value",
        ENGINE_ID,
        0.9,
        &MemoryPolicy::default(),
    );
    let now = chrono::Utc::now();
    knowledge.patterns.push(ProjectPattern {
        pattern_type: "runtime".into(),
        description: "ordinary pattern".into(),
        examples: vec!["safe example".into()],
        source_session: ENGINE_ID.into(),
        created_at: now,
    });
    knowledge.history.push(ConsolidatedInsight {
        summary: "ordinary history".into(),
        from_sessions: vec![ENGINE_ID.into()],
        timestamp: now,
    });
    knowledge.save().unwrap();

    let loaded = ProjectKnowledge::load(project).unwrap();
    assert_eq!(loaded.facts.len(), 1);
    assert_eq!(loaded.facts[0].source_session, ENGINE_ID);
    assert_eq!(loaded.patterns[0].source_session, ENGINE_ID);
    assert_eq!(loaded.history[0].from_sessions, [ENGINE_ID]);

    let admission = KnowledgeAdmission::capture(project).unwrap();
    let safe_legacy_fact = serde_json::json!({
        "key": "legacy",
        "value": "ordinary safe value"
    });
    assert!(admission.admits_archive_fact_value(&safe_legacy_fact));
    assert!(
        !admission.admits_typed_archive_fact_value(&safe_legacy_fact),
        "an incomplete legacy record is generically admissible but cannot bypass diagnostics"
    );
    let fact_value = serde_json::to_value(&loaded.facts[0]).unwrap();
    assert!(admission.admits_archive_fact_value(&fact_value));
    assert!(
        admission.admits_archive_pattern_value(&serde_json::to_value(&loaded.patterns[0]).unwrap())
    );
    assert!(
        admission.admits_archive_history_value(&serde_json::to_value(&loaded.history[0]).unwrap())
    );
    // Changed sample: these three used the id to show where the typed
    // exception ends. Typed and generic admission now agree on every Engine id
    // (3.11 detector rule, see above), so the id cannot show that any more. A
    // card number shows what remains observable: every position outside the
    // exact typed field is under generic admission, and so is the typed field
    // itself for anything that is not an Engine id.
    let mut card_session = fact_value.clone();
    card_session["source_session"] = serde_json::json!(format!("imported {card}"));
    assert!(
        !admission.admits_archive_fact_value(&card_session),
        "free text in the typed field is not an Engine id"
    );
    assert!(
        !admission.admits_archive_pattern_value(&card_session),
        "a fact schema read as a Pattern uses generic admission"
    );
    let mut extra_field = fact_value.clone();
    extra_field["unknown"] = serde_json::json!(card);
    assert!(
        !admission.admits_archive_fact_value(&extra_field),
        "unknown top-level archive fields use generic admission"
    );
    assert!(
        !admission.admits_archive_fact_value(&serde_json::json!({
            "metadata": {"source_session": card}
        })),
        "arbitrary nested JSON uses generic admission"
    );
    // Changed expectation: with the id in the unknown field this record was
    // refused. Generic admission no longer objects to the id, so it is kept.
    let mut id_in_unknown_field = fact_value.clone();
    id_in_unknown_field["unknown"] = serde_json::json!(ENGINE_ID);
    assert!(admission.admits_archive_fact_value(&id_in_unknown_field));

    let archives = scope(root.path(), || {
        (
            memory_archive::archive_items(
                MemoryStore::Facts,
                None,
                &loaded.facts,
                &ArchiveConfig::default(),
            )
            .unwrap()
            .unwrap(),
            memory_archive::archive_items(
                MemoryStore::History,
                Some(&loaded.project_hash),
                &loaded.history,
                &ArchiveConfig::default(),
            )
            .unwrap()
            .unwrap(),
            memory_archive::archive_items(
                MemoryStore::Patterns,
                Some(&loaded.project_hash),
                &loaded.patterns,
                &ArchiveConfig::default(),
            )
            .unwrap()
            .unwrap(),
        )
    });
    let restored = scope(root.path(), || {
        (
            memory_archive::restore_items::<KnowledgeFact>(&archives.0).unwrap(),
            memory_archive::restore_items::<ConsolidatedInsight>(&archives.1).unwrap(),
            memory_archive::restore_items::<ProjectPattern>(&archives.2).unwrap(),
        )
    });
    for archive in [&archives.0, &archives.1, &archives.2] {
        assert!(
            std::fs::read_to_string(archive)
                .unwrap()
                .contains(ENGINE_ID),
            "typed archive identity must survive global and project PII policy"
        );
    }
    assert_eq!(restored.0[0].source_session, ENGINE_ID);
    assert_eq!(restored.1[0].from_sessions, [ENGINE_ID]);
    assert_eq!(restored.2[0].source_session, ENGINE_ID);
}

#[test]
fn engine_id_exception_keeps_project_rules_and_free_text_admission() {
    const ENGINE_ID: &str = "20261004-123406-123456p43210s0";
    let _iso = crate::core::data_dir::isolated_data_dir();
    gateway_config("[context_gateway]\nmode = \"developer\"\npii = \"redact\"\n");

    // Changed sample: the free text carried the Engine id, whose date/time run
    // the detector took for a card. Since the 3.11 detector rule
    // (`pii::card_layout_plausible`) it does not, so the free-text cases use a
    // card number; the typed id stays the sample of the project-rule case.
    let card = card_like();
    let free_text_root = tempfile::tempdir().unwrap();
    let free_text_project = free_text_root.path().to_str().unwrap();
    let mut free_text = ProjectKnowledge::new(free_text_project);
    free_text.remember(
        "finding",
        "runtime",
        "ordinary value",
        &format!("imported session {card}"),
        0.9,
        &MemoryPolicy::default(),
    );
    assert!(free_text.save().is_err());

    let value_root = tempfile::tempdir().unwrap();
    let value_project = value_root.path().to_str().unwrap();
    let mut value = ProjectKnowledge::new(value_project);
    value.remember(
        "finding",
        "runtime",
        &card,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    assert!(
        value
            .facts
            .iter()
            .all(|fact| !fact.value.contains(card.as_str()))
    );

    // Changed expectation: the id as a fact value used to be masked. It is no
    // card under the 3.11 rule and is stored as written.
    let id_value_root = tempfile::tempdir().unwrap();
    let mut id_value = ProjectKnowledge::new(id_value_root.path().to_str().unwrap());
    id_value.remember(
        "finding",
        "runtime",
        ENGINE_ID,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
    assert!(id_value.facts.iter().any(|fact| fact.value == ENGINE_ID));

    let blocked_root = tempfile::tempdir().unwrap();
    let blocked_project = blocked_root.path().to_str().unwrap();
    policy(
        blocked_root.path(),
        &format!("[redaction]\nengine_session='{ENGINE_ID}'\n"),
    );
    let mut blocked = ProjectKnowledge::new(blocked_project);
    blocked.remember(
        "finding",
        "runtime",
        "ordinary value",
        ENGINE_ID,
        0.9,
        &MemoryPolicy::default(),
    );
    assert!(blocked.save().is_err());
}
