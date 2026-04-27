macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn added_re() -> &'static regex::Regex {
    static_regex!(r"added (\d+) packages?")
}
fn time_re() -> &'static regex::Regex {
    static_regex!(r"in (\d+\.?\d*\s*[ms]+)")
}
fn pkg_re() -> &'static regex::Regex {
    static_regex!(r"\+ (\S+)@(\S+)")
}
fn vuln_re() -> &'static regex::Regex {
    static_regex!(r"(\d+)\s+(critical|high|moderate|low)")
}
fn outdated_re() -> &'static regex::Regex {
    static_regex!(r"^(\S+)\s+(\S+)\s+(\S+)\s+(\S+)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("install") || command.contains("add") || command.contains("ci") {
        return Some(compress_install(output));
    }
    if command.contains("run") {
        return Some(compress_run(output));
    }
    if command.contains("test") {
        return Some(compress_test(output));
    }
    if command.contains("audit") {
        return Some(compress_audit(output));
    }
    if command.contains("outdated") {
        return Some(compress_outdated(output));
    }
    if command.contains("list") || command.contains("ls") {
        return Some(compress_list(output));
    }
    None
}

fn compress_install(output: &str) -> String {
    let mut packages = Vec::new();
    let mut dep_count = 0u32;
    let mut time = String::new();

    for line in output.lines() {
        if let Some(caps) = pkg_re().captures(line) {
            packages.push(format!("{}@{}", &caps[1], &caps[2]));
        }
        if let Some(caps) = added_re().captures(line) {
            dep_count = caps[1].parse().unwrap_or(0);
        }
        if let Some(caps) = time_re().captures(line) {
            time = caps[1].to_string();
        }
    }

    let pkg_str = if packages.is_empty() {
        String::new()
    } else {
        format!("+{}", packages.join(", +"))
    };

    let dep_str = if dep_count > 0 {
        format!(" ({dep_count} deps")
    } else {
        " (".to_string()
    };

    let time_str = if time.is_empty() {
        ")".to_string()
    } else {
        format!(", {time})")
    };

    if pkg_str.is_empty() && dep_count > 0 {
        format!(
            "ok ({dep_count} deps{}",
            if time.is_empty() {
                ")".to_string()
            } else {
                format!(", {time})")
            }
        )
    } else {
        format!("{pkg_str}{dep_str}{time_str}")
    }
}

fn compress_run(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with('>')
                && !t.starts_with("npm warn")
                && !t.contains("npm fund")
                && !t.contains("looking for funding")
        })
        .collect();

    if lines.len() <= 15 {
        return lines.join("\n");
    }

    let last = lines.len().saturating_sub(10);
    format!("...({} lines)\n{}", lines.len(), lines[last..].join("\n"))
}

fn compress_test(output: &str) -> String {
    let jest_re = static_regex!(
        r"Tests:\s+(?:(\d+)\s+failed,?\s*)?(?:(\d+)\s+skipped,?\s*)?(?:(\d+)\s+passed,?\s*)?(\d+)\s+total"
    );
    let vitest_re = static_regex!(
        r"Test Files\s+(?:(\d+)\s+failed\s*\|?\s*)?(?:(\d+)\s+passed\s*\|?\s*)?(\d+)\s+total"
    );
    let mocha_re = static_regex!(r"(\d+)\s+passing.*\n\s*(?:(\d+)\s+failing)?");
    let test_line_re = static_regex!(r"^\s*(✓|✗|✘|×|PASS|FAIL|ok|not ok)\s");

    for line in output.lines() {
        if let Some(caps) = jest_re.captures(line) {
            let failed: u32 = caps
                .get(1)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let skipped: u32 = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let passed: u32 = caps
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let total: u32 = caps
                .get(4)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            return format!("tests: {passed} pass, {failed} fail, {skipped} skip ({total} total)");
        }
        if let Some(caps) = vitest_re.captures(line) {
            let failed: u32 = caps
                .get(1)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let passed: u32 = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let total: u32 = caps
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            return format!("tests: {passed} pass, {failed} fail ({total} total)");
        }
    }

    if let Some(caps) = mocha_re.captures(output) {
        let passed: u32 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let failed: u32 = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        return format!("tests: {passed} pass, {failed} fail");
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    for line in output.lines() {
        let trimmed = line.trim();
        if test_line_re.is_match(trimmed) {
            let low = trimmed.to_lowercase();
            if low.starts_with("✓") || low.starts_with("pass") || low.starts_with("ok ") {
                passed += 1;
            } else {
                failed += 1;
            }
        }
    }

    if passed > 0 || failed > 0 {
        return format!("tests: {passed} pass, {failed} fail");
    }

    compact_output(output, 10)
}

fn compress_audit(output: &str) -> String {
    let mut severities = std::collections::HashMap::new();
    let mut total_vulns = 0u32;
    let mut detail_lines: Vec<String> = Vec::new();

    for line in output.lines() {
        if let Some(caps) = vuln_re().captures(line) {
            let count: u32 = caps[1].parse().unwrap_or(0);
            let severity = caps[2].to_string();
            *severities.entry(severity).or_insert(0u32) += count;
            total_vulns += count;
        }

        let lower = line.to_ascii_lowercase();
        let is_detail = lower.contains("cve-")
            || lower.contains("severity")
            || lower.contains("fix available")
            || lower.contains("package")
            || lower.contains("depends on vulnerable")
            || lower.contains("vulnerability")
            || lower.contains("moderate")
            || lower.contains("high")
            || lower.contains("critical");
        if is_detail && detail_lines.len() < 30 {
            detail_lines.push(line.to_string());
        }
    }

    if total_vulns == 0 {
        if output.to_lowercase().contains("no vulnerabilities") || output.trim().is_empty() {
            return "ok (0 vulnerabilities)".to_string();
        }
        return compact_output(output, 5);
    }

    let mut parts = Vec::new();
    for sev in &["critical", "high", "moderate", "low"] {
        if let Some(count) = severities.get(*sev) {
            parts.push(format!("{count} {sev}"));
        }
    }

    let summary = format!("{total_vulns} vulnerabilities: {}", parts.join(", "));
    if detail_lines.is_empty() {
        return summary;
    }

    format!("{summary}\n{}", detail_lines.join("\n"))
}

fn compress_outdated(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "all up-to-date".to_string();
    }

    let mut packages = Vec::new();
    for line in &lines[1..] {
        if let Some(caps) = outdated_re().captures(line) {
            let name = &caps[1];
            let current = &caps[2];
            let wanted = &caps[3];
            let latest = &caps[4];
            packages.push(format!("{name}: {current} → {latest} (wanted: {wanted})"));
        }
    }

    if packages.is_empty() {
        return "all up-to-date".to_string();
    }
    format!("{} outdated:\n{}", packages.len(), packages.join("\n"))
}

fn compress_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 5 {
        return output.to_string();
    }

    let top_level: Vec<&str> = lines
        .iter()
        .filter(|l| {
            l.starts_with("├──")
                || l.starts_with("└──")
                || l.starts_with("+--")
                || l.starts_with("`--")
        })
        .copied()
        .collect();

    if top_level.is_empty() {
        return compact_output(output, 10);
    }

    let cleaned: Vec<String> = top_level
        .iter()
        .map(|l| {
            l.replace("├──", "")
                .replace("└──", "")
                .replace("+--", "")
                .replace("`--", "")
                .trim()
                .to_string()
        })
        .collect();

    format!("{} packages:\n{}", cleaned.len(), cleaned.join("\n"))
}

fn compact_output(text: &str, max: usize) -> String {
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
