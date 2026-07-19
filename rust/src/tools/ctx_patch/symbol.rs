//! `replace_symbol` op: replace a whole symbol body by name (or path+line),
//! delegating to the LSP/IDE-aware `ctx_refactor::replace_symbol_body` (epic
//! #1008, plan Säule 2).
//!
//! ctx_patch's line-anchored ops are great for surgical line edits but a model
//! that wants to rewrite an entire function shouldn't have to enumerate every
//! line. `replace_symbol` gives that single, ergonomic surface while reusing
//! ctx_refactor's battle-tested symbol resolution, BLAKE3 CONFLICT guard and
//! atomic write — so there is exactly one symbol-edit implementation.
//!
//! It is intentionally **not** an [`super::anchors::AnchorOp`]: it goes through a
//! different (symbol-resolution) write path, so it cannot share the line-model
//! batch's single-preimage invariant and is handled as a standalone op.

use serde_json::{Map, Value};

/// True when the args describe a single `replace_symbol` op (never inside a
/// batch `ops[]`, which is reserved for the atomic line-model).
pub(crate) fn is_replace_symbol(args: &Map<String, Value>) -> bool {
    args.get("ops").is_none() && str_field(args, "op").as_deref() == Some("replace_symbol")
}

/// Translate ctx_patch `replace_symbol` args into the `ctx_refactor`
/// `replace_symbol_body` argument object. Pure + validated so it is unit-testable
/// without invoking the LSP layer.
///
/// Field mapping: `name`/`name_path` → `name_path`; `new_text` → `new_text`;
/// `path`+`line`(+`end_line`) and `expected_hash` pass through. `new_text` is the
/// single replacement-field name across every ctx_patch op (#1020).
pub(crate) fn build_refactor_args(args: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let new_text = str_field(args, "new_text").ok_or_else(|| {
        "replace_symbol requires 'new_text' (the full replacement declaration)".to_string()
    })?;

    let name = str_field(args, "name").or_else(|| str_field(args, "name_path"));
    let has_path = args.get("path").and_then(Value::as_str).is_some();
    if name.is_none() && !has_path {
        return Err("replace_symbol requires 'name' (symbol path) or 'path'+'line'".to_string());
    }

    let mut out = Map::new();
    out.insert(
        "action".to_string(),
        Value::String("replace_symbol_body".to_string()),
    );
    if let Some(n) = name {
        out.insert("name_path".to_string(), Value::String(n));
    }
    if has_path {
        if let Some(p) = args.get("path") {
            out.insert("path".to_string(), p.clone());
        }
        for key in ["line", "end_line"] {
            if let Some(v) = args.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    out.insert("new_text".to_string(), Value::String(new_text));
    if let Some(h) = str_field(args, "expected_hash") {
        out.insert("expected_hash".to_string(), Value::String(h));
    }
    Ok(out)
}

fn str_field(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("expected a JSON object"),
        }
    }

    #[test]
    fn detects_replace_symbol_only_as_single_op() {
        assert!(is_replace_symbol(&obj(
            json!({"op": "replace_symbol", "name": "foo"})
        )));
        assert!(!is_replace_symbol(&obj(json!({"op": "set_line"}))));
        // Never inside a batch — that path is the atomic line-model.
        assert!(!is_replace_symbol(&obj(json!({
            "ops": [{"op": "replace_symbol", "name": "foo"}]
        }))));
    }

    #[test]
    fn maps_name_route() {
        let out = build_refactor_args(&obj(json!({
            "op": "replace_symbol", "name": "Foo::bar", "new_text": "fn bar() {}"
        })))
        .unwrap();
        assert_eq!(out["action"], json!("replace_symbol_body"));
        assert_eq!(out["name_path"], json!("Foo::bar"));
        assert_eq!(out["new_text"], json!("fn bar() {}"));
        assert!(!out.contains_key("path"));
    }

    #[test]
    fn maps_path_line_route() {
        let out = build_refactor_args(&obj(json!({
            "op": "replace_symbol", "path": "src/a.rs", "line": 10, "end_line": 20,
            "new_text": "fn a() {}", "expected_hash": "abcd"
        })))
        .unwrap();
        assert_eq!(out["path"], json!("src/a.rs"));
        assert_eq!(out["line"], json!(10));
        assert_eq!(out["end_line"], json!(20));
        assert_eq!(out["new_text"], json!("fn a() {}"));
        assert_eq!(out["expected_hash"], json!("abcd"));
        assert!(!out.contains_key("name_path"));
    }

    #[test]
    fn requires_new_text() {
        let err =
            build_refactor_args(&obj(json!({"op": "replace_symbol", "name": "foo"}))).unwrap_err();
        assert!(err.contains("new_text"), "got: {err}");
    }

    #[test]
    fn requires_target() {
        let err = build_refactor_args(&obj(json!({
            "op": "replace_symbol", "new_text": "x"
        })))
        .unwrap_err();
        assert!(err.contains("name") || err.contains("path"), "got: {err}");
    }
}
