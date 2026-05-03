macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn built_artifact_re() -> &'static regex::Regex {
    static_regex!(r"^✓\s+Built\s+(.+?)(?:\s+\(([^)]+)\))?\s*\.?\s*$")
}
fn flutter_test_summary_re() -> &'static regex::Regex {
    static_regex!(r"^(\d{2}:\d{2}\s+\+\d+(?:\s+-\d+)?:\s+.+)$")
}
fn analyze_issues_re() -> &'static regex::Regex {
    static_regex!(r"(?i)(?:^Analyzing\s+.+\.\.\.\s*)?(\d+)\s+issues?\s+found")
}
fn analyze_issue_line_re() -> &'static regex::Regex {
    static_regex!(r"^\s*(error|warning|info)\s+•")
}
fn is_flutter_build_noise(line: &str) -> bool {
    let t = line.trim();
    let tl = t.to_ascii_lowercase();
    tl.starts_with("running gradle task")
        || tl.starts_with("running pod install")
        || tl.starts_with("building with sound null safety")
        || tl.starts_with("flutter build")
        || tl.contains("resolving dependencies")
        || (tl.contains("..") && tl.contains("ms") && tl.matches('%').count() >= 1)
        || tl.starts_with("warning: the flutter tool")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("flutter ") {
        let sub = cl.split_whitespace().nth(1).unwrap_or("");
        return match sub {
            "build" => Some(compress_flutter_build(output)),
            "test" => Some(compress_flutter_test(output)),
            "analyze" => Some(compress_analyze(output)),
            _ => None,
        };
    }
    if cl.starts_with("dart ") && cl.split_whitespace().nth(1) == Some("analyze") {
        return Some(compress_analyze(output));
    }
    None
}

fn compress_flutter_build(output: &str) -> String {
    let mut parts = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() {
            continue;
        }
        if is_flutter_build_noise(t) {
            continue;
        }
        let trim = t.trim();
        if let Some(caps) = built_artifact_re().captures(trim) {
            let path = caps[1].trim();
            let size = caps.get(2).map_or("", |m| m.as_str());
            if size.is_empty() {
                parts.push(format!("Built {path}"));
            } else {
                parts.push(format!("Built {path} ({size})"));
            }
            continue;
        }
        let tl = trim.to_ascii_lowercase();
        if tl.starts_with("error") || tl.contains(" error:") || tl.contains("compilation failed") {
            parts.push(trim.to_string());
        }
        if tl.starts_with("fail") && tl.contains("build") {
            parts.push(trim.to_string());
        }
    }

    if parts.is_empty() {
        compact_tail(output, 15)
    } else {
        parts.join("\n")
    }
}

fn compress_flutter_test(output: &str) -> String {
    let mut parts = Vec::new();
    let mut failures = Vec::new();

    for line in output.lines() {
        let trim = line.trim();
        if trim.is_empty() {
            continue;
        }
        if flutter_test_summary_re().is_match(trim) {
            parts.push(trim.to_string());
            continue;
        }
        let tl = trim.to_ascii_lowercase();
        if tl.contains("some tests failed")
            || tl == "failed."
            || tl.starts_with("test failed")
            || tl.contains("exception:") && tl.contains("test")
        {
            parts.push(trim.to_string());
        }
        if trim.starts_with("Expected:") || trim.starts_with("Actual:") {
            failures.push(trim.to_string());
        }
        if tl.contains("error:") && (tl.contains("test") || tl.contains("failed")) {
            parts.push(trim.to_string());
        }
    }

    if !failures.is_empty() {
        parts.push("assertion detail:".to_string());
        parts.extend(failures.into_iter().take(12).map(|l| format!("  {l}")));
    }

    if parts.is_empty() {
        compact_tail(output, 20)
    } else {
        parts.join("\n")
    }
}

fn compress_analyze(output: &str) -> String {
    let mut parts = Vec::new();
    let mut issues = Vec::new();
    let mut saw_header = false;

    for line in output.lines() {
        let trim = line.trim_end();
        if trim.trim().is_empty() {
            continue;
        }
        let t = trim.trim();
        let tl = t.to_ascii_lowercase();

        if tl.starts_with("analyzing ") {
            saw_header = true;
            parts.push(t.to_string());
            continue;
        }
        if tl.contains("no issues found") {
            parts.push(t.to_string());
            continue;
        }
        if let Some(caps) = analyze_issues_re().captures(t) {
            parts.push(format!("{} issues found", &caps[1]));
            continue;
        }
        if analyze_issue_line_re().is_match(t)
            || tl.starts_with("  error •")
            || tl.starts_with("  warning •")
            || tl.starts_with("  info •")
        {
            issues.push(t.to_string());
        }
    }

    if !issues.is_empty() {
        parts.push(format!("{} issue line(s):", issues.len()));
        for i in issues.into_iter().take(40) {
            parts.push(format!("  {i}"));
        }
    }

    if parts.is_empty() && saw_header {
        return "analyze (no summary matched)".to_string();
    }
    if parts.is_empty() {
        compact_tail(output, 25)
    } else {
        parts.join("\n")
    }
}

fn compact_tail(output: &str, max: usize) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return "ok".to_string();
    }
    if lines.len() <= max {
        return lines.join("\n");
    }
    let start = lines.len().saturating_sub(max);
    format!(
        "... ({} earlier lines)\n{}",
        start,
        lines[start..].join("\n")
    )
}
