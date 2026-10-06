// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::policy::runtime::{self, TestPolicyOverride};

fn policy() -> TestPolicyOverride {
    TestPolicyOverride::set(Some(crate::core::policy::load(
        "name='archive-test'\nversion='1.0.0'\ndescription='test'\n[filters]\nclassification='block'\nblocked_labels=['CONFIDENTIAL']\n",
    ).unwrap()))
}

fn scope<T>(root: &std::path::Path, operation: impl FnOnce() -> T) -> T {
    runtime::REQUEST_PROJECT
        .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
}

fn publish(root: &std::path::Path, path: &std::path::Path, output: &str) -> String {
    scope(root, || {
        runtime::with_source_view(|| {
            let mut remaining = crate::core::limits::max_read_bytes();
            let read = crate::tools::ctx_read::read_file_for_tool_rooted_with_path(
                path.to_str().unwrap(),
                root.to_str().unwrap(),
                "ctx_execute",
                &mut remaining,
            )
            .unwrap();
            let authority = authority::ArchiveAuthority::file(
                root,
                &read.canonical_path,
                "ctx_execute",
                &read.content,
            )
            .unwrap();
            store_with_authority(
                "ctx_execute",
                path.to_str().unwrap(),
                output,
                None,
                Some(&authority),
            )
            .unwrap()
            .id
        })
        .unwrap()
    })
}

#[test]
fn file_archive_rechecks_source_bytes_and_allows_identical_restoration() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "PUBLIC source").unwrap();
    let id = publish(root.path(), &path, "archived original payload");
    let original = std::fs::read(content_path(&id)).unwrap();
    scope(root.path(), || {
        assert_eq!(retrieve(&id).as_deref(), Some("archived original payload"));
        for source in ["CONFIDENTIAL\nsource", "different PUBLIC source"] {
            std::fs::write(&path, source).unwrap();
            assert!(retrieve(&id).is_none());
            assert!(list_entries(None).is_empty());
            assert!(crate::core::archive_fts::search("payload", 10).is_empty());
        }
        std::fs::remove_file(&path).unwrap();
        assert!(retrieve(&id).is_none());
        std::fs::write(&path, "PUBLIC source").unwrap();
        assert_eq!(retrieve(&id).as_deref(), Some("archived original payload"));
        assert_eq!(list_entries(None).len(), 1);
        let found = crate::core::archive_fts::search("payload", 10);
        assert_eq!(found.len(), 1);
        assert!(found[0].snippet.contains("payload"));
    });
    assert_eq!(std::fs::read(content_path(&id)).unwrap(), original);
}

#[test]
fn identical_output_does_not_rebind_another_projects_archive() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let pa = a.path().join("source.txt");
    let pb = b.path().join("source.txt");
    std::fs::write(&pa, "same source").unwrap();
    std::fs::write(&pb, "same source").unwrap();
    let aid = publish(a.path(), &pa, "same payload");
    scope(b.path(), || assert!(retrieve(&aid).is_none()));
    let bid = publish(b.path(), &pb, "same payload");
    assert_ne!(aid, bid);
    scope(a.path(), || {
        assert_eq!(retrieve(&aid).as_deref(), Some("same payload"));
        assert!(retrieve(&bid).is_none());
        assert_eq!(list_entries(None).len(), 1);
        assert_eq!(crate::core::archive_fts::search("payload", 10).len(), 1);
    });
    scope(b.path(), || {
        assert_eq!(retrieve(&bid).as_deref(), Some("same payload"));
    });
}

#[test]
fn legacy_recovery_is_preserved_without_policy_and_withheld_under_policy() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let id;
    {
        let _policy = TestPolicyOverride::set(None);
        id = store("ctx_shell", "legacy command", "legacy payload", None).unwrap();
        assert_eq!(retrieve(&id).as_deref(), Some("legacy payload"));
    }
    let bytes = std::fs::read(content_path(&id)).unwrap();
    {
        let _policy = policy();
        scope(root.path(), || {
            assert!(retrieve(&id).is_none());
            assert!(list_entries(None).is_empty());
            assert!(crate::core::archive_fts::search("payload", 10).is_empty());
            assert!(store("ctx_shell", "unbound", "new payload", None).is_none());
        });
    }
    let _policy = TestPolicyOverride::set(None);
    assert_eq!(retrieve(&id).as_deref(), Some("legacy payload"));
    assert_eq!(std::fs::read(content_path(&id)).unwrap(), bytes);
}

#[test]
fn bound_archive_rejects_corruption_and_untrusted_archive_ids() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let id = publish(root.path(), &path, "admitted output");
    std::fs::write(content_path(&id), "substituted output").unwrap();
    scope(root.path(), || {
        assert!(retrieve(&id).is_none());
        for invalid in ["../source", "é", "/tmp/file", "ffff/../../foo"] {
            assert!(retrieve(invalid).is_none());
        }
    });
}

#[test]
fn bound_namespace_never_downgrades_when_metadata_is_lost() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let id = {
        let _policy = policy();
        publish(root.path(), &path, "private archive marker")
    };
    let mut entry = read_entry(&id).unwrap();
    let original = std::fs::read(meta_path(&id)).unwrap();
    let _policy = TestPolicyOverride::set(None);
    scope(root.path(), || {
        assert!(retrieve(&id).is_some());
        let mut unsupported = serde_json::to_value(&entry).unwrap();
        unsupported["authority"]["version"] = serde_json::json!(255);
        std::fs::write(meta_path(&id), serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert!(retrieve(&id).is_none());
        entry.authority = None;
        write_metadata(&entry).unwrap();
        assert!(retrieve(&id).is_none());
        assert!(list_entries(None).is_empty());
        std::fs::write(meta_path(&id), "{broken").unwrap();
        assert!(retrieve(&id).is_none());
        std::fs::remove_file(meta_path(&id)).unwrap();
        assert!(retrieve(&id).is_none());
        assert!(crate::core::archive_fts::search("marker", 10).is_empty());
        std::fs::write(meta_path(&id), original).unwrap();
        assert!(retrieve(&id).is_some());
    });
}

#[test]
fn bound_discovery_does_not_limit_legacy_community_archives() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let id = {
        let _policy = policy();
        publish(root.path(), &path, "boundmarker payload")
    };
    let _policy = TestPolicyOverride::set(None);
    let mut legacy = Vec::new();
    for n in 0..140 {
        legacy.push(
            store(
                "ctx_shell",
                "legacy",
                &format!("legacymarker payload {n}"),
                None,
            )
            .unwrap(),
        );
    }
    // Simulate the obsolete candidate's derived FTS rows, even if its metadata
    // subsequently disappears. Search must remove their ranking/snippet input.
    let db = rusqlite::Connection::open(archive_base_dir().join("index.db")).unwrap();
    db.execute(
        "INSERT INTO archive_meta VALUES (?1,'ctx_execute','private path','2026-01-01')",
        [&id],
    )
    .unwrap();
    db.execute("INSERT INTO archive_fts (archive_id,tool,command,content) VALUES (?1,'ctx_execute','private path','boundmarker payload')", [&id]).unwrap();
    scope(other.path(), || {
        let entries = list_entries(None);
        assert_eq!(entries.len(), 140);
        assert!(!entries.iter().any(|entry| entry.id == id));
        let found = crate::core::archive_fts::search("legacymarker", 200);
        assert_eq!(found.len(), 140);
        assert!(found.iter().any(|entry| entry.archive_id == legacy[0]));
        assert!(crate::core::archive_fts::search("boundmarker", 200).is_empty());
        assert_eq!(crate::core::archive_fts::entry_count(), 140);
    });
    scope(root.path(), || {
        assert_eq!(list_entries(None).len(), 141);
        assert_eq!(crate::core::archive_fts::entry_count(), 141);
        let mixed = crate::core::archive_fts::search("payload", 2);
        assert_eq!(mixed.len(), 2);
        assert_eq!(mixed[0].archive_id, id);
        assert!(legacy.contains(&mixed[1].archive_id));
        assert_eq!(
            crate::core::archive_fts::search("boundmarker", 200).len(),
            1
        );
    });
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM archive_fts WHERE length(archive_id)=64",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    scope(other.path(), || {
        assert_eq!(
            crate::core::archive_fts::search("legacymarker", 200).len(),
            140
        );
        assert_eq!(crate::core::archive_fts::entry_count(), 140);
    });
    db.execute_batch("ROLLBACK").unwrap();
    assert!(content_path(&id).exists());
}

#[test]
fn known_alias_is_authorized_independently_of_discovery_limit() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let oldest = publish(root.path(), &path, "oldest payload");
    let mut entry = read_entry(&oldest).unwrap();
    entry.aliases.push("oldest_job".into());
    write_metadata(&entry).unwrap();
    for n in 0..130 {
        publish(root.path(), &path, &format!("new payload {n}"));
    }
    scope(root.path(), || {
        assert_eq!(list_entries(None).len(), 128);
        assert_eq!(
            resolve_alias("oldest_job").as_deref(),
            Some(oldest.as_str())
        );
        std::fs::remove_file(&path).unwrap();
        assert!(resolve_alias("oldest_job").is_none());
    });
}

#[test]
fn oversized_discovery_entry_does_not_hide_smaller_older_entries() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    // Sizes derive from the real bounds (the test build caps an archive at
    // 1 MiB): newer entries leave less room than `skipped` needs, while the
    // small oldest entry still fits behind it.
    let budget = crate::core::policy::content::MAX_PROTECTED_CONTENT_BYTES;
    let newer_size = MAX_ARCHIVE_SIZE * 9 / 10;
    let newer_count = budget / newer_size;
    let remaining = budget - newer_count * newer_size;
    assert!(
        remaining < MAX_ARCHIVE_SIZE && remaining > 64,
        "test premise"
    );
    let oldest = publish(root.path(), &path, "small oldest payload");
    let skipped = publish(root.path(), &path, &"b".repeat(MAX_ARCHIVE_SIZE));
    let newest: Vec<String> = (0..newer_count)
        .map(|n| {
            publish(
                root.path(),
                &path,
                &format!("{n:08}{}", "a".repeat(newer_size - 8)),
            )
        })
        .collect();
    scope(root.path(), || {
        let ids: Vec<_> = list_entries(None)
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert!(ids.contains(&oldest));
        for id in &newest {
            assert!(ids.contains(id));
        }
        assert!(!ids.contains(&skipped));
        assert!(retrieve(&skipped).is_some());
    });
}

#[tokio::test]
async fn file_handler_publishes_only_a_successful_verified_binding() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let ((_, outcome), authority) = authority::capture(async {
        crate::tools::ctx_execute::handle_file(path.to_str().unwrap(), None, root.path().to_str())
    })
    .await;
    assert_eq!(outcome, crate::server::tool_trait::ShellOutcome::Exit(0));
    assert!(authority.is_some());
    std::fs::write(&path, "CONFIDENTIAL\nsource").unwrap();
    let ((_, outcome), authority) = authority::capture(async {
        crate::tools::ctx_execute::handle_file(path.to_str().unwrap(), None, root.path().to_str())
    })
    .await;
    assert_eq!(outcome, crate::server::tool_trait::ShellOutcome::Blocked);
    assert!(authority.is_none());
}

#[test]
fn source_checks_share_the_original_byte_budget() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "four").unwrap();
    let path = std::fs::canonicalize(path).unwrap();
    let authority = authority::ArchiveAuthority::file(
        root.path(),
        path.to_str().unwrap(),
        "ctx_execute",
        "four",
    )
    .unwrap();
    scope(root.path(), || {
        let mut remaining = 8;
        assert!(authority.admitted_budgeted(&mut remaining));
        assert_eq!(remaining, 4);
        assert!(authority.admitted_budgeted(&mut remaining));
        assert_eq!(remaining, 0);
        assert!(!authority.admitted_budgeted(&mut remaining));
    });
}

#[test]
fn invalid_policy_does_not_fall_back_to_legacy_discovery_or_storage() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    let id = store("ctx_shell", "legacy command", "legacy payload", None).unwrap();
    std::fs::create_dir(root.path().join(".lean-ctx")).unwrap();
    std::fs::write(root.path().join(".lean-ctx/policy.toml"), "[invalid").unwrap();
    scope(root.path(), || {
        assert!(retrieve(&id).is_none());
        assert!(list_entries(None).is_empty());
        assert!(crate::core::archive_fts::search("payload", 10).is_empty());
        assert_eq!(crate::core::archive_fts::entry_count(), 0);
        assert!(store_background("ctx_shell", "shell_invalid", "new payload", None).is_none());
    });
    assert_eq!(
        std::fs::read_to_string(content_path(&id)).unwrap(),
        "legacy payload"
    );
}
