macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn pw_failed_re() -> &'static regex::Regex {
    static_regex!(r"^\s+\d+\)\s+(.+)$")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("cypress") {
        return Some(compress_cypress(output));
    }
    Some(compress_playwright(output))
}

fn compress_playwright(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failed_names = Vec::new();
    let mut duration = String::new();

    for line in trimmed.lines() {
        let l = line.trim().to_lowercase();
        if l.contains("passed") {
            if let Some(n) = extract_number(&l, "passed") {
                passed = n;
            }
        }
        if l.contains("failed") {
            if let Some(n) = extract_number(&l, "failed") {
                failed = n;
            }
        }
        if l.contains("skipped") {
            if let Some(n) = extract_number(&l, "skipped") {
                skipped = n;
            }
        }
        if let Some(caps) = pw_failed_re().captures(line) {
            failed_names.push(caps[1].trim().to_string());
        }
        if l.contains("finished in") || l.contains("duration") {
            duration = line.trim().to_string();
        }
    }

    let total = passed + failed + skipped;
    if total == 0 {
        return compact_output(trimmed, 10);
    }

    let mut parts = Vec::new();
    parts.push(format!(
        "{total} tests: {passed} passed, {failed} failed, {skipped} skipped"
    ));

    if !failed_names.is_empty() {
        parts.push("failed:".to_string());
        for name in failed_names.iter().take(10) {
            parts.push(format!("  {name}"));
        }
        if failed_names.len() > 10 {
            parts.push(format!("  ... +{} more", failed_names.len() - 10));
        }
    }

    if !duration.is_empty() {
        parts.push(duration);
    }

    parts.join("\n")
}

fn compress_cypress(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut pending = 0u32;

    for line in trimmed.lines() {
        let l = line.trim().to_lowercase();
        if l.contains("passing") {
            passed += extract_first_number(&l);
        }
        if l.contains("failing") {
            failed += extract_first_number(&l);
        }
        if l.contains("pending") {
            pending += extract_first_number(&l);
        }
    }

    let total = passed + failed + pending;
    if total == 0 {
        return compact_output(trimmed, 10);
    }

    format!("{total} tests: {passed} passed, {failed} failed, {pending} pending")
}

fn extract_number(line: &str, keyword: &str) -> Option<u32> {
    let pos = line.find(keyword)?;
    let before = &line[..pos];
    before.split_whitespace().last()?.parse().ok()
}

fn extract_first_number(line: &str) -> u32 {
    for word in line.split_whitespace() {
        if let Ok(n) = word.parse::<u32>() {
            return n;
        }
    }
    0
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
