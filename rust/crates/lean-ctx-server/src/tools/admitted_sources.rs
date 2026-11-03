// SPDX-License-Identifier: Apache-2.0
//! Admitted acquisition for tools (G01): the engine's
//! `core::admitted_sources`, plus the root of one tool call.

pub use lean_ctx::core::admitted_sources::*;

/// `core::admitted_sources::root_for` with the roots of one tool call: the
/// project root, then the session's trusted roots.
pub(crate) fn root_in_context(path: &str, ctx: &crate::server::tool_trait::ToolContext) -> String {
    let mut roots = Vec::with_capacity(1 + ctx.extra_roots.len());
    roots.push(ctx.project_root.clone());
    roots.extend(ctx.extra_roots.iter().cloned());
    root_for(path, &roots)
}
