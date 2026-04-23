use crate::core::patterns;
use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const MAX_COMMAND_BYTES: usize = 8192;

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

    if is_heredoc_file_write(command) {
        return Some(
            "ERROR: ctx_shell detected a heredoc writing to a file. \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output. \
             Note: heredocs for input piping (e.g. psql <<EOF) are allowed."
                .to_string(),
        );
    }

    None
}

/// Returns true only for heredocs that redirect to files (the dangerous pattern).
/// Legitimate heredoc uses (input piping, inline scripts) are allowed through.
fn is_heredoc_file_write(command: &str) -> bool {
    let has_heredoc = command.contains("<<");
    if !has_heredoc {
        return false;
    }
    // Only block: heredoc combined with file output redirect
    // e.g. `cat <<EOF > file.txt` or `cat <<'EOF' >> output.log`
    let cmd_lower = command.to_lowercase();
    let heredoc_patterns = ["<<eof", "<<'eof'", "<<\"eof\"", "<<end", "<<'end'"];
    let has_known_heredoc = heredoc_patterns.iter().any(|p| cmd_lower.contains(p));
    if !has_known_heredoc {
        return false;
    }
    has_file_write_redirect(command)
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

/// On Windows cmd.exe, `;` is not a valid command separator.
/// Convert `cmd1; cmd2` to `cmd1 && cmd2` when running under cmd.exe.
pub fn normalize_command_for_shell(command: &str) -> String {
    if !cfg!(windows) {
        return command.to_string();
    }
    let (_, flag) = crate::shell::shell_and_flag();
    if flag != "/C" {
        return command.to_string();
    }
    let bytes = command.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() + 16);
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\'' && !in_double {
            in_single = !in_single;
        } else if b == b'"' && !in_single {
            in_double = !in_double;
        } else if b == b';' && !in_single && !in_double {
            result.extend_from_slice(b" && ");
            continue;
        }
        result.push(b);
        let _ = i;
    }
    String::from_utf8(result).unwrap_or_else(|_| command.to_string())
}

pub fn handle(command: &str, output: &str, crp_mode: CrpMode) -> String {
    let original_tokens = count_tokens(output);

    if contains_auth_flow(output) {
        let savings = protocol::format_savings(original_tokens, original_tokens);
        return format!(
            "{output}\n[lean-ctx: auth/device-code flow detected — output preserved uncompressed]\n{savings}"
        );
    }

    let raw_compressed = match patterns::compress_output(command, output) {
        Some(c) => crate::core::compressor::safeguard_ratio(output, &c),
        None if is_search_command(command) => {
            let stripped = crate::core::compressor::strip_ansi(output);
            stripped.to_string()
        }
        None => generic_compress(output),
    };
    let compressed = crate::core::compressor::verbatim_compact(&raw_compressed);

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

fn is_search_command(command: &str) -> bool {
    let cmd = command.trim_start();
    cmd.starts_with("grep ")
        || cmd.starts_with("rg ")
        || cmd.starts_with("find ")
        || cmd.starts_with("fd ")
        || cmd.starts_with("ag ")
        || cmd.starts_with("ack ")
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

    if lines.len() <= 20 {
        return lines.join("\n");
    }

    let show_count = (lines.len() / 3).min(30);
    let half = show_count / 2;
    let first = &lines[..half];
    let last = &lines[lines.len() - half..];
    let omitted = lines.len() - (half * 2);
    format!(
        "{}\n[truncated: showing {}/{} lines, {} omitted. Use raw=true for full output.]\n{}",
        first.join("\n"),
        half * 2,
        lines.len(),
        omitted,
        last.join("\n")
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

/// Detects OAuth device code flow output that must not be compressed.
/// Uses a two-tier approach: strong signals match alone (very specific to
/// device code flows), weak signals require a URL/domain in the same output.
pub fn contains_auth_flow(output: &str) -> bool {
    let lower = output.to_lowercase();

    const STRONG_SIGNALS: &[&str] = &[
        "devicelogin",
        "deviceauth",
        "device_code",
        "device code",
        "device-code",
        "verification_uri",
        "user_code",
        "one-time code",
    ];

    if STRONG_SIGNALS.iter().any(|s| lower.contains(s)) {
        return true;
    }

    const WEAK_SIGNALS: &[&str] = &[
        "enter the code",
        "enter this code",
        "enter code:",
        "use the code",
        "use a web browser to open",
        "open the page",
        "authenticate by visiting",
        "sign in with the code",
        "sign in using a code",
        "verification code",
        "authorize this device",
        "waiting for authentication",
        "waiting for login",
        "waiting for you to authenticate",
        "open your browser",
        "open in your browser",
    ];

    let has_weak_signal = WEAK_SIGNALS.iter().any(|s| lower.contains(s));
    if !has_weak_signal {
        return false;
    }

    lower.contains("http://") || lower.contains("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_cmd_no_change_on_unix() {
        if cfg!(windows) {
            return;
        }
        assert_eq!(
            normalize_command_for_shell("cd /tmp; ls -la"),
            "cd /tmp; ls -la"
        );
    }

    #[test]
    fn validate_allows_safe_commands() {
        assert!(validate_command("git status").is_none());
        assert!(validate_command("cargo test").is_none());
        assert!(validate_command("npm run build").is_none());
        assert!(validate_command("ls -la").is_none());
    }

    #[test]
    fn validate_blocks_file_writes() {
        assert!(validate_command("echo 'data' > output.txt").is_some());
        assert!(validate_command("tee /tmp/file.txt").is_some());
        assert!(validate_command("printf 'hello' > test.txt").is_some());
    }

    #[test]
    fn validate_blocks_heredoc_with_file_redirect() {
        assert!(validate_command("cat > file.py <<'EOF'\nprint('hi')\nEOF").is_some());
        assert!(validate_command("cat <<EOF > output.txt\nhello\nEOF").is_some());
        assert!(validate_command("cat <<'END' >> logfile.txt\ndata\nEND").is_some());
    }

    #[test]
    fn validate_allows_heredoc_without_file_redirect() {
        assert!(validate_command("cat <<EOF\nhello world\nEOF").is_none());
        assert!(validate_command("psql -d mydb <<EOF\nSELECT 1;\nEOF").is_none());
        assert!(
            validate_command("git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"").is_none()
        );
        assert!(validate_command("grep pattern <<EOF\nfoo\nbar\nEOF").is_none());
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

    // --- Auth flow detection: strong signals (no URL needed) ---

    #[test]
    fn auth_flow_detects_azure_device_code() {
        let output = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate.";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_gh_auth_one_time_code() {
        let output =
            "! First copy your one-time code: ABCD-1234\n- Press Enter to open github.com in your browser...";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_device_code_json() {
        let output = r#"{"device_code":"abc123","user_code":"ABCD-1234","verification_uri":"https://example.com/activate"}"#;
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_verification_uri_field() {
        let output =
            r#"{"verification_uri": "https://login.microsoftonline.com/common/oauth2/deviceauth"}"#;
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_user_code_field() {
        let output = r#"{"user_code": "FGHJK-LMNOP", "expires_in": 900}"#;
        assert!(contains_auth_flow(output));
    }

    // --- Auth flow detection: weak signals (require URL) ---

    #[test]
    fn auth_flow_detects_gcloud_with_url() {
        let output = "Go to the following link in your browser:\n\n    https://accounts.google.com/o/oauth2/auth?response_type=code\n\nEnter verification code: ";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_aws_sso_with_url() {
        let output = "If the browser does not open, open the following URL:\nhttps://device.sso.us-east-1.amazonaws.com/\n\nThen enter the code:\nABCD-EFGH";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_firebase_with_url() {
        let output = "Visit this URL on this device to log in:\nhttps://accounts.google.com/o/oauth2/auth?...\n\nWaiting for authentication...";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_generic_browser_open_with_url() {
        let output =
            "Open your browser to https://login.example.com/device and enter the code XYZW-1234";
        assert!(contains_auth_flow(output));
    }

    // --- False positive protection ---

    #[test]
    fn auth_flow_ignores_normal_build_output() {
        let output = "Compiling lean-ctx v2.21.9\nFinished release profile\n";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_ignores_git_output() {
        let output = "On branch main\nYour branch is up to date with 'origin/main'.\nnothing to commit, working tree clean";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_ignores_npm_install_output() {
        let output = "added 150 packages in 3s\n\n24 packages are looking for funding\n  run `npm fund` for details\nhttps://npmjs.com/package/lean-ctx";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_ignores_docs_mentioning_auth() {
        let output = "The authorization code grant type is the most common OAuth flow.\nSee https://oauth.net/2/grant-types/ for details.";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_weak_signal_requires_url() {
        let output = "Please enter the code ABC123 in the terminal";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_weak_signal_without_url_is_ignored() {
        let output = "Waiting for authentication to complete... done!";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_ignores_virtualenv_activate() {
        let output = "Created virtualenv at .venv\nRun: source .venv/bin/activate";
        assert!(!contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_ignores_api_response_with_code_field() {
        let output = r#"{"status": "ok", "code": 200, "message": "success"}"#;
        assert!(!contains_auth_flow(output));
    }

    // --- Integration: handle() preserves auth flow ---

    #[test]
    fn handle_preserves_auth_flow_output_fully() {
        let output = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate.\nWaiting for you...\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9\nLine 10\nLine 11\nLine 12\nLine 13";
        let result = handle("az login --use-device-code", output, CrpMode::Off);
        assert!(result.contains("ABCD1234"), "auth code must be preserved");
        assert!(result.contains("devicelogin"), "URL must be preserved");
        assert!(
            result.contains("auth/device-code flow detected"),
            "detection note must be present"
        );
        assert!(
            result.contains("Line 13"),
            "all lines must be preserved (no truncation)"
        );
    }

    #[test]
    fn handle_compresses_normal_output_not_auth() {
        let lines: Vec<String> = (1..=20).map(|i| format!("Line {i} of output")).collect();
        let output = lines.join("\n");
        let result = handle("some-tool check", &output, CrpMode::Off);
        assert!(
            !result.contains("auth/device-code flow detected"),
            "normal output must not trigger auth detection"
        );
        assert!(
            result.len() < output.len() + 100,
            "normal output should be compressed, not inflated"
        );
    }

    #[test]
    fn is_search_command_detects_grep() {
        assert!(is_search_command("grep -r pattern src/"));
        assert!(is_search_command("rg pattern src/"));
        assert!(is_search_command("find . -name '*.rs'"));
        assert!(is_search_command("fd pattern"));
        assert!(is_search_command("ag pattern src/"));
        assert!(is_search_command("ack pattern"));
    }

    #[test]
    fn is_search_command_rejects_non_search() {
        assert!(!is_search_command("cargo build"));
        assert!(!is_search_command("git status"));
        assert!(!is_search_command("npm install"));
        assert!(!is_search_command("cat file.rs"));
    }

    #[test]
    fn generic_compress_preserves_short_output() {
        let lines: Vec<String> = (1..=20).map(|i| format!("Line {i}")).collect();
        let output = lines.join("\n");
        let result = generic_compress(&output);
        assert_eq!(result, output);
    }

    #[test]
    fn generic_compress_scales_with_length() {
        let lines: Vec<String> = (1..=60).map(|i| format!("Line {i}")).collect();
        let output = lines.join("\n");
        let result = generic_compress(&output);
        assert!(result.contains("truncated"));
        let shown_count = result.lines().count();
        assert!(
            shown_count > 10,
            "should show more than old 6-line limit, got {shown_count}"
        );
        assert!(shown_count < 60, "should be truncated, not full output");
    }

    #[test]
    fn handle_preserves_search_results() {
        let lines: Vec<String> = (1..=30)
            .map(|i| format!("src/file{i}.rs:42: fn search_result()"))
            .collect();
        let output = lines.join("\n");
        let result = handle("rg search_result src/", &output, CrpMode::Off);
        for i in 1..=30 {
            assert!(
                result.contains(&format!("file{i}")),
                "search result file{i} should be preserved in output"
            );
        }
    }
}
