#[must_use]
pub fn try_glab_pattern(cmd: &str, output: &str) -> Option<String> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() || parts[0] != "glab" {
        return None;
    }
    if parts.len() < 2 {
        return None;
    }

    match parts[1] {
        "issue" => try_glab_issue(parts.get(2).copied(), output),
        "mr" => try_glab_mr(parts.get(2).copied(), output),
        "ci" => try_glab_ci(parts.get(2).copied(), output),
        _ => None,
    }
}

fn try_glab_issue(subcommand: Option<&str>, output: &str) -> Option<String> {
    match subcommand.unwrap_or("") {
        "list" => Some(compress_table_output("glab issues", output)),
        "view" => Some(compress_detail_output("issue", output)),
        _ => None,
    }
}

fn try_glab_mr(subcommand: Option<&str>, output: &str) -> Option<String> {
    match subcommand.unwrap_or("") {
        "list" => Some(compress_table_output("glab MRs", output)),
        "view" => Some(compress_detail_output("MR", output)),
        _ => None,
    }
}

fn try_glab_ci(subcommand: Option<&str>, output: &str) -> Option<String> {
    match subcommand.unwrap_or("") {
        "status" | "list" | "view" => Some(compress_ci_output(output)),
        _ => None,
    }
}

fn compress_table_output(label: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return format!("{label}: (empty)");
    }
    let count = lines.len().saturating_sub(1);
    let mut result = format!("{label} ({count}):\n");
    for line in &lines {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            result.push_str(&format!("  {trimmed}\n"));
        }
    }
    result
}

fn compress_detail_output(kind: &str, output: &str) -> String {
    let mut result = String::new();
    let mut in_body = false;
    let mut body_lines = 0;
    const MAX_BODY_LINES: usize = 30;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("title:")
            || trimmed.starts_with("state:")
            || trimmed.starts_with("author:")
            || trimmed.starts_with("labels:")
            || trimmed.starts_with("milestone:")
            || trimmed.starts_with("assignees:")
            || trimmed.starts_with("created:")
            || trimmed.starts_with("updated:")
        {
            result.push_str(&format!("{trimmed}\n"));
            in_body = false;
        } else if trimmed == "--" || trimmed.starts_with("---") {
            in_body = true;
            result.push_str("---\n");
        } else if in_body {
            body_lines += 1;
            if body_lines <= MAX_BODY_LINES {
                result.push_str(&format!("{line}\n"));
            } else if body_lines == MAX_BODY_LINES + 1 {
                result.push_str(&format!("... ({kind} body truncated)\n"));
            }
        } else if !trimmed.is_empty() {
            result.push_str(&format!("{trimmed}\n"));
        }
    }
    result
}

fn compress_ci_output(output: &str) -> String {
    let mut result = String::from("CI pipeline:\n");
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        result.push_str(&format!("  {trimmed}\n"));
    }
    result
}
