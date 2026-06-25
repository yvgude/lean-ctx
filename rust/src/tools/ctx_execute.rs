use crate::core::sandbox::{self, SandboxResult};
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::ShellOutcome;

/// Executes a code snippet in a sandboxed environment.
/// Returns the formatted output plus the structured outcome so the MCP layer
/// can set `isError`/`structuredContent` on failures (GitHub #389).
#[must_use]
pub fn handle(
    language: &str,
    code: &str,
    intent: Option<&str>,
    timeout: Option<u64>,
) -> (String, ShellOutcome) {
    let result = sandbox::execute(language, code, timeout);
    (
        format_result(&result, intent),
        ShellOutcome::Exit(result.exit_code),
    )
}

/// Reads a file from disk, detects its language, and executes a processing script.
///
/// `project_root` is used for pathjail validation. If `None`, the current
/// directory is used as the jail root. Precondition failures (path rejected,
/// unreadable, too large) never execute anything and report `Blocked`.
#[must_use]
pub fn handle_file(
    path: &str,
    intent: Option<&str>,
    project_root: Option<&str>,
) -> (String, ShellOutcome) {
    let jail_root = match project_root {
        Some(r) => std::path::PathBuf::from(r),
        None => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    };
    let candidate = std::path::Path::new(path);
    let jailed = match crate::core::pathjail::jail_path(candidate, &jail_root) {
        Ok(p) => p,
        Err(e) => return (format!("Path rejected: {e}"), ShellOutcome::Blocked),
    };
    let path_str = jailed.to_string_lossy();

    let cap = crate::core::limits::max_read_bytes();
    let meta = match std::fs::metadata(&*jailed) {
        Ok(m) => m,
        Err(e) => {
            return (
                format!("Error reading {path_str}: {e}"),
                ShellOutcome::Blocked,
            );
        }
    };
    if meta.len() > cap as u64 {
        return (
            format!(
                "File too large ({} bytes, limit {cap} bytes). Use a line-range read instead.",
                meta.len()
            ),
            ShellOutcome::Blocked,
        );
    }
    let content = match std::fs::read_to_string(&*jailed) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("Error reading {path_str}: {e}"),
                ShellOutcome::Blocked,
            );
        }
    };

    let language = detect_language_from_extension(path);
    let code = build_file_processing_script(&language, &content, intent);
    let result = sandbox::execute(&language, &code, None);
    (
        format_result(&result, intent),
        ShellOutcome::Exit(result.exit_code),
    )
}

/// Executes multiple (language, code) pairs in parallel and returns aggregated
/// results. The outcome carries the first non-zero exit code (0 when all
/// tasks succeeded), so one failing task marks the whole batch as failed.
#[must_use]
pub fn handle_batch(items: &[(String, String)]) -> (String, ShellOutcome) {
    let results = sandbox::batch_execute(items);
    let mut output = Vec::new();

    for (i, result) in results.iter().enumerate() {
        let label = format!("[{}/{}] {}", i + 1, results.len(), result.language);
        if result.exit_code == 0 {
            let stdout = result.stdout.trim();
            if stdout.is_empty() {
                output.push(format!("{label}: (no output) [{} ms]", result.duration_ms));
            } else {
                output.push(format!("{label}: {stdout} [{} ms]", result.duration_ms));
            }
        } else {
            let stderr = result.stderr.trim();
            output.push(format!(
                "{label}: EXIT {} — {stderr} [{} ms]",
                result.exit_code, result.duration_ms
            ));
        }
    }

    let total_ms: u64 = results.iter().map(|r| r.duration_ms).sum();
    output.push(format!("\n{} tasks, {} ms total", results.len(), total_ms));
    let first_failure = results
        .iter()
        .map(|r| r.exit_code)
        .find(|c| *c != 0)
        .unwrap_or(0);
    (output.join("\n"), ShellOutcome::Exit(first_failure))
}

fn format_result(result: &SandboxResult, intent: Option<&str>) -> String {
    let mut parts = Vec::new();

    if result.exit_code == 0 {
        let stdout = result.stdout.trim();
        if stdout.is_empty() {
            parts.push("(no output)".to_string());
        } else {
            let raw_tokens = count_tokens(stdout);
            parts.push(stdout.to_string());

            if let Some(intent_desc) = intent
                && raw_tokens > 50
            {
                parts.push(format!("[intent: {intent_desc}]"));
            }
        }
    } else {
        if !result.stdout.is_empty() {
            parts.push(result.stdout.trim().to_string());
        }
        parts.push(format!(
            "EXIT {} — {}",
            result.exit_code,
            result.stderr.trim()
        ));
    }

    parts.push(format!("[{} | {} ms]", result.language, result.duration_ms));
    parts.join("\n")
}

fn detect_language_from_extension(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "py" | "json" | "csv" | "log" | "txt" | "xml" | "yaml" | "yml" | "md" | "html" => "python",
        "rb" => "ruby",
        "go" => "go",
        "rs" => "rust",
        "php" => "php",
        "pl" => "perl",
        "r" | "R" => "r",
        "ex" | "exs" => "elixir",
        _ => "shell",
    }
    .to_string()
}

fn sanitize_intent(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_' || *c == '.')
        .take(200)
        .collect()
}

/// Escape a path for safe embedding inside a Python raw double-quoted string.
/// Handles embedded double-quotes which would break `r"..."`.
fn escape_for_python_raw(path: &str) -> String {
    path.replace('"', r#"\" + '"' + r""#)
}

/// Escape a path for safe embedding inside a shell double-quoted string.
/// Handles `$`, backtick, `\`, and `"` which are special inside double quotes.
fn escape_for_shell_dq(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            '$' | '`' | '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn build_file_processing_script(language: &str, content: &str, intent: Option<&str>) -> String {
    let Ok(tmp) = tempfile::Builder::new()
        .prefix("lean-ctx-exec-")
        .suffix(".dat")
        .tempfile()
    else {
        return "echo 'lean-ctx: failed to create temp file'".to_string();
    };
    let _ = std::fs::write(tmp.path(), content);
    let tmp_path = tmp.path().to_string_lossy().to_string();
    let _keep = tmp.into_temp_path();
    let intent_str = sanitize_intent(intent.unwrap_or("summarize the content"));

    if language == "python" {
        let py_path = escape_for_python_raw(&tmp_path);
        format!(
            r#"
    import os

    with open(r"{py_path}", "r", encoding="utf-8") as f:
        data = f.read()
    os.remove(r"{py_path}")

    lines = data.strip().split('\n')
    total_lines = len(lines)
    total_bytes = len(data.encode('utf-8'))

    word_count = sum(len(line.split()) for line in lines)

    print(f"{{total_lines}} lines, {{total_bytes}} bytes, {{word_count}} words")
    print("Intent: {intent_str}")

    if total_lines > 10:
        print(f"First 3: {{lines[:3]}}")
        print(f"Last 3: {{lines[-3:]}}")
    "#
        )
    } else {
        let sh_path = escape_for_shell_dq(&tmp_path);
        format!(
            r#"
    data=$(cat "{sh_path}")
    rm -f "{sh_path}"
    lines=$(echo "$data" | wc -l | tr -d ' ')
    bytes=$(echo "$data" | wc -c | tr -d ' ')
    echo "$lines lines, $bytes bytes"
    echo 'Intent: {intent_str}'
    echo "$data" | head -3
    echo "..."
    echo "$data" | tail -3
    "#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_simple_python() {
        let (result, outcome) = handle("python", "print(2 + 2)", None, None);
        assert!(result.contains('4'));
        assert!(result.contains("python"));
        assert_eq!(outcome, ShellOutcome::Exit(0), "success must report exit 0");
    }

    #[test]
    fn handle_with_intent() {
        let (result, _) = handle(
            "python",
            "print('found 5 errors')",
            Some("count errors"),
            None,
        );
        assert!(result.contains("found 5 errors"));
    }

    #[test]
    fn handle_error_shows_stderr() {
        let (result, outcome) = handle("python", "raise Exception('boom')", None, None);
        assert!(result.contains("EXIT"));
        assert!(result.contains("boom"));
        assert!(
            outcome.is_error(),
            "non-zero sandbox exit must surface as a tool error (#389)"
        );
    }

    #[test]
    fn detect_language_from_path() {
        assert_eq!(detect_language_from_extension("test.py"), "python");
        assert_eq!(detect_language_from_extension("test.js"), "javascript");
        assert_eq!(detect_language_from_extension("test.rs"), "rust");
        assert_eq!(detect_language_from_extension("test.csv"), "python");
        assert_eq!(detect_language_from_extension("test.log"), "python");
    }

    #[test]
    fn escape_shell_dq_handles_special_chars() {
        assert_eq!(escape_for_shell_dq(r"C:\tmp\file"), r"C:\\tmp\\file");
        assert_eq!(escape_for_shell_dq("/tmp/normal"), "/tmp/normal");
        assert_eq!(escape_for_shell_dq("path with $VAR"), r"path with \$VAR");
        assert_eq!(escape_for_shell_dq(r#"path"quote"#), r#"path\"quote"#);
        assert_eq!(escape_for_shell_dq("has `backtick`"), r"has \`backtick\`");
    }

    #[test]
    fn escape_python_raw_handles_quotes() {
        assert_eq!(escape_for_python_raw("/tmp/normal"), "/tmp/normal");
        assert_eq!(escape_for_python_raw(r"C:\Users\test"), r"C:\Users\test");
    }

    #[test]
    fn script_with_spaces_in_path() {
        let script = build_file_processing_script("shell", "test data", None);
        let lines: Vec<&str> = script.lines().collect();
        for line in &lines {
            if line.contains("cat ") || line.contains("rm -f") {
                assert!(
                    line.contains('"'),
                    "path must be double-quoted in shell script: {line}"
                );
            }
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn batch_multiple_tasks() {
        let items = vec![
            ("python".to_string(), "print('task1')".to_string()),
            ("shell".to_string(), "echo task2".to_string()),
        ];
        let (result, outcome) = handle_batch(&items);
        assert!(result.contains("task1"));
        assert!(result.contains("task2"));
        assert!(result.contains("2 tasks"));
        assert_eq!(outcome, ShellOutcome::Exit(0), "all tasks succeeded");
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn batch_with_failing_task_reports_failure() {
        let items = vec![
            ("shell".to_string(), "echo ok".to_string()),
            ("shell".to_string(), "exit 3".to_string()),
        ];
        let (_, outcome) = handle_batch(&items);
        assert_eq!(
            outcome,
            ShellOutcome::Exit(3),
            "one failing task marks the whole batch as failed (#389)"
        );
    }

    #[test]
    fn handle_file_precondition_failure_is_blocked() {
        let (result, outcome) = handle_file("/nonexistent/definitely-missing.py", None, None);
        assert!(outcome.is_error(), "precondition failures are tool errors");
        assert_eq!(outcome, ShellOutcome::Blocked, "nothing was executed");
        assert!(!result.is_empty());
    }
}
