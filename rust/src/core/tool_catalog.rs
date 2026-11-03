// SPDX-License-Identifier: Apache-2.0
//! The MCP tool catalog: the definitions of every registered tool.
//!
//! The tools themselves live in the MCP server layer, which registers its
//! catalog here (`server::registry::register_tool_catalog`, called by the CLI
//! at start-up and by every MCP server construction). Engine code that only
//! needs the definitions — manifests, overhead measurement, discovery cards,
//! docs — reads them through this module instead of building the registry.
//!
//! Without a registration (library use that never starts a server) the
//! catalog is empty. It describes and counts tools; it never decides whether a
//! call is allowed — tool admission, policy and role gates do not read it.
//! Engine unit tests see it empty as well; the tests that measure the real
//! catalog register it and live with the server (`server/catalog_tests.rs`).

use std::sync::OnceLock;

use rmcp::model::Tool;

/// The registered catalog entry points.
#[derive(Clone, Copy)]
pub struct ToolCatalog {
    /// The official public surface: sorted, annotated definitions of every
    /// publicly advertised tool (what `tools/list` may offer).
    pub tool_defs: fn() -> Vec<Tool>,
    /// Every registered tool, including local compatibility substrate and
    /// internal host hooks that stay callable but are never advertised.
    pub registered_tool_defs: fn() -> Vec<Tool>,
}

static CATALOG: OnceLock<ToolCatalog> = OnceLock::new();

/// Register the tool catalog. The first registration wins.
pub fn register_tool_catalog(catalog: ToolCatalog) {
    let _ = CATALOG.set(catalog);
}

/// Whether a catalog has been registered.
#[must_use]
pub fn is_registered() -> bool {
    catalog().is_some()
}

fn catalog() -> Option<&'static ToolCatalog> {
    CATALOG.get()
}

/// Definitions of the publicly advertised tools; empty when unregistered.
#[must_use]
pub fn tool_defs() -> Vec<Tool> {
    catalog().map_or_else(Vec::new, |c| (c.tool_defs)())
}

/// Definitions of every registered tool; empty when unregistered.
#[must_use]
pub fn registered_tool_defs() -> Vec<Tool> {
    catalog().map_or_else(Vec::new, |c| (c.registered_tool_defs)())
}

/// Number of official MCP tools (the public surface).
#[must_use]
pub fn tool_count() -> usize {
    tool_defs().len()
}

/// Sorted names of every registered tool.
#[must_use]
pub fn registered_tool_names() -> Vec<String> {
    let mut names: Vec<String> = registered_tool_defs()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort_unstable();
    names
}
