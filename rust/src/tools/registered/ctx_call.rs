use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxCallTool;

pub(crate) type ResolvedCall = (String, Option<Map<String, Value>>);

/// Normalize the advertised `ctx` alias without discarding `ctx_call` gates.
pub(crate) fn normalize_outer(
    name: &str,
    args: Option<&Map<String, Value>>,
) -> Result<ResolvedCall, ErrorData> {
    if name != "ctx" {
        return Ok((name.to_owned(), args.cloned()));
    }
    let sub = args
        .and_then(|args| get_str(args, "tool"))
        .ok_or_else(|| ErrorData::invalid_params("'tool' is required for ctx meta-tool", None))?;
    let name = if sub.starts_with("ctx_") {
        sub
    } else {
        format!("ctx_{sub}")
    };
    let mut args = args.cloned().unwrap_or_default();
    args.remove("tool");
    Ok((name, Some(args)))
}

/// The same argument decoder must serve admission, planning and dispatch.
pub(crate) fn resolve_inner(args: Option<&Map<String, Value>>) -> Result<ResolvedCall, ErrorData> {
    let inner = args.and_then(|args| get_str(args, "name")).ok_or_else(|| {
        let hint = args
            .and_then(|args| {
                ["tool", "tool_name", "toolName"]
                    .iter()
                    .find(|key| args.contains_key(**key))
            })
            .map_or(String::new(), |bad| {
                format!(" (found '{bad}' — the key is 'name')")
            });
        ErrorData::invalid_params(format!("name is required{hint}"), None)
    })?;
    if inner == "ctx_call" {
        return Err(ErrorData::invalid_params(
            "ctx_call cannot invoke itself",
            None,
        ));
    }
    // Host hooks serve extensions directly and are not callable through the
    // agent-facing ctx_call path.
    if crate::server::dynamic_tools::INTERNAL_HOST_TOOLS.contains(&inner.as_str()) {
        return Err(ErrorData::invalid_params(
            format!("{inner} is an internal host hook and cannot be called via ctx_call"),
            None,
        ));
    }
    let arguments = match args.and_then(|args| args.get("arguments")) {
        None | Some(Value::Null) => {
            if let Some(args) = args
                && let Some(bad) = ["args", "params", "parameters", "arg"]
                    .iter()
                    .find(|key| args.contains_key(**key))
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "unknown key '{bad}' — pass the inner tool's arguments under 'arguments'"
                    ),
                    None,
                ));
            }
            // Preserve the existing flattened-call contract, including null.
            let flattened: Map<String, Value> = args
                .into_iter()
                .flat_map(|args| args.iter())
                .filter(|(key, _)| key.as_str() != "name")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            (!flattened.is_empty()).then_some(flattened)
        }
        Some(Value::Object(map)) => Some(map.clone()),
        Some(_) => {
            return Err(ErrorData::invalid_params(
                "arguments must be an object",
                None,
            ));
        }
    };
    Ok((inner, arguments))
}

/// Resolve semantic task identity; the actual dispatcher retains both gates.
pub(crate) fn resolve(
    name: &str,
    args: Option<&Map<String, Value>>,
) -> Result<ResolvedCall, ErrorData> {
    let (name, args) = normalize_outer(name, args)?;
    if name == "ctx_call" {
        resolve_inner(args.as_ref())
    } else {
        Ok((name, args))
    }
}

impl McpTool for CtxCallTool {
    fn name(&self) -> &'static str {
        "ctx_call"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_call",
            "Invoke any non-core lean-ctx tool by name — for tools not exposed as standalone MCP tools.\n\
            Categories: arch, debug, memory, batch, agent, util. Find exact names with\n\
            ctx_discover_tools (query=keyword; empty query lists all). Cannot invoke itself.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Tool name" },
                    "arguments": { "type": "object",                         "description": "Tool arguments" }
                },
                "required": ["name"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let name = get_str(args, "name")
            .ok_or_else(|| ErrorData::invalid_params("'name' is required", None))?;

        if name == "ctx_call" {
            return Err(ErrorData::invalid_params(
                "ctx_call cannot invoke itself",
                None,
            ));
        }

        Err(ErrorData::internal_error(
            format!(
                "ctx_call dispatch for '{name}' must be handled by the async dispatch layer. \
                 If you see this error, the tool was routed to the sync handler by mistake."
            ),
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_flattened_and_universal_calls_resolve_the_same_semantics() {
        let expected = (
            "ctx_compose".to_owned(),
            json!({"task":"inner task"}).as_object().cloned(),
        );
        for (name, args) in [
            ("ctx_compose", json!({"task":"inner task"})),
            ("ctx", json!({"tool":"compose", "task":"inner task"})),
            ("ctx", json!({"tool":"ctx_compose", "task":"inner task"})),
            (
                "ctx_call",
                json!({"name":"ctx_compose", "arguments":{"task":"inner task"}}),
            ),
            (
                "ctx_call",
                json!({"name":"ctx_compose", "task":"inner task"}),
            ),
            (
                "ctx",
                json!({"tool":"call", "name":"ctx_compose", "arguments":{"task":"inner task"}}),
            ),
        ] {
            assert_eq!(resolve(name, args.as_object()).unwrap(), expected);
        }
    }

    #[test]
    fn nested_arguments_win_and_flattened_null_contract_is_preserved() {
        let nested = json!({"name":"ctx_search", "query":"outer", "path":"outer", "arguments":{"query":"inner", "path":"inner"}});
        assert_eq!(
            resolve_inner(nested.as_object()).unwrap().1,
            json!({"query":"inner", "path":"inner"})
                .as_object()
                .cloned()
        );
        let null = json!({"name":"ctx_search", "arguments":null, "query":"flat"});
        assert_eq!(
            resolve_inner(null.as_object()).unwrap().1,
            json!({"arguments":null,"query":"flat"})
                .as_object()
                .cloned()
        );
        assert_eq!(
            resolve_inner(json!({"name":"ctx_search"}).as_object())
                .unwrap()
                .1,
            None
        );
    }

    #[test]
    fn invalid_envelopes_fail_before_any_inner_invocation() {
        for args in [
            json!({}),
            json!({"tool":"ctx_search"}),
            json!({"name":42}),
            json!({"name":"ctx_call"}),
            json!({"name":"ctx_search", "arguments":[]}),
            json!({"name":"ctx_search", "arguments":42}),
            json!({"name":"ctx_search", "arguments":false}),
            json!({"name":"ctx_search", "arguments":"query"}),
        ] {
            assert!(resolve_inner(args.as_object()).is_err(), "{args}");
        }
        for key in ["args", "params", "parameters", "arg"] {
            let mut args = json!({"name":"ctx_search"});
            args[key] = json!({"query":"quasar"});
            let error = resolve_inner(args.as_object()).unwrap_err();
            assert!(error.message.contains(key));
        }
        assert!(normalize_outer("ctx", None).is_err());
    }
}
