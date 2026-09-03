use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct SandboxResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub language: String,
    pub duration_ms: u64,
}

const TIMEOUT_SECS: u64 = 30;
const MAX_OUTPUT_BYTES: usize = 32_768;
/// Upper bound on the `code` payload. Generous for real scripts while preventing an
/// agent from forcing a multi-megabyte temp-file write / interpreter argv → memory abuse.
const MAX_CODE_BYTES: usize = 256 * 1024;

pub fn execute(language: &str, code: &str, timeout_secs: Option<u64>) -> SandboxResult {
    execute_in(language, code, timeout_secs, None)
}

/// Same as [`execute`], but runs the snippet in `cwd` (GH #1666).
///
/// A caller that already resolved a working directory — `ctx_shell` rerouting
/// an interpreter heredoc, for one — must be able to hand it over. Without
/// this, the snippet inherited the server process's directory while the rest
/// of the same tool call ran in `cwd`, so relative paths in the snippet
/// resolved against a different tree and quietly hit the wrong files.
pub fn execute_in(
    language: &str,
    code: &str,
    timeout_secs: Option<u64>,
    cwd: Option<&std::path::Path>,
) -> SandboxResult {
    if code.len() > MAX_CODE_BYTES {
        return SandboxResult {
            stdout: String::new(),
            stderr: format!(
                "Code exceeds the {MAX_CODE_BYTES}-byte limit ({} bytes). Split it into smaller scripts.",
                code.len()
            ),
            exit_code: 1,
            language: language.to_string(),
            duration_ms: 0,
        };
    }

    let timeout = timeout_secs.unwrap_or(TIMEOUT_SECS);
    let start = std::time::Instant::now();

    let Some(runtime) = resolve_runtime(language) else {
        return SandboxResult {
            stdout: String::new(),
            stderr: format!(
                "Unsupported language: {language}. Supported: javascript, typescript, python, shell, ruby, go, rust, php, perl, r, elixir"
            ),
            exit_code: 1,
            language: language.to_string(),
            duration_ms: 0,
        };
    };

    let sandbox_level = std::env::var("LEAN_CTX_SANDBOX_LEVEL")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or_else(|| crate::core::config::Config::load().sandbox_level);

    if sandbox_level >= 1 && cfg!(target_os = "macos") {
        let result = seatbelt_execute(&runtime, code, timeout, cwd);
        let duration_ms = start.elapsed().as_millis() as u64;
        return match result {
            Ok((stdout, stderr, exit_code)) => SandboxResult {
                stdout: truncate_output(&stdout),
                stderr: truncate_smart(&stderr, 2048),
                exit_code,
                language: language.to_string(),
                duration_ms,
            },
            Err(e) => SandboxResult {
                stdout: String::new(),
                stderr: format!("Seatbelt execution error: {e}"),
                exit_code: 1,
                language: language.to_string(),
                duration_ms,
            },
        };
    } else if sandbox_level >= 1 {
        #[cfg(target_os = "linux")]
        {
            let result = landlock_execute(&runtime, code, timeout, cwd);
            let duration_ms = start.elapsed().as_millis() as u64;
            return match result {
                Ok((stdout, stderr, exit_code)) => SandboxResult {
                    stdout: truncate_output(&stdout),
                    stderr: truncate_smart(&stderr, 2048),
                    exit_code,
                    language: language.to_string(),
                    duration_ms,
                },
                Err(e) => SandboxResult {
                    stdout: String::new(),
                    stderr: format!("Landlock execution error: {e}"),
                    exit_code: 1,
                    language: language.to_string(),
                    duration_ms,
                },
            };
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        eprintln!(
            "[lean-ctx] sandbox_level=1 requested but sandboxing not available on this platform; falling back to Level 0"
        );
    }

    let result = if runtime.needs_temp_file {
        execute_with_file(&runtime, code, timeout, cwd)
    } else {
        execute_with_stdin(&runtime, code, timeout, cwd)
    };

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok((stdout, stderr, code)) => SandboxResult {
            stdout: truncate_output(&stdout),
            stderr: truncate_smart(&stderr, 2048),
            exit_code: code,
            language: language.to_string(),
            duration_ms,
        },
        Err(e) => SandboxResult {
            stdout: String::new(),
            stderr: format!("Execution error: {e}"),
            exit_code: 1,
            language: language.to_string(),
            duration_ms,
        },
    }
}

pub fn batch_execute(items: &[(String, String)]) -> Vec<SandboxResult> {
    items
        .iter()
        .map(|(lang, code)| execute(lang, code, None))
        .collect()
}

struct RuntimeConfig {
    command: String,
    args: Vec<String>,
    needs_temp_file: bool,
    file_extension: String,
    env: HashMap<String, String>,
}

fn resolve_runtime(language: &str) -> Option<RuntimeConfig> {
    let lang = language.to_lowercase();
    let lang = lang.as_str();

    match lang {
        "javascript" | "js" | "node" => Some(RuntimeConfig {
            command: find_binary(&["bun", "node"])?,
            args: vec!["-e".to_string()],
            needs_temp_file: false,
            file_extension: "js".to_string(),
            env: HashMap::new(),
        }),
        "typescript" | "ts" => Some(RuntimeConfig {
            command: find_binary(&["bun", "npx"])?,
            args: if which_exists("bun") {
                vec!["-e".to_string()]
            } else {
                vec!["tsx".to_string(), "-e".to_string()]
            },
            needs_temp_file: false,
            file_extension: "ts".to_string(),
            env: HashMap::new(),
        }),
        "python" | "py" => Some(RuntimeConfig {
            command: find_binary(&["python3", "python"])?,
            args: vec!["-c".to_string()],
            needs_temp_file: false,
            file_extension: "py".to_string(),
            env: HashMap::from([("PYTHONDONTWRITEBYTECODE".into(), "1".into())]),
        }),
        "shell" | "bash" | "sh" => {
            #[cfg(target_os = "windows")]
            {
                Some(RuntimeConfig {
                    command: "cmd".to_string(),
                    args: vec!["/C".to_string()],
                    needs_temp_file: false,
                    file_extension: "bat".to_string(),
                    env: HashMap::new(),
                })
            }
            #[cfg(not(target_os = "windows"))]
            {
                Some(RuntimeConfig {
                    command: find_binary(&["bash", "sh"])?,
                    args: vec!["-c".to_string()],
                    needs_temp_file: false,
                    file_extension: "sh".to_string(),
                    env: HashMap::new(),
                })
            }
        }
        "ruby" | "rb" => Some(RuntimeConfig {
            command: find_binary(&["ruby"])?,
            args: vec!["-e".to_string()],
            needs_temp_file: false,
            file_extension: "rb".to_string(),
            env: HashMap::new(),
        }),
        "go" | "golang" => Some(RuntimeConfig {
            command: find_binary(&["go"])?,
            args: vec!["run".to_string()],
            needs_temp_file: true,
            file_extension: "go".to_string(),
            env: HashMap::new(),
        }),
        "rust" | "rs" => Some(RuntimeConfig {
            command: "rustc_script".to_string(),
            args: vec![],
            needs_temp_file: true,
            file_extension: "rs".to_string(),
            env: HashMap::new(),
        }),
        "php" => Some(RuntimeConfig {
            command: find_binary(&["php"])?,
            args: vec!["-r".to_string()],
            needs_temp_file: false,
            file_extension: "php".to_string(),
            env: HashMap::new(),
        }),
        "perl" | "pl" => Some(RuntimeConfig {
            command: find_binary(&["perl"])?,
            args: vec!["-e".to_string()],
            needs_temp_file: false,
            file_extension: "pl".to_string(),
            env: HashMap::new(),
        }),
        "r" => Some(RuntimeConfig {
            command: find_binary(&["Rscript"])?,
            args: vec!["-e".to_string()],
            needs_temp_file: false,
            file_extension: "R".to_string(),
            env: HashMap::new(),
        }),
        "elixir" | "ex" => Some(RuntimeConfig {
            command: find_binary(&["elixir"])?,
            args: vec!["-e".to_string()],
            needs_temp_file: false,
            file_extension: "exs".to_string(),
            env: HashMap::new(),
        }),
        _ => None,
    }
}

fn seatbelt_execute(
    runtime: &RuntimeConfig,
    code: &str,
    timeout: u64,
    cwd: Option<&std::path::Path>,
) -> Result<(String, String, i32), String> {
    let tmp_dir = std::env::temp_dir().join("lean-ctx-sandbox");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let env_pairs: Vec<(String, String)> = runtime
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if runtime.needs_temp_file {
        let suffix = format!(".{}", runtime.file_extension);
        let tmp = tempfile::Builder::new()
            .prefix("exec_")
            .suffix(&suffix)
            .tempfile_in(&tmp_dir)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let file_path = tmp.into_temp_path();
        std::fs::write(&file_path, code).map_err(|e| format!("Failed to write temp file: {e}"))?;

        let mut allowed = vec![file_path.to_path_buf()];
        // The profile denies by default, so a working directory the caller
        // chose has to be granted explicitly — otherwise the snippet starts
        // there and cannot read a thing (#1666). Write permission is
        // deliberately not granted: that boundary belongs to sandbox_level.
        if let Some(dir) = cwd {
            allowed.push(dir.to_path_buf());
        }
        let allowed_refs: Vec<&std::path::Path> =
            allowed.iter().map(std::path::PathBuf::as_path).collect();
        let file_str = file_path.to_string_lossy().to_string();

        let mut args: Vec<&str> = runtime
            .args
            .iter()
            .map(std::string::String::as_str)
            .collect();
        args.push(&file_str);

        let result = super::sandbox_seatbelt::execute_sandboxed(
            &runtime.command,
            &args,
            &allowed_refs,
            &env_pairs,
            timeout,
            cwd,
        );
        let _ = std::fs::remove_file(&file_path);
        result
    } else {
        let mut args: Vec<&str> = runtime
            .args
            .iter()
            .map(std::string::String::as_str)
            .collect();
        args.push(code);
        let allowed: Vec<std::path::PathBuf> =
            cwd.map(|d| vec![d.to_path_buf()]).unwrap_or_default();
        let allowed_refs: Vec<&std::path::Path> =
            allowed.iter().map(std::path::PathBuf::as_path).collect();
        super::sandbox_seatbelt::execute_sandboxed(
            &runtime.command,
            &args,
            &allowed_refs,
            &env_pairs,
            timeout,
            cwd,
        )
    }
}

#[cfg(target_os = "linux")]
fn landlock_execute(
    runtime: &RuntimeConfig,
    code: &str,
    timeout: u64,
    cwd: Option<&std::path::Path>,
) -> Result<(String, String, i32), String> {
    let tmp_dir = std::env::temp_dir().join("lean-ctx-sandbox");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let env_pairs: Vec<(String, String)> = runtime
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if runtime.needs_temp_file {
        let suffix = format!(".{}", runtime.file_extension);
        let tmp = tempfile::Builder::new()
            .prefix("exec_")
            .suffix(&suffix)
            .tempfile_in(&tmp_dir)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let file_path = tmp.into_temp_path();
        std::fs::write(&file_path, code).map_err(|e| format!("Failed to write temp file: {e}"))?;

        let mut allowed = vec![file_path.to_path_buf()];
        if let Some(dir) = cwd {
            allowed.push(dir.to_path_buf());
        }
        let allowed_refs: Vec<&std::path::Path> =
            allowed.iter().map(std::path::PathBuf::as_path).collect();
        let file_str = file_path.to_string_lossy().to_string();

        let mut args: Vec<&str> = runtime
            .args
            .iter()
            .map(std::string::String::as_str)
            .collect();
        args.push(&file_str);

        let result = super::sandbox_landlock::execute_sandboxed(
            &runtime.command,
            &args,
            &allowed_refs,
            &env_pairs,
            timeout,
            cwd,
        );
        let _ = std::fs::remove_file(&file_path);
        result
    } else {
        let mut args: Vec<&str> = runtime
            .args
            .iter()
            .map(std::string::String::as_str)
            .collect();
        args.push(code);
        let allowed: Vec<std::path::PathBuf> =
            cwd.map(|d| vec![d.to_path_buf()]).unwrap_or_default();
        let allowed_refs: Vec<&std::path::Path> =
            allowed.iter().map(std::path::PathBuf::as_path).collect();
        super::sandbox_landlock::execute_sandboxed(
            &runtime.command,
            &args,
            &allowed_refs,
            &env_pairs,
            timeout,
            cwd,
        )
    }
}

const SANDBOX_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SYSTEMROOT",
    "WINDIR",
];

fn apply_sandbox_env(cmd: &mut Command, runtime: &RuntimeConfig) {
    cmd.env_clear();
    for key in SANDBOX_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
    for (k, v) in &runtime.env {
        cmd.env(k, v);
    }
    cmd.env("LEAN_CTX_SANDBOX", "1");
}

/// Run the child in `cwd` when the caller resolved one (GH #1666).
///
/// A directory that no longer exists would make `spawn` fail with a message
/// about the *command*, which reads as a missing interpreter. Falling back to
/// the inherited directory is not an option either — running somewhere other
/// than the caller asked, without saying so, is the bug this exists to fix —
/// so an unusable directory is reported as itself.
fn apply_cwd(cmd: &mut Command, cwd: Option<&std::path::Path>) -> Result<(), String> {
    let Some(dir) = cwd else { return Ok(()) };
    if !dir.is_dir() {
        return Err(format!(
            "working directory does not exist: {}",
            dir.display()
        ));
    }
    cmd.current_dir(dir);
    Ok(())
}

fn execute_with_stdin(
    runtime: &RuntimeConfig,
    code: &str,
    timeout: u64,
    cwd: Option<&std::path::Path>,
) -> Result<(String, String, i32), String> {
    let mut cmd = Command::new(&runtime.command);
    apply_cwd(&mut cmd, cwd)?;
    for arg in &runtime.args {
        cmd.arg(arg);
    }
    cmd.arg(code);
    apply_sandbox_env(&mut cmd, runtime);
    // GH #1347: isolate child into its own process group so an interactive
    // shell (bash -ic) cannot SIGTSTP the MCP server, and close stdin to
    // prevent terminal job-control attempts.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {e}", runtime.command))?;

    let output = wait_with_timeout(child, timeout)?;
    Ok((
        crate::shell::decode_output(&output.stdout),
        crate::shell::decode_output(&output.stderr),
        output.status.code().unwrap_or(1),
    ))
}

fn execute_with_file(
    runtime: &RuntimeConfig,
    code: &str,
    timeout: u64,
    cwd: Option<&std::path::Path>,
) -> Result<(String, String, i32), String> {
    let tmp_dir = std::env::temp_dir().join("lean-ctx-sandbox");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let suffix = format!(".{}", runtime.file_extension);
    let tmp = tempfile::Builder::new()
        .prefix("exec_")
        .suffix(&suffix)
        .tempfile_in(&tmp_dir)
        .map_err(|e| format!("Failed to create temp file: {e}"))?;
    let file_path = tmp.into_temp_path();

    std::fs::write(&file_path, code).map_err(|e| format!("Failed to write temp file: {e}"))?;

    let result = if runtime.command == "rustc_script" {
        execute_rust(&file_path, timeout, cwd)
    } else {
        let mut cmd = Command::new(&runtime.command);
        apply_cwd(&mut cmd, cwd)?;
        for arg in &runtime.args {
            cmd.arg(arg);
        }
        cmd.arg(&file_path);
        apply_sandbox_env(&mut cmd, runtime);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {e}", runtime.command))?;
        let output = wait_with_timeout(child, timeout)?;
        Ok((
            crate::shell::decode_output(&output.stdout),
            crate::shell::decode_output(&output.stderr),
            output.status.code().unwrap_or(1),
        ))
    };

    let _ = std::fs::remove_file(&file_path);
    result
}

fn execute_rust(
    source_path: &std::path::Path,
    timeout: u64,
    cwd: Option<&std::path::Path>,
) -> Result<(String, String, i32), String> {
    let binary_path = source_path.with_extension("");

    let mut compile_cmd = Command::new("rustc");
    compile_cmd.arg(source_path).arg("-o").arg(&binary_path);
    compile_cmd.env_clear();
    for key in SANDBOX_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(key) {
            compile_cmd.env(key, val);
        }
    }
    compile_cmd.env("LEAN_CTX_SANDBOX", "1");

    let compile = compile_cmd
        .output()
        .map_err(|e| format!("rustc not found: {e}"))?;

    if !compile.status.success() {
        let stderr = crate::shell::decode_output(&compile.stderr);
        let _ = std::fs::remove_file(&binary_path);
        return Ok((String::new(), stderr, compile.status.code().unwrap_or(1)));
    }

    let mut run_cmd = Command::new(&binary_path);
    apply_cwd(&mut run_cmd, cwd)?;
    run_cmd.env_clear();
    for key in SANDBOX_ENV_ALLOWLIST {
        if let Ok(val) = std::env::var(key) {
            run_cmd.env(key, val);
        }
    }
    run_cmd.env("LEAN_CTX_SANDBOX", "1");
    run_cmd.stdin(std::process::Stdio::null());
    run_cmd.stdout(std::process::Stdio::piped());
    run_cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    run_cmd.process_group(0);

    let child = run_cmd
        .spawn()
        .map_err(|e| format!("Failed to run compiled binary: {e}"))?;

    let output = wait_with_timeout(child, timeout)?;
    let _ = std::fs::remove_file(&binary_path);

    Ok((
        crate::shell::decode_output(&output.stdout),
        crate::shell::decode_output(&output.stderr),
        output.status.code().unwrap_or(1),
    ))
}

fn wait_with_timeout(
    child: std::process::Child,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let mut child = child;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    kill_process_tree(&mut child);
                    // GH #1504: after killing, drain whatever the child wrote
                    // before the timeout so partial output is preserved instead
                    // of being silently discarded.
                    if let Ok(mut output) = child.wait_with_output() {
                        output.stderr.extend_from_slice(
                            format!("\n[lean-ctx] Execution timed out after {timeout_secs}s")
                                .as_bytes(),
                        );
                        return Ok(output);
                    }
                    return Err(format!("Execution timed out after {timeout_secs}s"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Kill a child process and its entire process group (Unix) or just the child
/// (non-Unix). Uses SIGKILL on the negative PID to hit the whole group created
/// by `process_group(0)`.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: libc::kill with negative pid targets the process group.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn find_binary(candidates: &[&str]) -> Option<String> {
    for name in candidates {
        if which_exists(name) {
            return Some(name.to_string());
        }
    }
    None
}

fn which_exists(name: &str) -> bool {
    #[cfg(target_os = "windows")]
    let check_cmd = Command::new("where")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    #[cfg(not(target_os = "windows"))]
    let check_cmd = Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    check_cmd.is_ok_and(|s| s.success())
}

fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    truncate_smart(output, MAX_OUTPUT_BYTES)
}

fn truncate_smart(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }

    let lines: Vec<&str> = output.lines().collect();
    let total_lines = lines.len();

    let head_count = (total_lines * 60) / 100;
    let tail_count = total_lines - head_count;

    let head: Vec<&str> = lines.iter().take(head_count).copied().collect();
    let tail: Vec<&str> = lines
        .iter()
        .skip(total_lines - tail_count)
        .copied()
        .collect();

    let head_text = head.join("\n");
    let tail_text = tail.join("\n");

    if head_text.len() + tail_text.len() + 100 > max_bytes {
        let half = max_bytes / 2;
        let h = &output[..output.floor_char_boundary(half.min(output.len()))];
        let t_start = output.ceil_char_boundary(output.len().saturating_sub(half));
        let t = &output[t_start..];
        let skipped = output.len() - h.len() - t.len();
        return format!("{h}\n\n... [{skipped} bytes truncated — showing head + tail] ...\n\n{t}");
    }

    let skipped_lines = total_lines - head_count - tail_count;
    let skipped_bytes = output.len() - head_text.len() - tail_text.len();
    format!(
        "{head_text}\n\n... [{skipped_lines} lines / {skipped_bytes} bytes truncated — showing first {head_count} + last {tail_count} lines] ...\n\n{tail_text}"
    )
}

pub fn supported_languages() -> &'static [&'static str] {
    &[
        "javascript",
        "typescript",
        "python",
        "shell",
        "ruby",
        "go",
        "rust",
        "php",
        "perl",
        "r",
        "elixir",
    ]
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn python_available() -> bool {
        find_binary(&["python3", "python"]).is_some()
    }

    #[test]
    fn execute_python_hello() {
        if !python_available() {
            return;
        }
        let result = execute("python", "print('hello sandbox')", None);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello sandbox"));
    }

    /// GH #1666: a snippet must run where the caller said, because relative
    /// paths inside it are resolved there. The `cwd: None` half of this test is
    /// the pre-fix behaviour — it is what made a script edit the project root
    /// while the caller was working in a worktree, and report success.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn execute_in_resolves_relative_paths_against_the_given_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("only-here.txt"), "x").expect("write marker");

        let scoped = execute_in("shell", "cat only-here.txt", None, Some(dir.path()));
        assert_eq!(scoped.exit_code, 0, "stderr: {}", scoped.stderr);
        assert!(scoped.stdout.contains('x'), "{scoped:?}");

        // Without a cwd the same relative path resolves somewhere else, which
        // is exactly what the reported bug did silently.
        let unscoped = execute_in("shell", "cat only-here.txt", None, None);
        assert_ne!(
            unscoped.exit_code, 0,
            "precondition: the marker is only reachable from the given directory"
        );
    }

    /// A directory that cannot be entered is reported as itself. Falling back
    /// to the inherited directory would reinstate the silent wrong-tree run.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn execute_in_reports_a_missing_directory_instead_of_falling_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");

        let result = execute_in("shell", "pwd", None, Some(&missing));
        assert_ne!(result.exit_code, 0, "{result:?}");
        assert!(
            result.stderr.contains("working directory does not exist"),
            "{result:?}"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn execute_shell_echo() {
        let result = execute("shell", "echo 'test output'", None);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("test output"));
    }

    #[test]
    fn execute_unsupported_language() {
        let result = execute("brainfuck", "++++", None);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("Unsupported language"));
    }

    #[test]
    fn execute_rejects_oversized_code() {
        let huge = "a".repeat(MAX_CODE_BYTES + 1);
        let result = execute("python", &huge, None);
        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.contains("exceeds the"));
    }

    #[test]
    fn execute_python_error() {
        if !python_available() {
            return;
        }
        let result = execute("python", "raise ValueError('test error')", None);
        assert_ne!(result.exit_code, 0);
        assert!(result.stderr.contains("ValueError"));
    }

    #[test]
    fn execute_with_timeout() {
        if !python_available() {
            return;
        }
        let result = execute("python", "import time; time.sleep(60)", Some(1));
        assert_ne!(result.exit_code, 0);
        assert!(
            result.stderr.contains("timed out"),
            "stderr should mention timeout: {}",
            result.stderr
        );
    }

    /// GH #1504: partial stdout emitted before a timeout must be preserved.
    #[test]
    fn timeout_preserves_partial_output() {
        if !python_available() {
            return;
        }
        let result = execute(
            "python",
            "import sys; sys.stdout.write('PARTIAL_1504'); sys.stdout.flush(); import time; time.sleep(60)",
            Some(2),
        );
        assert_ne!(result.exit_code, 0);
        assert!(
            result.stdout.contains("PARTIAL_1504"),
            "partial stdout before timeout must be preserved, got: {:?}",
            result.stdout
        );
        assert!(
            result.stderr.contains("timed out"),
            "stderr should mention timeout: {}",
            result.stderr
        );
    }

    #[test]
    fn truncate_preserves_head_and_tail() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("line {i}: some content here"))
            .collect();
        let output = lines.join("\n");
        let truncated = truncate_smart(&output, 500);
        assert!(truncated.contains("line 0:"));
        assert!(truncated.contains("line 99:"));
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn supported_languages_list() {
        let langs = supported_languages();
        assert!(langs.contains(&"python"));
        assert!(langs.contains(&"javascript"));
        assert!(langs.contains(&"rust"));
        assert_eq!(langs.len(), 11);
    }

    #[test]
    fn sandbox_env_is_set() {
        if !python_available() {
            return;
        }
        let result = execute(
            "python",
            "import os; print(os.environ.get('LEAN_CTX_SANDBOX', 'missing'))",
            None,
        );
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains('1'));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn batch_execute_multiple() {
        let items = vec![
            ("python".to_string(), "print(1+1)".to_string()),
            ("shell".to_string(), "echo hello".to_string()),
        ];
        let results = batch_execute(&items);
        assert_eq!(results.len(), 2);
        assert!(results[0].stdout.contains('2'));
        assert!(results[1].stdout.contains("hello"));
    }
}
