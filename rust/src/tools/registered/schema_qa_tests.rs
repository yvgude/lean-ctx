//! Runtime validation of published action-conditional tool schemas (#1008).

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

/// Prose regression guard (#1020): the published ctx_patch description teaches
/// callers which param each op needs. A rename that updates the code but not the
/// prose (e.g. new_body→new_text) leaves the schema description silently stale.
/// Assert every op's required param is still named, and the retired alias is not.
#[test]
fn patch_description_names_each_ops_required_param() {
    let def = CtxPatchTool.tool_def();
    let desc = def.description.as_deref().unwrap_or("");

    for token in [
        "line",
        "hash", // anchored ops: set_line/replace_lines/insert_after/delete
        "old_text",
        "new_text", // replace_unique(path,old_text,new_text)
        "replace_symbol",
        "create",
        "replace_all",
        "ops[]", // op routing
    ] {
        assert!(
            desc.contains(token),
            "ctx_patch description must name `{token}`; got: {desc}"
        );
    }

    // new_text is the single write-param name across every op (#1020); new_body
    // was fully retired (no alias) — it must never reappear in the published prose.
    assert!(
        !desc.contains("new_body"),
        "new_body is retired; published prose must say new_text"
    );
}
