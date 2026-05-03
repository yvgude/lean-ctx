use super::{ahead_re, compact_lines, status_branch_re};

pub(super) fn compress_status(output: &str) -> String {
    let mut branch = String::new();
    let mut ahead = 0u32;
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    let mut section = "";

    for line in output.lines() {
        if let Some(caps) = status_branch_re().captures(line) {
            branch = caps[1].to_string();
        }
        if let Some(caps) = ahead_re().captures(line) {
            ahead = caps[1].parse().unwrap_or(0);
        }

        if line.contains("Changes to be committed") {
            section = "staged";
        } else if line.contains("Changes not staged") {
            section = "unstaged";
        } else if line.contains("Untracked files") {
            section = "untracked";
        }

        let trimmed = line.trim();
        if trimmed.starts_with("new file:") {
            let file = trimmed.trim_start_matches("new file:").trim();
            if section == "staged" {
                staged.push(format!("+{file}"));
            }
        } else if trimmed.starts_with("modified:") {
            let file = trimmed.trim_start_matches("modified:").trim();
            match section {
                "staged" => staged.push(format!("~{file}")),
                "unstaged" => unstaged.push(format!("~{file}")),
                _ => {}
            }
        } else if trimmed.starts_with("deleted:") {
            let file = trimmed.trim_start_matches("deleted:").trim();
            if section == "staged" {
                staged.push(format!("-{file}"));
            }
        } else if trimmed.starts_with("renamed:") {
            let file = trimmed.trim_start_matches("renamed:").trim();
            if section == "staged" {
                staged.push(format!("→{file}"));
            }
        } else if trimmed.starts_with("copied:") {
            let file = trimmed.trim_start_matches("copied:").trim();
            if section == "staged" {
                staged.push(format!("©{file}"));
            }
        } else if section == "untracked"
            && !trimmed.is_empty()
            && !trimmed.starts_with('(')
            && !trimmed.starts_with("Untracked")
        {
            untracked.push(trimmed.to_string());
        }
    }

    if branch.is_empty() && staged.is_empty() && unstaged.is_empty() && untracked.is_empty() {
        return compact_lines(output.trim(), 10);
    }

    let mut parts = Vec::new();
    let branch_display = if branch.is_empty() {
        "?".to_string()
    } else {
        branch
    };
    let ahead_str = if ahead > 0 {
        format!(" ↑{ahead}")
    } else {
        String::new()
    };
    parts.push(format!("{branch_display}{ahead_str}"));

    if !staged.is_empty() {
        parts.push(format!("staged: {}", staged.join(" ")));
    }
    if !unstaged.is_empty() {
        parts.push(format!("unstaged: {}", unstaged.join(" ")));
    }
    if !untracked.is_empty() {
        parts.push(format!("untracked: {}", untracked.join(" ")));
    }

    if output.contains("nothing to commit") && parts.len() == 1 {
        parts.push("clean".to_string());
    }

    parts.join("\n")
}
