use std::collections::HashMap;

macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn diag_re() -> &'static regex::Regex {
    static_regex!(r"^(.+?):(\d+):(\d+):\s+(warning|error|note|fatal error):\s+(.+)")
}

fn include_stack_re() -> &'static regex::Regex {
    static_regex!(r"^In file included from .+:\d+:")
}

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("clang: ok".to_string());
    }

    if command.contains("--version") || command.contains("-v") {
        return Some(compress_version(trimmed));
    }

    Some(compress_diagnostics(trimmed))
}

fn compress_version(output: &str) -> String {
    let first_line = output.lines().next().unwrap_or("clang");
    first_line.trim().to_string()
}

fn compress_diagnostics(output: &str) -> String {
    let mut errors = Vec::new();
    let mut warning_groups: HashMap<String, Vec<String>> = HashMap::new();
    let mut notes = Vec::new();
    let mut include_stack_depth = 0u32;
    let mut generated_warnings = 0u32;
    let mut generated_errors = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();

        if include_stack_re().is_match(trimmed) {
            include_stack_depth += 1;
            continue;
        }

        if let Some(caps) = diag_re().captures(trimmed) {
            let file = &caps[1];
            let severity = &caps[4];
            let message = &caps[5];

            match severity {
                "error" | "fatal error" => {
                    generated_errors += 1;
                    if errors.len() < 20 {
                        errors.push(format!("{file}: {message}"));
                    }
                }
                "warning" => {
                    generated_warnings += 1;
                    let key = normalize_diagnostic(message);
                    let locations = warning_groups.entry(key).or_default();
                    if locations.len() < 3 {
                        locations.push(file.to_string());
                    }
                }
                "note" if notes.len() < 5 => {
                    notes.push(format!("{file}: {message}"));
                }
                _ => {}
            }
            continue;
        }

        if trimmed.contains("error generated")
            || trimmed.contains("errors generated")
            || trimmed.contains("warning generated")
            || trimmed.contains("warnings generated")
        {}
    }

    if errors.is_empty() && warning_groups.is_empty() {
        return "clang: ok".to_string();
    }

    let mut parts = Vec::new();

    if !errors.is_empty() {
        parts.push(format!("{generated_errors} errors:"));
        for e in errors.iter().take(10) {
            parts.push(format!("  {e}"));
        }
        if errors.len() > 10 {
            parts.push(format!("  ... +{} more", errors.len() - 10));
        }
    }

    if !warning_groups.is_empty() {
        parts.push(format!(
            "{generated_warnings} warnings ({} unique):",
            warning_groups.len()
        ));
        let mut sorted: Vec<_> = warning_groups.iter().collect();
        sorted.sort_by_key(|(_, locs)| std::cmp::Reverse(locs.len()));
        for (msg, locs) in sorted.iter().take(10) {
            let loc_str = if locs.len() <= 2 {
                locs.join(", ")
            } else {
                format!("{}, {} (+{} more)", locs[0], locs[1], locs.len() - 2)
            };
            parts.push(format!("  {msg} [{loc_str}]"));
        }
        if sorted.len() > 10 {
            parts.push(format!("  ... +{} more warning types", sorted.len() - 10));
        }
    }

    if include_stack_depth > 0 {
        parts.push(format!(
            "({include_stack_depth} include-stack lines collapsed)"
        ));
    }

    if !notes.is_empty() && errors.is_empty() {
        parts.push(format!("{} notes (first 5):", notes.len()));
        for n in &notes {
            parts.push(format!("  {n}"));
        }
    }

    parts.join("\n")
}

fn normalize_diagnostic(msg: &str) -> String {
    let cleaned = msg
        .trim_end_matches(" [-Wunused-variable]")
        .trim_end_matches(" [-Wunused-parameter]")
        .trim_end_matches(" [-Wunused-function]");

    let bracket_re = static_regex!(r"\s*\[-W[^\]]+\]$");
    let result = bracket_re.replace(cleaned, "");

    let quote_re = static_regex!(r"'[^']*'");
    quote_re.replace_all(&result, "'…'").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_errors() {
        let output = "src/main.c:10:5: error: use of undeclared identifier 'foo'\nsrc/main.c:20:5: error: expected ';'\n2 errors generated.\n";
        let result = compress("clang src/main.c", output).unwrap();
        assert!(result.contains("2 errors"), "should count errors");
        assert!(
            result.contains("undeclared identifier"),
            "should keep error msg"
        );
    }

    #[test]
    fn deduplicates_warnings() {
        let output = "a.c:1:5: warning: unused variable 'x' [-Wunused-variable]\nb.c:2:5: warning: unused variable 'y' [-Wunused-variable]\nc.c:3:5: warning: unused variable 'z' [-Wunused-variable]\n3 warnings generated.\n";
        let result = compress("clang -Wall a.c b.c c.c", output).unwrap();
        assert!(result.contains("3 warnings"), "should count total warnings");
        assert!(result.contains("1 unique"), "should deduplicate");
    }

    #[test]
    fn collapses_include_stacks() {
        let output = "In file included from main.c:1:\nIn file included from header.h:5:\nlib.h:10:5: warning: unused function 'helper' [-Wunused-function]\n1 warning generated.\n";
        let result = compress("clang main.c", output).unwrap();
        assert!(
            result.contains("include-stack lines collapsed"),
            "should report collapsed includes"
        );
    }

    #[test]
    fn clean_output() {
        let result = compress("clang -o app main.c", "").unwrap();
        assert_eq!(result, "clang: ok");
    }

    #[test]
    fn version_output() {
        let output = "clang version 17.0.6\nTarget: x86_64-pc-linux-gnu\nThread model: posix\n";
        let result = compress("clang --version", output).unwrap();
        assert!(result.contains("clang version 17.0.6"));
        assert!(
            !result.contains("Thread model"),
            "should only keep first line"
        );
    }
}
