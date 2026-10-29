// SPDX-License-Identifier: Apache-2.0
//! K26: large tool descriptions leave the proxy shortened, with their full
//! definition one expand call away.
//!
//! A host lists every tool on every turn, and a tool definition can carry
//! kilobytes of prose. The proxy keeps the opening of such a description and
//! points at the whole tool entry (description, schema, everything) in the
//! content-addressed tee store, through the Engine's expand tool as the host
//! itself listed it.
//!
//! Byte stability (#448, #498): only the `description` strings change. The
//! tools keep their order and every other key — `cache_control` markers
//! included — and the shortened form is a pure function of the tool entry and
//! the listed expand tool's name, so every turn of a conversation carries the
//! same bytes and the provider's cached prefix holds. The decision reads the
//! tool list only, never per-turn settings such as `tool_choice`, so it cannot
//! flip between turns of one conversation.
//!
//! Nothing is shortened when the way back cannot be followed or kept: no
//! expand tool in the host's list, protection active (the proxy keeps no
//! originless copy then), or the copy cannot be written.

use serde_json::Value;

/// Descriptions up to this many bytes stay as they are.
pub(crate) const MAX_DESCRIPTION_BYTES: usize = 1024;
/// The kept opening of a shortened description, at most this many bytes.
const SUMMARY_BYTES: usize = 320;

/// Shorten the large descriptions of `doc`'s `tools` (Messages, Chat
/// Completions or Responses shape). Returns whether the body changed.
pub(crate) fn shorten(doc: &mut Value) -> bool {
    if super::compress::protected() {
        return false;
    }
    let Some(tools) = doc.get_mut("tools").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(expand) = tools.iter().find_map(|tool| {
        tool_name(tool)
            .filter(|name| super::compress::expand_tool_name(name))
            .map(str::to_owned)
    }) else {
        return false;
    };
    let mut changed = false;
    for tool in tools.iter_mut() {
        if tool_name(tool).is_none_or(|name| name == expand) {
            continue;
        }
        let Some(short) = shortened_description(tool, &expand) else {
            continue;
        };
        if let Some(slot) = description_slot(tool) {
            *slot = Value::String(short);
            changed = true;
        }
    }
    changed
}

/// The name of a tool entry: `function.name` in Chat Completions, `name`
/// otherwise.
fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
}

fn description_slot(tool: &mut Value) -> Option<&mut Value> {
    if tool.get("function").is_some_and(Value::is_object) {
        return tool.get_mut("function")?.get_mut("description");
    }
    tool.get_mut("description")
}

/// The shortened description of `tool`, or `None` when it stays whole.
fn shortened_description(tool: &Value, expand: &str) -> Option<String> {
    let description = tool
        .get("function")
        .and_then(|function| function.get("description"))
        .or_else(|| tool.get("description"))
        .and_then(Value::as_str)?;
    if description.len() <= MAX_DESCRIPTION_BYTES {
        return None;
    }
    let definition = serde_json::to_string(tool).ok()?;
    let handle = super::ccr::persist_tool_definition(&definition)?;
    let short = format!("{}\n{}", summary(description), pointer(expand, &handle));
    (short.len() < description.len()).then_some(short)
}

/// The line that names the full definition. A pure function of its inputs.
pub(crate) fn pointer(expand: &str, handle: &str) -> String {
    format!("[lean-ctx: description shortened — full definition: {expand}(id=\"{handle}\")]")
}

/// The opening of `description`: its first paragraph, cut at the last
/// sentence end (or else word break) within [`SUMMARY_BYTES`].
fn summary(description: &str) -> String {
    let paragraph = description
        .trim_start()
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .trim_end();
    if paragraph.len() <= SUMMARY_BYTES {
        return paragraph.to_owned();
    }
    let mut end = SUMMARY_BYTES;
    while !paragraph.is_char_boundary(end) {
        end -= 1;
    }
    let window = &paragraph[..end];
    let cut = window
        .rfind(". ")
        .map(|at| at + 1)
        .filter(|&at| at >= SUMMARY_BYTES / 3)
        .or_else(|| window.rfind(char::is_whitespace))
        .unwrap_or(end);
    format!("{} …", window[..cut].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn long_description() -> String {
        let sentence =
            "Reads a file from the project and returns its content in the requested mode. ";
        format!(
            "{}\n\nDetails:\n{}",
            sentence.repeat(8),
            "- mode=full returns everything\n".repeat(60)
        )
    }

    fn anthropic_doc(description: &str) -> Value {
        json!({
            "model": "claude",
            "tools": [
                {"name": "read_file", "description": description,
                 "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}}},
                {"name": "small", "description": "Small tool", "input_schema": {}},
                {"name": "mcp__lean-ctx__ctx_expand", "description": description,
                 "input_schema": {}, "cache_control": {"type": "ephemeral"}}
            ],
            "messages": []
        })
    }

    fn names(doc: &Value) -> Vec<String> {
        doc["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool_name(tool).unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_large_description_is_shortened_with_a_pointer_to_the_full_definition() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let description = long_description();
        let original = anthropic_doc(&description);
        let mut doc = original.clone();
        assert!(shorten(&mut doc));

        let short = doc["tools"][0]["description"].as_str().unwrap();
        assert!(short.len() < description.len() / 4, "{short}");
        assert!(
            short.starts_with("Reads a file from the project"),
            "{short}"
        );
        let last = short.lines().last().unwrap();
        let handle = last
            .strip_prefix(
                "[lean-ctx: description shortened — full definition: mcp__lean-ctx__ctx_expand(id=\"",
            )
            .and_then(|rest| rest.strip_suffix("\")]"))
            .unwrap_or_else(|| panic!("pointer grammar: {last}"));
        let path = crate::proxy::ccr::resolve_tee(handle).expect("handle resolves");
        let stored: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(stored, original["tools"][0], "the whole tool entry");

        // Order, the other tools, schemas and cache markers stay as they were.
        assert_eq!(names(&doc), names(&original));
        assert_eq!(doc["tools"][1], original["tools"][1]);
        assert_eq!(
            doc["tools"][2], original["tools"][2],
            "the expand tool stays whole"
        );
        assert_eq!(
            doc["tools"][0]["input_schema"],
            original["tools"][0]["input_schema"]
        );
    }

    /// #498: the same tool list yields the same bytes on every turn.
    #[test]
    fn shortening_is_byte_stable_across_turns() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut first = anthropic_doc(&long_description());
        let mut second = first.clone();
        shorten(&mut first);
        shorten(&mut second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        assert!(!shorten(&mut first), "a shortened list is left alone");
    }

    #[test]
    fn without_a_listed_expand_tool_nothing_changes() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut doc = anthropic_doc(&long_description());
        doc["tools"].as_array_mut().unwrap().pop();
        let before = doc.clone();
        assert!(!shorten(&mut doc));
        assert_eq!(doc, before);
    }

    #[test]
    fn chat_and_responses_shapes_are_shortened_in_place() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let description = long_description();
        let mut chat = json!({"tools": [
            {"type": "function", "function": {"name": "read_file", "description": description, "parameters": {}}},
            {"type": "function", "function": {"name": "ctx_expand", "description": "Expand", "parameters": {}}}
        ]});
        assert!(shorten(&mut chat));
        let short = chat["tools"][0]["function"]["description"]
            .as_str()
            .unwrap();
        assert!(
            short.ends_with("\")]") && short.contains("ctx_expand(id=\""),
            "{short}"
        );
        assert!(chat["tools"][0].get("description").is_none());

        let mut responses = json!({"tools": [
            {"type": "function", "name": "read_file", "description": description, "parameters": {}},
            {"type": "function", "name": "ctx_expand", "description": "Expand", "parameters": {}}
        ]});
        assert!(shorten(&mut responses));
        assert!(
            responses["tools"][0]["description"]
                .as_str()
                .unwrap()
                .contains("full definition: ctx_expand(id=\"")
        );
    }

    #[test]
    fn small_descriptions_stay_whole() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut doc = anthropic_doc(&"x".repeat(MAX_DESCRIPTION_BYTES));
        let before = doc.clone();
        assert!(!shorten(&mut doc));
        assert_eq!(doc, before);
    }
}
