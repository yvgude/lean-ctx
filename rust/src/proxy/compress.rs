use crate::core::patterns;

pub fn compress_tool_result(content: &str, tool_name: Option<&str>) -> String {
    if content.trim().is_empty() {
        return content.to_string();
    }

    let original_len = content.len();
    if original_len < 200 {
        return content.to_string();
    }

    if let Some(compressed) = try_shell_compress(content, tool_name) {
        return compressed;
    }

    if let Some(compressed) = try_file_compress(content) {
        return compressed;
    }

    if let Some(compressed) = try_search_compress(content) {
        return compressed;
    }

    generic_compress(content)
}

fn try_shell_compress(content: &str, tool_name: Option<&str>) -> Option<String> {
    let is_shell = tool_name.is_some_and(|n| {
        let nl = n.to_lowercase();
        nl.contains("bash")
            || nl.contains("shell")
            || nl.contains("terminal")
            || nl.contains("command")
            || nl == "execute"
    });

    if !is_shell && !looks_like_shell_output(content) {
        return None;
    }

    let cmd_hint = extract_command_hint(content);
    let cmd = cmd_hint.as_deref().unwrap_or("");

    patterns::compress_output(cmd, content)
}

fn try_file_compress(content: &str) -> Option<String> {
    if !looks_like_file_content(content) {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 50 {
        return None;
    }

    let has_numbered_lines = lines
        .iter()
        .take(5)
        .any(|l| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains('|'));

    if has_numbered_lines {
        let first = &lines[..10.min(lines.len())];
        let last = &lines[lines.len().saturating_sub(10)..];
        let omitted = lines.len().saturating_sub(20);
        if omitted > 0 {
            return Some(format!(
                "{}\n[... {omitted} lines omitted ...]\n{}",
                first.join("\n"),
                last.join("\n"),
            ));
        }
    }

    None
}

fn try_search_compress(content: &str) -> Option<String> {
    if !looks_like_search_results(content) {
        return None;
    }

    patterns::compress_output("grep", content)
}

fn generic_compress(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    if lines.len() <= 100 {
        return content.to_string();
    }

    let mut deduped: Vec<&str> = Vec::with_capacity(lines.len());
    let mut last_line = "";
    let mut dup_count = 0u32;

    for line in &lines {
        if *line == last_line {
            dup_count += 1;
            continue;
        }
        if dup_count > 0 {
            deduped.push(last_line);
            if dup_count > 1 {
                deduped.push("  [... repeated ...]");
            }
            dup_count = 0;
        }
        last_line = line;
        deduped.push(line);
    }
    if dup_count > 0 && !last_line.is_empty() {
        deduped.push(last_line);
    }

    if deduped.len() > 200 {
        let first = &deduped[..30];
        let last = &deduped[deduped.len() - 30..];
        let omitted = deduped.len() - 60;
        format!(
            "{}\n[... {omitted} lines omitted ...]\n{}",
            first.join("\n"),
            last.join("\n"),
        )
    } else {
        deduped.join("\n")
    }
}

fn looks_like_shell_output(content: &str) -> bool {
    let first_lines: Vec<&str> = content.lines().take(5).collect();
    let indicators = [
        "$ ",
        "% ",
        "# ",
        "error:",
        "warning:",
        "Compiling",
        "Building",
        "running",
        "fatal:",
        "npm ",
        "yarn ",
        "cargo ",
        "git ",
        "make",
        "pip ",
    ];
    first_lines
        .iter()
        .any(|l| indicators.iter().any(|i| l.contains(i)))
}

fn looks_like_file_content(content: &str) -> bool {
    let first_lines: Vec<&str> = content.lines().take(10).collect();
    let code_indicators = [
        "import ",
        "from ",
        "use ",
        "pub fn",
        "fn ",
        "class ",
        "def ",
        "function ",
        "const ",
        "let ",
        "var ",
        "#include",
        "package ",
        "module ",
        "struct ",
        "interface ",
        "enum ",
        "trait ",
    ];

    let matches = first_lines
        .iter()
        .filter(|l| code_indicators.iter().any(|i| l.contains(i)))
        .count();

    matches >= 2
}

fn looks_like_search_results(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().take(10).collect();
    let pattern_count = lines
        .iter()
        .filter(|l| {
            l.contains(':') && {
                let parts: Vec<&str> = l.splitn(3, ':').collect();
                parts.len() >= 2 && parts[0].contains('.')
            }
        })
        .count();

    pattern_count >= 3
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
    fn shell_output_detected() {
        assert!(looks_like_shell_output("$ cargo build\nCompiling foo v0.1"));
        assert!(!looks_like_shell_output("just some text\nnothing special"));
    }

    #[test]
    fn file_content_detected() {
        let code = "use std::io;\nimport os\nfrom pathlib import Path\nclass Foo:\n  def bar(self):\n    pass\nconst x = 1;\nlet y = 2;\nvar z = 3;\nfn main() {}";
        assert!(looks_like_file_content(code));
    }

    #[test]
    fn search_results_detected() {
        let grep =
            "src/main.rs:10:fn main()\nsrc/lib.rs:5:pub mod foo\nsrc/utils.rs:20:fn helper()";
        assert!(looks_like_search_results(grep));
    }
}
