// SPDX-License-Identifier: Apache-2.0
#![recursion_limit = "256"]
#![warn(clippy::undocumented_unsafe_blocks)]

//! The MCP server and its tools, above the Engine library (`lean_ctx`): an
//! edit here re-checks this crate and the crates above it, not the Engine.

/// Agent-tools session server (`lean-ctx engine agent-tools`).
pub mod agent_tools_cmd;
/// Low-level Apache in-process tool-execution façade for Engine embedders.
pub mod engine;
#[doc(hidden)]
pub mod input_check_hook;
pub mod server;
pub mod tools;

/// The Engine's production guard must hold for this crate's unit tests too:
/// they see the Engine without `cfg(test)` and run as package
/// `lean-ctx-server`, and once sent telemetry to production from CI.
#[cfg(test)]
mod production_endpoint_tests {
    #[test]
    fn unit_tests_of_this_crate_are_refused_a_remote_endpoint() {
        assert!(lean_ctx::core::data_dir::sandboxed());
        let _url =
            lean_ctx::test_env::EnvVarGuard::set("LEAN_CTX_API_URL", "https://api.leanctx.com");
        assert_eq!(
            lean_ctx::telemetry_transport::api_url(),
            "http://127.0.0.1:9"
        );
    }
}

/// The containment helper test of this crate's unit-test harness, at the path
/// the Engine starts its helpers and guards on (`shell::containment::tests`).
#[cfg(all(test, unix))]
mod shell {
    mod containment {
        mod tests {
            #[test]
            #[ignore = "subprocess helper for the containment tests"]
            fn containment_helper() {
                lean_ctx::shell::containment::tests::containment_helper_main();
            }
        }
    }
}
