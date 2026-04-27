pub(super) fn compress_diff(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 500 {
        return compress_diff_keep_hunks(output);
    }

    let mut file_ranges: Vec<(usize, usize, String)> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("diff --git") {
            if let Some(last) = file_ranges.last_mut() {
                last.1 = i;
            }
            let name = line.split(" b/").nth(1).unwrap_or("?").to_string();
            file_ranges.push((i, lines.len(), name));
        }
    }

    let mut result = Vec::new();
    for (start, end, _name) in &file_ranges {
        let file_lines = &lines[*start..*end];
        if file_lines.len() <= 250 {
            for l in file_lines {
                result.push(l.to_string());
            }
        } else {
            for l in &file_lines[..200] {
                result.push(l.to_string());
            }
            let hidden = file_lines.len() - 250;
            result.push(format!(
                "[WARNING: diff truncated ({hidden} lines hidden). Use ctx_shell(raw=true) for full output]"
            ));
            for l in &file_lines[file_lines.len() - 50..] {
                result.push(l.to_string());
            }
        }
    }

    if result.is_empty() {
        return compress_diff_keep_hunks(output);
    }
    result.join("\n")
}

/// Trims `index` header lines and limits unchanged context lines to max 3 per hunk
/// while keeping all `+`/`-` lines (actual diff content) intact.
pub(super) fn compress_diff_keep_hunks(output: &str) -> String {
    let mut result = Vec::new();
    let mut context_run = 0u32;

    for line in output.lines() {
        if line.starts_with("diff --git") || line.starts_with("@@") {
            context_run = 0;
            result.push(line.to_string());
        } else if line.starts_with("index ") {
            // skip index lines
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            result.push(line.to_string());
        } else if line.starts_with('+') || line.starts_with('-') {
            context_run = 0;
            result.push(line.to_string());
        } else {
            context_run += 1;
            if context_run <= 3 {
                result.push(line.to_string());
            }
        }
    }

    if result.is_empty() {
        return output.to_string();
    }
    result.join("\n")
}
