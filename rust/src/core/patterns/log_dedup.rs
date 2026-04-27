macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn timestamp_re() -> &'static regex::Regex {
    static_regex!(r"^\[?\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}[^\]\s]*\]?\s*")
}

pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return None;
    }

    let mut deduped: Vec<(String, u32)> = Vec::new();
    let mut error_lines = Vec::new();

    for line in &lines {
        let stripped = timestamp_re().replace(line, "").trim().to_string();
        if stripped.is_empty() {
            continue;
        }

        let lower = stripped.to_lowercase();
        if lower.contains("error")
            || lower.contains("critical")
            || lower.contains("fatal")
            || lower.contains("panic")
            || lower.contains("exception")
        {
            error_lines.push(stripped.clone());
        }

        if let Some(last) = deduped.last_mut() {
            if last.0 == stripped {
                last.1 += 1;
                continue;
            }
        }
        deduped.push((stripped, 1));
    }

    let result: Vec<String> = deduped
        .iter()
        .map(|(line, count)| {
            if *count > 1 {
                format!("{line} (x{count})")
            } else {
                line.clone()
            }
        })
        .collect();

    let mut parts = Vec::new();
    parts.push(format!("{} lines → {} unique", lines.len(), deduped.len()));

    if !error_lines.is_empty() {
        parts.push(format!("{} errors:", error_lines.len()));
        for e in error_lines.iter().take(5) {
            parts.push(format!("  {e}"));
        }
        if error_lines.len() > 5 {
            parts.push(format!("  ... +{} more errors", error_lines.len() - 5));
        }
    }

    if result.len() > 30 {
        let tail = &result[result.len() - 15..];
        parts.push(format!("last 15 unique lines:\n{}", tail.join("\n")));
    } else {
        parts.push(result.join("\n"));
    }

    Some(parts.join("\n"))
}
