// SPDX-License-Identifier: Apache-2.0
#![recursion_limit = "256"]
#![warn(clippy::undocumented_unsafe_blocks)]

//! The MCP server and its tools, above the Engine library (`lean_ctx`): an
//! edit here re-checks this crate and the crates above it, not the Engine.

/// Agent-tools session server (`lean-ctx engine agent-tools`).
pub mod agent_tools_cmd;
/// Low-level Apache in-process tool-execution façade for Engine embedders.
pub mod engine;
pub mod server;
pub mod tools;

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
