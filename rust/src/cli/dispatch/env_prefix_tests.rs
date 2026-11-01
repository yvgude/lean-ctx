// SPDX-License-Identifier: Apache-2.0

use super::extract_and_apply_env_prefix;
use serial_test::serial;

#[test]
#[serial]
fn extracts_lean_ctx_disabled() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
    let result = extract_and_apply_env_prefix("LEAN_CTX_DISABLED=1 cargo test --lib");
    assert_eq!(result, "cargo test --lib");
    assert_eq!(std::env::var("LEAN_CTX_DISABLED").unwrap(), "1");
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
}

#[test]
#[serial]
fn ignores_non_lean_ctx_vars() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("FOO");
    let result = extract_and_apply_env_prefix("FOO=bar cargo test --lib");
    assert_eq!(result, "cargo test --lib");
    assert!(
        std::env::var("FOO").is_err(),
        "FOO must not be set in process env"
    );
}

#[test]
#[serial]
fn extracts_multiple_lean_ctx_vars() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
    crate::test_env::remove_var("LEAN_CTX_ACTIVE");
    let result =
        extract_and_apply_env_prefix("LEAN_CTX_DISABLED=1 LEAN_CTX_ACTIVE=1 cargo test --lib");
    assert_eq!(result, "cargo test --lib");
    assert_eq!(std::env::var("LEAN_CTX_DISABLED").unwrap(), "1");
    assert_eq!(std::env::var("LEAN_CTX_ACTIVE").unwrap(), "1");
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
    crate::test_env::remove_var("LEAN_CTX_ACTIVE");
}

#[test]
#[serial]
fn no_prefix_returns_unchanged() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let result = extract_and_apply_env_prefix("cargo test --lib");
    assert_eq!(result, "cargo test --lib");
}

#[test]
#[serial]
fn mixed_vars_extracts_only_lean_ctx() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("FOO");
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
    let result = extract_and_apply_env_prefix("FOO=bar LEAN_CTX_DISABLED=1 cargo test --lib");
    assert_eq!(result, "cargo test --lib");
    assert_eq!(std::env::var("LEAN_CTX_DISABLED").unwrap(), "1");
    assert!(std::env::var("FOO").is_err());
    crate::test_env::remove_var("LEAN_CTX_DISABLED");
}

/// `lean-ctx -c 'LEAN_CTX_SHELL_SECURITY=off …'` applied the prefix to its own
/// process before the allowlist gate ran, so the gate read `off`. Only the
/// run-control keys are applied.
#[test]
#[serial]
fn security_and_config_keys_are_never_applied() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let keys = [
        "LEAN_CTX_SHELL_SECURITY",
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "LEAN_CTX_CONFIG_DIR",
    ];
    for key in keys {
        crate::test_env::remove_var(key);
    }
    let result = extract_and_apply_env_prefix(
        "LEAN_CTX_SHELL_SECURITY=off LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE=bash \
         LEAN_CTX_CONFIG_DIR=/tmp/x bash -c id",
    );
    assert_eq!(result, "bash -c id");
    for key in keys {
        assert!(std::env::var(key).is_err(), "{key} must not be applied");
    }
}
