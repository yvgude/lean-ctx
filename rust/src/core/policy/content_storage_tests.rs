// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::policy::{parse, resolve, runtime::TestPolicyOverride};

fn protected(extra: &str) -> TestPolicyOverride {
    let pack = parse(&format!(
        "name = \"storage-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"
    ))
    .unwrap();
    TestPolicyOverride::set(Some(resolve(&pack).unwrap()))
}

#[test]
fn protected_originless_copies_are_withheld_before_persistence() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = protected("[redaction]\ncustomer = 'K-[0-9]{6}'");
    assert!(crate::shell::save_tee("show K-482193", "customer K-482193").is_none());
    assert!(crate::proxy::ccr::persist_conversation("customer K-482193").is_none());
    assert!(
        !crate::core::paths::state_dir()
            .unwrap()
            .join("tee")
            .exists()
    );
    // Live output still has a useful, filtered representation.
    assert_eq!(
        protect_active("customer K-482193").unwrap(),
        "customer [REDACTED:customer]"
    );
}

#[test]
fn stricter_policy_rechecks_old_recovery_and_denies_blocked_new_copies() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let content = "CONFIDENTIAL\ncustomer K-482193\n".repeat(32);
    {
        let _policy = TestPolicyOverride::set(None);
        crate::proxy::ccr::persist(&content).unwrap();
    }
    let hash = crate::proxy::ccr::litellm_hash(&content);
    {
        let _policy = protected("[redaction]\ncustomer = 'K-[0-9]{6}'");
        // Content masking cannot prove that the original source is still allowed.
        assert!(crate::proxy::ccr::retrieve_litellm(&hash).is_none());
        assert!(crate::proxy::ccr::persist(&content).is_none());
    }
    {
        let _policy = protected("[filters]\nclassification = 'block'");
        assert!(crate::proxy::ccr::retrieve_litellm(&hash).is_none());
        assert!(crate::proxy::ccr::persist_conversation("CONFIDENTIAL\nblocked").is_none());
        assert!(crate::shell::save_tee("show data", "CONFIDENTIAL\nblocked").is_none());
        assert!(protect_active("CONFIDENTIAL\nblocked").is_err());
    }
}

#[test]
fn tee_selectors_and_inband_do_not_reopen_originless_history() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let body = "visible marker\nordinary data\n".repeat(32) + "CONFIDENTIAL\n";
    let handle = {
        let _community = TestPolicyOverride::set(None);
        let handle = crate::proxy::ccr::persist(&body).unwrap();
        assert!(
            crate::tools::ctx_expand::handle(&serde_json::json!({"id": handle, "head": 1}))
                .contains("visible marker")
        );
        handle
    };
    let marker = crate::proxy::ccr::inband_marker(&handle).unwrap();
    let mut tracker = crate::core::relevance_tracker::RelevanceTracker::with_config(200, 0.0);
    tracker.register(handle.clone(), &body, "ctx_shell", 300, 0);
    {
        let _community = TestPolicyOverride::set(None);
        assert!(
            tracker
                .expand_if_relevant("visible")
                .unwrap()
                .contains("visible marker")
        );
    }
    for rules in [
        "[filters]\nclassification='block'",
        "[redaction]\ncustomer='K-[0-9]{6}'",
    ] {
        let _policy = protected(rules);
        assert!(tracker.expand_if_relevant("visible").is_none());
        for selector in [
            serde_json::json!({}),
            serde_json::json!({"head":1}),
            serde_json::json!({"tail":2}),
            serde_json::json!({"search":"visible"}),
            serde_json::json!({"start_line":1,"end_line":2}),
            serde_json::json!({"json_keys":true}),
            serde_json::json!({"json_path":"safe"}),
        ] {
            let mut args = selector;
            args["id"] = handle.clone().into();
            let result = crate::tools::ctx_expand::handle(&args);
            assert!(result.starts_with("ERROR:"), "{result}");
            assert!(!result.contains("visible marker"));
        }
        let mut request = serde_json::json!({"messages":[{"role":"assistant","content":marker}]});
        let before = request.clone();
        assert!(!crate::proxy::ccr::splice_inband_in_place(&mut request));
        assert_eq!(request, before);
        assert!(
            crate::proxy::ccr::retrieve_litellm(&crate::proxy::ccr::litellm_hash(&body)).is_none()
        );
    }
    // This is legacy Community history, not a protected bound record. It is
    // preserved, not deleted, when the operator enables a policy.
    let _community = TestPolicyOverride::set(None);
    assert_eq!(
        crate::proxy::ccr::read_checked_tee(std::path::Path::new(&handle)).unwrap(),
        body
    );
}

#[test]
fn tee_recovery_bounds_bytes_and_rejects_binary_and_missing_copies() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let handle = crate::shell::save_tee("print", "valid output").unwrap();
    let path = std::path::Path::new(&handle);
    let limit = crate::core::limits::max_read_bytes().min(MAX_PROTECTED_CONTENT_BYTES);
    std::fs::write(path, vec![b'x'; limit + 1]).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
    assert_eq!(
        crate::proxy::ccr::read_tee_detailed(path).unwrap_err(),
        "stored output exceeds the configured recovery byte limit"
    );
    assert!(crate::shell::save_tee("print", &"x".repeat(limit + 1)).is_none());
    assert!(crate::shell::save_tee("print", "binary\0output").is_none());
    std::fs::write(path, b"binary\0output").unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
    std::fs::write(path, [0xff, 0xfe]).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
    std::fs::remove_file(path).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
}

#[cfg(unix)]
#[test]
fn tee_recovery_rejects_links_and_writes_do_not_follow_them() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = TestPolicyOverride::set(None);
    let handle = crate::shell::save_tee("print", "first").unwrap();
    let path = std::path::Path::new(&handle);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target");
    std::fs::write(&target, "outside content").unwrap();
    std::fs::remove_file(path).unwrap();
    std::os::unix::fs::symlink(&target, path).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
    assert_eq!(
        crate::shell::save_tee("print", "replacement").unwrap(),
        handle
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside content");
    assert_eq!(
        crate::proxy::ccr::read_checked_tee(path).unwrap(),
        "replacement"
    );
    std::fs::remove_file(path).unwrap();
    std::fs::hard_link(&target, path).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
    std::fs::remove_file(path).unwrap();
    std::fs::create_dir(path).unwrap();
    assert!(crate::proxy::ccr::read_checked_tee(path).is_none());
}
