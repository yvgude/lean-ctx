// SPDX-License-Identifier: Apache-2.0
//! The Engine's integration tests as one test binary (target `engine`).
//!
//! Every tests/*.rs file used to be its own binary, and each one linked the
//! whole Engine: most of the CI test build. They are modules of this binary
//! now, so a test is `<old file stem>::<test>`. Add new Engine integration
//! tests as modules here, not as new tests/*.rs files (`autotests = false`).
//!
//! Kept as their own binaries (see `[[test]]` in Cargo.toml) because they need
//! a process nobody else touched: graph_provider_cold_open and
//! lazy_graph_build_latch_tests (the process-wide background-build latch),
//! outcome_pricing_e2e (env set before any cached resolver runs),
//! ocp_self_validation (data dir and audit-chain tail of the process) and
//! gateway_postprocess_e2e (points the whole process at one data dir, once).
//!
//! The rust/tests/suite and rust/tests/support trees are not part of this
//! binary: they are the `main` test target of package lean-ctx.

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

mod claude_config_dir;
mod closure_tests;
mod config_adopt_override;
mod dropin_install_tests;
mod evidence_bundle_e2e;
mod issue_249_index_observability;
mod neuro_physics_scenarios;
mod ocla_integration_suite;
mod p17_task_control_bounds;
mod p17_task_control_status;
mod p2_graph_embeddings_eval;
mod p4_docker_claude_mcp_selfheal_eval;
mod protocol_spine_v1;
mod v4_context_runtime_init;
mod v4_event_observation_authority;
mod v4_observation_storage;
mod work_graph_e2e;
mod workflow_tests;
mod xdg_legacy_contract;
