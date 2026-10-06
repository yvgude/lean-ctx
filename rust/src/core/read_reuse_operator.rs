// SPDX-License-Identifier: Apache-2.0

//! Pure native-Read response shaping shared by Local and Via Edge.
//!
//! Product policy, filesystem admission, delivery state and hook I/O stay in
//! their owning runtimes. This module only recognizes supported host response
//! shapes and replaces the one content-bearing string.

// This source is also compiled as a public module by `lean-ctx-operators`.
#![allow(unreachable_pub)]

/// Return the text-bearing field of a supported native Read response.
pub fn response_text(response: &serde_json::Value) -> Option<&str> {
    let slot = locate_content(response)?;
    slot_text(response, &slot)
}

/// Clone a supported native Read response and replace only its text-bearing
/// field. Unknown response shapes are rejected so callers can fail open.
pub fn replace_response_text(
    response: &serde_json::Value,
    replacement: &str,
) -> Option<serde_json::Value> {
    let slot = locate_content(response)?;
    replace_slot(response, &slot, replacement)
}

/// Deterministic unchanged marker used by native-Read delivery reuse.
pub fn render_unchanged_stub(path: &str, line_count: usize) -> String {
    format!(
        "{path} [unchanged {line_count}L · lean-ctx read-dedup]\nUnchanged since your last Read in this session — the full line-numbered content is already in this conversation above. It will be re-delivered automatically once the file changes on disk."
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContentSlot {
    WholeString,
    Field(Vec<String>),
    TextBlock(usize),
    ContentTextBlock(usize),
}

fn text_block_index(blocks: &[serde_json::Value]) -> Option<usize> {
    blocks.iter().position(|block| {
        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
            && block
                .get("text")
                .and_then(serde_json::Value::as_str)
                .is_some()
    })
}

fn locate_content(response: &serde_json::Value) -> Option<ContentSlot> {
    match response {
        serde_json::Value::String(_) => Some(ContentSlot::WholeString),
        serde_json::Value::Array(blocks) => text_block_index(blocks).map(ContentSlot::TextBlock),
        serde_json::Value::Object(object) => {
            if object
                .get("file")
                .and_then(|file| file.get("content"))
                .and_then(serde_json::Value::as_str)
                .is_some()
            {
                return Some(ContentSlot::Field(vec![
                    "file".to_string(),
                    "content".to_string(),
                ]));
            }
            match object.get("content") {
                Some(serde_json::Value::String(_)) => {
                    Some(ContentSlot::Field(vec!["content".to_string()]))
                }
                Some(serde_json::Value::Array(blocks)) => {
                    text_block_index(blocks).map(ContentSlot::ContentTextBlock)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn slot_text<'a>(response: &'a serde_json::Value, slot: &ContentSlot) -> Option<&'a str> {
    match slot {
        ContentSlot::WholeString => response.as_str(),
        ContentSlot::Field(keys) => {
            let mut current = response;
            for key in keys {
                current = current.get(key)?;
            }
            current.as_str()
        }
        ContentSlot::TextBlock(index) => response.get(*index)?.get("text")?.as_str(),
        ContentSlot::ContentTextBlock(index) => {
            response.get("content")?.get(*index)?.get("text")?.as_str()
        }
    }
}

fn replace_slot(
    response: &serde_json::Value,
    slot: &ContentSlot,
    replacement: &str,
) -> Option<serde_json::Value> {
    let replacement = serde_json::Value::String(replacement.to_string());
    let mut output = response.clone();
    match slot {
        ContentSlot::WholeString => Some(replacement),
        ContentSlot::Field(keys) => {
            let mut current = &mut output;
            let (last, parents) = keys.split_last()?;
            for key in parents {
                current = current.get_mut(key)?;
            }
            *current.get_mut(last)? = replacement;
            Some(output)
        }
        ContentSlot::TextBlock(index) => {
            *output.get_mut(*index)?.get_mut("text")? = replacement;
            Some(output)
        }
        ContentSlot::ContentTextBlock(index) => {
            *output
                .get_mut("content")?
                .get_mut(*index)?
                .get_mut("text")? = replacement;
            Some(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_supported_shapes_and_rejects_unknown_shape() {
        let cases = [
            serde_json::json!("body"),
            serde_json::json!({"file":{"content":"body","path":"kept"}}),
            serde_json::json!({"content":"body","flag":true}),
            serde_json::json!([{"type":"text","text":"body"},{"type":"image"}]),
            serde_json::json!({"content":[{"type":"text","text":"body"}]}),
        ];
        for case in cases {
            assert_eq!(response_text(&case), Some("body"));
            let replaced = replace_response_text(&case, "stub").unwrap();
            assert_eq!(response_text(&replaced), Some("stub"));
        }
        assert!(replace_response_text(&serde_json::json!({"unknown": "body"}), "stub").is_none());
    }

    #[test]
    fn stub_is_deterministic() {
        assert_eq!(
            render_unchanged_stub("/a/b.rs", 42),
            render_unchanged_stub("/a/b.rs", 42)
        );
    }
}
