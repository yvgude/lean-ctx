use crate::core::json_crush;

#[must_use]
pub fn compress(output: &str) -> Option<String> {
    let trimmed = output.trim();

    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return None;
    }

    let val: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Prefer the lossless crusher when the payload is redundant: it keeps all
    // data (reconstructible via `json_crush::reconstruct`) instead of the
    // schema outline that drops every value (#934 / #936).
    if let Some(text) = json_crush::crush_value_if_beneficial(&val, trimmed.len()) {
        return Some(text);
    }

    let schema = extract_schema(&val, 0);
    Some(schema)
}

/// Lossless crush of a verbatim data-command's JSON output (`gh api`, `jq`,
/// `kubectl get -o json`, `curl` …). Returns `Some` only when it at least
/// halves the payload, so an opt-in caller reshapes solely when it clearly
/// pays — and never loses a datum. Returns `None` for non-JSON or low-redundancy
/// output (the caller then keeps it verbatim).
pub fn crush_verbatim(output: &str) -> Option<String> {
    json_crush::crush_text_if_beneficial(output)
}

fn extract_schema(val: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    match val {
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return format!("{indent}{{}}");
            }
            if depth > 3 {
                return format!("{indent}{{...{} keys}}", map.len());
            }

            let mut entries = Vec::new();
            for (key, value) in map.iter().take(20) {
                let type_str = type_of(value);
                match value {
                    serde_json::Value::Object(inner) if !inner.is_empty() && depth < 3 => {
                        let nested = extract_schema(value, depth + 1);
                        entries.push(format!("{indent}  {key}: {{\n{nested}\n{indent}  }}"));
                    }
                    serde_json::Value::Array(arr) if !arr.is_empty() => {
                        let item_type = if let Some(first) = arr.first() {
                            type_of(first)
                        } else {
                            "any".to_string()
                        };
                        entries.push(format!("{indent}  {key}: [{item_type}...{}]", arr.len()));
                    }
                    _ => {
                        entries.push(format!("{indent}  {key}: {type_str}"));
                    }
                }
            }
            if map.len() > 20 {
                entries.push(format!("{indent}  ...+{} more keys", map.len() - 20));
            }
            entries.join("\n")
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return format!("{indent}[]");
            }
            let first_schema = extract_schema(&arr[0], depth + 1);
            format!(
                "{indent}[{} items, each:\n{first_schema}\n{indent}]",
                arr.len()
            )
        }
        other => format!("{indent}{}", type_of(other)),
    }
}

fn type_of(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(_) => "bool".to_string(),
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                "float".to_string()
            } else {
                "int".to_string()
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 50 {
                format!("str({})", s.len())
            } else {
                "str".to_string()
            }
        }
        serde_json::Value::Array(arr) => format!("[...{}]", arr.len()),
        serde_json::Value::Object(map) => format!("{{...{} keys}}", map.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Array of objects sharing constant `status`/`region`, only `id` varying —
    /// high redundancy the lossless crusher should factor into `_defaults`.
    fn redundant_array(n: usize) -> String {
        let items: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "{{\"status\":\"ok\",\"region\":\"us-east-1\",\"tier\":\"standard\",\"id\":{i}}}"
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn compress_prefers_lossless_crush_for_redundant_array() {
        let raw = redundant_array(12);
        let out = compress(&raw).expect("array-of-objects compresses");

        // Lossless crush is chosen (marker present) — not the value-dropping
        // schema outline.
        assert!(out.contains("_lc_crush"), "expected crushed form: {out}");
        assert!(!out.contains("items, each"), "schema outline leaked: {out}");

        // It actually pays: at least halves the payload.
        assert!(
            out.len() * 2 <= raw.len(),
            "crush must at least halve payload"
        );

        // And it is fully reversible.
        let restored = json_crush::reconstruct(&out).expect("crushed form reconstructs");
        let expected: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(restored, expected, "roundtrip must be lossless");
    }

    #[test]
    fn compress_falls_back_to_schema_for_heterogeneous_array() {
        // Every field varies → nothing to factor → crush not beneficial → the
        // (lossy) schema outline is used instead.
        let raw = r#"[{"id":1,"name":"alice","email":"a@x.io"},{"id":2,"name":"bob","email":"b@y.io"},{"id":3,"name":"cara","email":"c@z.io"}]"#;
        let out = compress(raw).expect("array compresses to schema");
        assert!(
            !out.contains("_lc_crush"),
            "should not crush low-redundancy"
        );
        assert!(
            out.contains("items, each"),
            "expected schema outline: {out}"
        );
    }

    #[test]
    fn crush_verbatim_some_only_when_it_pays() {
        // Redundant → reshaped (and reversible).
        let raw = redundant_array(20);
        let crushed = crush_verbatim(&raw).expect("redundant verbatim json is crushed");
        assert!(crushed.len() * 2 <= raw.len());
        let restored = json_crush::reconstruct(&crushed).unwrap();
        assert_eq!(
            restored,
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()
        );

        // Low redundancy → None (caller keeps it verbatim).
        let hetero = r#"[{"id":1,"k":"aaa"},{"id":2,"k":"bbb"}]"#;
        assert!(crush_verbatim(hetero).is_none());

        // Non-JSON → None.
        assert!(crush_verbatim("not json at all").is_none());
        assert!(crush_verbatim("").is_none());
    }

    #[test]
    fn compress_is_deterministic() {
        let raw = redundant_array(15);
        let a = compress(&raw).unwrap();
        let b = compress(&raw).unwrap();
        assert_eq!(a, b, "output must be a pure function of input");
    }
}
