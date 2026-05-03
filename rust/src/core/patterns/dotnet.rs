macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn build_summary_err_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*(\d+)\s+Error\(s\)\s*$")
}
fn build_summary_warn_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*(\d+)\s+Warning\(s\)\s*$")
}
fn build_result_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^(Build succeeded\.|Build FAILED\.)")
}
fn restored_proj_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*Restored\s+(.+\.csproj[^(\n]*)(?:\s*\([^)]*\))?\s*\.?\s*$")
}
fn restored_pkg_re() -> &'static regex::Regex {
    static_regex!(r"(?i)Restored\s+(\d+)\s+package")
}
fn test_total_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*Total tests:\s*(\d+)\s*$")
}
fn publish_arrow_re() -> &'static regex::Regex {
    static_regex!(r"\s+->\s+")
}

fn is_msbuild_noise(line: &str) -> bool {
    let t = line.trim_start();
    let tl = t.to_ascii_lowercase();
    if tl.starts_with("microsoft (r) build engine")
        || tl.starts_with("copyright (c) microsoft")
        || tl.contains("version ") && tl.contains("msbuild")
    {
        return true;
    }
    if tl.starts_with("verbosity:") || tl == "build started." {
        return true;
    }
    // Progress / target spam (typical minimal+ still has some)
    if tl.starts_with("time elapsed") && tl.contains("00:00:") {
        return true;
    }
    false
}

fn is_dotnet_restore_noise(line: &str) -> bool {
    let tl = line.trim().to_ascii_lowercase();
    tl.starts_with("determining projects to restore")
        || tl.contains("assets file has not changed")
        || tl.starts_with("writing assets file")
}

fn looks_like_build_error_line(line: &str) -> bool {
    let t = line.trim();
    let tl = t.to_ascii_lowercase();
    if tl.contains(": error ") || tl.starts_with("error ") {
        return true;
    }
    if tl.contains("msbuild : error") || tl.contains("error msb") {
        return true;
    }
    if tl.contains(": error cs") || tl.contains(": error mc") {
        return true;
    }
    false
}

fn looks_like_build_warning_line(line: &str) -> bool {
    let t = line.trim();
    let tl = t.to_ascii_lowercase();
    (tl.contains(": warning ") || tl.starts_with("warning ")) && !tl.contains("warning(s)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let cl = command.trim().to_ascii_lowercase();
    if !cl.starts_with("dotnet ") {
        return None;
    }

    let sub = cl
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .trim_start_matches('-');
    match sub {
        "build" | "msbuild" => return Some(compress_build(output)),
        "test" | "vstest" => return Some(compress_test(output)),
        "restore" => return Some(compress_restore(output)),
        "publish" => return Some(compress_publish(output)),
        _ => {}
    }

    None
}

fn compress_build(output: &str) -> String {
    let mut parts = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut result_line: Option<String> = None;
    let mut summary_errors: Option<String> = None;
    let mut summary_warnings: Option<String> = None;

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() || is_msbuild_noise(t) {
            continue;
        }
        let trim = t.trim();
        if build_result_re().is_match(trim) {
            result_line = Some(trim.to_string());
            continue;
        }
        if let Some(caps) = build_summary_err_re().captures(trim) {
            summary_errors = Some(format!("{} Error(s)", &caps[1]));
            continue;
        }
        if let Some(caps) = build_summary_warn_re().captures(trim) {
            summary_warnings = Some(format!("{} Warning(s)", &caps[1]));
            continue;
        }
        if looks_like_build_error_line(trim) {
            errors.push(trim.to_string());
            continue;
        }
        if looks_like_build_warning_line(trim) {
            warnings.push(trim.to_string());
        }
    }

    if let Some(r) = result_line {
        parts.push(r);
    }
    if let Some(s) = summary_warnings {
        parts.push(s);
    }
    if let Some(s) = summary_errors {
        parts.push(s);
    }
    if !errors.is_empty() {
        parts.push(format!("{} error lines:", errors.len()));
        parts.extend(errors.into_iter().map(|e| format!("  {e}")));
    }
    if !warnings.is_empty() && warnings.len() <= 20 {
        parts.push(format!("{} warning lines:", warnings.len()));
        parts.extend(warnings.into_iter().map(|w| format!("  {w}")));
    } else if !warnings.is_empty() {
        parts.push(format!("{} warnings (omitted detail)", warnings.len()));
    }

    if parts.is_empty() {
        compact_or_ok(output, 8)
    } else {
        parts.join("\n")
    }
}

fn compress_test(output: &str) -> String {
    let mut parts = Vec::new();
    let mut in_failure = false;
    let mut failure_block: Vec<String> = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        let trim = t.trim();
        let tl = trim.to_ascii_lowercase();

        if tl.contains("test run failed") || tl == "failed!" {
            parts.push(trim.to_string());
        }
        if tl.starts_with("passed!") || tl.starts_with("failed!") {
            parts.push(trim.to_string());
        }
        if test_total_re().is_match(trim) {
            parts.push(trim.to_string());
        }
        if tl.starts_with("passed:")
            || tl.starts_with("failed:")
            || tl.starts_with("skipped:")
            || tl.starts_with("total:")
        {
            parts.push(trim.to_string());
        }
        if tl.contains("error message:") || tl.contains("stack trace:") {
            in_failure = true;
        }
        if looks_like_build_error_line(trim) && tl.contains("error") {
            parts.push(trim.to_string());
        }

        if in_failure && !trim.is_empty() {
            failure_block.push(trim.to_string());
            if failure_block.len() > 40 {
                in_failure = false;
            }
        }
    }

    if !failure_block.is_empty() {
        parts.push("failure detail:".to_string());
        parts.extend(failure_block.into_iter().take(25).map(|l| format!("  {l}")));
    }

    if parts.is_empty() {
        compact_or_ok(output, 12)
    } else {
        parts.join("\n")
    }
}

fn compress_restore(output: &str) -> String {
    let mut restored_projects = Vec::new();
    let mut pkg_summary: Option<String> = None;

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() || is_dotnet_restore_noise(t) {
            continue;
        }
        let trim = t.trim();
        if let Some(caps) = restored_proj_re().captures(trim) {
            restored_projects.push(caps[1].trim().to_string());
            continue;
        }
        if let Some(caps) = restored_pkg_re().captures(trim) {
            pkg_summary = Some(format!("Restored {} packages (summary line)", &caps[1]));
        }
        if looks_like_build_error_line(trim) {
            restored_projects.push(format!("ERR: {trim}"));
        }
    }

    let mut parts = Vec::new();
    if !restored_projects.is_empty() {
        parts.push(format!("Restored {} project(s):", restored_projects.len()));
        for p in restored_projects {
            parts.push(format!("  {p}"));
        }
    }
    if let Some(s) = pkg_summary {
        parts.push(s);
    }

    if parts.is_empty() {
        compact_or_ok(output, 10)
    } else {
        parts.join("\n")
    }
}

fn compress_publish(output: &str) -> String {
    let mut parts = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() || is_msbuild_noise(t) {
            continue;
        }
        let trim = t.trim();
        if publish_arrow_re().is_match(trim) {
            parts.push(trim.to_string());
            continue;
        }
        if trim.to_ascii_lowercase().contains("published to")
            || trim.to_ascii_lowercase().contains("output path")
        {
            parts.push(trim.to_string());
        }
        if build_result_re().is_match(trim) {
            parts.push(trim.to_string());
        }
        if looks_like_build_error_line(trim) {
            parts.push(trim.to_string());
        }
    }

    if parts.is_empty() {
        compact_or_ok(output, 10)
    } else {
        parts.join("\n")
    }
}

fn compact_or_ok(output: &str, max: usize) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return "ok".to_string();
    }
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
