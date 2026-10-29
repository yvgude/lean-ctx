// SPDX-License-Identifier: Apache-2.0
//! Project-root transitions at the knowledge boundary (Q14.4): a store that
//! was written while its project root existed must stay byte-identical once
//! the root is deleted or becomes unreadable, and nothing new is admitted.

use super::*;
use crate::core::memory_policy::MemoryPolicy;

fn remember(knowledge: &mut ProjectKnowledge, key: &str, value: &str) {
    knowledge.remember(
        "finding",
        key,
        value,
        "fixture",
        0.9,
        &MemoryPolicy::default(),
    );
}

fn store(base: &Path, knowledge: &ProjectKnowledge) -> std::path::PathBuf {
    base.join("knowledge")
        .join(&knowledge.project_hash)
        .join("knowledge.json")
}

/// Every `knowledge.json` below the isolated data directory.
fn knowledge_stores(base: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(projects) = std::fs::read_dir(base.join("knowledge")) else {
        return found;
    };
    for project in projects.flatten() {
        let candidate = project.path().join("knowledge.json");
        if candidate.exists() {
            found.push(candidate);
        }
    }
    found.sort();
    found
}

/// The assertions shared by both transitions: with the project context gone,
/// no fact is admitted, no publication path writes, and the store written
/// earlier keeps its exact bytes.
fn assert_withheld_and_unchanged(
    base: &Path,
    root: &str,
    knowledge: &mut ProjectKnowledge,
    path: &Path,
    before: &[u8],
) {
    let facts_before = serde_json::to_value(&knowledge.facts).unwrap();
    remember(knowledge, "after-transition", "second ordinary value");
    assert_eq!(
        serde_json::to_value(&knowledge.facts).unwrap(),
        facts_before,
        "missing project context cannot admit or alter a fact"
    );
    assert!(
        knowledge
            .facts
            .iter()
            .all(|fact| fact.key != "after-transition")
    );

    assert!(KnowledgeAdmission::capture(root).is_err());
    assert!(
        knowledge.save().is_err(),
        "publication must fail closed without a usable project root"
    );
    let mut called = false;
    assert!(ProjectKnowledge::mutate_locked(root, |_| called = true).is_err());
    assert!(!called, "a refused mutation never runs its closure");
    assert!(
        ProjectKnowledge::load(root).is_none(),
        "the store is not served without its project context"
    );

    assert_eq!(
        std::fs::read(path).unwrap(),
        before,
        "the existing store must be byte-identical"
    );
    assert_eq!(
        knowledge_stores(base),
        [path.to_path_buf()],
        "no second store may appear under another project hash"
    );
}

#[test]
fn root_deleted_after_a_saved_fact_withholds_new_facts_and_keeps_the_store() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();

    let mut knowledge = ProjectKnowledge::new(&root);
    remember(&mut knowledge, "before-transition", "ordinary value");
    assert_eq!(knowledge.facts.len(), 1, "the root exists: fact admitted");
    knowledge.save().unwrap();
    let path = store(iso.path(), &knowledge);
    let before = std::fs::read(&path).unwrap();
    assert!(
        String::from_utf8_lossy(&before).contains("before-transition"),
        "the fixture store holds the saved fact"
    );

    std::fs::remove_dir_all(project.path()).unwrap();
    assert!(!project.path().exists());

    assert_withheld_and_unchanged(iso.path(), &root, &mut knowledge, &path, &before);
}

#[cfg(unix)]
struct RestorePermissions(std::path::PathBuf);

#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

#[cfg(unix)]
#[test]
fn unreadable_project_root_fails_closed_and_keeps_the_store() {
    use std::os::unix::fs::PermissionsExt;

    let iso = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().to_string();

    let mut knowledge = ProjectKnowledge::new(&root);
    remember(&mut knowledge, "before-transition", "ordinary value");
    assert_eq!(
        knowledge.facts.len(),
        1,
        "the root is readable: fact admitted"
    );
    knowledge.save().unwrap();
    let path = store(iso.path(), &knowledge);
    let before = std::fs::read(&path).unwrap();

    // The root still exists, but its policy inputs can no longer be inspected.
    std::fs::set_permissions(project.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestorePermissions(project.path().to_path_buf());
    if std::fs::read_dir(project.path()).is_ok() {
        // A privileged process is not bound by permission bits, so the
        // unreadable state cannot be produced here.
        return;
    }
    assert!(project.path().exists());

    assert_withheld_and_unchanged(iso.path(), &root, &mut knowledge, &path, &before);
}
