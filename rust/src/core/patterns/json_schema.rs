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

    let schema = extract_schema(&val, 0);
    Some(schema)
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
