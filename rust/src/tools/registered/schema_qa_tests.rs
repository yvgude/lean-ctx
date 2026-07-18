//! #1008 schema QA: guards that action-driven `ctx_*` tools keep modelling
//! their action-conditional `required` fields, so a dropped conditional fails CI.

use serde_json::Value;

use super::ctx_callgraph::CtxCallgraphTool;
use super::ctx_expand::CtxExpandTool;
use super::ctx_graph::CtxGraphTool;
use super::ctx_knowledge::CtxKnowledgeTool;
use crate::server::tool_trait::McpTool;

fn schema_of(tool: &dyn McpTool) -> Value {
    let def = tool.tool_def();
    Value::Object((*def.input_schema).clone())
}

fn required(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Does an `allOf` branch select `action` (via `if.properties.action`
/// const/enum) and require every field in `needs` (via `then.required`)?
fn models_conditional(schema: &Value, action: &str, needs: &[&str]) -> bool {
    let Some(branches) = schema.get("allOf").and_then(Value::as_array) else {
        return false;
    };
    branches.iter().any(|b| {
        let selects = b
            .get("if")
            .and_then(|i| i.get("properties"))
            .and_then(|p| p.get("action"))
            .is_some_and(|a| {
                a.get("const").and_then(Value::as_str) == Some(action)
                    || a.get("enum")
                        .and_then(Value::as_array)
                        .is_some_and(|e| e.iter().any(|x| x.as_str() == Some(action)))
            });
        if !selects {
            return false;
        }
        let then_req: Vec<&str> = b
            .get("then")
            .and_then(|t| t.get("required"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        needs.iter().all(|n| then_req.contains(n))
    })
}

/// Any `allOf` branch whose `then.required` contains `field` — for the
/// `ctx_expand` default-retrieve rule, which selects via `if.not`.
fn some_then_requires(schema: &Value, field: &str) -> bool {
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches.iter().any(|b| {
                b.get("then")
                    .and_then(|t| t.get("required"))
                    .and_then(Value::as_array)
                    .is_some_and(|r| r.iter().any(|v| v.as_str() == Some(field)))
            })
        })
}

#[test]
fn callgraph_models_action_conditionals() {
    let s = schema_of(&CtxCallgraphTool);
    assert!(required(&s).contains(&"action".to_string()), "action must be required");
    for a in ["callers", "callees", "risk"] {
        assert!(models_conditional(&s, a, &["symbol"]), "{a} must require symbol");
    }
    assert!(models_conditional(&s, "trace", &["from", "to"]), "trace must require from+to");
}

#[test]
fn expand_models_action_conditionals() {
    let s = schema_of(&CtxExpandTool);
    assert!(models_conditional(&s, "search_all", &["query"]), "search_all must require query");
    // retrieve is the default action; its rule selects via `if.not`, so assert
    // some branch requires `id` (an empty ctx_expand({}) must be invalid).
    assert!(some_then_requires(&s, "id"), "retrieve path must require id");
}

#[test]
fn graph_models_action_conditionals() {
    let s = schema_of(&CtxGraphTool);
    assert!(required(&s).contains(&"action".to_string()), "action must be required");
    for a in ["symbol", "neighbors", "impact"] {
        assert!(models_conditional(&s, a, &["path"]), "{a} must require path");
    }
    assert!(models_conditional(&s, "path", &["path", "to"]), "path action must require path+to");
}

#[test]
fn knowledge_models_action_conditionals() {
    let s = schema_of(&CtxKnowledgeTool);
    assert!(required(&s).contains(&"action".to_string()), "action must be required");
    assert!(models_conditional(&s, "remember", &["category"]), "remember must require category");
    assert!(models_conditional(&s, "search", &["query"]), "search must require query");
    assert!(models_conditional(&s, "pattern", &["value"]), "pattern must require value");
    assert!(
        models_conditional(&s, "gotcha", &["trigger", "resolution"]),
        "gotcha must require trigger+resolution"
    );
}
