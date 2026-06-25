use std::sync::Arc;

use rmcp::model::Tool;
use serde_json::{Map, Value};

mod granular;
pub use granular::{granular_tool_defs, unified_tool_defs};

#[must_use]
pub fn tool_def(name: &'static str, description: &'static str, schema_value: Value) -> Tool {
    let mut schema: Map<String, Value> = match schema_value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    normalize_for_strict_validators(&mut schema);
    Tool::new(name, description, Arc::new(schema))
}

/// Make a tool input schema acceptable to *strict* JSON-Schema validators.
///
/// OpenAI/Azure (Pydantic-based), Claude thinking models and OpenAI-compatible
/// backends like `SGLang` reject tool schemas that are valid JSON Schema but
/// omit fields the spec treats as optional. Community-reported failures
/// (`OpenCode`: "Invalid schema for function 'lean-ctx_ctx_expand': None is not
/// of type 'array'"):
///
/// - `type: "object"` with `properties` but no `required` → clients forward
///   `required: null` and the backend 400s. We always emit an explicit array.
/// - `type: "array"` without `items` → "array schema missing items". We emit
///   a permissive `items: {}` so the wire schema is self-contained.
///
/// Runs recursively over every nested schema position (`properties`, `items`,
/// `anyOf`/`oneOf`/`allOf`, object-shaped `additionalProperties`) so nested
/// definitions get the same guarantees. Existing `required` arrays are
/// preserved verbatim — this never changes which parameters are mandatory.
pub fn normalize_for_strict_validators(schema: &mut Map<String, Value>) {
    let is_object = schema.get("type").and_then(Value::as_str) == Some("object");
    let is_array = schema.get("type").and_then(Value::as_str) == Some("array");

    if is_object && schema.contains_key("properties") && !schema.contains_key("required") {
        schema.insert("required".into(), Value::Array(Vec::new()));
    }
    if is_array && !schema.contains_key("items") {
        schema.insert("items".into(), Value::Object(Map::new()));
    }

    if let Some(Value::Object(props)) = schema.get_mut("properties") {
        for prop in props.values_mut() {
            if let Value::Object(p) = prop {
                normalize_for_strict_validators(p);
            }
        }
    }
    if let Some(Value::Object(items)) = schema.get_mut("items") {
        normalize_for_strict_validators(items);
    }
    if let Some(Value::Object(ap)) = schema.get_mut("additionalProperties") {
        normalize_for_strict_validators(ap);
    }
    for combinator in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(branches)) = schema.get_mut(combinator) {
            for branch in branches.iter_mut() {
                if let Value::Object(b) = branch {
                    normalize_for_strict_validators(b);
                }
            }
        }
    }
}

pub const CORE_TOOL_NAMES: &[&str] = &[
    "ctx_read",
    "ctx_shell",
    "shell",
    "ctx_search",
    "ctx_glob",
    "ctx_tree",
    "ctx_session",
    "ctx_compose",
    "ctx_graph",
    "ctx_call",
    "ctx_expand",
];

#[must_use]
pub fn core_tool_names() -> &'static [&'static str] {
    CORE_TOOL_NAMES
}

#[must_use]
pub fn lazy_tool_defs() -> Vec<Tool> {
    let all = granular_tool_defs();
    all.into_iter()
        .filter(|t| CORE_TOOL_NAMES.contains(&t.name.as_ref()))
        .collect()
}

#[must_use]
pub fn discover_tools(query: &str) -> String {
    // Derived from the registry (single source of truth) so discovery results
    // never drift from the advertised tool schemas (#141).
    let all = crate::server::registry::build_registry().tool_defs();
    let query_lower = query.to_lowercase();
    let matches: Vec<(String, String)> = all
        .iter()
        .filter_map(|t| {
            let name = t.name.as_ref();
            let desc = t.description.as_deref().unwrap_or("");
            if name.to_lowercase().contains(&query_lower)
                || desc.to_lowercase().contains(&query_lower)
            {
                Some((name.to_string(), desc.to_string()))
            } else {
                None
            }
        })
        .collect();

    if matches.is_empty() {
        return format!(
            "No tools found matching '{query}'. Try broader terms like: graph, cost, session, search, compress, agent, workflow, gain."
        );
    }

    let mut out = format!("{} tools matching '{query}':\n", matches.len());
    for (name, desc) in &matches {
        // First line only — registry descriptions can be multi-line.
        let first = desc.lines().next().unwrap_or(desc);
        let short = if first.len() > 80 {
            &first[..first.floor_char_boundary(80)]
        } else {
            first
        };
        out.push_str(&format!("  {name} — {short}\n"));
    }
    out.push_str(
        "\nIf your MCP client registers tools only once at startup (static tools/list), \
use ctx_call (available in lazy mode) to invoke discovered tools:\n\
  ctx_call {\"name\":\"ctx_graph\",\"arguments\":{\"action\":\"status\"}}\n",
    );
    out
}

#[must_use]
pub fn is_full_mode() -> bool {
    std::env::var("LEAN_CTX_FULL_TOOLS").is_ok_and(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        || std::env::var("LEAN_CTX_LAZY_TOOLS")
            .is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("false"))
}
