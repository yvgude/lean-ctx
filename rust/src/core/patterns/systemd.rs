use std::collections::HashMap;

pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.starts_with("systemctl") {
        return Some(compress_systemctl(cmd, trimmed));
    }
    if cmd.starts_with("journalctl") {
        return Some(compress_journal(trimmed));
    }

    Some(compact_lines(trimmed, 15))
}

fn compress_systemctl(cmd: &str, output: &str) -> String {
    if cmd.contains("status") {
        return compress_status(output);
    }
    if cmd.contains("list-units")
        || cmd.contains("list-unit-files")
        || (!cmd.contains("start")
            && !cmd.contains("stop")
            && !cmd.contains("restart")
            && !cmd.contains("enable")
            && !cmd.contains("disable"))
    {
        return compress_list(output);
    }
    compact_lines(output, 10)
}

fn compress_status(output: &str) -> String {
    let mut parts = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Active:")
            || trimmed.starts_with("Loaded:")
            || trimmed.starts_with("Main PID:")
            || trimmed.starts_with("Memory:")
            || trimmed.starts_with("CPU:")
            || trimmed.starts_with("Tasks:")
        {
            parts.push(trimmed.to_string());
        }
        if trimmed.contains(".service") && trimmed.contains('-') && parts.is_empty() {
            parts.insert(0, trimmed.to_string());
        }
    }
    if parts.is_empty() {
        return compact_lines(output, 10);
    }
    parts.join("\n")
}

fn compress_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 20 {
        return lines.join("\n");
    }

    let mut by_state: HashMap<String, u32> = HashMap::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let state = parts[2].to_string();
            *by_state.entry(state).or_insert(0) += 1;
        }
    }

    let header = lines.first().unwrap_or(&"");
    let mut result = format!("{header}\n{} units:", lines.len() - 1);
    for (state, count) in &by_state {
        result.push_str(&format!("\n  {state}: {count}"));
    }
    result
}

fn compress_journal(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 30 {
        return lines.join("\n");
    }

    let mut deduped: HashMap<String, u32> = HashMap::new();
    for line in &lines {
        let parts: Vec<&str> = line.splitn(4, ' ').collect();
        let key = if parts.len() >= 4 {
            parts[3].to_string()
        } else {
            line.to_string()
        };
        *deduped.entry(key).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = deduped.into_iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));

    let top: Vec<String> = sorted
        .iter()
        .take(20)
        .map(|(msg, count)| {
            if *count > 1 {
                format!("  ({count}x) {msg}")
            } else {
                format!("  {msg}")
            }
        })
        .collect();

    format!(
        "{} log lines (deduped to {}):\n{}",
        lines.len(),
        top.len(),
        top.join("\n")
    )
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
