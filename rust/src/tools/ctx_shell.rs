use crate::core::patterns;
use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const MAX_COMMAND_BYTES: usize = 8192;

const HEREDOC_PATTERNS: &[&str] = &[
    "<< 'EOF'", "<<'EOF'", "<< 'ENDOFFILE'", "<<'ENDOFFILE'",
    "<< 'END'", "<<'END'", "<< EOF", "<<EOF", "cat <<",
];

/// Validates a shell command before execution. Returns Some(error_message) if
/// the command should be rejected, None if it's safe to run.
pub fn validate_command(command: &str) -> Option<String> {
    if command.len() > MAX_COMMAND_BYTES {
        return Some(format!(
            "ERROR: Command too large ({} bytes, limit {}). \
             If you're writing file content, use the native Write/Edit tool instead. \
             ctx_shell is for reading command output only (git, cargo, npm, etc.).",
            command.len(),
            MAX_COMMAND_BYTES
        ));
    }

    if has_file_write_redirect(command) {
        return Some(
            "ERROR: ctx_shell detected a file-write command (shell redirect > or >>). \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output (git status, cargo test, npm run, etc.). \
             File writes via shell cause MCP protocol corruption on large payloads."
                .to_string(),
        );
    }

    let cmd_lower = command.to_lowercase();

    if cmd_lower.starts_with("tee ") || cmd_lower.contains("| tee ") {
        return Some(
            "ERROR: ctx_shell detected a file-write command (tee). \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output."
                .to_string(),
        );
    }

    for pattern in HEREDOC_PATTERNS {
        if cmd_lower.contains(&pattern.to_lowercase()) {
            return Some(
                "ERROR: ctx_shell detected a heredoc file-write command. \
                 Use the native Write tool to create/modify files. \
                 ctx_shell is ONLY for reading command output."
                    .to_string(),
            );
        }
    }

    None
}

/// Detects shell redirect operators (`>` or `>>`) that write to files.
/// Ignores `>` inside quotes, `2>` (stderr), `/dev/null`, and comparison operators.
fn has_file_write_redirect(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let c = bytes[i];
        if c == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if c == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if c == b'>' && !in_single_quote && !in_double_quote {
            if i > 0 && bytes[i - 1] == b'2' {
                i += 1;
                continue;
            }
            let target_start = if i + 1 < len && bytes[i + 1] == b'>' {
                i + 2
            } else {
                i + 1
            };
            let target: String = command[target_start..]
                .trim_start()
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect();
            if target == "/dev/null" {
                i += 1;
                continue;
            }
            if !target.is_empty() {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub fn handle(command: &str, output: &str, crp_mode: CrpMode) -> String {
    let original_tokens = count_tokens(output);

    let compressed = match patterns::compress_output(command, output) {
        Some(c) => c,
        None => generic_compress(output),
    };

    if crp_mode.is_tdd() && looks_like_code(&compressed) {
        let ext = detect_ext_from_command(command);
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&compressed, ext);
        for ident in &idents {
            sym.register(ident);
        }
        if !sym.is_empty() {
            let mapped = sym.apply(&compressed);
            let sym_table = sym.format_table();
            let result = format!("{mapped}{sym_table}");
            let sent = count_tokens(&result);
            let savings = protocol::format_savings(original_tokens, sent);
            return format!("{result}\n{savings}");
        }
    }

    let sent = count_tokens(&compressed);
    let savings = protocol::format_savings(original_tokens, sent);

    format!("{compressed}\n{savings}")
}

fn generic_compress(output: &str) -> String {
    let output = crate::core::compressor::strip_ansi(output);
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
        })
        .collect();

    if lines.len() <= 10 {
        return lines.join("\n");
    }

    let first_3 = &lines[..3];
    let last_3 = &lines[lines.len() - 3..];
    let omitted = lines.len() - 6;
    format!(
        "{}\n[truncated: showing 6/{} lines, {} omitted. Use raw=true for full output.]\n{}",
        first_3.join("\n"),
        lines.len(),
        omitted,
        last_3.join("\n")
    )
}

fn looks_like_code(text: &str) -> bool {
    let indicators = [
        "fn ",
        "pub ",
        "let ",
        "const ",
        "impl ",
        "struct ",
        "enum ",
        "function ",
        "class ",
        "import ",
        "export ",
        "def ",
        "async ",
        "=>",
        "->",
        "::",
        "self.",
        "this.",
    ];
    let total_lines = text.lines().count();
    if total_lines < 3 {
        return false;
    }
    let code_lines = text
        .lines()
        .filter(|l| indicators.iter().any(|i| l.contains(i)))
        .count();
    code_lines as f64 / total_lines as f64 > 0.15
}

fn detect_ext_from_command(command: &str) -> &str {
    let cmd = command.to_lowercase();
    if cmd.contains("cargo") || cmd.contains(".rs") {
        "rs"
    } else if cmd.contains("npm")
        || cmd.contains("node")
        || cmd.contains(".ts")
        || cmd.contains(".js")
    {
        "ts"
    } else if cmd.contains("python") || cmd.contains("pip") || cmd.contains(".py") {
        "py"
    } else if cmd.contains("go ") || cmd.contains(".go") {
        "go"
    } else {
        "rs"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_allows_safe_commands() {
        assert!(validate_command("git status").is_none());
        assert!(validate_command("cargo test").is_none());
        assert!(validate_command("npm run build").is_none());
        assert!(validate_command("ls -la").is_none());
    }

    #[test]
    fn validate_blocks_file_writes() {
        assert!(validate_command("cat > file.py << 'EOF'\nprint('hi')\nEOF").is_some());
        assert!(validate_command("echo 'data' > output.txt").is_some());
        assert!(validate_command("tee /tmp/file.txt").is_some());
        assert!(validate_command("printf 'hello' > test.txt").is_some());
        assert!(validate_command("cat << EOF\ncontent\nEOF").is_some());
    }

    #[test]
    fn validate_blocks_oversized_commands() {
        let huge = "x".repeat(MAX_COMMAND_BYTES + 1);
        let result = validate_command(&huge);
        assert!(result.is_some());
        assert!(result.unwrap().contains("too large"));
    }

    #[test]
    fn validate_allows_cat_without_redirect() {
        assert!(validate_command("cat file.txt").is_none());
    }
}
