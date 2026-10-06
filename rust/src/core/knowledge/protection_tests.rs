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

fn scope<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    runtime::REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), f)
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
    assert!(safe.facts[0].value.contains("REDACTED"));
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
fn archives_are_scoped_masked_and_reauthorized_on_restore() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    policy(root.path(), MASK);
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
    assert!(!original.contains("CUS-1234"));
    assert!(original.contains("ACC-9876"));
    policy(
        root.path(),
        "[redaction]\ncustomer='CUS-[0-9]{4}|ACC-[0-9]{4}'\n",
    );
    let restored: Vec<serde_json::Value> =
        scope(root.path(), || memory_archive::restore_items(&path)).unwrap();
    assert!(
        !serde_json::to_string(&restored)
            .unwrap()
            .contains("ACC-9876")
    );
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
    policy(root.path(), "[filters]\nclassification='block'\n");
    let mut k = ProjectKnowledge::new(root.path().to_str().unwrap());
    let mut memory = MemoryPolicy::default();
    memory.compaction.enabled = true;
    memory.compaction.min_cluster = 3;
    memory.compaction.similarity = 0.5;
    memory.compaction.max_confidence = 0.5;
    memory.compaction.max_confirmations = 2;
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
    let before = serde_json::to_value(&k.facts).unwrap();
    let mut candidate = k.facts.clone();
    let (collapsed, archived) = crate::core::memory_lifecycle::compact_clusters(
        &mut candidate,
        &crate::core::memory_lifecycle::ClusterCompactionConfig {
            min_cluster: 3,
            similarity: 0.5,
            max_confidence: 0.5,
            max_confirmations: 2,
        },
    );
    assert!(
        collapsed > 0 && !archived.is_empty(),
        "fixture must attempt archival"
    );
    let active = current(root.path().to_str().unwrap()).unwrap().unwrap();
    assert!(crate::core::policy::content::evaluate_text(&archived[0].value, &active).blocked);
    // The object itself supplies project authority even without an MCP scope.
    assert_eq!(k.compact_low_value_clusters(&memory), 0);
    assert_eq!(serde_json::to_value(&k.facts).unwrap(), before);
    memory.lifecycle.low_confidence_threshold = 0.9;
    assert!(k.run_memory_lifecycle(&memory).is_err());
    assert_eq!(serde_json::to_value(&k.facts).unwrap(), before);
    assert!(
        scope(root.path(), || memory_archive::list_archives(
            MemoryStore::Facts,
            None
        ))
        .is_empty()
    );
}

#[test]
fn explicit_empty_root_recovery_remains_available_only_without_protection() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_str().unwrap();
    let memory = MemoryPolicy::default();
    let mut legacy = ProjectKnowledge::new("");
    legacy.remember(
        "finding",
        "legacy",
        "Customer CUS-1234",
        "old",
        0.9,
        &memory,
    );
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
