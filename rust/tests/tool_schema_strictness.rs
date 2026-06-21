//! Every advertised MCP tool schema must survive *strict* JSON-Schema
//! validators (OpenAI/Azure Pydantic backends, Claude thinking models,
//! `SGLang`, ...). Community-reported failure that motivated this gate
//! (`OpenCode` on Windows → OpenAI):
//!
//!   Invalid schema for function 'lean-ctx_ctx_expand': None is not of type 'array'
//!
//! Strict backends require an explicit `required` array on every object
//! schema that declares `properties`, and an `items` definition on every
//! array schema — at every nesting level. JSON Schema itself treats both as
//! optional, so plain spec-validity is not enough.

use serde_json::Value;

fn check_schema(tool: &str, node: &Value, path: &str, errors: &mut Vec<String>) {
    match node {
        Value::Null => errors.push(format!("{tool}: {path} is null")),
        Value::Object(map) => {
            let ty = map.get("type").and_then(Value::as_str);

            if ty == Some("object") && map.contains_key("properties") {
                match map.get("required") {
                    Some(Value::Array(_)) => {}
                    Some(other) => errors.push(format!(
                        "{tool}: {path}.required must be an array, got {other}"
                    )),
                    None => errors.push(format!(
                        "{tool}: {path} is an object schema with properties but no explicit `required` array (strict validators reject this)"
                    )),
                }
            }
            if ty == Some("array") && !map.contains_key("items") {
                errors.push(format!(
                    "{tool}: {path} is an array schema without `items` (strict validators reject this)"
                ));
            }

            for (key, value) in map {
                check_schema(tool, value, &format!("{path}.{key}"), errors);
            }
        }
        Value::Array(values) => {
            for (i, value) in values.iter().enumerate() {
                check_schema(tool, value, &format!("{path}[{i}]"), errors);
            }
        }
        _ => {}
    }
}

#[test]
fn all_tool_schemas_survive_strict_validators() {
    let registry = lean_ctx::server::registry::build_registry();
    let tools = registry.tool_defs();
    assert!(
        tools.len() >= 60,
        "registry unexpectedly small ({}) — wiring broken?",
        tools.len()
    );

    let mut errors = Vec::new();
    for tool in &tools {
        let schema = Value::Object((*tool.input_schema).clone());
        check_schema(tool.name.as_ref(), &schema, "schema", &mut errors);
    }

    assert!(
        errors.is_empty(),
        "tool schemas not strict-validator safe:\n  {}",
        errors.join("\n  ")
    );
}

#[test]
fn granular_and_unified_defs_are_strict_safe() {
    let mut errors = Vec::new();
    for tool in lean_ctx::tool_defs::granular_tool_defs()
        .iter()
        .chain(lean_ctx::tool_defs::unified_tool_defs().iter())
    {
        let schema = Value::Object((*tool.input_schema).clone());
        check_schema(tool.name.as_ref(), &schema, "schema", &mut errors);
    }
    assert!(
        errors.is_empty(),
        "tool schemas not strict-validator safe:\n  {}",
        errors.join("\n  ")
    );
}
