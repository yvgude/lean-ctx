use crate::core::sandbox::{self, SandboxResult};
use crate::core::tokens::count_tokens;

/// Executes a code snippet in a sandboxed environment and returns formatted output.
pub fn handle(language: &str, code: &str, intent: Option<&str>, timeout: Option<u64>) -> String {
    let result = sandbox::execute(language, code, timeout);
    format_result(&result, intent)
}

/// Reads a file from disk, detects its language, and executes a processing script.
pub fn handle_file(path: &str, intent: Option<&str>) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("Error reading {path}: {e}"),
    };

    let language = detect_language_from_extension(path);
    let code = build_file_processing_script(&language, &content, intent);
    let result = sandbox::execute(&language, &code, None);
    format_result(&result, intent)
}

/// Executes multiple (language, code) pairs in parallel and returns aggregated results.
pub fn handle_batch(items: &[(String, String)]) -> String {
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
    output.join("\n")
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

            if let Some(intent_desc) = intent {
                if raw_tokens > 50 {
                    parts.push(format!("[intent: {intent_desc}]"));
                }
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

fn build_file_processing_script(language: &str, content: &str, intent: Option<&str>) -> String {
    let escaped = content.replace('\\', "\\\\").replace('\'', "\\'");
    let intent_str = intent.unwrap_or("summarize the content");

    match language {
        "python" => {
            format!(
                r#"
import json, re
from collections import Counter

data = '''{escaped}'''

lines = data.strip().split('\n')
total_lines = len(lines)
total_bytes = len(data.encode('utf-8'))

word_count = sum(len(line.split()) for line in lines)

print(f"{{total_lines}} lines, {{total_bytes}} bytes, {{word_count}} words")
print(f"Intent: {intent_str}")

if total_lines > 10:
    print(f"First 3: {{lines[:3]}}")
    print(f"Last 3: {{lines[-3:]}}")
"#
            )
        }
        _ => {
            format!(
                r#"
data='{escaped}'
lines=$(echo "$data" | wc -l | tr -d ' ')
bytes=$(echo "$data" | wc -c | tr -d ' ')
echo "$lines lines, $bytes bytes"
echo "Intent: {intent_str}"
echo "$data" | head -3
echo "..."
echo "$data" | tail -3
"#
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_simple_python() {
        let result = handle("python", "print(2 + 2)", None, None);
        assert!(result.contains('4'));
        assert!(result.contains("python"));
    }

    #[test]
    fn handle_with_intent() {
        let result = handle(
            "python",
            "print('found 5 errors')",
            Some("count errors"),
            None,
        );
        assert!(result.contains("found 5 errors"));
    }

    #[test]
    fn handle_error_shows_stderr() {
        let result = handle("python", "raise Exception('boom')", None, None);
        assert!(result.contains("EXIT"));
        assert!(result.contains("boom"));
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
    #[cfg(not(target_os = "windows"))]
    fn batch_multiple_tasks() {
        let items = vec![
            ("python".to_string(), "print('task1')".to_string()),
            ("shell".to_string(), "echo task2".to_string()),
        ];
        let result = handle_batch(&items);
        assert!(result.contains("task1"));
        assert!(result.contains("task2"));
        assert!(result.contains("2 tasks"));
    }
}
