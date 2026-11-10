// SPDX-License-Identifier: Apache-2.0
//! This package's own integration tests as one test binary (target
//! `integration`), beside the merged suite (target `main`, tests/main.rs).
//!
//! Every other tests/*.rs file used to be its own binary, and each one linked
//! the whole Engine. They are modules of this binary now, so a test is
//! `<old file stem>::<test>`. They stay out of the suite: it isolates its
//! environment before `main` (suite::hermetic_env), and several of these
//! re-run this binary with a cleared environment or mutate it themselves.
//! Add new integration tests as modules here or in the suite, not as new
//! tests/*.rs files (`autotests = false`).
//!
//! Kept as their own binaries (see `[[test]]` in Cargo.toml): codex_proxy_554
//! and issue_326_parallel_knowledge, which point the whole process at a
//! throwaway data or config dir.

mod libtest_serial {
    //! These modules mutate the process environment or other process-global
    //! state, and their locks only cover their own module. nextest runs every
    //! test in its own process; plain libtest (`cargo test`, a direct run)
    //! runs the binary's tests on parallel threads of one process, so it gets
    //! one thread unless `RUST_TEST_THREADS` or `--test-threads` says otherwise.

    extern "C" fn serialize_libtest() {
        if std::env::var_os("RUST_TEST_THREADS").is_none() {
            // SAFETY: runs from the init section, before `main` and before any
            // other thread exists, so nothing reads the environment concurrently.
            unsafe { std::env::set_var("RUST_TEST_THREADS", "1") };
        }
    }

    #[used]
    #[cfg_attr(
        target_vendor = "apple",
        unsafe(link_section = "__DATA,__mod_init_func")
    )]
    #[cfg_attr(
        any(target_os = "linux", target_os = "android", target_os = "freebsd"),
        unsafe(link_section = ".init_array")
    )]
    #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
    static SERIALIZE_LIBTEST: extern "C" fn() = serialize_libtest;
}

mod context_budget_cli;
mod doctor_json_stdout;
mod engine_context_plan_transport;
mod engine_context_sources_transport;
mod engine_source_receipt_transport;
mod engine_transport_compatibility;
mod evidence_tamper_tests;
mod host_receipt_cli;
mod index_scoping_scenarios;
mod oneshot_call_no_stub;
mod oneshot_semantic_terminates;
mod p17_relay_store;
mod p17_replay_review_regressions;
mod p17_response_atomicity;
mod proxy_stale_env_scenarios;
mod ticket_fixes_scenarios;
mod v3_receipt_fixture_migration;
mod v4_checkpoint_package;
mod v4_session_load;
