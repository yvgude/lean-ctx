macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn eslint_file_re() -> &'static regex::Regex {
    static_regex!(r"^(/\S+|[A-Z]:\\\S+|\S+\.\w+)$")
}
fn eslint_error_re() -> &'static regex::Regex {
    static_regex!(r"^\s+(\d+):(\d+)\s+(error|warning)\s+(.+?)\s{2,}(\S+)$")
}
fn eslint_summary_re() -> &'static regex::Regex {
    static_regex!(r"(\d+)\s+problems?\s*\((\d+)\s+errors?,\s*(\d+)\s+warnings?\)")
}
fn biome_diag_re() -> &'static regex::Regex {
    static_regex!(r"^([\w/.-]+):(\d+):(\d+)\s+(\w+)\s+(.+)$")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("biome") {
        return Some(compress_biome(output));
    }
    if command.contains("stylelint") {
        return Some(compress_stylelint(output));
    }
    Some(compress_eslint(output))
}

fn compress_eslint(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "clean".to_string();
    }

    let mut by_rule: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();
    let mut file_count = 0u32;
    let mut total_errors = 0u32;
    let mut total_warnings = 0u32;

    for line in trimmed.lines() {
        let l = line.trim();
        if eslint_file_re().is_match(l) {
            file_count += 1;
            continue;
        }
        if let Some(caps) = eslint_error_re().captures(line) {
            let severity = &caps[3];
            let rule = caps[5].to_string();
            let entry = by_rule.entry(rule).or_insert((0, 0));
            if severity == "error" {
                entry.0 += 1;
                total_errors += 1;
            } else {
                entry.1 += 1;
                total_warnings += 1;
            }
        }
        if let Some(caps) = eslint_summary_re().captures(line) {
            total_errors = caps[2].parse().unwrap_or(total_errors);
            total_warnings = caps[3].parse().unwrap_or(total_warnings);
        }
    }

    if by_rule.is_empty() && total_errors == 0 && total_warnings == 0 {
        if trimmed.lines().count() <= 5 {
            return trimmed.to_string();
        }
        return compact_output(trimmed, 10);
    }

    let mut parts = Vec::new();
    parts.push(format!(
        "{total_errors} errors, {total_warnings} warnings in {file_count} files"
    ));

    let mut rules: Vec<(String, (u32, u32))> = by_rule.into_iter().collect();
    rules.sort_by_key(|(_, (errors, warnings))| std::cmp::Reverse(*errors + *warnings));

    for (rule, (errors, warnings)) in rules.iter().take(10) {
        let total = errors + warnings;
        parts.push(format!("  {rule}: {total}"));
    }
    if rules.len() > 10 {
        parts.push(format!("  ... +{} more rules", rules.len() - 10));
    }

    parts.join("\n")
}

fn compress_biome(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.contains("No diagnostics") {
        return "clean".to_string();
    }

    let mut errors = 0u32;
    let mut warnings = 0u32;
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in trimmed.lines() {
        if let Some(caps) = biome_diag_re().captures(line) {
            files.insert(caps[1].to_string());
            let severity = &caps[4];
            if severity.contains("error") || severity.contains("ERROR") {
                errors += 1;
            } else {
                warnings += 1;
            }
        }
    }

    if errors == 0 && warnings == 0 {
        return compact_output(trimmed, 10);
    }

    format!(
        "{errors} errors, {warnings} warnings in {} files",
        files.len()
    )
}

fn compress_stylelint(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "clean".to_string();
    }
    compress_eslint(output)
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
