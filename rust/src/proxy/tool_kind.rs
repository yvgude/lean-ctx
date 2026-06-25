//! Classifies what produced a `tool_result` so the proxy never lossy-compresses
//! a file/source-code read the model still needs (e.g. mid-refactor).
//!
//! The request body only carries the tool *result* plus an id linking it to the
//! originating tool *call*. We resolve that id → tool name from the assistant's
//! `tool_use` / `tool_calls` / `function_call` items, then map the name to a
//! [`ToolResultKind`]. A content heuristic ([`looks_like_source_code`]) is the
//! fallback for unknown/custom tools so a file read through a non-standard tool
//! is still protected.

use std::collections::HashMap;

use serde_json::Value;

/// What kind of tool produced a `tool_result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultKind {
    /// A file/source read — must reach the model intact (it is what gets edited).
    FileRead,
    /// Shell/command output — safe to run through the pattern compressors.
    Shell,
    /// Search/listing output — safe to compress.
    Search,
    /// Unknown — fall back to the content heuristic before compressing.
    Other,
}

/// Maps a tool name (from any agent) to a [`ToolResultKind`].
///
/// Matching is case-insensitive and substring-based so vendor prefixes
/// (`mcp__fs__read_file`, `functions.read`) and casing variants are covered.
#[must_use]
pub fn classify_tool_name(name: &str) -> ToolResultKind {
    let n = name.to_ascii_lowercase();

    // Order matters: a "read_file" must not be caught by a generic "file".
    const FILE_READ: &[&str] = &[
        "read_file",
        "readfile",
        "file_read",
        "fsread",
        "fs_read",
        "view_file",
        "viewfile",
        "open_file",
        "notebookread",
        "notebook_read",
        "cat_file",
        "get_file",
        "fetch_file",
        "ctx_read",
        "ctx_multi_read",
        "multi_read",
        "multiread",
        "read_many", // Gemini CLI `read_many_files`
        "read_files",
        "str_replace_editor", // view sub-mode returns file content
    ];
    if FILE_READ.iter().any(|k| n.contains(k)) {
        return ToolResultKind::FileRead;
    }
    // Bare "read"/"view"/"cat" as a whole token (Claude Code `Read`, Pi `read`).
    if matches!(n.as_str(), "read" | "view" | "cat" | "open") {
        return ToolResultKind::FileRead;
    }

    const SEARCH: &[&str] = &[
        "grep",
        "ripgrep",
        "search",
        "find",
        "glob",
        "list_dir",
        "listdir",
        "list_files",
        "listfiles",
        "ls",
        "codebase_search",
        "ctx_search",
        "ctx_tree",
    ];
    if SEARCH.iter().any(|k| n.contains(k)) {
        return ToolResultKind::Search;
    }

    const SHELL: &[&str] = &[
        "bash",
        "shell",
        "terminal",
        "run_command",
        "run_terminal",
        "runterminal",
        "execute_command",
        "exec_command",
        "command_exec",
        "ctx_shell",
    ];
    if SHELL.iter().any(|k| n.contains(k)) {
        return ToolResultKind::Shell;
    }
    if matches!(n.as_str(), "run" | "exec" | "execute" | "command" | "sh") {
        return ToolResultKind::Shell;
    }

    // Vendor-prefix fallback. Foreign harnesses namespace their tools
    // (`forge_read`, `pi.shell`, `fs:grep`), which the substring lists above
    // miss. Matching the name's path-like *segments* as whole words catches
    // those without the false positives a bare substring would cause
    // (`thread`, `research`, `already`). FileRead is checked first so a read is
    // never misclassified as compressible.
    for seg in n.split(|c: char| !c.is_ascii_alphanumeric()) {
        match seg {
            "read" | "view" | "cat" | "open" => return ToolResultKind::FileRead,
            "grep" | "search" | "find" | "glob" | "ls" | "rg" => return ToolResultKind::Search,
            "shell" | "bash" | "exec" | "run" | "terminal" | "cmd" => return ToolResultKind::Shell,
            _ => {}
        }
    }

    ToolResultKind::Other
}

/// Builds a `tool_use_id → tool_name` map from Anthropic `messages`.
///
/// Scans every assistant content block of `type:"tool_use"`.
#[must_use]
pub fn anthropic_tool_names(messages: &[Value]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            if let (Some(id), Some(name)) = (
                block.get("id").and_then(|v| v.as_str()),
                block.get("name").and_then(|v| v.as_str()),
            ) {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

/// Builds a `tool_call_id → function_name` map from `OpenAI` Chat Completions
/// `messages` (assistant `tool_calls[]`).
#[must_use]
pub fn openai_tool_names(messages: &[Value]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        let Some(calls) = msg.get("tool_calls").and_then(|c| c.as_array()) else {
            continue;
        };
        for call in calls {
            let id = call.get("id").and_then(|v| v.as_str());
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str());
            if let (Some(id), Some(name)) = (id, name) {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

/// Builds a `call_id → name` map from `OpenAI` Responses `input` items
/// (`type:"function_call"`).
#[must_use]
pub fn responses_tool_names(input: &[Value]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for item in input {
        if item.get("type").and_then(|t| t.as_str()) != Some("function_call") {
            continue;
        }
        if let (Some(id), Some(name)) = (
            item.get("call_id").and_then(|v| v.as_str()),
            item.get("name").and_then(|v| v.as_str()),
        ) {
            map.insert(id.to_string(), name.to_string());
        }
    }
    map
}

/// Whether a `tool_result` with the given resolved kind and content must be
/// preserved intact (never lossy-compressed) by the proxy.
///
/// File reads are always protected; unknown tools are protected only when the
/// content heuristically looks like source code. Shell/search output is never
/// protected here — it flows through the normal pattern compressors.
#[must_use]
pub fn should_protect(kind: ToolResultKind, content: &str) -> bool {
    match kind {
        ToolResultKind::FileRead => true,
        ToolResultKind::Other => looks_like_source_code(content),
        ToolResultKind::Shell | ToolResultKind::Search => false,
    }
}

/// Heuristic fallback: does this text look like source code (vs command output)?
///
/// Deliberately conservative — it only returns `true` when code signals clearly
/// dominate and shell/log signals are essentially absent, so genuine logs and
/// build output are still compressed. Used only when the tool name is unknown.
#[must_use]
pub fn looks_like_source_code(content: &str) -> bool {
    let mut code_signals = 0usize;
    let mut shell_signals = 0usize;
    let mut considered = 0usize;

    for raw in content.lines().take(200) {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        considered += 1;

        // Command/log markers — strong evidence this is NOT a file read.
        if trimmed.starts_with("$ ")
            || trimmed.starts_with("% ")
            || trimmed.starts_with(">>> ")
            || trimmed.starts_with("warning:")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("INFO ")
            || trimmed.starts_with("WARN ")
            || trimmed.starts_with("DEBUG ")
            || trimmed.starts_with("ERROR ")
            || trimmed.starts_with("Compiling ")
            || trimmed.starts_with("Downloaded ")
            || trimmed.starts_with("test result:")
        {
            shell_signals += 1;
            continue;
        }

        // Code markers.
        let is_indented = line.len() != trimmed.len();
        let has_code_punct = trimmed.ends_with('{')
            || trimmed.ends_with('}')
            || trimmed.ends_with(';')
            || trimmed.ends_with("=>")
            || trimmed.ends_with("->")
            || trimmed.ends_with(':');
        let has_keyword = [
            "fn ",
            "def ",
            "class ",
            "import ",
            "from ",
            "function ",
            "func ",
            "pub ",
            "const ",
            "let ",
            "var ",
            "package ",
            "public ",
            "private ",
            "struct ",
            "enum ",
            "impl ",
            "#include",
            "return ",
            "async ",
            "export ",
        ]
        .iter()
        .any(|k| trimmed.starts_with(k) || trimmed.contains(k));

        if (is_indented && has_code_punct) || has_keyword {
            code_signals += 1;
        }
    }

    if considered < 5 || shell_signals > 0 {
        return false;
    }
    // Require a clear majority of code-shaped lines.
    code_signals * 2 >= considered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_file_read_tools() {
        for name in [
            "Read",
            "read_file",
            "view_file",
            "ctx_read",
            "mcp__fs__readFile",
            // Multi-file reads return file content and must be protected too.
            "ctx_multi_read",
            "read_many_files",
        ] {
            assert_eq!(
                classify_tool_name(name),
                ToolResultKind::FileRead,
                "{name} should be FileRead"
            );
        }
    }

    #[test]
    fn classifies_shell_and_search() {
        assert_eq!(classify_tool_name("Bash"), ToolResultKind::Shell);
        assert_eq!(
            classify_tool_name("run_terminal_cmd"),
            ToolResultKind::Shell
        );
        assert_eq!(classify_tool_name("Grep"), ToolResultKind::Search);
        assert_eq!(
            classify_tool_name("codebase_search"),
            ToolResultKind::Search
        );
    }

    #[test]
    fn unknown_tool_is_other() {
        assert_eq!(classify_tool_name("submit_pr"), ToolResultKind::Other);
    }

    #[test]
    fn classifies_vendor_prefixed_foreign_tools() {
        // Foreign harnesses (forge / pi) namespace their tools; the segment
        // fallback must still route them so source reads stay protected and
        // shell/search output stays compressible.
        assert_eq!(classify_tool_name("forge_read"), ToolResultKind::FileRead);
        assert_eq!(classify_tool_name("pi.read"), ToolResultKind::FileRead);
        assert_eq!(classify_tool_name("forge_shell"), ToolResultKind::Shell);
        assert_eq!(classify_tool_name("forge_exec"), ToolResultKind::Shell);
        assert_eq!(classify_tool_name("fs:grep"), ToolResultKind::Search);
    }

    #[test]
    fn segment_fallback_has_no_substring_false_positives() {
        // Whole-word segments only: "thread" contains "read", "spread" contains
        // "read" — neither may be misclassified as a file read.
        assert_eq!(classify_tool_name("thread_create"), ToolResultKind::Other);
        assert_eq!(classify_tool_name("spread_values"), ToolResultKind::Other);
        assert_eq!(
            classify_tool_name("readme_generator"),
            ToolResultKind::Other
        );
        assert_eq!(classify_tool_name("submit_pull"), ToolResultKind::Other);
    }

    #[test]
    fn anthropic_names_resolve_from_tool_use() {
        let messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "reading"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "x"}]
            }),
        ];
        let names = anthropic_tool_names(&messages);
        assert_eq!(names.get("toolu_1").map(String::as_str), Some("Read"));
    }

    #[test]
    fn openai_names_resolve_from_tool_calls() {
        let messages = vec![serde_json::json!({
            "role": "assistant",
            "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read_file"}}]
        })];
        let names = openai_tool_names(&messages);
        assert_eq!(names.get("call_1").map(String::as_str), Some("read_file"));
    }

    #[test]
    fn responses_names_resolve_from_function_call() {
        let input = vec![serde_json::json!({
            "type": "function_call", "call_id": "call_1", "name": "Read", "arguments": "{}"
        })];
        let names = responses_tool_names(&input);
        assert_eq!(names.get("call_1").map(String::as_str), Some("Read"));
    }

    #[test]
    fn source_code_detected() {
        let code = "pub fn build(cfg: &Config) -> Result<App> {\n    let mut app = App::new();\n    app.configure(cfg);\n    for route in cfg.routes() {\n        app.register(route);\n    }\n    Ok(app)\n}";
        assert!(looks_like_source_code(code));
    }

    #[test]
    fn command_output_not_code() {
        let log = "$ cargo build\n   Compiling foo v0.1.0\n   Compiling bar v0.2.0\nwarning: unused variable\n    Finished dev target\nerror: could not compile";
        assert!(!looks_like_source_code(log));
    }

    #[test]
    fn plain_prose_not_code() {
        let prose = "This is a normal paragraph of text.\nIt has several sentences.\nNone of them are code.\nThey are just words on lines.\nMore words follow here.";
        assert!(!looks_like_source_code(prose));
    }
}
