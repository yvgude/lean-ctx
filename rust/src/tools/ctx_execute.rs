use crate::core::sandbox::{self, SandboxResult};
use crate::core::tokens::count_tokens;
use crate::server::tool_trait::ShellOutcome;

/// Executes a code snippet in a sandboxed environment.
/// Returns the formatted output plus the structured outcome so the MCP layer
/// can set `isError`/`structuredContent` on failures (GitHub #389).
pub fn handle(
    language: &str,
    code: &str,
    intent: Option<&str>,
    timeout: Option<u64>,
) -> (String, ShellOutcome) {
    handle_in(language, code, intent, timeout, None)
}

/// Same as [`handle`], but runs the snippet in `cwd` (GH #1666).
///
/// `ctx_shell` reroutes an interpreter heredoc here and has already resolved a
/// working directory for the rest of the same call; passing it on is what keeps
/// both halves of that call in one directory.
pub fn handle_in(
    language: &str,
    code: &str,
    intent: Option<&str>,
    timeout: Option<u64>,
    cwd: Option<&std::path::Path>,
) -> (String, ShellOutcome) {
    if language.eq_ignore_ascii_case("shell")
        && let Err(message) = crate::core::shell_allowlist::check_shell_allowlist(code)
    {
        return (message.to_string(), ShellOutcome::Blocked);
    }
    let result = sandbox::execute_in(language, code, timeout, cwd);
    (
        format_result(&result, intent),
        ShellOutcome::Exit(result.exit_code),
    )
}

/// Summarize admitted file content as data, without executing it or creating
/// a plaintext temporary copy. Source rules run before statistics or previews.
pub fn handle_file(
    path: &str,
    intent: Option<&str>,
    project_root: Option<&str>,
) -> (String, ShellOutcome) {
    let root = match project_root {
        Some(root) => std::path::PathBuf::from(root),
        None => match std::env::current_dir() {
            Ok(root) => root,
            Err(_) => return ("Source project unavailable".into(), ShellOutcome::Blocked),
        },
    };
    let result =
        crate::core::policy::runtime::with_project_source_view(&root.to_string_lossy(), || {
            handle_file_in_view(path, intent, &root)
        });
    match result {
        Ok((text, outcome, authority)) => {
            if let Some(authority) = authority {
                authority.publish();
            }
            (text, outcome)
        }
        Err(_) => (
            "File processing withheld: source authority changed or could not be verified.".into(),
            ShellOutcome::Blocked,
        ),
    }
}

fn handle_file_in_view(
    path: &str,
    intent: Option<&str>,
    root: &std::path::Path,
) -> (
    String,
    ShellOutcome,
    Option<crate::core::archive::authority::ArchiveAuthority>,
) {
    let mut remaining = crate::core::limits::max_read_bytes();
    let read = match crate::tools::ctx_read::read_file_for_tool_rooted_with_path(
        path,
        &root.to_string_lossy(),
        "ctx_execute",
        &mut remaining,
    ) {
        Ok(content) => content,
        Err(error) => {
            return (
                format!("File processing refused: {error}"),
                ShellOutcome::Blocked,
                None,
            );
        }
    };
    let authority = crate::core::archive::authority::ArchiveAuthority::file(
        root,
        &read.canonical_path,
        "ctx_execute",
        &read.content,
    );
    (
        summarize_file_content(&read.content, intent),
        ShellOutcome::Exit(0),
        authority,
    )
}

fn summarize_file_content(content: &str, intent: Option<&str>) -> String {
    let lines = content.lines();
    let count = lines.clone().count();
    let mut output = format!(
        "Admitted text: {count} lines, {} bytes, {} words (after decoding and filtering)\n",
        content.len(),
        content.split_whitespace().count(),
    );
    if let Some(intent) = intent {
        output.push_str(&format!("Intent: {}\n", sanitize_intent(intent)));
    }
    // At most six lines of 1024 Unicode characters each; do not collect the
    // entire source into a second line buffer just to produce a preview.
    let append = |output: &mut String, line: &str| {
        if let Some((end, _)) = line.char_indices().nth(1024) {
            output.push_str(&line[..end]);
            output.push_str(" [line truncated]");
        } else {
            output.push_str(line);
        }
        output.push('\n');
    };
    if count <= 6 {
        for line in lines {
            append(&mut output, line);
        }
    } else {
        for line in lines.clone().take(3) {
            append(&mut output, line);
        }
        output.push_str(&format!("... {} middle lines omitted ...\n", count - 6));
        let tail: Vec<_> = lines.rev().take(3).collect();
        for line in tail.into_iter().rev() {
            append(&mut output, line);
        }
    }
    output
}

/// Executes multiple (language, code) pairs in parallel and returns aggregated
/// results. The outcome carries the first non-zero exit code (0 when all
/// tasks succeeded), so one failing task marks the whole batch as failed.
pub fn handle_batch(items: &[(String, String)]) -> (String, ShellOutcome) {
    for (index, (language, code)) in items.iter().enumerate() {
        if language.eq_ignore_ascii_case("shell")
            && let Err(message) = crate::core::shell_allowlist::check_shell_allowlist(code)
        {
            return (
                format!(
                    "batch item {} blocked by shell policy:\n{message}",
                    index + 1
                ),
                ShellOutcome::Blocked,
            );
        }
    }
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

fn sanitize_intent(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_' || *c == '.')
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_simple_python() {
        let _lock = crate::core::data_dir::test_env_lock();
        let (result, outcome) = handle("python", "print(2 + 2)", None, None);
        assert!(result.contains('4'));
        assert!(result.contains("python"));
        assert_eq!(outcome, ShellOutcome::Exit(0), "success must report exit 0");
    }

    #[test]
    fn handle_with_intent() {
        let _lock = crate::core::data_dir::test_env_lock();
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
        let _lock = crate::core::data_dir::test_env_lock();
        let (result, outcome) = handle("python", "raise Exception('boom')", None, None);
        assert!(result.contains("EXIT"));
        assert!(result.contains("boom"));
        assert!(
            outcome.is_error(),
            "non-zero sandbox exit must surface as a tool error (#389)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn shell_execution_cannot_bypass_ctx_shell_allowlist() {
        // Both guards are needed: `#[serial]` and `test_env_lock` are separate
        // mutexes. The override below replaces the allowlist wholesale, which
        // broke `gh391_strict_mode_blocks_substitution_in_args` — a test that
        // serializes on the lock, not on `#[serial]`.
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
        let (result, outcome) = handle("shell", "definitely_not_allowed_command", None, None);
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        assert_eq!(outcome, ShellOutcome::Blocked);
        assert!(
            result.contains("shell allowlist"),
            "unexpected result: {result}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn shell_batch_is_preflighted_before_any_item_runs() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
        let items = vec![
            ("shell".to_string(), "echo safe".to_string()),
            (
                "shell".to_string(),
                "definitely_not_allowed_command".to_string(),
            ),
        ];
        let (result, outcome) = handle_batch(&items);
        crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
        assert_eq!(outcome, ShellOutcome::Blocked);
        assert!(result.contains("batch item 2 blocked"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn batch_multiple_tasks() {
        let _lock = crate::core::data_dir::test_env_lock();
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
        let _lock = crate::core::data_dir::test_env_lock();
        let items = vec![
            ("python".to_string(), "print('ok')".to_string()),
            ("python".to_string(), "raise SystemExit(3)".to_string()),
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

#[cfg(test)]
#[path = "ctx_execute_file_tests.rs"]
mod file_tests;
