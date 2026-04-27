use super::model::{GotchaCategory, GotchaSeverity};

// ---------------------------------------------------------------------------
// Error pattern detection
// ---------------------------------------------------------------------------

pub struct DetectedError {
    pub category: GotchaCategory,
    pub severity: GotchaSeverity,
    pub raw_message: String,
}

pub fn detect_error_pattern(output: &str, command: &str, exit_code: i32) -> Option<DetectedError> {
    let cmd_lower = command.to_lowercase();
    let out_lower = output.to_lowercase();

    // Rust / Cargo
    if cmd_lower.starts_with("cargo ") || cmd_lower.contains("rustc") {
        if let Some(msg) = extract_pattern(output, r"error\[E\d{4}\]: .+") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: msg,
            });
        }
        if out_lower.contains("cannot find") || out_lower.contains("mismatched types") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: extract_first_error_line(output),
            });
        }
        if out_lower.contains("test result: failed") || out_lower.contains("failures:") {
            return Some(DetectedError {
                category: GotchaCategory::Test,
                severity: GotchaSeverity::Critical,
                raw_message: extract_first_error_line(output),
            });
        }
    }

    // npm / pnpm / yarn
    if (cmd_lower.starts_with("npm ")
        || cmd_lower.starts_with("pnpm ")
        || cmd_lower.starts_with("yarn "))
        && (out_lower.contains("err!") || out_lower.contains("eresolve"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Dependency,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Node.js
    if cmd_lower.starts_with("node ") || cmd_lower.contains("tsx ") || cmd_lower.contains("ts-node")
    {
        for pat in &[
            "syntaxerror",
            "typeerror",
            "referenceerror",
            "cannot find module",
        ] {
            if out_lower.contains(pat) {
                return Some(DetectedError {
                    category: GotchaCategory::Runtime,
                    severity: GotchaSeverity::Critical,
                    raw_message: extract_first_error_line(output),
                });
            }
        }
    }

    // Python
    if (cmd_lower.starts_with("python")
        || cmd_lower.starts_with("pip ")
        || cmd_lower.starts_with("uv "))
        && (out_lower.contains("traceback")
            || out_lower.contains("importerror")
            || out_lower.contains("modulenotfounderror"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Runtime,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Go
    if cmd_lower.starts_with("go ")
        && (out_lower.contains("cannot use") || out_lower.contains("undefined:"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // TypeScript / tsc
    if cmd_lower.contains("tsc") || cmd_lower.contains("typescript") {
        if let Some(msg) = extract_pattern(output, r"TS\d{4}: .+") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: msg,
            });
        }
    }

    // Docker
    if cmd_lower.starts_with("docker ")
        && out_lower.contains("error")
        && (out_lower.contains("failed to") || out_lower.contains("copy failed"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Git
    if cmd_lower.starts_with("git ")
        && (out_lower.contains("conflict")
            || out_lower.contains("rejected")
            || out_lower.contains("diverged"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Config,
            severity: GotchaSeverity::Warning,
            raw_message: extract_first_error_line(output),
        });
    }

    // pytest
    if cmd_lower.contains("pytest") && (out_lower.contains("failed") || out_lower.contains("error"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Test,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Jest / Vitest
    if (cmd_lower.contains("jest") || cmd_lower.contains("vitest"))
        && (out_lower.contains("fail") || out_lower.contains("typeerror"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Test,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Make / CMake
    if (cmd_lower.starts_with("make") || cmd_lower.contains("cmake"))
        && out_lower.contains("error")
        && (out_lower.contains("undefined reference") || out_lower.contains("no rule"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Generic: non-zero exit + substantial stderr
    if exit_code != 0
        && output.len() > 50
        && (out_lower.contains("error")
            || out_lower.contains("fatal")
            || out_lower.contains("failed"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Runtime,
            severity: GotchaSeverity::Warning,
            raw_message: extract_first_error_line(output),
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Signature normalization
// ---------------------------------------------------------------------------

pub fn normalize_error_signature(raw: &str) -> String {
    let mut sig = raw.to_string();

    sig = regex_replace(&sig, r"(/[A-Za-z][\w.-]*/)+", "");
    sig = regex_replace(&sig, r"[A-Z]:\\[\w\\.-]+\\", "");
    sig = regex_replace(&sig, r":\d+:\d+", ":_:_");
    sig = regex_replace(&sig, r"line \d+", "line _");
    sig = regex_replace(&sig, r"\s+", " ");

    if sig.len() > 200 {
        sig.truncate(200);
    }

    sig.trim().to_string()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn command_base(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() >= 2 {
        format!("{} {}", parts[0], parts[1])
    } else {
        parts.first().unwrap_or(&"").to_string()
    }
}

fn extract_pattern(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}

fn extract_first_error_line(output: &str) -> String {
    for line in output.lines() {
        let ll = line.to_lowercase();
        if ll.contains("error") || ll.contains("failed") || ll.contains("traceback") {
            let trimmed = line.trim();
            if trimmed.len() > 200 {
                return trimmed[..200].to_string();
            }
            return trimmed.to_string();
        }
    }
    output.lines().next().unwrap_or("unknown error").to_string()
}

fn regex_replace(text: &str, pattern: &str, replacement: &str) -> String {
    match regex::Regex::new(pattern) {
        Ok(re) => re.replace_all(text, replacement).to_string(),
        Err(_) => text.to_string(),
    }
}
