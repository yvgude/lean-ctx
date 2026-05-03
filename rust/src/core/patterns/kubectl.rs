macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn log_ts_re() -> &'static regex::Regex {
    static_regex!(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\S*\s+")
}
fn resource_action_re() -> &'static regex::Regex {
    static_regex!(r"(\S+/\S+)\s+(configured|created|unchanged|deleted)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("logs") || command.contains("log ") {
        return Some(compress_logs(output));
    }
    if command.contains("describe") {
        return Some(compress_describe(output));
    }
    if command.contains("apply") {
        return Some(compress_apply(output));
    }
    if command.contains("delete") {
        return Some(compress_delete(output));
    }
    if command.contains("get") {
        return Some(compress_get(output));
    }
    if command.contains("exec") {
        return Some(compress_exec(output));
    }
    if command.contains("top") {
        return Some(compress_top(output));
    }
    if command.contains("rollout") {
        return Some(compress_rollout(output));
    }
    if command.contains("scale") {
        return Some(compress_simple(output));
    }
    Some(compact_table(output))
}

fn compress_get(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return "no resources".to_string();
    }
    if lines.len() == 1 && lines[0].starts_with("No resources") {
        return "no resources".to_string();
    }

    if lines.len() <= 1 {
        return output.trim().to_string();
    }

    let header = lines[0];
    let cols: Vec<&str> = header.split_whitespace().collect();

    let mut rows = Vec::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let name = parts[0];
        let relevant: Vec<&str> = parts.iter().skip(1).take(4).copied().collect();
        rows.push(format!("{name} {}", relevant.join(" ")));
    }

    if rows.is_empty() {
        return "no resources".to_string();
    }

    let col_hint = cols
        .iter()
        .skip(1)
        .take(4)
        .copied()
        .collect::<Vec<&str>>()
        .join(" ");
    format!("[{col_hint}]\n{}", rows.join("\n"))
}

fn compress_logs(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return output.to_string();
    }

    let mut deduped: Vec<(String, u32)> = Vec::new();
    for line in &lines {
        let stripped = log_ts_re().replace(line, "").trim().to_string();
        if stripped.is_empty() {
            continue;
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

    if result.len() > 30 {
        let tail = &result[result.len() - 20..];
        return format!("... ({} lines total)\n{}", lines.len(), tail.join("\n"));
    }
    result.join("\n")
}

fn compress_describe(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 20 {
        return output.to_string();
    }

    let mut sections = Vec::new();
    let mut current_section = String::new();
    let mut current_lines: Vec<&str> = Vec::new();
    for line in &lines {
        if !line.starts_with(' ')
            && !line.starts_with('\t')
            && line.ends_with(':')
            && !line.contains("  ")
        {
            if !current_section.is_empty() {
                let count = current_lines.len();
                if count <= 3 {
                    sections.push(format!("{current_section}\n{}", current_lines.join("\n")));
                } else {
                    sections.push(format!("{current_section} ({count} lines)"));
                }
            }
            current_section = line.trim_end_matches(':').to_string();
            current_lines.clear();
            // Events section detected
        } else {
            current_lines.push(line);
        }
    }

    if !current_section.is_empty() {
        let count = current_lines.len();
        if current_section == "Events" && count > 5 {
            let last_events = &current_lines[count.saturating_sub(5)..];
            sections.push(format!(
                "Events (last 5 of {count}):\n{}",
                last_events.join("\n")
            ));
        } else if count <= 5 {
            sections.push(format!("{current_section}\n{}", current_lines.join("\n")));
        } else {
            sections.push(format!("{current_section} ({count} lines)"));
        }
    }

    sections.join("\n")
}

fn compress_apply(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut configured = 0u32;
    let mut created = 0u32;
    let mut unchanged = 0u32;
    let mut deleted = 0u32;
    let mut resources = Vec::new();

    for line in trimmed.lines() {
        if let Some(caps) = resource_action_re().captures(line) {
            let resource = &caps[1];
            let action = &caps[2];
            match action {
                "configured" => configured += 1,
                "created" => created += 1,
                "unchanged" => unchanged += 1,
                "deleted" => deleted += 1,
                _ => {}
            }
            resources.push(format!("{resource} {action}"));
        }
    }

    let total = configured + created + unchanged + deleted;
    if total == 0 {
        return compact_output(trimmed, 5);
    }

    let mut summary = Vec::new();
    if created > 0 {
        summary.push(format!("{created} created"));
    }
    if configured > 0 {
        summary.push(format!("{configured} configured"));
    }
    if unchanged > 0 {
        summary.push(format!("{unchanged} unchanged"));
    }
    if deleted > 0 {
        summary.push(format!("{deleted} deleted"));
    }

    format!("ok ({total} resources: {})", summary.join(", "))
}

fn compress_delete(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let deleted: Vec<&str> = trimmed.lines().filter(|l| l.contains("deleted")).collect();

    if deleted.is_empty() {
        return compact_output(trimmed, 3);
    }
    format!("deleted {} resources", deleted.len())
}

fn compress_exec(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > 20 {
        let tail = &lines[lines.len() - 10..];
        return format!("... ({} lines)\n{}", lines.len(), tail.join("\n"));
    }
    trimmed.to_string()
}

fn compress_top(output: &str) -> String {
    compact_table(output)
}

fn compress_rollout(output: &str) -> String {
    compact_output(output, 5)
}

fn compress_simple(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    compact_output(trimmed, 3)
}

fn compact_table(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 15 {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more rows)",
        lines[..15].join("\n"),
        lines.len() - 15
    )
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
