macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn progress_re() -> &'static regex::Regex {
    static_regex!(r"^\s*\d+K\s+.*\d+%")
}

pub fn compress(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    let useful: Vec<&str> = trimmed
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !progress_re().is_match(t)
                && !t.starts_with("Length:")
                && !t.starts_with("Connecting to")
                && !t.starts_with("Resolving")
                && !t.starts_with("HTTP request sent")
                && !t.starts_with("Reusing existing")
        })
        .collect();

    let saved = trimmed.lines().find(|l| l.contains("saved"));

    if let Some(saved_line) = saved {
        let mut result = Vec::new();
        for line in &useful {
            if line.contains("Saving to") || line.contains("saved") || line.contains("--") {
                result.push(line.to_string());
            }
        }
        if result.is_empty() {
            result.push(saved_line.trim().to_string());
        }
        return Some(result.join("\n"));
    }

    if useful.len() <= 5 {
        return Some(useful.join("\n"));
    }
    Some(format!(
        "{}\n... ({} more lines)",
        useful[..3].join("\n"),
        useful.len() - 3
    ))
}
