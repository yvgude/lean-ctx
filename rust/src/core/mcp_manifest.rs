use std::path::PathBuf;

use md5::{Digest, Md5};
use rmcp::model::Tool;
use serde_json::{json, Value};

const READ_MODES: [&str; 10] = [
    "auto",
    "full",
    "map",
    "signatures",
    "diff",
    "aggressive",
    "entropy",
    "task",
    "reference",
    "lines:N-M",
];

fn extract_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for k in keys {
        if let Some(val) = v.get(*k) {
            return Some(val);
        }
    }
    None
}

fn md5_hex(s: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn normalize_tool_entry(tool_json: &Value) -> Value {
    let name = extract_field(tool_json, &["name"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = extract_field(tool_json, &["description"])
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let schema = extract_field(tool_json, &["inputSchema", "input_schema"])
        .cloned()
        .unwrap_or(Value::Null);
    let schema_str = serde_json::to_string(&schema).unwrap_or_default();
    let schema_md5 = md5_hex(&schema_str);

    json!({
        "name": name,
        "description": description,
        "input_schema": schema,
        "schema_md5": schema_md5
    })
}

pub fn default_manifest_path() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    repo_root.join("website/generated/mcp-tools.json")
}

pub fn manifest_value() -> Value {
    let mut granular_tools: Vec<Tool> = crate::tool_defs::granular_tool_defs();
    granular_tools.sort_by_key(|t| t.name.clone());

    let mut unified_tools: Vec<Tool> = crate::tool_defs::unified_tool_defs();
    unified_tools.sort_by_key(|t| t.name.clone());

    let granular: Vec<Value> = granular_tools
        .into_iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .map(|v| normalize_tool_entry(&v))
        .collect();

    let unified: Vec<Value> = unified_tools
        .into_iter()
        .filter_map(|t| serde_json::to_value(t).ok())
        .map(|v| normalize_tool_entry(&v))
        .collect();

    json!({
        "schema_version": 1,
        "counts": {
            "granular": granular.len(),
            "unified": unified.len()
        },
        "read_modes": {
            "count": READ_MODES.len(),
            "modes": READ_MODES
        },
        "tools": {
            "granular": granular,
            "unified": unified
        }
    })
}

pub fn manifest_pretty_json() -> String {
    let mut s =
        serde_json::to_string_pretty(&manifest_value()).unwrap_or_else(|_| "{}".to_string());
    s.push('\n');
    s
}
