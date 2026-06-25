#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("list") || cmd.contains("ls") {
        return Some(compress_list(trimmed));
    }
    if cmd.contains("install") || cmd.contains("upgrade") {
        return Some(compress_install(trimmed));
    }
    if cmd.contains("status") {
        return Some(compress_status(trimmed));
    }
    if cmd.contains("template") || cmd.contains("dry-run") {
        return Some(compress_template(trimmed));
    }
    if cmd.contains("repo") {
        return Some(compress_repo(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "no releases".to_string();
    }

    let header = lines[0];
    let releases: Vec<&str> = lines[1..]
        .iter()
        .copied()
        .filter(|l| !l.trim().is_empty())
        .collect();
    if releases.len() <= 15 {
        return output.to_string();
    }
    format!(
        "{header}\n{}\n... ({} more)",
        releases[..10].join("\n"),
        releases.len() - 10
    )
}

fn compress_install(output: &str) -> String {
    let mut name = String::new();
    let mut status = String::new();
    let mut notes_start = false;
    let mut notes = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("NAME:") {
            name = trimmed
                .strip_prefix("NAME:")
                .unwrap_or("")
                .trim()
                .to_string();
        } else if trimmed.starts_with("STATUS:") {
            status = trimmed
                .strip_prefix("STATUS:")
                .unwrap_or("")
                .trim()
                .to_string();
        } else if trimmed == "NOTES:" {
            notes_start = true;
        } else if notes_start && !trimmed.is_empty() && notes.len() < 5 {
            notes.push(trimmed.to_string());
        }
    }

    let mut result = format!("{name}: {status}");
    if !notes.is_empty() {
        result.push_str(&format!("\nnotes: {}", notes.join(" | ")));
    }
    result
}

fn compress_status(output: &str) -> String {
    let mut parts = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("NAME:")
            || trimmed.starts_with("STATUS:")
            || trimmed.starts_with("NAMESPACE:")
            || trimmed.starts_with("REVISION:")
            || trimmed.starts_with("LAST DEPLOYED:")
        {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        return compact_lines(output, 10);
    }
    parts.join("\n")
}

fn compress_template(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let yaml_docs: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim() == "---")
        .map(|(i, _)| i)
        .collect();

    let mut kinds = Vec::new();
    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("kind:") {
            kinds.push(
                trimmed
                    .strip_prefix("kind:")
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            );
        }
    }

    if kinds.is_empty() {
        return format!("{} lines of YAML", lines.len());
    }

    let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for k in &kinds {
        *counts.entry(k.as_str()).or_insert(0) += 1;
    }
    let summary: Vec<String> = counts.iter().map(|(k, v)| format!("  {k}: {v}")).collect();
    format!(
        "{} YAML docs ({} resources):\n{}",
        yaml_docs.len().max(1),
        kinds.len(),
        summary.join("\n")
    )
}

fn compress_repo(output: &str) -> String {
    compact_lines(output, 10)
}

fn compact_lines(text: &str, max: usize) -> String {
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
