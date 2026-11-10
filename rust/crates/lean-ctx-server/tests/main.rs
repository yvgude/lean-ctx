// SPDX-License-Identifier: Apache-2.0
//! The server's integration tests as one test binary (target `server`).
//!
//! Every tests/*.rs file used to be its own binary, and each one linked the
//! whole Engine. They are modules of this binary now, so a test is
//! `<old file stem>::<test>`. Add new integration tests as modules here, not
//! as new tests/*.rs files (`autotests = false`).
//!
//! Kept as its own binary (see `[[test]]` in Cargo.toml): quality_loop_golden,
//! which sets `LEAN_CTX_DATA_DIR` before the process first touches the
//! edit-quality store's `OnceLock`.

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

mod agent_wait_roundtrip;
mod archive_expand_tests;
mod bazsi_reported_scenarios;
mod ciso_great_filter_e2e;
mod ctx_compose_scenarios;
mod dual_process_rehydrate;
mod intensive_benchmarks;
mod issue_244_ledger_reset;
mod local_free_invariant;
mod memory_benchmark_suite;
mod operator_boundary;
mod output_contracts_knowledge;
mod p4_adaptive_mode_policy_eval;
mod p4_ctx_feedback_eval;
mod p4_ctx_handoff_eval;
mod p4_ctx_prefetch_eval;
mod power_user_worksession;
mod product_capability_classification;
mod read_only_roots_e2e;
mod savings_verification;
mod subagent_contract_roundtrip;
mod terse_agent_tests;
mod v4_session_import;
mod wiring_memory_persistence;
mod workflow_done_scenarios;
