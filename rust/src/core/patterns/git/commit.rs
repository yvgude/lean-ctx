use super::{
    clone_objects_re, commit_hash_re, compact_lines, extract_change_stats, is_diff_or_stat_line,
    stash_re,
};

pub(super) fn compress_add(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= 3 {
        return trimmed.to_string();
    }
    format!("ok (+{} files)", lines.len())
}

pub(super) fn compress_commit(output: &str) -> String {
    let mut hook_lines: Vec<&str> = Vec::new();
    let mut commit_part = String::new();
    let mut found_commit = false;

    for line in output.lines() {
        if !found_commit && commit_hash_re().is_match(line) {
            found_commit = true;
        }
        if !found_commit {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                hook_lines.push(trimmed);
            }
        }
    }

    if let Some(caps) = commit_hash_re().captures(output) {
        let branch = &caps[1];
        let hash = &caps[2];
        let stats = extract_change_stats(output);
        let msg = output
            .lines()
            .find(|l| commit_hash_re().is_match(l))
            .and_then(|l| l.split(']').nth(1))
            .map_or("", str::trim);
        commit_part = if stats.is_empty() {
            format!("{hash} ({branch}) {msg}")
        } else {
            format!("{hash} ({branch}) {msg} [{stats}]")
        };
    }

    if commit_part.is_empty() {
        let trimmed = output.trim();
        if trimmed.is_empty() {
            return "ok".to_string();
        }
        return compact_lines(trimmed, 5);
    }

    if hook_lines.is_empty() {
        return commit_part;
    }

    let failed: Vec<&&str> = hook_lines
        .iter()
        .filter(|l| {
            let low = l.to_lowercase();
            low.contains("failed") || low.contains("error") || low.contains("warning")
        })
        .collect();
    let passed_count = hook_lines.len() - failed.len();

    let hook_output = if !failed.is_empty() {
        let mut parts = Vec::new();
        if passed_count > 0 {
            parts.push(format!("{passed_count} checks passed"));
        }
        for f in failed.iter().take(5) {
            parts.push(f.to_string());
        }
        if failed.len() > 5 {
            parts.push(format!("... ({} more failures)", failed.len() - 5));
        }
        parts.join("\n")
    } else if hook_lines.len() > 5 {
        format!("{} hooks passed", hook_lines.len())
    } else {
        hook_lines.join("\n")
    };

    format!("{hook_output}\n{commit_part}")
}

pub(super) fn compress_push(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut ref_line = String::new();
    let mut remote_urls: Vec<String> = Vec::new();
    let mut rejected = false;

    for line in trimmed.lines() {
        let l = line.trim();

        if l.contains("rejected") {
            rejected = true;
        }

        if l.contains("->") && !l.starts_with("remote:") {
            ref_line = l.to_string();
        }

        if l.contains("Everything up-to-date") {
            return "ok (up-to-date)".to_string();
        }

        if l.starts_with("remote:") || l.starts_with("To ") {
            let content = l.trim_start_matches("remote:").trim();
            if content.contains("http")
                || content.contains("pipeline")
                || content.contains("merge_request")
                || content.contains("pull/")
            {
                remote_urls.push(content.to_string());
            }
        }
    }

    if rejected {
        let reject_lines: Vec<&str> = trimmed
            .lines()
            .filter(|l| l.contains("rejected") || l.contains("error") || l.contains("remote:"))
            .collect();
        return format!("REJECTED:\n{}", compact_lines(&reject_lines.join("\n"), 5));
    }

    let mut parts = Vec::new();
    if ref_line.is_empty() {
        parts.push("ok (pushed)".to_string());
    } else {
        parts.push(format!("ok {ref_line}"));
    }
    for url in &remote_urls {
        parts.push(url.clone());
    }

    parts.join("\n")
}

pub(super) fn compress_pull(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.contains("Already up to date") {
        return "ok (up-to-date)".to_string();
    }

    let stats = extract_change_stats(trimmed);
    if !stats.is_empty() {
        return format!("ok {stats}");
    }

    compact_lines(trimmed, 5)
}

pub(super) fn compress_fetch(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut new_branches = Vec::new();
    for line in trimmed.lines() {
        let l = line.trim();
        if l.contains("[new branch]") || l.contains("[new tag]") {
            if let Some(name) = l.split("->").last() {
                new_branches.push(name.trim().to_string());
            }
        }
    }

    if new_branches.is_empty() {
        return "ok (fetched)".to_string();
    }
    format!("ok (new: {})", new_branches.join(", "))
}

pub(super) fn compress_clone(output: &str) -> String {
    let mut objects = 0u32;
    for line in output.lines() {
        if let Some(caps) = clone_objects_re().captures(line) {
            objects = caps[1].parse().unwrap_or(0);
        }
    }

    let into = output
        .lines()
        .find(|l| l.contains("Cloning into"))
        .and_then(|l| l.split('\'').nth(1))
        .unwrap_or("repo");

    if objects > 0 {
        format!("cloned '{into}' ({objects} objects)")
    } else {
        format!("cloned '{into}'")
    }
}

pub(super) fn compress_branch(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let branches: Vec<String> = trimmed
        .lines()
        .filter_map(|line| {
            let l = line.trim();
            if l.is_empty() {
                return None;
            }
            if let Some(rest) = l.strip_prefix('*') {
                Some(format!("*{}", rest.trim()))
            } else {
                Some(l.to_string())
            }
        })
        .collect();

    branches.join(", ")
}

pub(super) fn compress_checkout(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    for line in trimmed.lines() {
        let l = line.trim();
        if l.starts_with("Switched to") || l.starts_with("Already on") {
            let branch = l.split('\'').nth(1).unwrap_or(l);
            return format!("→ {branch}");
        }
    }

    compact_lines(trimmed, 3)
}

pub(super) fn compress_merge(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.contains("Already up to date") {
        return "ok (up-to-date)".to_string();
    }
    if trimmed.contains("CONFLICT") {
        let conflicts: Vec<&str> = trimmed.lines().filter(|l| l.contains("CONFLICT")).collect();
        return format!(
            "CONFLICT ({} files):\n{}",
            conflicts.len(),
            conflicts.join("\n")
        );
    }

    let stats = extract_change_stats(trimmed);
    if !stats.is_empty() {
        return format!("merged {stats}");
    }
    compact_lines(trimmed, 3)
}

pub(super) fn compress_stash(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let line_count = trimmed.lines().count();
    if line_count <= 5 {
        return trimmed.to_string();
    }

    let stashes: Vec<String> = trimmed
        .lines()
        .filter_map(|line| {
            stash_re()
                .captures(line)
                .map(|caps| format!("@{}: {}", &caps[1], &caps[2]))
        })
        .collect();

    if stashes.is_empty() {
        return compact_lines(trimmed, 30);
    }
    stashes.join("\n")
}

pub(super) fn compress_tag(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let tags: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).collect();
    if tags.len() <= 10 {
        return tags.join(", ");
    }
    format!("{} (... {} total)", tags[..5].join(", "), tags.len())
}

pub(super) fn compress_reset(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut unstaged: Vec<&str> = Vec::new();
    for line in trimmed.lines() {
        let l = line.trim();
        if l.starts_with("Unstaged changes after reset:") {
            continue;
        }
        if l.starts_with('M') || l.starts_with('D') || l.starts_with('A') {
            unstaged.push(l);
        }
    }

    if unstaged.is_empty() {
        return compact_lines(trimmed, 3);
    }
    format!("reset ok ({} files unstaged)", unstaged.len())
}

pub(super) fn compress_remote(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut remotes = std::collections::HashMap::new();
    for line in trimmed.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            remotes
                .entry(parts[0].to_string())
                .or_insert_with(|| parts[1].to_string());
        }
    }

    if remotes.is_empty() {
        return trimmed.to_string();
    }

    remotes
        .iter()
        .map(|(name, url)| format!("{name}: {url}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn compress_blame(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 100 {
        return output.to_string();
    }

    let unique_authors: std::collections::HashSet<&str> = lines
        .iter()
        .filter_map(|l| l.split('(').nth(1)?.split_whitespace().next())
        .collect();

    let mut result = format!("{} lines, {} authors:\n", lines.len(), unique_authors.len());
    let mut current_author = String::new();
    let mut range_start = 0usize;
    let mut range_end = 0usize;

    for (i, line) in lines.iter().enumerate() {
        let author = line
            .split('(')
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("?");
        if author == current_author {
            range_end = i + 1;
        } else {
            if !current_author.is_empty() {
                result.push_str(&format!(
                    "  L{}-{}: {current_author}\n",
                    range_start + 1,
                    range_end
                ));
            }
            current_author = author.to_string();
            range_start = i;
            range_end = i + 1;
        }
    }
    if !current_author.is_empty() {
        result.push_str(&format!(
            "  L{}-{}: {current_author}\n",
            range_start + 1,
            range_end
        ));
    }
    result.trim_end().to_string()
}

pub(super) fn compress_cherry_pick(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    if trimmed.contains("CONFLICT") {
        return "CONFLICT (cherry-pick)".to_string();
    }
    let stats = extract_change_stats(trimmed);
    if !stats.is_empty() {
        return format!("ok {stats}");
    }
    compact_lines(trimmed, 3)
}

pub(super) fn compress_show(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut hash = String::new();
    let mut message = String::new();
    let mut diff_start: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("commit ") && hash.is_empty() {
            hash = trimmed[7..14.min(trimmed.len())].to_string();
        } else if trimmed.starts_with("diff --git") && diff_start.is_none() {
            diff_start = Some(i);
        } else if !trimmed.is_empty()
            && !trimmed.starts_with("Author:")
            && !trimmed.starts_with("Date:")
            && !trimmed.starts_with("Merge:")
            && !is_diff_or_stat_line(trimmed)
            && message.is_empty()
            && !hash.is_empty()
            && diff_start.is_none()
        {
            message = trimmed.to_string();
        }
    }

    if hash.is_empty() {
        return compact_lines(output.trim(), 10);
    }

    let mut result = format!("{hash} {message}");

    if let Some(start) = diff_start {
        let diff_portion: String = lines[start..].join("\n");
        let compressed_diff = super::diff::compress_diff_keep_hunks(&diff_portion);
        result.push('\n');
        result.push_str(&compressed_diff);
    } else {
        let stats = extract_change_stats(output);
        if !stats.is_empty() {
            result.push_str(&format!(" [{stats}]"));
        }
    }

    result
}

pub(super) fn compress_rebase(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    if trimmed.contains("Already up to date") || trimmed.contains("is up to date") {
        return "ok (up-to-date)".to_string();
    }
    if trimmed.contains("Successfully rebased") {
        let stats = extract_change_stats(trimmed);
        return if stats.is_empty() {
            "ok (rebased)".to_string()
        } else {
            format!("ok (rebased) {stats}")
        };
    }
    if trimmed.contains("CONFLICT") {
        let conflicts: Vec<&str> = trimmed.lines().filter(|l| l.contains("CONFLICT")).collect();
        return format!(
            "CONFLICT ({} files):\n{}",
            conflicts.len(),
            conflicts.join("\n")
        );
    }
    compact_lines(trimmed, 5)
}

pub(super) fn compress_submodule(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut modules = Vec::new();
    for line in trimmed.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let status_char = if line.starts_with('+') {
                "~"
            } else if line.starts_with('-') {
                "!"
            } else {
                ""
            };
            modules.push(format!("{status_char}{}", parts.last().unwrap_or(&"?")));
        }
    }

    if modules.is_empty() {
        return compact_lines(trimmed, 5);
    }
    format!("{} submodules: {}", modules.len(), modules.join(", "))
}

pub(super) fn compress_worktree(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut worktrees = Vec::new();
    let mut current_path = String::new();
    let mut current_branch = String::new();

    for line in trimmed.lines() {
        let l = line.trim();
        if !l.contains(' ') && !l.is_empty() && current_path.is_empty() {
            current_path = l.to_string();
        } else if l.starts_with("HEAD ") {
            // skip
        } else if l.starts_with("branch ") || l.contains("detached") || l.contains("bare") {
            current_branch = l.to_string();
        } else if l.is_empty() && !current_path.is_empty() {
            let short_path = current_path.rsplit('/').next().unwrap_or(&current_path);
            worktrees.push(format!("{short_path} [{current_branch}]"));
            current_path.clear();
            current_branch.clear();
        }
    }
    if !current_path.is_empty() {
        let short_path = current_path.rsplit('/').next().unwrap_or(&current_path);
        worktrees.push(format!("{short_path} [{current_branch}]"));
    }

    if worktrees.is_empty() {
        return compact_lines(trimmed, 5);
    }
    format!("{} worktrees:\n{}", worktrees.len(), worktrees.join("\n"))
}

pub(super) fn compress_bisect(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    for line in trimmed.lines() {
        let l = line.trim();
        if l.contains("is the first bad commit") {
            let hash = l.split_whitespace().next().unwrap_or("?");
            let short = &hash[..7.min(hash.len())];
            return format!("found: {short} is first bad commit");
        }
        if l.starts_with("Bisecting:") {
            return l.to_string();
        }
    }

    compact_lines(trimmed, 5)
}
