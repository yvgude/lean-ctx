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

    let has_patches =
        command.contains("-p") || command.contains("--patch") || command.contains("--diff");

    let commits = split_into_commits(output);
    let commit_count = commits.len();

    if has_patches && commit_count > 0 {
        return compress_log_with_patches(&commits, commit_count, max_entries);
    }

    compress_log_summary(&lines, max_entries)
}

struct CommitBlock {
    header: String,
    message: String,
    diff_content: String,
    files_changed: Vec<String>,
    additions: u32,
    deletions: u32,
}

fn split_into_commits(output: &str) -> Vec<CommitBlock> {
    let mut commits = Vec::new();
    let mut current_header = String::new();
    let mut current_message = String::new();
    let mut current_diff = String::new();
    let mut current_files: Vec<String> = Vec::new();
    let mut additions: u32 = 0;
    let mut deletions: u32 = 0;
    let mut in_header = false;
    let mut in_diff = false;
    let mut got_message = false;

    for line in output.lines() {
        if line.starts_with("commit ") && line.len() >= 10 {
            if in_header || in_diff || got_message {
                commits.push(CommitBlock {
                    header: current_header.clone(),
                    message: current_message.clone(),
                    diff_content: current_diff.clone(),
                    files_changed: current_files.clone(),
                    additions,
                    deletions,
                });
            }
            let hash = &line[7..14.min(line.len())];
            current_header = hash.to_string();
            current_message = String::new();
            current_diff = String::new();
            current_files = Vec::new();
            additions = 0;
            deletions = 0;
            in_header = true;
            in_diff = false;
            got_message = false;
            continue;
        }

        if in_header
            && (line.starts_with("Author:")
                || line.starts_with("Date:")
                || line.starts_with("Merge:"))
        {
            continue;
        }

        if line.starts_with("diff --git") {
            in_diff = true;
            in_header = false;
            if let Some(name) = line.split(" b/").nth(1) {
                current_files.push(name.to_string());
            }
            current_diff.push_str(line);
            current_diff.push('\n');
            continue;
        }

        if in_diff {
            if line.starts_with('+') && !line.starts_with("+++") {
                additions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
            current_diff.push_str(line);
            current_diff.push('\n');
            continue;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            if in_header {
                in_header = false;
            }
            continue;
        }

        if !got_message && !in_diff {
            current_message = trimmed.to_string();
            got_message = true;
        }
    }

    if !current_header.is_empty() {
        commits.push(CommitBlock {
            header: current_header,
            message: current_message,
            diff_content: current_diff,
            files_changed: current_files,
            additions,
            deletions,
        });
    }

    commits
}

fn compress_log_with_patches(
    commits: &[CommitBlock],
    commit_count: usize,
    max_entries: usize,
) -> String {
    let mut result = Vec::new();

    if commit_count <= 3 {
        for c in commits.iter().take(max_entries) {
            result.push(format!("{} {}", c.header, c.message));
            if !c.diff_content.is_empty() {
                let compressed = super::diff::compress_diff_keep_hunks(&c.diff_content);
                result.push(compressed);
            }
            result.push(String::new());
        }
    } else if commit_count <= 20 {
        if let Some(first) = commits.first() {
            result.push(format!("{} {}", first.header, first.message));
            if !first.diff_content.is_empty() {
                let compressed = super::diff::compress_diff_keep_hunks(&first.diff_content);
                result.push(compressed);
            }
            result.push(String::new());
        }

        for c in commits.iter().skip(1).take(max_entries.saturating_sub(1)) {
            let files_str = if c.files_changed.is_empty() {
                String::new()
            } else {
                format!(" [{}]", c.files_changed.join(", "))
            };
            let stats = if c.additions > 0 || c.deletions > 0 {
                format!(" +{}/-{}", c.additions, c.deletions)
            } else {
                String::new()
            };
            result.push(format!("{} {}{}{}", c.header, c.message, files_str, stats));
        }

        let total_add: u32 = commits.iter().map(|c| c.additions).sum();
        let total_del: u32 = commits.iter().map(|c| c.deletions).sum();
        if total_add > 0 || total_del > 0 {
            result.push(format!(
                "\n[{commit_count} commits, +{total_add}/-{total_del} total]"
            ));
        }
    } else {
        for c in commits.iter().take(max_entries) {
            result.push(format!("{} {}", c.header, c.message));
        }
        if commits.len() > max_entries {
            result.push(format!(
                "... ({} more commits)",
                commits.len() - max_entries
            ));
        }
        let total_add: u32 = commits.iter().map(|c| c.additions).sum();
        let total_del: u32 = commits.iter().map(|c| c.deletions).sum();
        if total_add > 0 || total_del > 0 {
            result.push(format!(
                "[{commit_count} commits, +{total_add}/-{total_del} total]"
            ));
        }
    }

    result.join("\n")
}

fn compress_log_summary(lines: &[&str], max_entries: usize) -> String {
    let has_diff = lines.iter().any(|l| l.starts_with("diff --git"));
    let has_stat = lines
        .iter()
        .any(|l| l.contains(" | ") && l.trim().ends_with(['+', '-']));
    let mut total_additions = 0u32;
    let mut total_deletions = 0u32;

    let mut entries = Vec::new();
    let mut in_diff = false;
    let mut got_message = false;

    for line in lines {
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
        return lines.join("\n");
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
