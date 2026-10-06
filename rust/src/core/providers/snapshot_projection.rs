// SPDX-License-Identifier: Apache-2.0
//! Shared deterministic snapshot projection for acquisition and source revalidation.
use serde_json::Value;

pub(crate) const MAX_SNAPSHOT_ARRAY_ITEMS: usize = 100;
pub(crate) const MAX_SNAPSHOT_STRING_BYTES: usize = 4096;
pub(crate) const MAX_SNAPSHOT_BODY_BYTES: usize = 32 * 1024;

/// Apply the existing mandatory secret redactor to every string field before
/// canonicalization.  This includes nested claims and caller-supplied filters.
pub(crate) fn redact_and_bound(value: &mut Value, key: Option<&str>) -> bool {
    match value {
        Value::String(text) => {
            let max_bytes = if key == Some("body") {
                MAX_SNAPSHOT_BODY_BYTES
            } else {
                MAX_SNAPSHOT_STRING_BYTES
            };
            let redacted = crate::core::redaction::redact_text(text);
            let truncated = redacted.len() > max_bytes;
            *text = truncate_utf8(&redacted, max_bytes);
            truncated
        }
        Value::Array(values) => {
            let mut changed = false;
            if matches!(key, Some("items" | "claims" | "labels"))
                && values.len() > MAX_SNAPSHOT_ARRAY_ITEMS
            {
                values.truncate(MAX_SNAPSHOT_ARRAY_ITEMS);
                changed = true;
            }
            for value in values {
                changed |= redact_and_bound(value, None);
            }
            changed
        }
        Value::Object(object) => {
            let mut changed = false;
            for (key, value) in object {
                changed |= redact_and_bound(value, Some(key));
            }
            changed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub(crate) fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
