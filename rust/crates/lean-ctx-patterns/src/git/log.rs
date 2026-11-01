// SPDX-License-Identifier: Apache-2.0

use super::{numstat_row_re, shortstat_line_re};

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
    // Totals come only from sources git itself makes unambiguous (#1893):
    // its `--stat`/`--shortstat` summary lines, `--numstat` rows, or `+`/`-`
    // lines inside real `@@` hunks. Commit bodies are indented by four spaces
    // and never counted — a markdown bullet (`- fix …`) is not a deletion.
    let mut shortstat = ChangeTotals::default();
    let mut numstat = ChangeTotals::default();
    let mut hunks = ChangeTotals::default();

    let mut entries = Vec::new();
    let mut in_hunk = false;
    let mut got_message = false;

    for line in lines {
        if line.starts_with("commit ") {
            let hash = &line[7..14.min(line.len())];
            entries.push(hash.to_string());
            in_hunk = false;
            got_message = false;
            continue;
        }
        if line.starts_with("diff --git") {
            in_hunk = false;
            hunks.seen = true;
            continue;
        }
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if in_hunk {
            if line.starts_with('+') {
                hunks.additions += 1;
                continue;
            }
            if line.starts_with('-') {
                hunks.deletions += 1;
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\\') {
                continue;
            }
            in_hunk = false;
        }
        if let Some(caps) = shortstat_line_re().captures(line) {
            shortstat.seen = true;
            shortstat.additions += capture_u32(&caps, 1);
            shortstat.deletions += capture_u32(&caps, 2);
            continue;
        }
        if let Some(caps) = numstat_row_re().captures(line) {
            numstat.seen = true;
            numstat.additions += capture_u32(&caps, 1);
            numstat.deletions += capture_u32(&caps, 2);
            continue;
        }
        let trimmed = line.trim();
        let is_header = ["Author:", "Date:", "Merge:"]
            .iter()
            .any(|h| trimmed.starts_with(h));
        // The subject is the first body line after the header block. git
        // indents it by four spaces, but re-emitted logs may not — don't
        // require the indent, or the subject silently disappears.
        if !got_message && !is_header && !trimmed.is_empty() {
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

    let totals = [shortstat, numstat, hunks].into_iter().find(|t| t.seen);
    if let Some(t) = totals.filter(|t| t.additions > 0 || t.deletions > 0) {
        result.push_str(&format!(
            "\n[{} commits, +{}/-{} total]",
            entries.len(),
            t.additions,
            t.deletions
        ));
    }

    result
}

#[derive(Default, Clone, Copy)]
struct ChangeTotals {
    seen: bool,
    additions: u32,
    deletions: u32,
}

fn capture_u32(caps: &regex::Captures<'_>, idx: usize) -> u32 {
    caps.get(idx)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::compress_log;

    const BULLET_BODY_STAT: &str = "commit 8c93333c58aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
Merge: efd449b d411020
Author: A <a@example.com>
Date:   Mon Sep 28 10:00:00 2026 +0200

    Merge pull request #1883

commit d4110207aabbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
Author: A <a@example.com>
Date:   Mon Sep 28 09:00:00 2026 +0200

    refactor(updater): move release-asset selection

    - moves platform matching
    - drops the old helper
    --- not a diff header either

 rust/src/updater/mod.rs      | 40 +++++-----------
 rust/src/updater/platform.rs | 12 ++++++
 2 files changed, 20 insertions(+), 32 deletions(-)

commit 7994901f1dcccccccccccccccccccccccccccccccc
Author: A <a@example.com>
Date:   Sun Sep 27 09:00:00 2026 +0200

    fix(text_decode): use as_chunks

    - one
    - two
    + not an addition

 rust/src/text_decode.rs | 1 +
 1 file changed, 1 insertion(+)
";

    #[test]
    fn stat_totals_come_from_git_summary_lines_not_body_bullets() {
        let out = compress_log("git log --stat -3", BULLET_BODY_STAT);
        assert!(out.contains("[3 commits, +21/-32 total]"), "{out}");
        assert!(out.contains("d411020 refactor(updater): move release-asset selection"));
        assert!(out.contains("8c93333 Merge pull request #1883"));
    }

    #[test]
    fn body_bullets_without_stats_produce_no_totals() {
        let input = "commit abcdef0123456789\nAuthor: A <a@e.com>\nDate:   Mon\n\n    feat: x\n\n    - a bullet\n    - another\n";
        let out = compress_log("git log -1", input);
        assert!(
            !out.contains("total]"),
            "no reliable source → no totals: {out}"
        );
        assert!(out.contains("abcdef0 feat: x"));
    }

    #[test]
    fn shortstat_only_output_is_summed() {
        let input = "commit aaaaaaa1111111\nAuthor: A\nDate:   Mon\n\n    one\n\n 1 file changed, 5 insertions(+)\n\ncommit bbbbbbb2222222\nAuthor: A\nDate:   Mon\n\n    two\n\n 3 files changed, 2 insertions(+), 9 deletions(-)\n";
        let out = compress_log("git log --shortstat", input);
        assert!(out.contains("[2 commits, +7/-9 total]"), "{out}");
    }

    #[test]
    fn numstat_rows_are_summed() {
        let input = "commit aaaaaaa1111111\nAuthor: A\nDate:   Mon\n\n    - bullet body\n\n10\t2\tsrc/a.rs\n-\t-\tlogo.png\n3\t0\tsrc/b.rs\n";
        let out = compress_log("git log --numstat", input);
        assert!(out.contains("[1 commits, +13/-2 total]"), "{out}");
    }

    #[test]
    fn hunk_lines_count_only_inside_hunks() {
        // Reached without -p in the command (e.g. `log.showDiff`/aliases).
        let input = "commit aaaaaaa1111111\nAuthor: A\nDate:   Mon\n\n    - body bullet\n\ndiff --git a/q.sql b/q.sql\nindex 1..2 100644\n--- a/q.sql\n+++ b/q.sql\n@@ -1,2 +1,2 @@\n--- removed sql comment\n+-- added sql comment\n unchanged\n";
        let out = compress_log("git log", input);
        assert!(out.contains("[1 commits, +1/-1 total]"), "{out}");
    }
}
