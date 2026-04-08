use std::io::Read;

pub fn handle_rewrite() {
    let binary = resolve_binary();
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }

    let tool = extract_json_field(&input, "tool_name");
    if !matches!(tool.as_deref(), Some("Bash" | "bash")) {
        return;
    }

    let cmd = match extract_json_field(&input, "command") {
        Some(c) => c,
        None => return,
    };

    if cmd.starts_with("lean-ctx ") || cmd.starts_with(&format!("{binary} ")) {
        return;
    }

    let should_rewrite = REWRITABLE_PREFIXES
        .iter()
        .any(|prefix| cmd.starts_with(prefix) || cmd == prefix.trim_end_matches(' '));

    if should_rewrite {
        let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
        let rewrite = format!("{binary} -c \\\"{escaped}\\\"");
        print!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{{\"command\":\"{rewrite}\"}}}}}}"
        );
    }
}

pub fn handle_redirect() {
    // Allow all native tools (Read, Grep, ListFiles) to pass through.
    // Blocking them breaks Edit (which requires native Read) and causes
    // unnecessary friction. The MCP instructions already guide the AI
    // to prefer ctx_read/ctx_search/ctx_tree.
}

const REWRITABLE_PREFIXES: &[&str] = &[
    "git ", "gh ", "cargo ", "npm ", "pnpm ", "yarn ", "docker ", "kubectl ", "pip ", "pip3 ",
    "ruff ", "go ", "curl ", "grep ", "rg ", "find ", "cat ", "head ", "tail ", "ls ", "ls",
    "aws ", "helm ", "eslint", "prettier", "tsc", "pytest", "mypy",
];

fn resolve_binary() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string())
}

fn extract_json_field(input: &str, field: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", field);
    let start = input.find(&pattern)? + pattern.len();
    let rest = &input[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_quotes_pipe_commands() {
        let cmd = "git log --oneline | grep fix";
        let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
        let rewrite = format!("lean-ctx -c \\\"{escaped}\\\"");
        assert!(
            rewrite.contains("\\\"git log"),
            "pipe command must be quoted: {rewrite}"
        );
    }

    #[test]
    fn rewrite_simple_command() {
        let cmd = "git status";
        let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
        let rewrite = format!("lean-ctx -c \\\"{escaped}\\\"");
        assert_eq!(rewrite, "lean-ctx -c \\\"git status\\\"");
    }

    #[test]
    fn extract_field_works() {
        let input = r#"{"tool_name":"Bash","command":"git status"}"#;
        assert_eq!(
            extract_json_field(input, "tool_name"),
            Some("Bash".to_string())
        );
        assert_eq!(
            extract_json_field(input, "command"),
            Some("git status".to_string())
        );
    }
}
