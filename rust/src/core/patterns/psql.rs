pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if is_table_output(trimmed) {
        return Some(compress_table(trimmed));
    }

    if cmd.contains("\\dt") || cmd.contains("\\d") {
        return Some(compress_describe(trimmed));
    }

    if trimmed.starts_with("INSERT")
        || trimmed.starts_with("UPDATE")
        || trimmed.starts_with("DELETE")
        || trimmed.starts_with("CREATE")
        || trimmed.starts_with("ALTER")
        || trimmed.starts_with("DROP")
    {
        return Some(trimmed.lines().next().unwrap_or(trimmed).to_string());
    }

    Some(compact_lines(trimmed, 20))
}

fn is_table_output(output: &str) -> bool {
    let lines: Vec<&str> = output.lines().collect();
    lines.len() >= 3
        && lines
            .iter()
            .any(|l| l.contains("---+---") || l.contains("-+-"))
}

fn compress_table(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut separator_idx = 0;
    let mut data_rows = 0u32;

    for (i, line) in lines.iter().enumerate() {
        if line.contains("---+---") || line.contains("-+-") {
            separator_idx = i;
            break;
        }
    }

    for line in lines.iter().skip(separator_idx + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('(') {
            continue;
        }
        data_rows += 1;
    }

    let row_count_line = lines
        .iter()
        .rev()
        .find(|l| l.trim().starts_with('(') && l.contains("row"));
    let count_str =
        row_count_line.map_or_else(|| format!("({data_rows} rows)"), |l| l.trim().to_string());

    if data_rows <= 20 {
        return output.to_string();
    }

    let preview_end = (separator_idx + 11).min(lines.len());
    let preview: Vec<&str> = lines[..preview_end].to_vec();
    format!("{}\n... {count_str}", preview.join("\n"))
}

fn compress_describe(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 30 {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..20].join("\n"),
        lines.len() - 20
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
