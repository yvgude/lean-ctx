pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if is_table_output(trimmed) {
        return Some(compress_table(trimmed));
    }

    if cmd.contains("show databases") || cmd.contains("show tables") {
        return Some(compress_show(trimmed));
    }

    if trimmed.starts_with("Query OK") || trimmed.starts_with("Empty set") {
        return Some(trimmed.lines().next().unwrap_or(trimmed).to_string());
    }

    Some(compact_lines(trimmed, 20))
}

fn is_table_output(output: &str) -> bool {
    let lines: Vec<&str> = output.lines().collect();
    lines.len() >= 3
        && lines
            .iter()
            .any(|l| l.starts_with('+') && l.contains("---"))
}

fn compress_table(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let data_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| !l.starts_with('+') && !l.trim().is_empty())
        .collect();

    let row_count = if data_lines.len() > 1 {
        data_lines.len() - 1
    } else {
        0
    };

    if row_count <= 20 {
        return output.to_string();
    }

    let header_end = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('+'))
        .nth(1)
        .map_or(3, |(i, _)| i + 1);

    let preview_end = (header_end + 10).min(lines.len());
    let preview = &lines[..preview_end];
    format!("{}\n... ({row_count} rows total)", preview.join("\n"))
}

fn compress_show(output: &str) -> String {
    let items: Vec<&str> = output
        .lines()
        .filter(|l| !l.starts_with('+') && !l.trim().is_empty() && !l.contains("---"))
        .filter(|l| !l.contains("Database") && !l.contains("Tables_in"))
        .map(|l| l.trim().trim_matches('|').trim())
        .filter(|l| !l.is_empty())
        .collect();

    if items.is_empty() {
        return "empty".to_string();
    }
    if items.len() <= 30 {
        return format!("{} items: {}", items.len(), items.join(", "));
    }
    format!(
        "{} items: {}, ... +{} more",
        items.len(),
        items[..20].join(", "),
        items.len() - 20
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
