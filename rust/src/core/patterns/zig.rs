#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("test") {
        return Some(compress_test(trimmed));
    }
    if cmd.contains("build") {
        return Some(compress_build(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("1/1 test") || trimmed.contains("test passed") {
            passed += 1;
        }
        if trimmed.contains("FAIL") || trimmed.contains("test failed") {
            failed += 1;
            failures.push(trimmed.to_string());
        }
        if trimmed.contains("All")
            && trimmed.contains("passed")
            && let Some(n) = trimmed
                .split_whitespace()
                .nth(1)
                .and_then(|w| w.parse().ok())
        {
            passed = n;
        }
    }

    if passed == 0 && failed == 0 {
        return compact_lines(output, 10);
    }

    let mut result = format!("zig test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  {f}"));
    }
    result
}

fn compress_build(output: &str) -> String {
    let errors: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("error:") || l.contains("Error"))
        .collect();
    let warnings: Vec<&str> = output.lines().filter(|l| l.contains("warning:")).collect();

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

    if !warnings.is_empty() {
        return format!("ok ({} warnings)", warnings.len());
    }

    "ok".to_string()
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
