// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::{
    auto_capture, auto_findings::AutoFinding, debug_log, journal, knowledge::ProjectKnowledge,
};

const BASE: &str = "name='diagnostics'\nversion='1.0.0'\ndescription='fixture'\n";
const MASK: &str = "[redaction]\ncustomer='CUS-[0-9]{4}|^731902$'\n";

fn scope<T>(root: &Path, action: impl FnOnce() -> T) -> T {
    runtime::REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), action)
}

fn policy(root: &Path, rules: &str) {
    let dir = root.join(".lean-ctx");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("policy.toml"), format!("{BASE}{rules}")).unwrap();
}

struct Enabled;
impl Enabled {
    fn new() -> Self {
        for name in [
            "LEAN_CTX_DEBUG_LOG",
            "LEAN_CTX_JOURNAL",
            "LEAN_CTX_AUTO_CAPTURE",
        ] {
            crate::test_env::set_var(name, "1");
        }
        Self
    }
}
impl Drop for Enabled {
    fn drop(&mut self) {
        for name in [
            "LEAN_CTX_DEBUG_LOG",
            "LEAN_CTX_JOURNAL",
            "LEAN_CTX_AUTO_CAPTURE",
        ] {
            crate::test_env::remove_var(name);
        }
    }
}

#[test]
fn nested_fields_numbers_keys_and_collisions_are_checked_before_preview() {
    let active = runtime::ActivePolicy::from_resolved(
        super::super::resolve(&super::super::parse(&format!("{BASE}{MASK}")).unwrap()).unwrap(),
    );
    let secret = format!("sk-proj-{}", "aB3_".repeat(9));
    let raw =
        serde_json::json!({"CUS-1234": [731902, {"value":"CUS-9999", "nested": [secret.clone()]}]});
    let safe = inspect(&raw, Some(&active)).unwrap().to_string();
    assert!(!safe.contains(&secret));
    for marker in ["CUS-1234", "CUS-9999", "731902"] {
        assert!(!safe.contains(marker));
    }
    assert!(safe.contains("REDACTED"));
    assert!(
        inspect(
            &serde_json::json!({"CUS-1234":1,"CUS-9999":2}),
            Some(&active)
        )
        .is_none()
    );
    let blocked = runtime::ActivePolicy::from_resolved(
        super::super::resolve(
            &super::super::parse(&format!("{BASE}[filters]\nclassification='block'\n")).unwrap(),
        )
        .unwrap(),
    );
    assert!(inspect(&serde_json::json!({"nested":[format!("{}\nCONFIDENTIAL\nCUS-1234", "safe ".repeat(100))]}), Some(&blocked)).is_none());
    assert!(
        fields(
            &[(
                "result",
                &"x".repeat(content::MAX_PROTECTED_CONTENT_BYTES + 1)
            )],
            None
        )
        .is_none()
    );
    assert!(inspect(&serde_json::json!(["a\0b"]), Some(&active)).is_none());
}

#[test]
fn real_logs_are_project_scoped_and_rechecked_before_read_and_append() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    policy(a.path(), MASK);
    policy(b.path(), MASK);
    std::fs::create_dir_all(iso.path().join("logs")).unwrap();
    std::fs::write(iso.path().join("logs/debug.log"), "LEGACY-CUS-1234").unwrap();
    let a_log = scope(a.path(), || {
        let args = serde_json::json!({"nested":{"CUS-1234":[731902,"CUS-9999"]}});
        debug_log::log_mcp_call(
            "ctx_read",
            args.as_object(),
            "CUS-1234 ACC-5678",
            20,
            0,
            Duration::ZERO,
        );
        journal::log_tool_call("ctx_read", "CUS-1234 ACC-5678");
        let path = debug_log::log_path().unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        for marker in ["CUS-1234", "CUS-9999", "731902", "LEGACY"] {
            assert!(!raw.contains(marker));
        }
        assert!(raw.contains("ACC-5678"));
        assert!(!journal::read_journal(0).contains("CUS-1234"));
        path
    });
    let b_log = scope(b.path(), || {
        debug_log::log_mcp_call("ctx_read", None, "team-beta", 9, 0, Duration::ZERO);
        debug_log::log_path().unwrap()
    });
    assert_ne!(a_log, b_log);
    policy(
        a.path(),
        "[redaction]\ncustomer='CUS-[0-9]{4}|ACC-[0-9]{4}'\n",
    );
    scope(a.path(), || {
        let read = debug_log::read_log(0);
        assert!(
            !read.contains("ACC-5678") && !read.contains("team-beta") && !read.contains("LEGACY")
        );
        assert!(!journal::read_journal(0).contains("ACC-5678"));
        debug_log::log_mcp_call("ctx_read", None, "next safe record", 16, 0, Duration::ZERO);
        journal::log_tool_call("ctx_read", "next safe record");
        assert!(
            !std::fs::read_to_string(&a_log)
                .unwrap()
                .contains("ACC-5678")
        );
    });
}

#[test]
fn classification_beyond_preview_blocks_actual_debug_and_journal_writes() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), "[filters]\nclassification='block'\n");
    scope(root.path(), || {
        debug_log::log_mcp_call("ctx_read", None, "initial safe", 12, 0, Duration::ZERO);
        journal::log_tool_call("ctx_read", "initial safe");
        let path = debug_log::log_path().unwrap();
        let before = std::fs::read(&path).unwrap();
        let journal_before = journal::read_journal(0);
        let blocked = format!(
            "preview that looks safe\n{}\nCONFIDENTIAL\nCUS-1234",
            "x".repeat(500)
        );
        debug_log::log_mcp_call("ctx_read", None, &blocked, blocked.len(), 0, Duration::ZERO);
        debug_log::log_mcp_error(
            "ctx_read",
            serde_json::json!({"nested":[blocked]}).as_object(),
            "safe error",
        );
        journal::log_tool_call("ctx_read", &blocked);
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(journal::read_journal(0), journal_before);
    });
}

#[test]
fn queued_writer_observes_new_rules_and_cannot_use_a_legacy_global_target() {
    use fs2::FileExt;
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let base = iso.path().join("probe.log");
    let legacy = scope(root.path(), || target(base.clone()).unwrap());
    policy(root.path(), "");
    let mut entered = false;
    legacy.with_lock(|| entered = true);
    assert!(!entered && !base.exists());
    let target = scope(root.path(), || target(base).unwrap());
    std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
    let lock = open_append(&target.path.with_extension("lock")).unwrap();
    lock.lock_exclusive().unwrap();
    let output_path = target.path.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(()).unwrap();
        target.with_lock(|| {
            use std::io::Write;
            let Value::String(safe) = target
                .inspect(Some("ctx_read"), &Value::String("CUS-1234".into()))
                .unwrap()
            else {
                panic!()
            };
            open_append(&target.path)
                .unwrap()
                .write_all(safe.as_bytes())
                .unwrap();
        });
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    policy(root.path(), MASK);
    FileExt::unlock(&lock).unwrap();
    worker.join().unwrap();
    let result = std::fs::read_to_string(output_path).unwrap();
    assert!(result.contains("REDACTED") && !result.contains("CUS-1234"));
}

#[test]
fn automatic_capture_checks_fields_and_does_not_republish_unsafe_legacy_data() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), MASK);
    let finding = AutoFinding {
        file: Some("CUS-1234.rs".into()),
        summary: "test CUS-9999".into(),
    };
    auto_capture::capture_finding_from_tool(
        root.path().to_str().unwrap(),
        &finding,
        "ctx_read",
        &finding.summary,
    );
    let loaded = ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap();
    let serialized = serde_json::to_string(&loaded).unwrap();
    assert!(!serialized.contains("CUS-1234") && !serialized.contains("CUS-9999"));
    assert!(serialized.contains("REDACTED"));
    let active_path = iso
        .path()
        .join("knowledge")
        .join(&loaded.project_hash)
        .join("knowledge.json");
    let before_block = std::fs::read(&active_path).unwrap();
    policy(
        root.path(),
        &format!("{MASK}[filters]\nclassification='block'\n"),
    );
    auto_capture::capture_finding_from_tool(
        root.path().to_str().unwrap(),
        &AutoFinding {
            file: None,
            summary: "safe extracted preview".into(),
        },
        "ctx_read",
        "safe preview\nCONFIDENTIAL\nCUS-1234",
    );
    assert_eq!(std::fs::read(active_path).unwrap(), before_block);
    let other = tempfile::tempdir().unwrap();
    let mut legacy = ProjectKnowledge::load_or_create(other.path().to_str().unwrap());
    legacy.remember(
        "finding",
        "old",
        "CUS-1234",
        "fixture",
        0.9,
        &crate::core::memory_policy::MemoryPolicy::default(),
    );
    legacy.save().unwrap();
    let path = iso
        .path()
        .join("knowledge")
        .join(&legacy.project_hash)
        .join("knowledge.json");
    let before = std::fs::read(&path).unwrap();
    policy(other.path(), MASK);
    auto_capture::capture_finding(
        other.path().to_str().unwrap(),
        &AutoFinding {
            file: None,
            summary: "safe new finding".into(),
        },
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
    );
}

#[test]
fn adding_policy_preserves_existing_safe_automatic_fact_keys() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    let finding = AutoFinding {
        file: Some("safe.rs".into()),
        summary: "safe finding".into(),
    };
    auto_capture::capture_finding(root.path().to_str().unwrap(), &finding);
    policy(root.path(), MASK);
    auto_capture::capture_finding(root.path().to_str().unwrap(), &finding);
    let data = serde_json::to_value(ProjectKnowledge::load(root.path().to_str().unwrap()).unwrap())
        .unwrap();
    let facts = data["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["key"], "auto:safe.rs");
}

#[test]
fn source_revocation_while_capture_waits_prevents_the_write() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), "");
    let path = root.path().to_str().unwrap().to_owned();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let owner_path = path.clone();
    let owner = std::thread::spawn(move || {
        ProjectKnowledge::with_project_lock_checked(&owner_path, || {
            ready_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        })
        .unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let worker = std::thread::spawn(move || {
        auto_capture::capture_finding_from_tool(
            &path,
            &AutoFinding {
                file: None,
                summary: "CUS-1234".into(),
            },
            "ctx_read",
            "CUS-1234",
        );
    });
    policy(root.path(), "[context]\ndeny_tools=['ctx_read']\n");
    release_tx.send(()).unwrap();
    owner.join().unwrap();
    worker.join().unwrap();
    let store = ProjectKnowledge::load_or_create(root.path().to_str().unwrap());
    assert!(
        !iso.path()
            .join("knowledge")
            .join(store.project_hash)
            .join("knowledge.json")
            .exists()
    );
}

#[test]
fn existing_diagnostics_are_filtered_before_rotation_to_archives() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), MASK);
    scope(root.path(), || {
        let debug = debug_log::log_path().unwrap();
        std::fs::write(&debug, format!("ACC-5678\n{}", "x".repeat(5 * 1024 * 1024))).unwrap();
        let journal_target = target(iso.path().join("journal.md")).unwrap();
        std::fs::create_dir_all(journal_target.path.parent().unwrap()).unwrap();
        std::fs::write(
            &journal_target.path,
            format!("ACC-5678\n{}", "x".repeat(4 * 1024 * 1024)),
        )
        .unwrap();
        policy(
            root.path(),
            "[redaction]\ncustomer='CUS-[0-9]{4}|ACC-[0-9]{4}'\n",
        );
        debug_log::log_mcp_call("ctx_read", None, "next record", 11, 0, Duration::ZERO);
        journal::log_tool_call("ctx_read", "next record");
        let backup = debug.with_file_name("debug.log.1");
        assert!(backup.exists());
        assert!(
            !std::fs::read_to_string(backup)
                .unwrap()
                .contains("ACC-5678")
        );
        let archives: Vec<_> = std::fs::read_dir(journal_target.path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("journal-"))
            .collect();
        assert_eq!(archives.len(), 1);
        assert!(
            !std::fs::read_to_string(archives[0].path())
                .unwrap()
                .contains("ACC-5678")
        );
    });
}

#[cfg(unix)]
#[test]
fn diagnostic_symlinks_cannot_read_or_modify_a_different_file() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    policy(root.path(), MASK);
    let victim = iso.path().join("victim");
    std::fs::write(&victim, "CUS-1234").unwrap();
    let target = scope(root.path(), || {
        target(iso.path().join("debug.log")).unwrap()
    });
    std::fs::create_dir_all(target.path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &target.path).unwrap();
    let mut entered = false;
    target.with_lock(|| entered = true);
    assert!(!entered && target.read().is_none());
    assert_eq!(std::fs::read_to_string(victim).unwrap(), "CUS-1234");
}

#[test]
fn corrupt_scoped_location_cannot_fall_back_to_readable_global_log() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    let base = iso.path().join("debug.log");
    std::fs::write(&base, "LEGACY-PRIVATE-CUS-1234").unwrap();
    std::fs::write(iso.path().join("projects"), "not a directory").unwrap();
    assert!(scope(root.path(), || target(base)).is_none());
}

#[test]
fn capture_never_migrates_legacy_or_replaces_invalid_existing_knowledge() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let _enabled = Enabled::new();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().to_str().unwrap();
    std::fs::write(root.path().join(".lean-ctx-id"), "capture-fixture").unwrap();
    policy(root.path(), MASK);
    let mut canonical = ProjectKnowledge::new(project);
    canonical.remember(
        "finding",
        "old",
        "CUS-1234",
        "fixture",
        0.9,
        &crate::core::memory_policy::MemoryPolicy::default(),
    );
    let old_hash = crate::core::project_hash::hash_path_only(project);
    assert_ne!(old_hash, canonical.project_hash);
    let legacy = iso.path().join("knowledge").join(old_hash);
    std::fs::create_dir_all(&legacy).unwrap();
    let mut old = serde_json::to_value(&canonical).unwrap();
    old["project_hash"] = legacy.file_name().unwrap().to_str().unwrap().into();
    let legacy_bytes = old.to_string();
    std::fs::write(legacy.join("knowledge.json"), &legacy_bytes).unwrap();
    let finding = AutoFinding {
        file: Some("safe.rs".into()),
        summary: "Read safe.rs".into(),
    };
    auto_capture::capture_finding(project, &finding);
    let destination = iso
        .path()
        .join("knowledge")
        .join(&canonical.project_hash)
        .join("knowledge.json");
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read_to_string(legacy.join("knowledge.json")).unwrap(),
        legacy_bytes
    );
    std::fs::write(&destination, "invalid existing CUS-1234").unwrap();
    auto_capture::capture_finding(project, &finding);
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "invalid existing CUS-1234"
    );
}
