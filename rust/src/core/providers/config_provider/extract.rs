//! JSON field extraction using dot-notation paths.
//!
//! Navigates JSON values by paths like `"data.items"`, `"fields.status.name"`,
//! or `"labels[].name"` (iterates over array elements).

use serde_json::Value;

use super::schema::FieldMapping;
use crate::core::providers::ProviderItem;

/// Navigate a JSON value by a dot-notation path.
///
/// Supports:
/// - `"field"` — direct key access
/// - `"parent.child"` — nested access
/// - `"array[].field"` — map over array elements and collect
///
/// Returns `None` if the path doesn't resolve.
#[must_use]
pub fn extract_value(json: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(json.clone());
    }

    let segments: Vec<&str> = path.split('.').collect();
    navigate(json, &segments)
}

fn navigate(json: &Value, segments: &[&str]) -> Option<Value> {
    if segments.is_empty() {
        return Some(json.clone());
    }

    let segment = segments[0];
    let rest = &segments[1..];

    // Array iteration: "labels[]" or "labels[].name"
    if let Some(key) = segment.strip_suffix("[]") {
        let array = if key.is_empty() {
            json.as_array()?
        } else {
            json.get(key)?.as_array()?
        };

        if rest.is_empty() {
            return Some(Value::Array(array.clone()));
        }

        let collected: Vec<Value> = array
            .iter()
            .filter_map(|item| navigate(item, rest))
            .collect();

        if collected.is_empty() {
            None
        } else {
            Some(Value::Array(collected))
        }
    } else {
        let next = json.get(segment)?;
        navigate(next, rest)
    }
}

/// Extract a string value, handling different JSON types gracefully.
fn value_to_string(val: &Value) -> Option<String> {
    match val {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().filter_map(value_to_string).collect();
            if items.is_empty() {
                None
            } else {
                Some(items.join(", "))
            }
        }
        Value::Object(_) => Some(val.to_string()),
    }
}

/// Extract a `Vec<String>` from a JSON value (for labels, tags, etc.).
fn value_to_string_vec(val: &Value) -> Vec<String> {
    match val {
        Value::Array(arr) => arr.iter().filter_map(value_to_string).collect(),
        Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Navigate to the items array in a JSON response using the configured root path.
pub fn extract_items_array(response: &Value, root: Option<&str>) -> Result<Vec<Value>, String> {
    let target = match root {
        Some(path) => extract_value(response, path)
            .ok_or_else(|| format!("Root path '{path}' not found in response"))?,
        None => response.clone(),
    };

    match target {
        Value::Array(items) => Ok(items),
        Value::Object(_) => Ok(vec![target]),
        _ => Err(format!(
            "Expected array at root path, got {}",
            type_name(&target)
        )),
    }
}

fn type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Map a single JSON object to a `ProviderItem` using the field mapping.
#[must_use]
pub fn map_item(json: &Value, mapping: &FieldMapping) -> Option<ProviderItem> {
    let id = extract_value(json, &mapping.id).and_then(|v| value_to_string(&v))?;
    let title = extract_value(json, &mapping.title)
        .and_then(|v| value_to_string(&v))
        .unwrap_or_else(|| "(untitled)".into());

    Some(ProviderItem {
        id,
        title,
        state: mapping
            .state
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        author: mapping
            .author
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        created_at: mapping
            .created_at
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        updated_at: mapping
            .updated_at
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        url: mapping
            .url
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        labels: mapping
            .labels
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .map(|v| value_to_string_vec(&v))
            .unwrap_or_default(),
        body: mapping
            .body
            .as_ref()
            .and_then(|p| extract_value(json, p))
            .and_then(|v| value_to_string(&v)),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_simple_field() {
        let data = json!({"name": "Alice", "age": 30});
        assert_eq!(extract_value(&data, "name"), Some(json!("Alice")));
        assert_eq!(extract_value(&data, "age"), Some(json!(30)));
    }

    #[test]
    fn extract_nested_field() {
        let data = json!({"user": {"profile": {"name": "Bob"}}});
        assert_eq!(
            extract_value(&data, "user.profile.name"),
            Some(json!("Bob"))
        );
    }

    #[test]
    fn extract_missing_field_returns_none() {
        let data = json!({"name": "Alice"});
        assert_eq!(extract_value(&data, "email"), None);
        assert_eq!(extract_value(&data, "user.name"), None);
    }

    #[test]
    fn extract_array_iteration() {
        let data = json!({
            "labels": [
                {"name": "bug", "color": "red"},
                {"name": "fix", "color": "green"}
            ]
        });
        assert_eq!(
            extract_value(&data, "labels[].name"),
            Some(json!(["bug", "fix"]))
        );
    }

    #[test]
    fn extract_nested_array_iteration() {
        let data = json!({
            "data": {
                "issues": {
                    "nodes": [
                        {"title": "Issue 1"},
                        {"title": "Issue 2"}
                    ]
                }
            }
        });
        assert_eq!(
            extract_value(&data, "data.issues.nodes[].title"),
            Some(json!(["Issue 1", "Issue 2"]))
        );
    }

    #[test]
    fn extract_items_array_with_root() {
        let response = json!({
            "data": {
                "items": [
                    {"id": 1, "name": "A"},
                    {"id": 2, "name": "B"}
                ]
            }
        });
        let items = extract_items_array(&response, Some("data.items")).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn extract_items_array_no_root() {
        let response = json!([
            {"id": 1, "name": "A"},
            {"id": 2, "name": "B"}
        ]);
        let items = extract_items_array(&response, None).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn map_item_full_mapping() {
        let data = json!({
            "key": "PROJ-42",
            "fields": {
                "summary": "Auth bug",
                "description": "Login fails",
                "status": {"name": "Open"},
                "reporter": {"displayName": "Dev"},
                "labels": ["bug", "critical"],
                "created": "2025-01-01",
                "updated": "2025-06-15"
            },
            "self": "https://jira.example.com/PROJ-42"
        });

        let mapping = FieldMapping {
            id: "key".into(),
            title: "fields.summary".into(),
            body: Some("fields.description".into()),
            state: Some("fields.status.name".into()),
            author: Some("fields.reporter.displayName".into()),
            url: Some("self".into()),
            labels: Some("fields.labels".into()),
            created_at: Some("fields.created".into()),
            updated_at: Some("fields.updated".into()),
        };

        let item = map_item(&data, &mapping).unwrap();
        assert_eq!(item.id, "PROJ-42");
        assert_eq!(item.title, "Auth bug");
        assert_eq!(item.body, Some("Login fails".into()));
        assert_eq!(item.state, Some("Open".into()));
        assert_eq!(item.author, Some("Dev".into()));
        assert_eq!(item.labels, vec!["bug", "critical"]);
    }

    #[test]
    fn map_item_minimal_mapping() {
        let data = json!({"id": 99, "title": "Quick fix"});
        let mapping = FieldMapping {
            id: "id".into(),
            title: "title".into(),
            body: None,
            state: None,
            author: None,
            url: None,
            labels: None,
            created_at: None,
            updated_at: None,
        };
        let item = map_item(&data, &mapping).unwrap();
        assert_eq!(item.id, "99");
        assert_eq!(item.title, "Quick fix");
        assert!(item.body.is_none());
        assert!(item.labels.is_empty());
    }

    #[test]
    fn value_to_string_handles_types() {
        assert_eq!(value_to_string(&json!("hello")), Some("hello".into()));
        assert_eq!(value_to_string(&json!(42)), Some("42".into()));
        assert_eq!(value_to_string(&json!(true)), Some("true".into()));
        assert_eq!(value_to_string(&json!(null)), None);
        assert_eq!(value_to_string(&json!(["a", "b"])), Some("a, b".into()));
    }

    #[test]
    fn extract_empty_path_returns_root() {
        let data = json!({"key": "val"});
        assert_eq!(extract_value(&data, ""), Some(data));
    }
}
