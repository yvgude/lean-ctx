// SPDX-License-Identifier: Apache-2.0
//! The Engine's exec gate before any policy is registered.
//!
//! This file is its own test binary on purpose: it never calls
//! `lean_ctx_shell_policy::install()`, so the process-wide gate stays
//! unregistered whatever the test runner (cargo test or nextest).

/// Warn-only mode lets a blocked command through once a policy runs, but with
/// no policy registered there is nothing to warn about: the gate refuses (126).
#[test]
fn allowlist_gate_refuses_in_warn_only_mode_without_a_registered_policy() {
    // SAFETY: the only test in this binary; nothing else reads the environment.
    unsafe {
        std::env::set_var("LEAN_CTX_ALLOWLIST_WARN_ONLY", "1");
        std::env::set_var("LEAN_CTX_SHELL_SECURITY", "warn");
        std::env::remove_var("LEAN_CTX_HOOK_CHILD");
    }
    assert!(!lean_ctx::core::shell_allowlist::gate_registered());
    assert_eq!(lean_ctx::shell::exec::allowlist_gate("echo hi"), Some(126));
    assert!(matches!(
        lean_ctx::shell::exec::prepare_exec("echo hi"),
        Err(126)
    ));
}
