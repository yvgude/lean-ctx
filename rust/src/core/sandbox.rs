use std::collections::HashMap;
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

pub fn execute(language: &str, code: &str, timeout_secs: Option<u64>) -> SandboxResult {
    let timeout = timeout_secs.unwrap_or(TIMEOUT_SECS);
    let start = std::time::Instant::now();

    let Some(runtime) = resolve_runtime(language) else {
        return SandboxResult {
                stdout: String::new(),
                stderr: format!("Unsupported language: {language}. Supported: javascript, typescript, python, shell, ruby, go, rust, php, perl, r, elixir"),
                exit_code: 1,
                language: language.to_string(),
                duration_ms: 0,
            };
    };

    let result = if runtime.needs_temp_file {
        execute_with_file(&runtime, code, timeout)
    } else {
        execute_with_stdin(&runtime, code, timeout)
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

fn execute_with_stdin(
    runtime: &RuntimeConfig,
    code: &str,
    timeout: u64,
) -> Result<(String, String, i32), String> {
    let mut cmd = Command::new(&runtime.command);
    for arg in &runtime.args {
        cmd.arg(arg);
    }
    cmd.arg(code);

    for (k, v) in &runtime.env {
        cmd.env(k, v);
    }

    cmd.env("LEAN_CTX_SANDBOX", "1");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

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
) -> Result<(String, String, i32), String> {
    let tmp_dir = std::env::temp_dir().join("lean-ctx-sandbox");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let file_name = format!(
        "exec_{}.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        runtime.file_extension
    );
    let file_path = tmp_dir.join(&file_name);

    std::fs::write(&file_path, code).map_err(|e| format!("Failed to write temp file: {e}"))?;

    let result = if runtime.command == "rustc_script" {
        execute_rust(&file_path, timeout)
    } else {
        let mut cmd = Command::new(&runtime.command);
        for arg in &runtime.args {
            cmd.arg(arg);
        }
        cmd.arg(&file_path);
        for (k, v) in &runtime.env {
            cmd.env(k, v);
        }
        cmd.env("LEAN_CTX_SANDBOX", "1");
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

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
) -> Result<(String, String, i32), String> {
    let binary_path = source_path.with_extension("");

    let compile = Command::new("rustc")
        .arg(source_path)
        .arg("-o")
        .arg(&binary_path)
        .output()
        .map_err(|e| format!("rustc not found: {e}"))?;

    if !compile.status.success() {
        let stderr = crate::shell::decode_output(&compile.stderr);
        let _ = std::fs::remove_file(&binary_path);
        return Ok((String::new(), stderr, compile.status.code().unwrap_or(1)));
    }

    let child = Command::new(&binary_path)
        .env("LEAN_CTX_SANDBOX", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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
                    let _ = child.kill();
                    return Err(format!("Execution timed out after {timeout_secs}s"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
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
        let h = &output[..half.min(output.len())];
        let t_start = output.len().saturating_sub(half);
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
mod tests {
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
