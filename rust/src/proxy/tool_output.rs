use serde_json::Value;

#[cfg(test)]
use crate::core::tokens::TokenizerFamily;

use super::compress::compress_tool_result;
#[cfg(test)]
use super::compress::compress_tool_result_for;
use super::tool_kind::{ToolResultKind, should_protect};

enum JsonRewrite {
    NotJson,
    Unchanged,
    Changed(String),
}

pub(super) fn compress_text(
    text: &mut String,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    match rewrite_json_payload_text(text, kind, |inner| {
        if should_protect(kind, inner) {
            return None;
        }
        let compressed = compress_tool_result(inner, tool_name);
        (compressed.len() < inner.len()).then_some(compressed)
    }) {
        JsonRewrite::Changed(compressed) => {
            *text = compressed;
            return true;
        }
        JsonRewrite::Unchanged => return false,
        JsonRewrite::NotJson => {}
    }

    if should_protect(kind, text) {
        return false;
    }
    let compressed = compress_tool_result(text, tool_name);
    if compressed.len() < text.len() {
        *text = compressed;
        return true;
    }
    false
}

#[cfg(test)]
pub(super) fn compress_text_for(
    text: &mut String,
    tool_name: Option<&str>,
    kind: ToolResultKind,
    family: TokenizerFamily,
) -> bool {
    match rewrite_json_payload_text(text, kind, |inner| {
        if should_protect(kind, inner) {
            return None;
        }
        let compressed = compress_tool_result_for(inner, tool_name, family);
        (compressed.len() < inner.len()).then_some(compressed)
    }) {
        JsonRewrite::Changed(compressed) => {
            *text = compressed;
            return true;
        }
        JsonRewrite::Unchanged => return false,
        JsonRewrite::NotJson => {}
    }

    if should_protect(kind, text) {
        return false;
    }
    let compressed = compress_tool_result_for(text, tool_name, family);
    if compressed.len() < text.len() {
        *text = compressed;
        return true;
    }
    false
}

pub(super) fn compress_value(
    value: &mut Value,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    match value {
        Value::String(text) => compress_text(text, tool_name, kind),
        Value::Array(parts) => {
            let mut changed = false;
            for part in parts.iter_mut() {
                if let Some(Value::String(text)) = part.get_mut("text") {
                    changed |= compress_text(text, tool_name, kind);
                }
            }
            changed
        }
        _ => false,
    }
}

pub(super) fn prune_text(text: &mut String, kind: ToolResultKind) -> bool {
    match rewrite_json_payload_text(text, kind, |inner| {
        super::history_prune::prune_output_text(inner, kind)
    }) {
        JsonRewrite::Changed(pruned) => {
            *text = pruned;
            return true;
        }
        JsonRewrite::Unchanged => return false,
        JsonRewrite::NotJson => {}
    }

    if let Some(pruned) = super::history_prune::prune_output_text(text, kind) {
        *text = pruned;
        return true;
    }
    false
}

pub(super) fn prune_value(value: &mut Value, kind: ToolResultKind) -> bool {
    match value {
        Value::String(text) => prune_text(text, kind),
        Value::Array(parts) => {
            let mut changed = false;
            for part in parts.iter_mut() {
                if let Some(Value::String(text)) = part.get_mut("text") {
                    changed |= prune_text(text, kind);
                }
            }
            changed
        }
        _ => false,
    }
}

fn rewrite_json_payload_text(
    text: &str,
    kind: ToolResultKind,
    mut rewrite: impl FnMut(&str) -> Option<String>,
) -> JsonRewrite {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return JsonRewrite::NotJson;
    }
    let Ok(mut value) = serde_json::from_str::<Value>(trimmed) else {
        return JsonRewrite::NotJson;
    };
    let mut touched = false;
    // The strings inside the payload that were replaced, as they were.
    let mut replaced = Vec::new();
    rewrite_json_text_values(&mut value, kind, &mut rewrite, &mut touched, &mut replaced);
    if !touched || replaced.is_empty() {
        return JsonRewrite::Unchanged;
    }
    match serde_json::to_string(&value) {
        // The re-serialized text is a new text: the lineage of `text` does
        // not carry over to it by itself. Under protection it leaves only
        // when it is on record as derived (the shared gate decides, from the
        // lineages of `text` and of every replaced string); otherwise `text`
        // stays whole.
        Ok(serialized) if serialized.len() < text.len() => {
            super::compress::gate::rewrapped(text, serialized, &replaced)
                .map_or(JsonRewrite::Unchanged, JsonRewrite::Changed)
        }
        _ => JsonRewrite::Unchanged,
    }
}

fn rewrite_json_text_values(
    value: &mut Value,
    kind: ToolResultKind,
    rewrite: &mut impl FnMut(&str) -> Option<String>,
    touched: &mut bool,
    replaced: &mut Vec<String>,
) {
    match value {
        Value::Object(map) => {
            let is_text_part = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| matches!(t, "text" | "input_text" | "output_text"));
            let rewrite_all_strings =
                matches!(kind, ToolResultKind::Shell | ToolResultKind::Search);
            for (key, child) in map.iter_mut() {
                if let Value::String(s) = child
                    && (rewrite_all_strings || (is_text_part && key == "text"))
                {
                    *touched = true;
                    if let Some(next) = rewrite(s) {
                        replaced.push(std::mem::replace(s, next));
                    }
                    continue;
                }
                rewrite_json_text_values(child, kind, rewrite, touched, replaced);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_json_text_values(item, kind, rewrite, touched, replaced);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tokens::count_tokens_for;

    #[test]
    fn compress_text_for_uses_selected_tokenizer_family() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let paragraph = "Grüezi 世界 means hello in Swiss German, and this repeated sentence carries multilingual text for tokenizer accounting. 🤝";
        let input = format!("{}\n", [paragraph; 20].join("\n\n"));
        let mut cl100k = input.clone();
        let mut o200k = input;

        assert!(compress_text_for(
            &mut cl100k,
            Some("shell"),
            ToolResultKind::Shell,
            TokenizerFamily::Cl100k,
        ));
        assert!(compress_text_for(
            &mut o200k,
            Some("shell"),
            ToolResultKind::Shell,
            TokenizerFamily::O200kBase,
        ));
        assert_ne!(
            count_tokens_for(&cl100k, TokenizerFamily::Cl100k),
            count_tokens_for(&o200k, TokenizerFamily::O200kBase),
            "the selected tokenizer family must produce model-specific accounting"
        );
    }
}
