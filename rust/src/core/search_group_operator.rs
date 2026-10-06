// SPDX-License-Identifier: Apache-2.0

//! Pure deterministic grouping/ranking candidate for grep-style search output.

use std::collections::HashMap;

/// Group `path:line:content` matches by file, rank files by match count and
/// apply the established Local per-file limits. Product-specific benefit and
/// plan-budget gates remain with the caller.
pub fn group_matches(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 3 {
        return None;
    }

    let mut by_file: HashMap<&str, Vec<(usize, &str)>> = HashMap::new();
    let mut total_matches = 0usize;
    for line in &lines {
        if let Some((file, rest)) = parse_grep_line(line) {
            total_matches += 1;
            by_file
                .entry(file)
                .or_default()
                .push((extract_line_num(rest), strip_line_num(rest)));
        }
    }
    if total_matches == 0 {
        return None;
    }

    let max_matches_per_file = if total_matches > 200 { 5 } else { 10 };
    let mut result = format!("{total_matches} matches in {}F:\n", by_file.len());
    let mut sorted_files: Vec<_> = by_file.iter().collect();
    sorted_files.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));

    for (file, matches) in sorted_files {
        result.push_str(&format!("\n{} ({}):", shorten_path(file), matches.len()));
        for (line, content) in matches.iter().take(max_matches_per_file) {
            let trimmed = content.trim();
            let short_content = if trimmed.len() > 120 {
                format!("{}…", trimmed.chars().take(119).collect::<String>())
            } else {
                trimmed.to_string()
            };
            if *line > 0 {
                result.push_str(&format!("\n  {line}: {short_content}"));
            } else {
                result.push_str(&format!("\n  {short_content}"));
            }
        }
        if matches.len() > max_matches_per_file {
            result.push_str(&format!(
                "\n  ... +{} more",
                matches.len() - max_matches_per_file
            ));
        }
    }
    Some(result)
}

fn parse_grep_line(line: &str) -> Option<(&str, &str)> {
    let start = if line.len() >= 2
        && line.as_bytes()[0].is_ascii_alphabetic()
        && line.as_bytes()[1] == b':'
    {
        2
    } else {
        0
    };
    let position = line[start..].find(':')? + start;
    let file = &line[..position];
    (file.contains('/') || file.contains('\\') || file.contains('.'))
        .then_some((file, &line[position + 1..]))
}

fn extract_line_num(rest: &str) -> usize {
    rest.find(':')
        .and_then(|position| rest[..position].parse().ok())
        .unwrap_or(0)
}

fn strip_line_num(rest: &str) -> &str {
    if let Some(position) = rest.find(':')
        && rest[..position]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return &rest[position + 1..];
    }
    rest
}

fn shorten_path(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_and_output_are_deterministic() {
        let input = [
            "src/z.rs:1:z-one",
            "src/a.rs:1:a-one",
            "src/z.rs:2:z-two",
            "src/a.rs:2:a-two",
            "src/z.rs:3:z-three",
        ]
        .join("\n");
        let first = group_matches(&input).unwrap();
        assert_eq!(first, group_matches(&input).unwrap());
        assert!(first.find("src/z.rs").unwrap() < first.find("src/a.rs").unwrap());
    }

    #[test]
    fn lexical_path_breaks_equal_match_count_ties() {
        let input = [
            "src/z.rs:1:z-one",
            "src/a.rs:1:a-one",
            "src/z.rs:2:z-two",
            "src/a.rs:2:a-two",
        ]
        .join("\n");
        let output = group_matches(&input).unwrap();
        assert!(output.find("src/a.rs").unwrap() < output.find("src/z.rs").unwrap());
    }
}
