// SPDX-License-Identifier: Apache-2.0
//! The project a proxied model request belongs to.
//!
//! Agents name their working folder in the request itself: Claude Code in its
//! system prompt (`Primary working directory: …`), Codex in its environment
//! context (`<cwd>…</cwd>`). The proxy reads that folder from the request it
//! forwards anyway and keeps only its project key
//! ([`crate::core::savings_ledger::project_scope::project_key_of`]), never the
//! path. A request that names no folder belongs to no project.

use std::path::Path;
use std::sync::OnceLock;

use serde_json::Value;

/// Strings looked at per request, and the bytes of them scanned: the folder
/// sits in the system prompt or the first messages, never deep in a turn.
const MAX_STRINGS: usize = 48;
const MAX_SCAN_BYTES: usize = 512 * 1024;
const MAX_DEPTH: usize = 5;
/// Leading messages or input items searched after the system prompt.
const LEADING_ITEMS: usize = 4;

fn folder_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(
            r"(?m)(?:^[ \t]*(?:-[ \t]*)?(?:Primary working directory|Working directory):[ \t]*([^\r\n]+?)[ \t]*$|<cwd>([^<\r\n]+)</cwd>)",
        )
        .expect("static regex")
    })
}

/// The working folder a request body names, if any.
pub(crate) fn working_folder(body: &Value) -> Option<String> {
    let mut strings: Vec<&str> = Vec::new();
    for key in ["system", "instructions"] {
        if let Some(value) = body.get(key) {
            collect(value, 0, &mut strings);
        }
    }
    for key in ["messages", "input"] {
        match body.get(key) {
            Some(Value::Array(items)) => {
                for item in items.iter().take(LEADING_ITEMS) {
                    collect(item, 0, &mut strings);
                }
            }
            Some(other) => collect(other, 0, &mut strings),
            None => {}
        }
    }
    let mut scanned = 0usize;
    for text in strings {
        if scanned >= MAX_SCAN_BYTES {
            break;
        }
        let text = &text[..floor_char_boundary(text, MAX_SCAN_BYTES - scanned)];
        scanned += text.len();
        for caps in folder_pattern().captures_iter(text) {
            let Some(folder) = caps.get(1).or_else(|| caps.get(2)) else {
                continue;
            };
            let folder = folder.as_str().trim();
            if is_absolute(folder) {
                return Some(folder.to_string());
            }
        }
    }
    None
}

/// The project key of the folder a request body names.
pub(crate) fn project_key(body: &Value) -> Option<String> {
    let folder = working_folder(body)?;
    crate::core::savings_ledger::project_scope::project_key_of(Path::new(&folder))
}

fn collect<'a>(value: &'a Value, depth: usize, out: &mut Vec<&'a str>) {
    if depth > MAX_DEPTH || out.len() >= MAX_STRINGS {
        return;
    }
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => {
            for item in items {
                collect(item, depth + 1, out);
            }
        }
        Value::Object(map) => {
            for key in ["text", "content", "input_text"] {
                if let Some(inner) = map.get(key) {
                    collect(inner, depth + 1, out);
                }
            }
        }
        _ => {}
    }
}

fn is_absolute(folder: &str) -> bool {
    let bytes = folder.as_bytes();
    folder.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

fn floor_char_boundary(text: &str, max: usize) -> usize {
    if max >= text.len() {
        return text.len();
    }
    (0..=max)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_code_names_its_folder_in_the_system_prompt() {
        let body = json!({
            "model": "claude",
            "system": [
                {"type": "text", "text": "You are Claude Code."},
                {"type": "text", "text": "# Environment\n - Primary working directory: /work/app\n - Is a git repository: true"}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(working_folder(&body).as_deref(), Some("/work/app"));

        let older =
            json!({"system": "<env>\nWorking directory: /work/old\nPlatform: darwin\n</env>"});
        assert_eq!(working_folder(&older).as_deref(), Some("/work/old"));
    }

    #[test]
    fn codex_names_its_folder_in_the_environment_context() {
        let body = json!({
            "model": "gpt",
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "rules"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text",
                    "text": "<environment_context>\n  <cwd>/work/codex</cwd>\n  <shell>zsh</shell>\n</environment_context>"}]}
            ]
        });
        assert_eq!(working_folder(&body).as_deref(), Some("/work/codex"));
    }

    #[test]
    fn a_request_without_an_absolute_folder_names_no_project() {
        assert_eq!(
            working_folder(
                &json!({"messages": [{"role": "user", "content": "Working directory: relative/path"}]})
            ),
            None
        );
        assert_eq!(
            working_folder(&json!({"messages": [{"role": "user", "content": "plain question"}]})),
            None
        );
        assert_eq!(project_key(&json!({"model": "m"})), None);
        // A folder mentioned deep in a long conversation is not the agent's own.
        let mut messages = vec![json!({"role": "user", "content": "start"}); LEADING_ITEMS];
        messages.push(json!({"role": "user", "content": "<cwd>/work/late</cwd>"}));
        assert_eq!(working_folder(&json!({"messages": messages})), None);
    }
}
