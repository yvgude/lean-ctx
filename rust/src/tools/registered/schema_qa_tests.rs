//! #1008 schema QA: compiles each action-driven tool's published JSONSchema and
//! asserts it accepts good calls and rejects action-conditional bad ones.

use serde_json::{Value, json};

use super::ctx_callgraph::CtxCallgraphTool;
use super::ctx_expand::CtxExpandTool;
use super::ctx_graph::CtxGraphTool;
use super::ctx_knowledge::CtxKnowledgeTool;
use crate::server::tool_trait::McpTool;

/// Compile a tool's advertised input schema (the one MCP hosts receive) into a
/// validator, so the tests exercise real validation, not schema structure.
fn validator(tool: &dyn McpTool) -> jsonschema::Validator {
    let schema = Value::Object((*tool.tool_def().input_schema).clone());
    jsonschema::validator_for(&schema).expect("tool schema must be a valid JSONSchema")
}

#[test]
fn callgraph_enforces_action_conditionals() {
    let v = validator(&CtxCallgraphTool);
    assert!(v.is_valid(&json!({"action": "callers", "symbol": "f"})));
    assert!(v.is_valid(&json!({"action": "trace", "from": "a", "to": "b"})));
    assert!(!v.is_valid(&json!({})), "action is required");
    assert!(
        !v.is_valid(&json!({"action": "callers"})),
        "callers needs symbol"
    );
    assert!(
        !v.is_valid(&json!({"action": "trace", "from": "a"})),
        "trace needs to"
    );
}

#[test]
fn expand_enforces_action_conditionals() {
    let v = validator(&CtxExpandTool);
    assert!(
        v.is_valid(&json!({"id": "F1"})),
        "documented retrieve-by-id call"
    );
    assert!(v.is_valid(&json!({"action": "list"})));
    assert!(v.is_valid(&json!({"action": "search_all", "query": "x"})));
    assert!(!v.is_valid(&json!({})), "empty retrieve needs id");
    assert!(
        !v.is_valid(&json!({"action": "search_all"})),
        "search_all needs query"
    );
}

#[test]
fn graph_enforces_action_conditionals() {
    let v = validator(&CtxGraphTool);
    assert!(v.is_valid(&json!({"action": "symbol", "path": "f.rs::g"})));
    assert!(v.is_valid(&json!({"action": "path", "path": "a", "to": "b"})));
    assert!(v.is_valid(&json!({"action": "status"})));
    assert!(!v.is_valid(&json!({})), "action is required");
    assert!(
        !v.is_valid(&json!({"action": "symbol"})),
        "symbol needs path"
    );
    assert!(
        !v.is_valid(&json!({"action": "path", "path": "a"})),
        "path needs to"
    );
}

#[test]
fn knowledge_enforces_action_conditionals() {
    let v = validator(&CtxKnowledgeTool);
    assert!(v.is_valid(&json!({"action": "remember", "category": "c", "value": "v"})));
    assert!(v.is_valid(&json!({"action": "remember", "category": "c", "content": "v"})));
    assert!(v.is_valid(&json!({"action": "gotcha", "trigger": "t", "resolution": "r"})));
    assert!(v.is_valid(&json!({"action": "status"})));
    assert!(!v.is_valid(&json!({})), "action is required");
    assert!(
        !v.is_valid(&json!({"action": "remember", "category": "c"})),
        "needs value/content"
    );
    assert!(
        !v.is_valid(&json!({"action": "remember", "value": "v"})),
        "remember needs category"
    );
    assert!(
        !v.is_valid(&json!({"action": "search"})),
        "search needs query"
    );
    assert!(
        !v.is_valid(&json!({"action": "gotcha", "trigger": "t"})),
        "gotcha needs resolution"
    );
}
