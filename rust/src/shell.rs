use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};

use crate::core::patterns;
use crate::core::stats;
use crate::core::tokens::count_tokens;

pub fn exec(command: &str) -> i32 {
    let real_shell = detect_shell();

    let child = Command::new(&real_shell)
        .arg("-c")
        .arg(command)
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

    exit_code
}

pub fn interactive() {
    let real_shell = detect_shell();

    eprintln!("lean-ctx shell v1.3.1 (wrapping {real_shell})");
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

fn find_real_shell() -> String {
    for shell in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell.to_string();
        }
    }
    "/bin/sh".to_string()
}
