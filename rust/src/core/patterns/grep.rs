use std::collections::HashMap;

pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 3 || lines.len() <= 100 {
        return Some(output.to_string());
    }

    let mut by_file: HashMap<&str, Vec<(usize, &str)>> = HashMap::new();
    let mut total_matches = 0usize;

    for line in &lines {
        if let Some((file, rest)) = parse_grep_line(line) {
            total_matches += 1;
            let line_num = extract_line_num(rest);
            let content = strip_line_num(rest);
            by_file.entry(file).or_default().push((line_num, content));
        }
    }

    if total_matches == 0 {
        return None;
    }

    let mut result = format!("{total_matches} matches in {}F:\n", by_file.len());
    let mut sorted_files: Vec<_> = by_file.iter().collect();
    sorted_files.sort_by_key(|(_, matches)| std::cmp::Reverse(matches.len()));

    for (file, matches) in &sorted_files {
        let short = shorten_path(file);
        result.push_str(&format!("\n{short} ({}):", matches.len()));
        let show = matches.iter().take(10);
        for (ln, content) in show {
            let trimmed = content.trim();
            let short_content = if trimmed.len() > 160 {
                let truncated: String = trimmed.chars().take(159).collect();
                format!("{truncated}…")
            } else {
                trimmed.to_string()
            };
            if *ln > 0 {
                result.push_str(&format!("\n  {ln}: {short_content}"));
            } else {
                result.push_str(&format!("\n  {short_content}"));
            }
        }
        if matches.len() > 10 {
            result.push_str(&format!("\n  ... +{} more", matches.len() - 10));
        }
    }

    Some(result)
}

fn parse_grep_line(line: &str) -> Option<(&str, &str)> {
    if let Some(pos) = line.find(':') {
        let file = &line[..pos];
        if file.contains('/') || file.contains('.') {
            let rest = &line[pos + 1..];
            return Some((file, rest));
        }
    }
    None
}

fn extract_line_num(rest: &str) -> usize {
    if let Some(pos) = rest.find(':') {
        rest[..pos].parse().unwrap_or(0)
    } else {
        0
    }
}

fn strip_line_num(rest: &str) -> &str {
    if let Some(pos) = rest.find(':') {
        if rest[..pos].chars().all(|c| c.is_ascii_digit()) {
            return &rest[pos + 1..];
        }
    }
    rest
}

fn shorten_path(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}
