use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

use crate::core::config;
use crate::core::patterns;
use crate::core::stats;
use crate::core::tokens::count_tokens;

pub fn exec(command: &str) -> i32 {
    let (shell, shell_flag) = shell_and_flag();

    let child = Command::new(&shell)
        .arg(&shell_flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lean-ctx: failed to execute: {e}");
            return 127;
        }
    };

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("lean-ctx: failed to wait: {e}");
            return 127;
        }
    };

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let full_output = if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };

    let input_tokens = count_tokens(&full_output);
    let (compressed, output_tokens) = compress_and_measure(command, &stdout, &stderr);

    stats::record(command, input_tokens, output_tokens);

    if !compressed.is_empty() {
        let _ = io::stdout().write_all(compressed.as_bytes());
        if !compressed.ends_with('\n') {
            let _ = io::stdout().write_all(b"\n");
        }
    }

    let cfg = config::Config::load();
    if cfg.tee_on_error && exit_code != 0 && !full_output.trim().is_empty() {
        if let Some(path) = save_tee(command, &full_output) {
            eprintln!("[lean-ctx: output saved to {path} (secrets redacted, auto-deleted after 24h)]");
        }
    }

    exit_code
}

pub fn interactive() {
    let real_shell = detect_shell();

    eprintln!("lean-ctx shell v2.1.0 (wrapping {real_shell})");
    eprintln!("All command output is automatically compressed.");
    eprintln!("Type 'exit' to quit.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        let _ = write!(stdout, "lean-ctx> ");
        let _ = stdout.flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }

        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == "exit" || cmd == "quit" {
            break;
        }
        if cmd == "gain" {
            println!("{}", stats::format_gain());
            continue;
        }

        let exit_code = exec(cmd);

        if exit_code != 0 {
            let _ = writeln!(stdout, "[exit: {exit_code}]");
        }
    }
}

fn compress_and_measure(command: &str, stdout: &str, stderr: &str) -> (String, usize) {
    let compressed_stdout = compress_if_beneficial(command, stdout);
    let compressed_stderr = compress_if_beneficial(command, stderr);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    let output_tokens = count_tokens(&result);
    (result, output_tokens)
}

fn compress_if_beneficial(command: &str, output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    let original_tokens = count_tokens(output);

    if original_tokens < 50 {
        return output.to_string();
    }

    if let Some(compressed) = patterns::compress_output(command, output) {
        if !compressed.trim().is_empty() {
            let compressed_tokens = count_tokens(&compressed);
            if compressed_tokens < original_tokens {
                let saved = original_tokens - compressed_tokens;
                let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
                return format!("{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]");
            }
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30 {
        let first = &lines[..5];
        let last = &lines[lines.len() - 5..];
        let omitted = lines.len() - 10;
        let compressed = format!(
            "{}\n... ({omitted} lines omitted) ...\n{}",
            first.join("\n"),
            last.join("\n")
        );
        let compressed_tokens = count_tokens(&compressed);
        if compressed_tokens < original_tokens {
            let saved = original_tokens - compressed_tokens;
            let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
            return format!("{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]");
        }
    }

    output.to_string()
}

pub fn shell_and_flag() -> (String, String) {
    let shell = detect_shell();
    let flag = if cfg!(windows) {
        let name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("powershell") || name.contains("pwsh") {
            "-Command".to_string()
        } else {
            "/C".to_string()
        }
    } else {
        "-c".to_string()
    };
    (shell, flag)
}

fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("LEAN_CTX_SHELL") {
        return shell;
    }

    if let Ok(shell) = std::env::var("SHELL") {
        let bin = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sh");

        if bin == "lean-ctx" {
            return find_real_shell();
        }
        return shell;
    }

    find_real_shell()
}

#[cfg(unix)]
fn find_real_shell() -> String {
    for shell in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell.to_string();
        }
    }
    "/bin/sh".to_string()
}

#[cfg(windows)]
fn find_real_shell() -> String {
    if let Ok(comspec) = std::env::var("COMSPEC") {
        return comspec;
    }
    "cmd.exe".to_string()
}

fn save_tee(command: &str, output: &str) -> Option<String> {
    let tee_dir = dirs::home_dir()?.join(".lean-ctx").join("tee");
    std::fs::create_dir_all(&tee_dir).ok()?;

    cleanup_old_tee_logs(&tee_dir);

    let cmd_slug: String = command
        .chars()
        .take(40)
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let ts = chrono::Local::now().format("%Y-%m-%d_%H%M%S");
    let filename = format!("{ts}_{cmd_slug}.log");
    let path = tee_dir.join(&filename);

    let masked = mask_sensitive_data(output);
    std::fs::write(&path, masked).ok()?;
    Some(path.to_string_lossy().to_string())
}

fn mask_sensitive_data(input: &str) -> String {
    use regex::Regex;

    let patterns: Vec<(&str, Regex)> = vec![
        ("Bearer token", Regex::new(r"(?i)(bearer\s+)[a-zA-Z0-9\-_\.]{8,}").unwrap()),
        ("Authorization header", Regex::new(r"(?i)(authorization:\s*(?:basic|bearer|token)\s+)[^\s\r\n]+").unwrap()),
        ("API key param", Regex::new(r#"(?i)((?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|token|password|passwd|pwd|secret)\s*[=:]\s*)[^\s\r\n,;&"']+"#).unwrap()),
        ("AWS key", Regex::new(r"(AKIA[0-9A-Z]{12,})").unwrap()),
        ("Private key block", Regex::new(r"(?s)(-----BEGIN\s+(?:RSA\s+)?PRIVATE\s+KEY-----).+?(-----END\s+(?:RSA\s+)?PRIVATE\s+KEY-----)").unwrap()),
        ("GitHub token", Regex::new(r"(gh[pousr]_)[a-zA-Z0-9]{20,}").unwrap()),
        ("Generic long hex/base64 secret", Regex::new(r#"(?i)(?:key|token|secret|password|credential|auth)\s*[=:]\s*['"]?([a-zA-Z0-9+/=\-_]{32,})['"]?"#).unwrap()),
    ];

    let mut result = input.to_string();
    for (label, re) in &patterns {
        result = re.replace_all(&result, |caps: &regex::Captures| {
            if let Some(prefix) = caps.get(1) {
                format!("{}[REDACTED:{}]", prefix.as_str(), label)
            } else {
                format!("[REDACTED:{}]", label)
            }
        }).to_string();
    }
    result
}

fn cleanup_old_tee_logs(tee_dir: &std::path::Path) {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60));
    let cutoff = match cutoff {
        Some(t) => t,
        None => return,
    };

    if let Ok(entries) = std::fs::read_dir(tee_dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}
