use std::collections::HashMap;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    let has_recap = trimmed.contains("PLAY RECAP");
    if has_recap {
        return Some(compress_playbook(trimmed));
    }

    if trimmed.contains("TASK [") {
        return Some(compress_tasks(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_playbook(output: &str) -> String {
    let mut recap_lines = Vec::new();
    let mut in_recap = false;

    for line in output.lines() {
        if line.contains("PLAY RECAP") {
            in_recap = true;
            continue;
        }
        if in_recap {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                recap_lines.push(trimmed.to_string());
            }
        }
    }

    if recap_lines.is_empty() {
        return compact_lines(output, 15);
    }

    let mut result = String::from("PLAY RECAP:");
    for line in &recap_lines {
        result.push_str(&format!("\n  {line}"));
    }
    result
}

fn compress_tasks(output: &str) -> String {
    let mut tasks: HashMap<String, Vec<String>> = HashMap::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("ok:")
            || trimmed.starts_with("changed:")
            || trimmed.starts_with("failed:")
            || trimmed.starts_with("skipping:")
        {
            let status = trimmed.split(':').next().unwrap_or("?").to_string();
            tasks.entry(status).or_default().push(trimmed.to_string());
        }
    }

    if tasks.is_empty() {
        return compact_lines(output, 15);
    }

    let mut result = Vec::new();
    for (status, items) in &tasks {
        result.push(format!("{status}: {}", items.len()));
    }
    result.join(", ")
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
