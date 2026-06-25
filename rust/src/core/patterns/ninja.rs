macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn progress_re() -> &'static regex::Regex {
    static_regex!(r"^\[(\d+)/(\d+)\]\s+")
}

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ninja: ok".to_string());
    }

    if command.contains("-t targets") || command.contains("-t rules") {
        return Some(compress_query(trimmed));
    }

    Some(compress_build(trimmed))
}

fn compress_build(output: &str) -> String {
    let mut total_steps = 0u32;
    let mut max_total = 0u32;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut warning_seen = std::collections::HashSet::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if let Some(caps) = progress_re().captures(trimmed) {
            if let (Ok(current), Ok(total)) = (caps[1].parse::<u32>(), caps[2].parse::<u32>()) {
                total_steps = current;
                max_total = total;
            }
            continue;
        }

        if is_error_line(trimmed) {
            if errors.len() < 20 {
                errors.push(trimmed.to_string());
            }
            continue;
        }

        if is_warning_line(trimmed) {
            let key = normalize_warning(trimmed);
            if warning_seen.insert(key) {
                warnings.push(trimmed.to_string());
            }
        }
    }

    if !errors.is_empty() {
        let mut result = format!("ninja: FAILED ({} errors", errors.len());
        if !warnings.is_empty() {
            result.push_str(&format!(", {} unique warnings", warnings.len()));
        }
        result.push_str(&format!(", {total_steps}/{max_total} steps)"));
        for e in errors.iter().take(10) {
            result.push_str(&format!("\n  {e}"));
        }
        if errors.len() > 10 {
            result.push_str(&format!("\n  ... +{} more errors", errors.len() - 10));
        }
        return result;
    }

    let mut result = format!("ninja: ok ({total_steps}/{max_total} steps)");
    if !warnings.is_empty() {
        result.push_str(&format!("\n{} unique warnings:", warnings.len()));
        for w in warnings.iter().take(10) {
            result.push_str(&format!("\n  {w}"));
        }
        if warnings.len() > 10 {
            result.push_str(&format!("\n  ... +{} more", warnings.len() - 10));
        }
    }
    result
}

fn compress_query(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 20 {
        return format!("{} entries:\n{}", lines.len(), lines.join("\n"));
    }
    format!(
        "{} entries:\n{}\n... +{} more",
        lines.len(),
        lines[..20].join("\n"),
        lines.len() - 20
    )
}

fn is_error_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("error:") || l.contains("fatal error") || l.contains("ninja: error")
}

fn is_warning_line(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("warning:")
}

fn normalize_warning(line: &str) -> String {
    let re = static_regex!(r"[^\s:]+:\d+:\d+:\s*");
    let without_location = re.replace_all(line, "");
    without_location.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_successful_build() {
        let output = "[1/10] Compiling foo.c\n[2/10] Compiling bar.c\n[10/10] Linking app\n";
        let result = compress("ninja", output).unwrap();
        assert!(result.contains("10/10"), "should show final progress");
        assert!(result.contains("ok"), "should indicate success");
    }

    #[test]
    fn keeps_errors() {
        let output =
            "[1/5] Compiling foo.c\n[2/5] Compiling bar.c\nerror: undefined reference to `main`\n";
        let result = compress("ninja", output).unwrap();
        assert!(result.contains("FAILED"), "should indicate failure");
        assert!(result.contains("undefined reference"), "should keep errors");
    }

    #[test]
    fn deduplicates_warnings() {
        let output = "[1/3] Compiling a.c\nsrc/a.c:10:5: warning: unused variable\nsrc/b.c:20:5: warning: unused variable\n[3/3] Linking\n";
        let result = compress("ninja", output).unwrap();
        assert!(
            result.contains("1 unique warning"),
            "should deduplicate same warning at different locations: {result}"
        );
    }

    #[test]
    fn empty_output() {
        let result = compress("ninja", "").unwrap();
        assert_eq!(result, "ninja: ok");
    }

    #[test]
    fn compresses_target_query() {
        let output = "target1: phony\ntarget2: cc\ntarget3: link\n";
        let result = compress("ninja -t targets", output).unwrap();
        assert!(result.contains("3 entries"), "should count targets");
    }
}
