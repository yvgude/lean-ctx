/// Proxy compression: delegates to the unified `compress_if_beneficial` pipeline.
///
/// For shell-like tool results, we attempt to extract a command hint from `$ ...` prefixes
/// so the pattern engine gets the same routing as CLI and MCP paths.
pub fn compress_tool_result(content: &str, tool_name: Option<&str>) -> String {
    if content.trim().is_empty() || content.len() < 200 {
        return content.to_string();
    }

    let cmd = infer_command(content, tool_name);
    crate::shell::compress::engine::compress_if_beneficial(&cmd, content)
}

fn infer_command(content: &str, tool_name: Option<&str>) -> String {
    if let Some(cmd) = extract_command_hint(content) {
        return cmd;
    }

    if let Some(name) = tool_name {
        let nl = name.to_lowercase();
        if nl.contains("bash") || nl.contains("shell") || nl.contains("terminal") {
            return "shell".to_string();
        }
        if nl.contains("search") || nl.contains("grep") || nl.contains("find") {
            return "grep".to_string();
        }
    }

    String::new()
}

fn extract_command_hint(content: &str) -> Option<String> {
    for line in content.lines().take(3) {
        let trimmed = line.trim();
        if let Some(cmd) = trimmed.strip_prefix("$ ") {
            return Some(cmd.to_string());
        }
        if let Some(cmd) = trimmed.strip_prefix("% ") {
            return Some(cmd.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_unchanged() {
        let short = "hello world";
        assert_eq!(compress_tool_result(short, None), short);
    }

    #[test]
    fn empty_content_unchanged() {
        assert_eq!(compress_tool_result("", None), "");
        assert_eq!(compress_tool_result("   ", None), "   ");
    }

    #[test]
    fn command_hint_extraction() {
        assert_eq!(
            extract_command_hint("$ cargo build\nCompiling foo"),
            Some("cargo build".to_string())
        );
        assert_eq!(extract_command_hint("no prefix here"), None);
    }

    #[test]
    fn tool_name_inference() {
        assert_eq!(infer_command("some text", Some("bash_execute")), "shell");
        assert_eq!(infer_command("some text", Some("search_files")), "grep");
        assert_eq!(infer_command("some text", Some("unknown_tool")), "");
    }
}
