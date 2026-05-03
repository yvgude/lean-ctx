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
    if cmd.contains("package resolve") || cmd.contains("package update") {
        return Some(compress_resolve(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut time = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Test Case") && trimmed.contains("passed") {
            passed += 1;
        } else if trimmed.contains("Test Case") && trimmed.contains("failed") {
            failed += 1;
            failures.push(trimmed.to_string());
        }
        if trimmed.starts_with("Test Suite") && trimmed.contains("Executed") {
            time = trimmed.to_string();
        }
        if trimmed.contains("Executed") && trimmed.contains("tests") {
            if let Some(pos) = trimmed.find("Executed") {
                time = trimmed[pos..].to_string();
            }
        }
    }

    if passed == 0 && failed == 0 {
        return compact_lines(output, 10);
    }

    let mut result = format!("swift test: {passed} passed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    if !time.is_empty() {
        result.push_str(&format!("\n  {time}"));
    }
    for f in failures.iter().take(5) {
        result.push_str(&format!("\n  FAIL: {f}"));
    }
    result
}

fn compress_build(output: &str) -> String {
    let mut compiling = 0u32;
    let mut linking = false;
    let mut errors = Vec::new();
    let mut warnings = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Compiling") || trimmed.contains('[') && trimmed.contains(']') {
            compiling += 1;
        }
        if trimmed.starts_with("Linking") || trimmed.contains("Linking") {
            linking = true;
        }
        if trimmed.contains("error:") {
            errors.push(trimmed.to_string());
        }
        if trimmed.contains("warning:") {
            warnings += 1;
        }
    }

    if !errors.is_empty() {
        let mut result = format!("{} errors", errors.len());
        if warnings > 0 {
            result.push_str(&format!(", {warnings} warnings"));
        }
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {e}"));
        }
        return result;
    }

    let mut result = format!("Build ok ({compiling} compiled");
    if linking {
        result.push_str(", linked");
    }
    if warnings > 0 {
        result.push_str(&format!(", {warnings} warnings"));
    }
    result.push(')');
    result
}

fn compress_resolve(output: &str) -> String {
    let mut fetched = 0u32;
    let mut resolved = 0u32;
    for line in output.lines() {
        if line.contains("Fetching") {
            fetched += 1;
        }
        if line.contains("Resolving") || line.contains("resolved") {
            resolved += 1;
        }
    }
    if fetched == 0 && resolved == 0 {
        return compact_lines(output, 5);
    }
    format!("{fetched} fetched, {resolved} resolved")
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
