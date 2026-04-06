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
        let rewrite = format!("{binary} -c {cmd}");
        print!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{{\"command\":\"{rewrite}\"}}}}}}"
        );
    }
}

pub fn handle_redirect() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }

    let tool = match extract_json_field(&input, "tool_name") {
        Some(t) => t,
        None => return,
    };

    let reason = match tool.as_str() {
        "Read" | "read" | "ReadFile" | "read_file" | "View" | "view" => {
            "STOP. Use ctx_read(path) from the lean-ctx MCP server instead. \
             It saves 60-80% input tokens via caching and compression. \
             Available modes: full, map, signatures, diff, lines:N-M. \
             Never use native Read — always use ctx_read."
        }
        "Grep" | "grep" | "Search" | "search" | "RipGrep" | "ripgrep" => {
            "STOP. Use ctx_search(pattern, path) from the lean-ctx MCP server instead. \
             It provides compact, token-efficient results with .gitignore awareness. \
             Never use native Grep — always use ctx_search."
        }
        "ListFiles" | "list_files" | "ListDirectory" | "list_directory" => {
            "STOP. Use ctx_tree(path, depth) from the lean-ctx MCP server instead. \
             It provides compact directory maps with file counts. \
             Never use native ListFiles — always use ctx_tree."
        }
        _ => return,
    };

    if let Some(path) = extract_tool_path(&input) {
        if is_path_excluded(&path) {
            return;
        }
    }

    if !is_lean_ctx_running() {
        return;
    }

    print!(
        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"{reason}\"}}}}"
    );
}

fn extract_tool_path(input: &str) -> Option<String> {
    extract_json_field(input, "file_path")
        .or_else(|| extract_json_field(input, "path"))
        .or_else(|| extract_json_field(input, "directory"))
}

fn is_path_excluded(path: &str) -> bool {
    let patterns = load_exclude_patterns();
    if patterns.is_empty() {
        return false;
    }
    patterns.iter().any(|pattern| glob_match(pattern, path))
}

fn load_exclude_patterns() -> Vec<String> {
    if let Ok(env_val) = std::env::var("LEAN_CTX_HOOK_EXCLUDE") {
        let patterns: Vec<String> = env_val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !patterns.is_empty() {
            return patterns;
        }
    }

    let cfg = crate::core::config::Config::load();
    cfg.redirect_exclude
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let norm_path = path.replace('\\', "/");
    let norm_pattern = pattern.replace('\\', "/");

    if norm_pattern.contains('/') {
        glob_match_segment(&norm_pattern, &norm_path)
    } else {
        norm_path
            .split('/')
            .any(|segment| glob_match_segment(&norm_pattern, segment))
    }
}

fn glob_match_segment(pattern: &str, text: &str) -> bool {
    if pattern == "**" {
        return true;
    }

    if let Some(rest) = pattern.strip_suffix("/**") {
        let prefix = rest;
        return text == prefix
            || text.starts_with(&format!("{prefix}/"))
            || text.contains(&format!("/{prefix}/"))
            || text.ends_with(&format!("/{prefix}"));
    }

    let pat_chars: Vec<char> = pattern.chars().collect();
    let txt_chars: Vec<char> = text.chars().collect();
    glob_match_chars(&pat_chars, &txt_chars)
}

fn glob_match_chars(pattern: &[char], text: &[char]) -> bool {
    let (mut pi, mut ti) = (0, 0);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0);

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_pi = pi;
            star_ti = ti;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
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

fn is_lean_ctx_running() -> bool {
    if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq lean-ctx.exe", "/NH"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("lean-ctx"))
            .unwrap_or(false)
    } else {
        std::process::Command::new("pgrep")
            .args(["-f", "lean-ctx"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
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
    fn glob_match_simple_extension() {
        assert!(glob_match("*.json", "settings.json"));
        assert!(glob_match("*.json", "src/config/settings.json"));
        assert!(!glob_match("*.json", "settings.toml"));
    }

    #[test]
    fn glob_match_directory_pattern() {
        assert!(glob_match(".wolf/**", ".wolf/config.yaml"));
        assert!(glob_match(".wolf/**", ".wolf/sub/deep/file.txt"));
        assert!(glob_match(".claude/**", ".claude/settings.json"));
        assert!(!glob_match(".wolf/**", "src/wolf/file.txt"));
    }

    #[test]
    fn glob_match_hidden_dirs() {
        assert!(glob_match(".cursor/**", ".cursor/rules/lean-ctx.mdc"));
        assert!(glob_match(".claude/**", "project/.claude/CLAUDE.md"));
    }

    #[test]
    fn glob_match_exact_filename() {
        assert!(glob_match("CLAUDE.md", "CLAUDE.md"));
        assert!(glob_match("CLAUDE.md", "/home/user/.claude/CLAUDE.md"));
        assert!(!glob_match("CLAUDE.md", "CLAUDE.md.bak"));
    }

    #[test]
    fn extract_tool_path_variants() {
        let input = r#"{"tool_name":"Read","file_path":"/src/main.rs"}"#;
        assert_eq!(extract_tool_path(input), Some("/src/main.rs".to_string()));

        let input = r#"{"tool_name":"ListDirectory","path":"/src"}"#;
        assert_eq!(extract_tool_path(input), Some("/src".to_string()));

        let input = r#"{"tool_name":"Grep","directory":"/src"}"#;
        assert_eq!(extract_tool_path(input), Some("/src".to_string()));
    }
}
