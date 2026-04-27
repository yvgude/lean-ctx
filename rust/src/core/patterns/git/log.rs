use super::is_diff_or_stat_line;

pub(super) fn compress_log(command: &str, output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let user_limited = command.contains("-n ")
        || command.contains("-n=")
        || command.contains("--max-count")
        || command.contains("-1")
        || command.contains("-2")
        || command.contains("-3")
        || command.contains("-5")
        || command.contains("-10");

    let max_entries: usize = if user_limited { usize::MAX } else { 100 };

    let is_oneline = !lines[0].starts_with("commit ");
    if is_oneline {
        if lines.len() <= max_entries {
            return lines.join("\n");
        }
        let shown = &lines[..max_entries];
        return format!(
            "{}\n... ({} more commits, use git log --max-count=N to see all)",
            shown.join("\n"),
            lines.len() - max_entries
        );
    }

    let has_diff = lines.iter().any(|l| l.starts_with("diff --git"));
    let has_stat = lines
        .iter()
        .any(|l| l.contains(" | ") && l.trim().ends_with(['+', '-']));
    let mut total_additions = 0u32;
    let mut total_deletions = 0u32;

    let mut entries = Vec::new();
    let mut in_diff = false;
    let mut got_message = false;

    for line in &lines {
        let trimmed = line.trim();

        if trimmed.starts_with("commit ") {
            let hash = &trimmed[7..14.min(trimmed.len())];
            entries.push(hash.to_string());
            in_diff = false;
            got_message = false;
            continue;
        }

        if trimmed.starts_with("Author:")
            || trimmed.starts_with("Date:")
            || trimmed.starts_with("Merge:")
        {
            continue;
        }

        if trimmed.starts_with("diff --git") || trimmed.starts_with("---") && trimmed.contains("a/")
        {
            in_diff = true;
        }

        if in_diff || is_diff_or_stat_line(trimmed) {
            if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
                total_additions += 1;
            } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
                total_deletions += 1;
            }
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        if !got_message {
            if let Some(last) = entries.last_mut() {
                *last = format!("{last} {trimmed}");
            }
            got_message = true;
        }
    }

    if entries.is_empty() {
        return output.to_string();
    }

    let mut result = if entries.len() > max_entries {
        let shown = &entries[..max_entries];
        format!(
            "{}\n... ({} more commits, use git log --max-count=N to see all)",
            shown.join("\n"),
            entries.len() - max_entries
        )
    } else {
        entries.join("\n")
    };

    if (has_diff || has_stat) && (total_additions > 0 || total_deletions > 0) {
        result.push_str(&format!(
            "\n[{} commits, +{total_additions}/-{total_deletions} total]",
            entries.len()
        ));
    }

    result
}
