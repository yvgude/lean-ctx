pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("test") {
        return Some(compress_test(trimmed));
    }
    if cmd.contains("lint") {
        return Some(compress_lint(trimmed));
    }
    if cmd.contains("check") || cmd.contains("compile") {
        return Some(compress_check(trimmed));
    }
    if cmd.contains("fmt") {
        return Some(compress_fmt(trimmed));
    }
    if cmd.contains("task") || cmd.contains("run") {
        return Some(compact_lines(trimmed, 15));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut ignored = 0u32;
    let mut time = String::new();
    let mut failures = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ok |") || trimmed.starts_with("test result:") {
            for part in trimmed.split_whitespace() {
                if let Ok(n) = part.parse::<u32>() {
                    if trimmed.contains("passed") && passed == 0 {
                        passed = n;
                    } else if trimmed.contains("failed") && failed == 0 {
                        failed = n;
                    } else if trimmed.contains("ignored") && ignored == 0 {
                        ignored = n;
                    }
                }
            }
            if let Some(pos) = trimmed.rfind('(') {
                time = trimmed[pos..]
                    .trim_matches(|c: char| c == '(' || c == ')')
                    .to_string();
            }
        }
        if trimmed.starts_with("FAILED") || trimmed.starts_with("failures:") {
            failures.push(trimmed.to_string());
        }
        if trimmed.contains("... FAILED") {
            failures.push(trimmed.to_string());
        }
    }

    if passed == 0 && failed == 0 {
        return compact_lines(output, 10);
    }

    let mut result = format!("deno test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    if ignored > 0 {
        result.push_str(&format!(", {ignored} ignored"));
    }
    if !time.is_empty() {
        result.push_str(&format!(" ({time})"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  {f}"));
    }
    result
}

fn compress_lint(output: &str) -> String {
    let mut issues = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains('(') && (trimmed.contains("warning") || trimmed.contains("error")) {
            issues.push(trimmed.to_string());
        }
    }
    if issues.is_empty() {
        if output.contains("No problems found") || output.trim().is_empty() {
            return "clean".to_string();
        }
        return compact_lines(output, 10);
    }
    format!(
        "{} lint issues:\n{}",
        issues.len(),
        issues
            .iter()
            .take(10)
            .map(|i| format!("  {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn compress_check(output: &str) -> String {
    let errors: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("error") || l.contains("Error"))
        .collect();
    if errors.is_empty() {
        return "ok (type check passed)".to_string();
    }
    format!(
        "{} errors:\n{}",
        errors.len(),
        errors
            .iter()
            .take(10)
            .map(|e| format!("  {}", e.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn compress_fmt(output: &str) -> String {
    let files: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if files.is_empty() {
        return "ok (formatted)".to_string();
    }
    format!("{} files formatted", files.len())
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
