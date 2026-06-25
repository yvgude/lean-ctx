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
    if cmd.contains("query") {
        return Some(compress_query(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("PASSED") {
            passed += 1;
        }
        if trimmed.contains("FAILED") {
            failed += 1;
            failures.push(trimmed.to_string());
        }
    }

    let summary = output
        .lines()
        .find(|l| l.contains("executed") || l.contains("test(s)"));

    if passed == 0 && failed == 0 {
        if let Some(s) = summary {
            return format!("bazel test: {}", s.trim());
        }
        return compact_lines(output, 10);
    }

    let mut result = format!("bazel test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  {f}"));
    }
    result
}

fn compress_build(output: &str) -> String {
    let mut targets = 0u32;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if (trimmed.contains("up-to-date") || trimmed.contains("Build completed"))
            && let Some(n) = trimmed
                .split_whitespace()
                .find_map(|w| w.parse::<u32>().ok())
        {
            targets = n;
        }
        if trimmed.starts_with("ERROR:") || trimmed.starts_with("error:") {
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

    let info_line = output
        .lines()
        .rev()
        .find(|l| l.contains("INFO: Build completed") || l.contains("up-to-date"));
    if let Some(info) = info_line {
        return info.trim().to_string();
    }

    format!("ok ({targets} targets)")
}

fn compress_query(output: &str) -> String {
    let targets: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if targets.len() <= 20 {
        return targets.join("\n");
    }
    format!(
        "{} targets:\n{}\n... ({} more)",
        targets.len(),
        targets[..15].join("\n"),
        targets.len() - 15
    )
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
