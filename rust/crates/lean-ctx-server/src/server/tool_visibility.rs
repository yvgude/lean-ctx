// SPDX-License-Identifier: Apache-2.0
//! The tool-visibility policy lives in [`lean_ctx::core::tool_visibility`] so
//! engine code outside the MCP server can read it; re-exported here for the
//! server's own callers.

pub use lean_ctx::core::tool_visibility::*;
