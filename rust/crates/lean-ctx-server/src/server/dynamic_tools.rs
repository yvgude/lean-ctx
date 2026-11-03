// SPDX-License-Identifier: Apache-2.0
//! The name logic lives in [`lean_ctx::core::dynamic_tools`]; this module keeps the
//! helpers that resolve a call through `ctx_call`/`ctx` to its inner tool.

pub use lean_ctx::core::dynamic_tools::*;

/// Whether a call — direct, or through `ctx_call`/`ctx` — targets an internal
/// host hook. A host replaces a native tool's output with the hook's success
/// body, so every gate refusal on such a call must read as an error: then the
/// host keeps the original output instead of a block message (#2089).
#[must_use]
pub fn targets_internal_host_tool(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    crate::tools::registered::ctx_call::resolve(name, args)
        .is_ok_and(|(inner, _)| INTERNAL_HOST_TOOLS.contains(&inner.as_str()))
}

/// The fixed answer for a call — direct, or through `ctx_call`/`ctx` — to a
/// withheld Hive tool, else `None`. Stable text (#498).
#[must_use]
pub fn withheld_hive_tool_refusal(
    name: &str,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<&'static str> {
    crate::tools::registered::ctx_call::resolve(name, args)
        .is_ok_and(|(inner, _)| is_withheld_hive_tool(&inner))
        .then_some(lean_ctx_hive::TOOL_NOT_AVAILABLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_tools_point_at_a_live_public_tool() {
        assert_eq!(
            removed_tool("ctx_multi_read").unwrap().replacement,
            Some("ctx_read")
        );
        assert!(removed_tool("ctx_read").is_none());
        // No removed name may send agents to a tool that is gone or hidden.
        let registry = crate::server::registry::build_registry();
        let live = registry.names();
        for name in [
            "ctx_smart_read",
            "ctx_multi_read",
            "ctx_semantic_search",
            "ctx_symbol",
            "ctx_overview",
            "ctx_summary",
            "ctx_explore",
            "ctx_plan",
            "ctx_compile",
            "ctx_fill",
            "ctx_impact",
            "ctx_architecture",
            "ctx_repomap",
            "ctx_routes",
            "ctx_retrieve",
            "ctx_cache",
        ] {
            let replacement = removed_tool(name).and_then(|r| r.replacement).unwrap();
            assert!(!live.contains(&name), "{name} is still registered");
            assert!(
                live.contains(&replacement) && is_publicly_advertised_tool(replacement),
                "{name} points at {replacement}, which is not a live public tool"
            );
        }
    }

    /// #2089: hosts reach ctx_shape through the listed ctx_call, so a call is a
    /// host-hook call in every spelling — and only then.
    #[test]
    fn host_hook_calls_are_recognised_in_every_spelling() {
        let args = |v: serde_json::Value| v.as_object().cloned();
        let shape = args(serde_json::json!({"tool": "Bash", "output": "x"}));
        assert!(targets_internal_host_tool("ctx_shape", shape.as_ref()));
        let via_call =
            args(serde_json::json!({"name": "ctx_shape", "arguments": {"tool": "Bash"}}));
        assert!(targets_internal_host_tool("ctx_call", via_call.as_ref()));
        let via_ctx = args(serde_json::json!({"tool": "shape", "output": "x"}));
        assert!(targets_internal_host_tool("ctx", via_ctx.as_ref()));

        let read = args(serde_json::json!({"name": "ctx_read", "arguments": {"path": "a"}}));
        assert!(!targets_internal_host_tool("ctx_call", read.as_ref()));
        assert!(!targets_internal_host_tool("ctx_read", None));
        assert!(!targets_internal_host_tool("ctx_call", None));
    }
}
