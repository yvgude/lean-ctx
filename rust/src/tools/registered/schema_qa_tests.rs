//! Runtime validation of published action-conditional tool schemas (#1008).

use std::collections::HashSet;

use serde_json::{Value, json};

use super::ctx_callgraph::CtxCallgraphTool;
use super::ctx_execute::CtxExecuteTool;
use super::ctx_expand::CtxExpandTool;
use super::ctx_graph::CtxGraphTool;
use super::ctx_knowledge::CtxKnowledgeTool;
use super::ctx_patch::CtxPatchTool;
use super::ctx_search::CtxSearchTool;
use crate::server::tool_trait::McpTool;

fn validator(tool: &dyn McpTool) -> jsonschema::Validator {
    let schema = Value::Object((*tool.tool_def().input_schema).clone());
    jsonschema::validator_for(&schema).expect("published tool schema must compile")
}

#[test]
fn callgraph_expand_and_graph_require_action_inputs() {
    let callgraph = validator(&CtxCallgraphTool);
    assert!(callgraph.is_valid(&json!({"action":"callers","symbol":"f"})));
    assert!(callgraph.is_valid(&json!({"action":"trace","from":"a","to":"b"})));
    assert!(!callgraph.is_valid(&json!({})));
    assert!(!callgraph.is_valid(&json!({"action":"trace","from":"a"})));

    let expand = validator(&CtxExpandTool);
    assert!(expand.is_valid(&json!({"id":"F1"})));
    assert!(expand.is_valid(&json!({"action":"list"})));
    assert!(!expand.is_valid(&json!({})));
    assert!(!expand.is_valid(&json!({"action":"search_all"})));

    let graph = validator(&CtxGraphTool);
    assert!(graph.is_valid(&json!({"action":"status"})));
    assert!(graph.is_valid(&json!({"action":"path","path":"a","to":"b"})));
    assert!(!graph.is_valid(&json!({"action":"symbol"})));
    assert!(!graph.is_valid(&json!({"action":"path","path":"a"})));
}

#[test]
fn knowledge_search_and_execute_require_mode_specific_inputs() {
    let knowledge = validator(&CtxKnowledgeTool);
    assert!(knowledge.is_valid(&json!({"action":"remember","category":"decision","value":"v"})));
    assert!(knowledge.is_valid(&json!({"action":"recall"})));
    assert!(!knowledge.is_valid(&json!({"action":"remember","value":"v"})));
    assert!(!knowledge.is_valid(&json!({"action":"gotcha","trigger":"t"})));

    let search = validator(&CtxSearchTool);
    assert!(search.is_valid(&json!({"pattern":"needle"})));
    assert!(search.is_valid(&json!({"action":"symbol","handle":"f.rs#f@L1"})));
    assert!(!search.is_valid(&json!({})));
    assert!(!search.is_valid(&json!({"action":"semantic"})));

    let execute = validator(&CtxExecuteTool);
    assert!(execute.is_valid(&json!({"language":"python","code":"print(1)"})));
    assert!(execute.is_valid(&json!({"action":"file","path":"a.py"})));
    assert!(!execute.is_valid(&json!({})));
    assert!(!execute.is_valid(&json!({"action":"batch"})));
}

/// Collect every declared `properties` key anywhere in a schema (top level and
/// nested — object params, `if`/`then` applicators, array `items`).
fn collect_property_names(node: &Value, out: &mut HashSet<String>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("properties") {
                out.extend(props.keys().cloned());
            }
            for v in map.values() {
                collect_property_names(v, out);
            }
        }
        Value::Array(arr) => arr.iter().for_each(|v| collect_property_names(v, out)),
        _ => {}
    }
}

/// Collect every string listed in any `required` array anywhere in a schema —
/// including the `then.required` of conditional (`if`/`then`) subschemas.
fn collect_required(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::Array(req)) = map.get("required") {
                out.extend(req.iter().filter_map(|r| r.as_str().map(str::to_string)));
            }
            for v in map.values() {
                collect_required(v, out);
            }
        }
        Value::Array(arr) => arr.iter().for_each(|v| collect_required(v, out)),
        _ => {}
    }
}

/// Generic schema-integrity guard (#1020): once per-op requirements live in the
/// schema (as `if`/`then` conditionals) rather than only in imperative parser
/// code, the schema IS the source of truth — and requiring a param the schema
/// never declares is a self-contradiction a client hits before the handler runs.
/// Assert, for every registered tool, that every `required` token is a declared
/// property. A `then: {required:["new_body"]}` left after `new_body` was renamed
/// to `new_text` fails here, for all tools, with no per-tool hardcoding.
#[test]
fn every_required_param_is_a_declared_property() {
    let registry = crate::server::registry::build_registry();
    let mut violations = Vec::new();

    for def in registry.tool_defs() {
        let schema = Value::Object((*def.input_schema).clone());
        let mut props = HashSet::new();
        collect_property_names(&schema, &mut props);
        let mut required = Vec::new();
        collect_required(&schema, &mut required);
        for r in required {
            if !props.contains(&r) {
                violations.push(format!(
                    "{}: schema requires `{r}` but declares no such property",
                    def.name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "tool schemas require undeclared params (retired/typo'd field?):\n  {}",
        violations.join("\n  ")
    );
}

/// The sharp ctx_patch schema (#1020) encodes per-op required params, so a
/// client knows before calling that `replace_lines` needs `new_text` — not the
/// retired `new_body`. Exercise the conditionals directly.
#[test]
fn patch_schema_encodes_per_op_required_params() {
    let patch = validator(&CtxPatchTool);

    // set_line requires new_text; the retired new_body does not satisfy it.
    assert!(
        patch.is_valid(&json!({"op":"set_line","path":"a","line":1,"hash":"aa","new_text":"x"}))
    );
    assert!(
        !patch.is_valid(&json!({"op":"set_line","path":"a","line":1,"hash":"aa","new_body":"x"}))
    );

    // replace_lines requires all four anchors + new_text.
    assert!(patch.is_valid(&json!({
        "op":"replace_lines","path":"a",
        "start_line":1,"start_hash":"aa","end_line":2,"end_hash":"bb","new_text":"y"
    })));
    assert!(!patch.is_valid(&json!({
        "op":"replace_lines","path":"a",
        "start_line":1,"start_hash":"aa","end_line":2,"end_hash":"bb"
    })));

    // create/replace_all bodies.
    assert!(!patch.is_valid(&json!({"op":"create","path":"a"})));
    assert!(patch.is_valid(&json!({"op":"create","path":"a","new_text":""})));
    assert!(!patch.is_valid(&json!({"op":"replace_all","path":"a","find":"x"})));
    assert!(patch.is_valid(&json!({"op":"replace_all","path":"a","find":"x","replace":"y"})));

    // Conditionals stay dormant for batch calls: `op` lives inside ops[], so no
    // top-level `if op==…` fires and the batch validates without body params.
    assert!(patch.is_valid(&json!({
        "ops":[{"op":"set_line","path":"a","line":1,"hash":"aa","new_text":"x"}]
    })));
}
