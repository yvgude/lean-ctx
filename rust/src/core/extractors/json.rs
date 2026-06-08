//! JSON → clean text + structure-aware chunks (EPIC 12.13).
//!
//! Valid JSON is rendered as stable pretty text and chunked by its top-level
//! structure (one chunk per array element / object entry) so each chunk is a
//! self-contained record. Invalid JSON degrades gracefully to a single chunk —
//! the seam must never panic or drop content for arbitrary input.

use serde_json::Value;

/// Render `input` as normalized JSON text (pretty, stable key order via serde).
/// Falls back to the trimmed input when it is not valid JSON.
#[must_use]
pub fn to_text(input: &str) -> String {
    match serde_json::from_str::<Value>(input) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| input.trim().to_string()),
        Err(_) => input.trim().to_string(),
    }
}

/// Structure-aware chunks: array ⇒ one chunk per element; object ⇒ one chunk per
/// `"key": value` entry; scalar/invalid ⇒ a single chunk. Never empty for
/// non-empty input.
#[must_use]
pub fn chunks(input: &str) -> Vec<String> {
    let out = match serde_json::from_str::<Value>(input) {
        Ok(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|v| serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
            .collect(),
        Ok(Value::Object(map)) if !map.is_empty() => map
            .iter()
            .map(|(k, v)| {
                let val = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
                format!("{}: {}", serde_json::to_string(k).unwrap_or_default(), val)
            })
            .collect(),
        Ok(other) => {
            vec![serde_json::to_string_pretty(&other).unwrap_or_else(|_| other.to_string())]
        }
        Err(_) => vec![input.trim().to_string()],
    };
    out.into_iter().filter(|c| !c.trim().is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_chunks_per_element() {
        let c = chunks(r#"[{"a":1},{"b":2}]"#);
        assert_eq!(c.len(), 2);
        assert!(c[0].contains("\"a\""));
        assert!(c[1].contains("\"b\""));
    }

    #[test]
    fn object_chunks_per_entry() {
        let c = chunks(r#"{"name":"x","age":3}"#);
        assert_eq!(c.len(), 2);
        assert!(c.iter().any(|s| s.contains("\"name\"")));
    }

    #[test]
    fn invalid_json_is_single_chunk() {
        let c = chunks("not json at all");
        assert_eq!(c, vec!["not json at all".to_string()]);
    }

    #[test]
    fn to_text_normalizes() {
        let t = to_text(r#"{"b":2,"a":1}"#);
        assert!(t.contains("\"a\": 1"));
    }
}
