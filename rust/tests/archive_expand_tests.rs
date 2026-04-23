use lean_ctx::core::archive;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_test_dir<F: FnOnce()>(f: F) {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("LEAN_CTX_DATA_DIR", dir.path());
    std::env::set_var("LEAN_CTX_ARCHIVE", "1");
    f();
    std::env::remove_var("LEAN_CTX_DATA_DIR");
    std::env::remove_var("LEAN_CTX_ARCHIVE");
}

#[test]
fn archive_store_and_retrieve() {
    with_test_dir(|| {
        let content = "Hello from archive test\nLine 2\nLine 3";
        let id = archive::store("ctx_shell", "echo test", content, None).unwrap();
        assert!(!id.is_empty());
        assert_eq!(archive::retrieve(&id).unwrap(), content);
    });
}

#[test]
fn archive_retrieve_range() {
    with_test_dir(|| {
        let content = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let id = archive::store("ctx_read", "cat file", &content, None).unwrap();
        let range = archive::retrieve_with_range(&id, 5, 10).unwrap();
        assert!(range.contains("line 5"), "expected line 5 in: {range}");
        assert!(range.contains("line 10"), "expected line 10 in: {range}");
        assert!(
            !range.contains("line 4"),
            "should not contain line 4: {range}"
        );
        assert!(
            !range.contains("line 11"),
            "should not contain line 11: {range}"
        );
    });
}

#[test]
fn archive_search() {
    with_test_dir(|| {
        let content = "INFO: ok\nWARN: check\nERROR: fail\nINFO: done\nERROR: crash";
        let id = archive::store("ctx_shell", "run", content, None).unwrap();
        let result = archive::retrieve_with_search(&id, "ERROR").unwrap();
        assert!(result.contains("2 match"), "expected 2 matches: {result}");
        assert!(result.contains("ERROR: fail"));
        assert!(result.contains("ERROR: crash"));
    });
}

#[test]
fn archive_search_no_match() {
    with_test_dir(|| {
        let content = "just some normal output";
        let id = archive::store("ctx_shell", "cmd", content, None).unwrap();
        let result = archive::retrieve_with_search(&id, "NOTFOUND").unwrap();
        assert!(result.contains("No matches"));
    });
}

#[test]
fn archive_idempotent() {
    with_test_dir(|| {
        let content = "same content for idempotency test";
        let id1 = archive::store("ctx_shell", "cmd1", content, None).unwrap();
        let id2 = archive::store("ctx_shell", "cmd2", content, None).unwrap();
        assert_eq!(id1, id2, "same content should produce same ID");
    });
}

#[test]
fn archive_session_filtering() {
    with_test_dir(|| {
        archive::store(
            "ctx_shell",
            "c1",
            "content-alpha-unique-test",
            Some("session-a"),
        )
        .unwrap();
        archive::store(
            "ctx_shell",
            "c2",
            "content-beta-unique-test",
            Some("session-b"),
        )
        .unwrap();
        archive::store(
            "ctx_read",
            "c3",
            "content-gamma-unique-test",
            Some("session-a"),
        )
        .unwrap();

        let all = archive::list_entries(None);
        assert_eq!(all.len(), 3);

        let sess_a = archive::list_entries(Some("session-a"));
        assert_eq!(sess_a.len(), 2);
        assert!(sess_a
            .iter()
            .all(|e| e.session_id.as_deref() == Some("session-a")));
    });
}

#[test]
fn archive_cleanup_expired() {
    with_test_dir(|| {
        let id = archive::store("ctx_shell", "old", "old-content-for-cleanup-test", None).unwrap();
        let meta = archive::list_entries(None);
        assert_eq!(meta.len(), 1);

        // Manually backdate the entry
        let data_dir = std::env::var("LEAN_CTX_DATA_DIR").unwrap();
        let prefix = &id[..2];
        let meta_path = std::path::PathBuf::from(&data_dir)
            .join("archives")
            .join(prefix)
            .join(format!("{id}.meta.json"));
        let mut entry: archive::ArchiveEntry =
            serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
        entry.created_at = chrono::Utc::now() - chrono::Duration::hours(999);
        std::fs::write(&meta_path, serde_json::to_string(&entry).unwrap()).unwrap();

        let removed = archive::cleanup();
        assert!(removed >= 1, "expected cleanup to remove expired entry");
        assert!(
            archive::retrieve(&id).is_none(),
            "content should be gone after cleanup"
        );
    });
}

#[test]
fn archive_nonexistent_returns_none() {
    assert!(archive::retrieve("does_not_exist_abc123").is_none());
}

#[test]
fn archive_format_hint_contains_expand() {
    let hint = archive::format_hint("test123", 8000, 2000);
    assert!(hint.contains("ctx_expand"));
    assert!(hint.contains("test123"));
    assert!(hint.contains("8000"));
    assert!(hint.contains("2000"));
}

#[test]
fn archive_should_archive_respects_threshold() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_ARCHIVE", "1");
    std::env::set_var("LEAN_CTX_ARCHIVE_THRESHOLD", "100");
    assert!(!archive::should_archive("short"));
    assert!(archive::should_archive(&"x".repeat(101)));
    std::env::remove_var("LEAN_CTX_ARCHIVE_THRESHOLD");
    std::env::remove_var("LEAN_CTX_ARCHIVE");
}

#[test]
fn archive_disabled_returns_false() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("LEAN_CTX_ARCHIVE", "0");
    assert!(!archive::should_archive(&"x".repeat(99999)));
    std::env::remove_var("LEAN_CTX_ARCHIVE");
}

#[test]
fn archive_disk_usage_starts_zero() {
    with_test_dir(|| {
        assert_eq!(archive::disk_usage_bytes(), 0);
        archive::store("ctx_shell", "test", "some content for size", None).unwrap();
        assert!(archive::disk_usage_bytes() > 0);
    });
}
