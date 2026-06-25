#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("--build") || cmd.contains("make") {
        return Some(compress_build(trimmed));
    }
    if cmd.contains("ctest") || cmd.contains("test") {
        return Some(compress_test(trimmed));
    }

    Some(compress_configure(trimmed))
}

fn compress_configure(output: &str) -> String {
    let mut found_generators = Vec::new();
    let mut found_compilers = Vec::new();
    let mut warnings = 0u32;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("-- The") && trimmed.contains("compiler") {
            found_compilers.push(trimmed.to_string());
        }
        if trimmed.starts_with("-- Generating")
            || (trimmed.starts_with("--") && trimmed.contains("generator"))
        {
            found_generators.push(trimmed.to_string());
        }
        if trimmed.contains("CMake Warning") || trimmed.starts_with("WARNING:") {
            warnings += 1;
        }
        if trimmed.contains("CMake Error") || trimmed.starts_with("ERROR:") {
            errors.push(trimmed.to_string());
        }
    }

    if !errors.is_empty() {
        let mut result = format!("{} errors:", errors.len());
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {e}"));
        }
        return result;
    }

    let success =
        output.contains("Configuring done") || output.contains("Build files have been written");
    let mut result = if success {
        "CMake configured ok".to_string()
    } else {
        "CMake configure:".to_string()
    };
    if warnings > 0 {
        result.push_str(&format!(" ({warnings} warnings)"));
    }
    if !found_compilers.is_empty() {
        result.push_str(&format!("\n  compilers: {}", found_compilers.len()));
    }
    result
}

fn compress_build(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let errors: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("error:") || l.contains("Error:"))
        .collect();
    let warnings: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("warning:") || l.contains("Warning:"))
        .collect();

    let progress_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| l.trim().starts_with('[') && l.contains(']'))
        .collect();

    if !errors.is_empty() {
        let mut result = format!("{} errors", errors.len());
        if !warnings.is_empty() {
            result.push_str(&format!(", {} warnings", warnings.len()));
        }
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {}", e.trim()));
        }
        return result;
    }

    let mut result = format!("Build ok ({} steps", progress_lines.len().max(lines.len()));
    if !warnings.is_empty() {
        result.push_str(&format!(", {} warnings", warnings.len()));
    }
    result.push(')');
    result
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Passed") {
            for word in trimmed.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    passed = n;
                    break;
                }
            }
        }
        if trimmed.contains("Failed") && !trimmed.starts_with("The following") {
            for word in trimmed.split_whitespace() {
                if let Ok(n) = word.parse::<u32>() {
                    failed = n;
                    break;
                }
            }
        }
        if trimmed.starts_with("Failed")
            || (trimmed.contains("***Failed") || trimmed.contains("***Exception"))
        {
            failures.push(trimmed.to_string());
        }
    }

    let summary_line = output
        .lines()
        .rev()
        .find(|l| l.contains("tests passed") || l.contains("% tests passed"));
    if let Some(summary) = summary_line {
        let mut result = format!("ctest: {}", summary.trim());
        for f in failures.iter().take(5) {
            result.push_str(&format!("\n  {f}"));
        }
        return result;
    }

    if passed == 0 && failed == 0 {
        return compact_lines(output, 10);
    }

    let mut result = format!("ctest: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  {f}"));
    }
    result
}

fn compact_lines(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
