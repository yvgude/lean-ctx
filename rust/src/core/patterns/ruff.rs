macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn ruff_line_re() -> &'static regex::Regex {
    static_regex!(r"^(.+?):(\d+):(\d+):\s+([A-Z]\d+)\s+(.+)$")
}
fn ruff_fixed_re() -> &'static regex::Regex {
    static_regex!(r"Found (\d+) errors?.*?(\d+) fixable")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("format") || command.contains("fmt") {
        return Some(compress_format(output));
    }
    Some(compress_check(output))
}

fn compress_check(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.contains("All checks passed") {
        return "clean".to_string();
    }

    let mut by_rule: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut issue_lines: Vec<&str> = Vec::new();

    for line in trimmed.lines() {
        if let Some(caps) = ruff_line_re().captures(line) {
            let file = caps[1].to_string();
            let rule = caps[4].to_string();
            files.insert(file);
            *by_rule.entry(rule).or_insert(0) += 1;
            issue_lines.push(line);
        }
    }

    if by_rule.is_empty() {
        if let Some(caps) = ruff_fixed_re().captures(trimmed) {
            return format!("{} errors ({} fixable)", &caps[1], &caps[2]);
        }
        return compact_output(trimmed, 10);
    }

    let total: u32 = by_rule.values().sum();

    if total <= 30 {
        return trimmed.to_string();
    }

    let mut rules: Vec<(String, u32)> = by_rule.into_iter().collect();
    rules.sort_by_key(|x| std::cmp::Reverse(x.1));

    let mut parts = Vec::new();
    parts.push(format!("{total} issues in {} files", files.len()));
    for line in issue_lines.iter().take(20) {
        parts.push(format!("  {line}"));
    }
    if issue_lines.len() > 20 {
        parts.push(format!("  ... +{} more issues", issue_lines.len() - 20));
    }
    parts.push(String::new());
    parts.push("by rule:".to_string());
    for (rule, count) in rules.iter().take(8) {
        parts.push(format!("  {rule}: {count}"));
    }
    if rules.len() > 8 {
        parts.push(format!("  ... +{} more rules", rules.len() - 8));
    }

    parts.join("\n")
}

fn compress_format(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok (formatted)".to_string();
    }

    let reformatted: Vec<&str> = trimmed
        .lines()
        .filter(|l| l.contains("reformatted") || l.contains("would reformat"))
        .collect();

    let unchanged: Vec<&str> = trimmed
        .lines()
        .filter(|l| l.contains("left unchanged") || l.contains("already formatted"))
        .collect();

    if !reformatted.is_empty() {
        return format!("{} files reformatted", reformatted.len());
    }
    if !unchanged.is_empty() {
        return format!("ok ({} files already formatted)", unchanged.len());
    }

    compact_output(trimmed, 5)
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
