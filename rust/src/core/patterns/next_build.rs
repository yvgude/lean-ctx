macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn route_re() -> &'static regex::Regex {
    static_regex!(r"^[○●λƒ◐]\s+(/\S*)")
}
fn size_re() -> &'static regex::Regex {
    static_regex!(r"(\d+\.?\d*)\s*(kB|MB|B)\b")
}
fn build_time_re() -> &'static regex::Regex {
    static_regex!(r"(?:compiled|built|done)\s+(?:in\s+)?(\d+\.?\d*\s*[ms]+)")
}
fn vite_chunk_re() -> &'static regex::Regex {
    static_regex!(r"dist/(\S+)\s+(\d+\.?\d*\s*[kKMm]?B)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("vite") {
        return Some(compress_vite(output));
    }
    Some(compress_next(output))
}

fn compress_next(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut routes = Vec::new();
    let mut total_size = 0f64;
    let mut build_time = String::new();
    let mut errors = Vec::new();

    for line in trimmed.lines() {
        if let Some(caps) = route_re().captures(line) {
            let route = &caps[1];
            let size = extract_size(line);
            routes.push(format!("{route} ({size})"));
        }
        if let Some(caps) = build_time_re().captures(line) {
            build_time = caps[1].to_string();
        }
        if line.to_lowercase().contains("error") && !line.contains("0 error") {
            errors.push(line.trim().to_string());
        }
        if let Some(caps) = size_re().captures(line) {
            let val: f64 = caps[1].parse().unwrap_or(0.0);
            let unit = &caps[2];
            total_size += match unit {
                "MB" => val * 1024.0,
                "kB" => val,
                _ => val / 1024.0,
            };
        }
    }

    if !errors.is_empty() {
        return format!("BUILD ERROR:\n{}", errors.join("\n"));
    }

    let mut parts = Vec::new();
    if build_time.is_empty() {
        parts.push("built".to_string());
    } else {
        parts.push(format!("built ({build_time})"));
    }

    if !routes.is_empty() {
        parts.push(format!("{} routes:", routes.len()));
        for r in routes.iter().take(15) {
            parts.push(format!("  {r}"));
        }
        if routes.len() > 15 {
            parts.push(format!("  ... +{} more", routes.len() - 15));
        }
    }

    if total_size > 0.0 {
        if total_size > 1024.0 {
            parts.push(format!("total: {:.1} MB", total_size / 1024.0));
        } else {
            parts.push(format!("total: {total_size:.0} kB"));
        }
    }

    if parts.len() == 1 && parts[0] == "built" {
        return compact_output(trimmed, 10);
    }

    parts.join("\n")
}

fn compress_vite(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut chunks = Vec::new();
    let mut build_time = String::new();

    for line in trimmed.lines() {
        if let Some(caps) = vite_chunk_re().captures(line) {
            chunks.push(format!("{}: {}", &caps[1], &caps[2]));
        }
        if let Some(caps) = build_time_re().captures(line) {
            build_time = caps[1].to_string();
        }
    }

    let mut parts = Vec::new();
    if build_time.is_empty() {
        parts.push("built".to_string());
    } else {
        parts.push(format!("built ({build_time})"));
    }

    if !chunks.is_empty() {
        parts.push(format!("{} chunks:", chunks.len()));
        for c in chunks.iter().take(10) {
            parts.push(format!("  {c}"));
        }
        if chunks.len() > 10 {
            parts.push(format!("  ... +{} more", chunks.len() - 10));
        }
    }

    if parts.len() == 1 {
        return compact_output(trimmed, 10);
    }
    parts.join("\n")
}

fn extract_size(line: &str) -> String {
    if let Some(caps) = size_re().captures(line) {
        format!("{} {}", &caps[1], &caps[2])
    } else {
        "?".to_string()
    }
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
