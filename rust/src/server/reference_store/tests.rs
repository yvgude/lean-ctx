// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::policy::runtime::TestPolicyOverride;
use std::path::Path;

fn policy() -> TestPolicyOverride {
    TestPolicyOverride::set(Some(crate::core::policy::load(
        "name='reference-test'\nversion='1.0.0'\ndescription='test'\n[redaction]\ncustomer='CUS-[0-9]{4}'\n[filters]\nclassification='block'\nblocked_labels=['CONFIDENTIAL']\n",
    ).unwrap()))
}

fn scope<T>(root: &Path, operation: impl FnOnce() -> T) -> T {
    // Other full-suite tests change the process-wide role without the data-dir
    // guard. Keep this fixture's expected caller independent of those tests.
    crate::core::roles::with_test_active_role(
        crate::core::roles::load_role("coder").unwrap(),
        || {
            runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), operation)
        },
    )
}

#[test]
fn source_reference_rechecks_current_secret_path_permission() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(".env");
    std::fs::write(&path, "allowed source").unwrap();
    scope(root.path(), || {
        let mut permitted = crate::core::roles::load_role("coder").unwrap();
        permitted.io.allow_secret_paths = true;
        crate::core::roles::with_test_active_role(permitted, || {
            let authority = origin_in_scope(root.path(), &path);
            let id = store_with_authority("role-bound payload", Some(&authority)).unwrap();
            crate::core::roles::with_test_active_role(
                crate::core::roles::load_role("coder").unwrap(),
                || assert!(resolve(&id).is_none()),
            );
            assert_eq!(resolve(&id).as_deref(), Some("role-bound payload"));
        });
    });
}

fn origin(root: &Path, path: &Path) -> ArchiveAuthority {
    scope(root, || origin_in_scope(root, path))
}

fn origin_in_scope(root: &Path, path: &Path) -> ArchiveAuthority {
    runtime::with_source_view(|| {
        let mut remaining = crate::core::limits::max_read_bytes();
        let read = crate::tools::ctx_read::read_file_for_tool_rooted_with_path(
            path.to_str().unwrap(),
            root.to_str().unwrap(),
            "ctx_execute",
            &mut remaining,
        )
        .unwrap();
        ArchiveAuthority::file(root, &read.canonical_path, "ctx_execute", &read.content).unwrap()
    })
    .unwrap()
}

#[test]
fn source_reference_preserves_rendered_output_and_rechecks_original_source() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "original allowed source").unwrap();
    let authority = origin(root.path(), &path);
    scope(root.path(), || {
        let id = store_with_authority("rendered summary CUS-1234", Some(&authority)).unwrap();
        let original = resolve(&id).unwrap();
        assert!(original.contains("rendered summary"));
        assert!(!original.contains("CUS-1234"));
        assert!(!original.contains("original allowed source"));
        assert_eq!(
            store_with_authority("rendered summary CUS-1234", Some(&authority)),
            Some(id.clone())
        );
        for replacement in ["CONFIDENTIAL\nblocked source", "different allowed source"] {
            std::fs::write(&path, replacement).unwrap();
            assert!(resolve(&id).is_none());
        }
        std::fs::remove_file(&path).unwrap();
        assert!(resolve(&id).is_none());
        std::fs::write(&path, "original allowed source").unwrap();
        assert_eq!(resolve(&id).as_deref(), Some(original.as_str()));
    });
}

#[test]
fn source_reference_requires_explicit_matching_project_and_fixed_binding() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let initial_policy = policy();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let path = a.path().join("source.txt");
    std::fs::write(&path, "original allowed source").unwrap();
    let authority = origin(a.path(), &path);
    let id = scope(a.path(), || {
        store_with_authority("project payload", Some(&authority)).unwrap()
    });
    scope(b.path(), || assert!(resolve(&id).is_none()));
    runtime::REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(None), || {
        assert!(resolve(&id).is_none());
    });
    scope(a.path(), || {
        assert_eq!(resolve(&id).as_deref(), Some("project payload"));
    });
    // Dropping policy does not turn this source binding into an unbound reference.
    drop(initial_policy);
    let _no_policy = TestPolicyOverride::set(None);
    std::fs::remove_file(&path).unwrap();
    scope(a.path(), || assert!(resolve(&id).is_none()));
    // A missing binding cannot retain the protected namespace/content identity.
    store_lock().lock().unwrap().get_mut(&id).unwrap().authority = None;
    scope(a.path(), || assert!(resolve(&id).is_none()));
}

#[test]
fn public_reference_survives_community_use_but_not_policy_activation() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let no_policy = TestPolicyOverride::set(None);
    let id = store("community payload").unwrap();
    assert_eq!(store("community payload"), Some(id.clone()));
    assert_eq!(resolve(&id).as_deref(), Some("community payload"));
    drop(no_policy);
    {
        let _policy = policy();
        assert!(resolve(&id).is_none());
        assert!(store("unbound protected output").is_none());
    }
    let _no_policy = TestPolicyOverride::set(None);
    assert_eq!(resolve(&id).as_deref(), Some("community payload"));
}

#[test]
fn references_filter_current_output_rules() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let initial_policy = policy();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "allowed source without customer fields").unwrap();
    let authority = origin(root.path(), &path);
    scope(root.path(), || {
        let id = store_with_authority("result account ID-1234", Some(&authority)).unwrap();
        drop(initial_policy);
        let new = crate::core::policy::load("name='updated'\nversion='1.0.0'\ndescription='test'\n[redaction]\naccount='ID-[0-9]{4}'\n").unwrap();
        let _updated = TestPolicyOverride::set(Some(new));
        let result = resolve(&id).unwrap();
        assert!(result.contains("result account"));
        assert!(!result.contains("ID-1234"));
    });
}

#[test]
fn expired_or_modified_reference_is_not_retrievable() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(None);
    let id = store("ttl payload").unwrap();
    store_lock()
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .created_at = Instant::now().checked_sub(TTL).unwrap();
    assert!(resolve(&id).is_none());
    let id = store("integrity payload").unwrap();
    store_lock().lock().unwrap().get_mut(&id).unwrap().content = Arc::from("substituted content");
    assert!(resolve(&id).is_none());
}

#[test]
fn invalid_policy_cannot_restore_or_create_an_unbound_reference() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(None);
    let root = tempfile::tempdir().unwrap();
    let id = store("community retained payload").unwrap();
    std::fs::create_dir(root.path().join(".lean-ctx")).unwrap();
    std::fs::write(root.path().join(".lean-ctx/policy.toml"), "[invalid").unwrap();
    scope(root.path(), || {
        assert!(resolve(&id).is_none());
        assert!(store("new unverified payload").is_none());
    });
    assert_eq!(resolve(&id).as_deref(), Some("community retained payload"));
}

#[test]
fn resolve_waits_out_a_concurrent_store_instead_of_reporting_missing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(None);
    let id = store("briefly contended payload").unwrap();
    let held = store_lock().lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let resolver = std::thread::spawn({
        let id = id.clone();
        move || {
            tx.send(()).unwrap();
            resolve(&id)
        }
    });
    rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(5));
    drop(held);
    assert_eq!(
        resolver.join().unwrap().as_deref(),
        Some("briefly contended payload")
    );
}

#[test]
fn reference_pool_enforces_entry_and_aggregate_byte_budgets() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(None);
    store_lock().lock().unwrap().clear();
    let cap = entry_budget();
    assert!(store(&"x".repeat(cap + 1)).is_none());
    let mut first = None;
    for n in 0..=MAX_STORE_BYTES / cap {
        let body = format!("{n:02}{}", "x".repeat(cap - 2));
        // `store` never blocks on the shared pool (try_lock); a parallel test
        // elsewhere in the crate may hold it for a moment. Every body here is
        // within budget, so only contention can make a single attempt miss.
        let id = (0..200)
            .find_map(|_| {
                store(&body).or_else(|| {
                    std::thread::sleep(Duration::from_millis(2));
                    None
                })
            })
            .expect("in-budget entry is stored once the pool is free");
        if first.is_none() {
            first = Some(id);
        }
        let (count, bytes) = stats();
        assert!(count <= MAX_ENTRIES);
        assert!(bytes <= MAX_STORE_BYTES);
    }
    assert!(resolve(first.as_deref().unwrap()).is_none());
    store_lock().lock().unwrap().clear();
}

#[tokio::test]
async fn concurrent_capture_scopes_do_not_exchange_source_authority() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = policy();
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a.txt");
    let b = root.path().join("b.txt");
    std::fs::write(&a, "first source").unwrap();
    std::fs::write(&b, "second source").unwrap();
    let first = origin(root.path(), &a);
    let second = origin(root.path(), &b);
    let (left, right) = tokio::join!(
        crate::core::archive::authority::capture(async {
            first.clone().publish();
            tokio::task::yield_now().await;
            "first output"
        }),
        crate::core::archive::authority::capture(async {
            second.clone().publish();
            tokio::task::yield_now().await;
            "second output"
        }),
    );
    assert_eq!(left, ("first output", Some(first)));
    assert_eq!(right, ("second output", Some(second)));
}
