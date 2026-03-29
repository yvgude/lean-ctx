use regex::Regex;
use std::sync::OnceLock;

static MYPY_ERROR_RE: OnceLock<Regex> = OnceLock::new();
static MYPY_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();

fn error_re() -> &'static Regex {
    MYPY_ERROR_RE.get_or_init(|| {
        Regex::new(r"^(.+?):(\d+):\s+(error|warning|note):\s+(.+?)(?:\s+\[(.+)\])?$").unwrap()
    })
}

fn summary_re() -> &'static Regex {
    MYPY_SUMMARY_RE.get_or_init(|| {
        Regex::new(r"Found (\d+) errors? in (\d+) files?").unwrap()
    })
}

pub fn compress(_command: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if trimmed == "Success: no issues found in source files"
        || trimmed.contains("no issues found")
    {
        return Some("clean".to_string());
    }

    let mut by_code: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut by_severity: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first_errors: Vec<String> = Vec::new();

    for line in trimmed.lines() {
        if let Some(caps) = error_re().captures(line) {
            let file = caps[1].to_string();
            let severity = caps[3].to_string();
            let msg = caps[4].to_string();
            let code = caps.get(5).map(|m| m.as_str().to_string());

            files.insert(file.clone());
            *by_severity.entry(severity).or_insert(0) += 1;

            if let Some(ref c) = code {
                *by_code.entry(c.clone()).or_insert(0) += 1;
            }

            if first_errors.len() < 5 {
                let short_file = file.rsplit('/').next().unwrap_or(&file);
                let line_num = &caps[2];
                let code_str = code.as_deref().unwrap_or("?");
                first_errors.push(format!("  {short_file}:{line_num} [{code_str}] {msg}"));
            }
        }
    }

    if let Some(caps) = summary_re().captures(trimmed) {
        let errors = &caps[1];
        let file_count = &caps[2];

        let mut parts = vec![format!("{errors} errors in {file_count} files")];

        if !by_code.is_empty() {
            let mut codes: Vec<(String, u32)> = by_code.into_iter().collect();
            codes.sort_by(|a, b| b.1.cmp(&a.1));
            for (code, count) in codes.iter().take(6) {
                parts.push(format!("  [{code}]: {count}"));
            }
            if codes.len() > 6 {
                parts.push(format!("  ... +{} more codes", codes.len() - 6));
            }
        }

        if !first_errors.is_empty() {
            parts.push("Top errors:".to_string());
            parts.extend(first_errors);
        }

        return Some(parts.join("\n"));
    }

    if !files.is_empty() {
        let total: u32 = by_severity.values().sum();
        let err_count = by_severity.get("error").copied().unwrap_or(0);
        let warn_count = by_severity.get("warning").copied().unwrap_or(0);

        let mut parts = vec![format!(
            "{total} issues in {} files ({err_count} errors, {warn_count} warnings)",
            files.len()
        )];

        if !first_errors.is_empty() {
            parts.extend(first_errors);
        }

        return Some(parts.join("\n"));
    }

    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= 8 {
        Some(trimmed.to_string())
    } else {
        Some(format!(
            "{}\n... ({} more lines)",
            lines[..8].join("\n"),
            lines.len() - 8
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mypy_clean_output() {
        let output = "Success: no issues found in source files";
        assert_eq!(compress("mypy .", output).unwrap(), "clean");
    }

    #[test]
    fn mypy_empty_output() {
        assert_eq!(compress("mypy .", "").unwrap(), "ok");
    }

    #[test]
    fn mypy_errors_with_summary() {
        let output = r#"src/auth.py:42: error: Argument 1 to "validate" has incompatible type "str"; expected "int"  [arg-type]
src/auth.py:55: error: Missing return statement  [return]
src/db.py:10: error: Name "cursor" is not defined  [name-defined]
Found 3 errors in 2 files (checked 15 source files)"#;
        let result = compress("mypy .", output).unwrap();
        assert!(result.contains("3 errors in 2 files"));
        assert!(result.contains("[arg-type]"));
    }

    #[test]
    fn mypy_errors_without_summary() {
        let output = r#"src/main.py:10: error: Incompatible return value type  [return-value]
src/main.py:20: warning: Unused "type: ignore" comment  [unused-ignore]"#;
        let result = compress("mypy src/", output).unwrap();
        assert!(result.contains("2 issues"));
        assert!(result.contains("1 errors"));
        assert!(result.contains("1 warnings"));
    }

    #[test]
    fn mypy_with_notes() {
        let output = "src/api.py:5: error: Missing type annotation  [no-untyped-def]\nsrc/api.py:5: note: Use --disallow-untyped-defs\nFound 1 error in 1 file (checked 3 source files)";
        let result = compress("mypy .", output).unwrap();
        assert!(result.contains("1 error"));
    }
}
