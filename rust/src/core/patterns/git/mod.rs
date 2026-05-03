mod commit;
mod diff;
mod log;
mod parser;
mod status;

use parser::extract_git_subcommand;

macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn status_branch_re() -> &'static regex::Regex {
    static_regex!(r"On branch (\S+)")
}
fn ahead_re() -> &'static regex::Regex {
    static_regex!(r"ahead of .+ by (\d+) commit")
}
fn commit_hash_re() -> &'static regex::Regex {
    static_regex!(r"\[([\w/.:-]+)\s+([a-f0-9]+)\]")
}
fn insertions_re() -> &'static regex::Regex {
    static_regex!(r"(\d+) insertions?\(\+\)")
}
fn deletions_re() -> &'static regex::Regex {
    static_regex!(r"(\d+) deletions?\(-\)")
}
fn files_changed_re() -> &'static regex::Regex {
    static_regex!(r"(\d+) files? changed")
}
fn clone_objects_re() -> &'static regex::Regex {
    static_regex!(r"Receiving objects:.*?(\d+)")
}
fn stash_re() -> &'static regex::Regex {
    static_regex!(r"stash@\{(\d+)\}:\s*(.+)")
}

fn is_diff_or_stat_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("diff --git")
        || t.starts_with("index ")
        || t.starts_with("--- a/")
        || t.starts_with("+++ b/")
        || t.starts_with("@@ ")
        || t.starts_with("Binary files")
        || t.starts_with("new file mode")
        || t.starts_with("deleted file mode")
        || t.starts_with("old mode")
        || t.starts_with("new mode")
        || t.starts_with("similarity index")
        || t.starts_with("rename from")
        || t.starts_with("rename to")
        || t.starts_with("copy from")
        || t.starts_with("copy to")
        || (t.starts_with('+') && !t.starts_with("+++"))
        || (t.starts_with('-') && !t.starts_with("---"))
        || (t.contains(" | ") && t.chars().any(|c| c == '+' || c == '-'))
}

fn extract_change_stats(output: &str) -> String {
    let files = files_changed_re()
        .captures(output)
        .and_then(|c| c[1].parse::<u32>().ok())
        .unwrap_or(0);
    let ins = insertions_re()
        .captures(output)
        .and_then(|c| c[1].parse::<u32>().ok())
        .unwrap_or(0);
    let del = deletions_re()
        .captures(output)
        .and_then(|c| c[1].parse::<u32>().ok())
        .unwrap_or(0);

    if files > 0 || ins > 0 || del > 0 {
        format!("{files} files, +{ins}/-{del}")
    } else {
        String::new()
    }
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

pub fn compress(command: &str, output: &str) -> Option<String> {
    let sub = extract_git_subcommand(command)?;
    match sub {
        "status" => Some(status::compress_status(output)),
        "log" => Some(log::compress_log(command, output)),
        "diff" => Some(diff::compress_diff(output)),
        "add" => Some(commit::compress_add(output)),
        "commit" => Some(commit::compress_commit(output)),
        "push" => Some(commit::compress_push(output)),
        "pull" => Some(commit::compress_pull(output)),
        "fetch" => Some(commit::compress_fetch(output)),
        "clone" => Some(commit::compress_clone(output)),
        "branch" => Some(commit::compress_branch(output)),
        "checkout" | "switch" => Some(commit::compress_checkout(output)),
        "merge" => Some(commit::compress_merge(output)),
        "stash" => {
            if command.contains("stash show") || command.contains("show stash") {
                return Some(commit::compress_show(output));
            }
            Some(commit::compress_stash(output))
        }
        "tag" => Some(commit::compress_tag(output)),
        "reset" => Some(commit::compress_reset(output)),
        "remote" => {
            if command.contains("remote add") {
                return Some(commit::compress_add(output));
            }
            Some(commit::compress_remote(output))
        }
        "blame" => Some(commit::compress_blame(output)),
        "cherry-pick" => Some(commit::compress_cherry_pick(output)),
        "show" => Some(commit::compress_show(output)),
        "rebase" => Some(commit::compress_rebase(output)),
        "submodule" => Some(commit::compress_submodule(output)),
        "worktree" => Some(commit::compress_worktree(output)),
        "bisect" => Some(commit::compress_bisect(output)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_compresses() {
        let output = "On branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n\n\tmodified:   src/main.rs\n\tmodified:   src/lib.rs\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n";
        let result = compress("git status", output).unwrap();
        assert!(result.contains("main"), "should contain branch name");
        assert!(result.contains("main.rs"), "should list modified files");
        assert!(result.len() < output.len(), "should be shorter than input");
    }

    #[test]
    fn git_add_compresses_to_ok() {
        let result = compress("git add .", "").unwrap();
        assert!(result.contains("ok"), "git add should compress to 'ok'");
    }

    #[test]
    fn git_commit_extracts_hash() {
        let output =
            "[main abc1234] fix: resolve bug\n 2 files changed, 10 insertions(+), 3 deletions(-)\n";
        let result = compress("git commit -m 'fix'", output).unwrap();
        assert!(result.contains("abc1234"), "should extract commit hash");
    }

    #[test]
    fn git_push_compresses() {
        let output = "Enumerating objects: 5, done.\nCounting objects: 100% (5/5), done.\nDelta compression using up to 8 threads\nCompressing objects: 100% (3/3), done.\nWriting objects: 100% (3/3), 1.2 KiB | 1.2 MiB/s, done.\nTotal 3 (delta 2), reused 0 (delta 0)\nTo github.com:user/repo.git\n   abc1234..def5678  main -> main\n";
        let result = compress("git push", output).unwrap();
        assert!(result.len() < output.len(), "should compress push output");
    }

    #[test]
    fn git_log_compresses() {
        let output = "commit abc1234567890\nAuthor: User <user@email.com>\nDate:   Mon Mar 25 10:00:00 2026 +0100\n\n    feat: add feature\n\ncommit def4567890abc\nAuthor: User <user@email.com>\nDate:   Sun Mar 24 09:00:00 2026 +0100\n\n    fix: resolve issue\n";
        let result = compress("git log", output).unwrap();
        assert!(result.len() < output.len(), "should compress log output");
    }

    #[test]
    fn git_log_oneline_truncates_long() {
        let lines: Vec<String> = (0..150)
            .map(|i| format!("abc{i:04} feat: commit number {i}"))
            .collect();
        let output = lines.join("\n");
        let result = compress("git log --oneline", &output).unwrap();
        assert!(
            result.contains("... (50 more commits"),
            "should truncate to 100 entries"
        );
        assert!(
            result.lines().count() <= 102,
            "should have at most 101 lines (100 + summary)"
        );
    }

    #[test]
    fn git_log_oneline_short_unchanged() {
        let output = "abc1234 feat: one\ndef5678 fix: two\nghi9012 docs: three";
        let result = compress("git log --oneline", output).unwrap();
        assert_eq!(result, output, "short oneline should pass through");
    }

    #[test]
    fn git_log_standard_truncates_long() {
        let mut output = String::new();
        for i in 0..130 {
            output.push_str(&format!(
                "commit {i:07}abc1234\nAuthor: U <u@e.com>\nDate:   Mon\n\n    msg {i}\n\n"
            ));
        }
        let result = compress("git log", &output).unwrap();
        assert!(
            result.contains("... (30 more commits"),
            "should truncate standard log at 100"
        );
    }

    #[test]
    fn git_diff_compresses() {
        let output = "diff --git a/src/main.rs b/src/main.rs\nindex abc1234..def5678 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n fn main() {\n+    println!(\"hello\");\n     let x = 1;\n }";
        let result = compress("git diff", output).unwrap();
        assert!(result.contains("main.rs"), "should reference changed file");
    }

    #[test]
    fn git_push_preserves_pipeline_url() {
        let output = "Enumerating objects: 5, done.\nCounting objects: 100% (5/5), done.\nDelta compression using up to 8 threads\nCompressing objects: 100% (3/3), done.\nWriting objects: 100% (3/3), 1.2 KiB | 1.2 MiB/s, done.\nTotal 3 (delta 2), reused 0 (delta 0)\nremote:\nremote: To create a merge request for main, visit:\nremote:   https://gitlab.com/user/repo/-/merge_requests/new?source=main\nremote:\nremote: View pipeline for this push:\nremote:   https://gitlab.com/user/repo/-/pipelines/12345\nremote:\nTo gitlab.com:user/repo.git\n   abc1234..def5678  main -> main\n";
        let result = compress("git push", output).unwrap();
        assert!(
            result.contains("pipeline"),
            "should preserve pipeline URL, got: {result}"
        );
        assert!(
            result.contains("merge_request"),
            "should preserve merge request URL"
        );
        assert!(result.contains("->"), "should contain ref update line");
    }

    #[test]
    fn git_push_preserves_github_pr_url() {
        let output = "Enumerating objects: 5, done.\nremote:\nremote: Create a pull request for 'feature' on GitHub by visiting:\nremote:   https://github.com/user/repo/pull/new/feature\nremote:\nTo github.com:user/repo.git\n   abc1234..def5678  feature -> feature\n";
        let result = compress("git push", output).unwrap();
        assert!(
            result.contains("pull/"),
            "should preserve GitHub PR URL, got: {result}"
        );
    }

    #[test]
    fn git_commit_preserves_hook_output() {
        let output = "Running pre-commit hooks...\ncheck-yaml..........passed\ncheck-json..........passed\nruff.................failed\nfixing src/app.py\n[main abc1234] fix: resolve bug\n 2 files changed, 10 insertions(+), 3 deletions(-)\n";
        let result = compress("git commit -m 'fix'", output).unwrap();
        assert!(
            result.contains("ruff"),
            "should preserve hook output, got: {result}"
        );
        assert!(
            result.contains("abc1234"),
            "should still extract commit hash"
        );
    }

    #[test]
    fn git_commit_no_hooks() {
        let output =
            "[main abc1234] fix: resolve bug\n 2 files changed, 10 insertions(+), 3 deletions(-)\n";
        let result = compress("git commit -m 'fix'", output).unwrap();
        assert!(result.contains("abc1234"), "should extract commit hash");
        assert!(
            !result.contains("hook"),
            "should not mention hooks when none present"
        );
    }

    #[test]
    fn git_log_with_patch_filters_diff_content() {
        let output = "commit abc1234567890\nAuthor: User <user@email.com>\nDate:   Mon Mar 25 10:00:00 2026 +0100\n\n    feat: add feature\n\ndiff --git a/src/main.rs b/src/main.rs\nindex abc1234..def5678 100644\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,4 @@\n fn main() {\n+    println!(\"hello\");\n     let x = 1;\n }\n\ncommit def4567890abc\nAuthor: User <user@email.com>\nDate:   Sun Mar 24 09:00:00 2026 +0100\n\n    fix: resolve issue\n\ndiff --git a/src/lib.rs b/src/lib.rs\nindex 111..222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n+pub fn helper() {}\n";
        let result = compress("git log -p", output).unwrap();
        assert!(
            !result.contains("println"),
            "should NOT contain diff content, got: {result}"
        );
        assert!(result.contains("abc1234"), "should contain commit hash");
        assert!(
            result.contains("feat: add feature"),
            "should contain commit message"
        );
        assert!(
            result.len() < output.len() / 2,
            "compressed should be less than half of original ({} vs {})",
            result.len(),
            output.len()
        );
    }

    #[test]
    fn git_log_with_stat_filters_stat_content() {
        let mut output = String::new();
        for i in 0..5 {
            output.push_str(&format!(
                "commit {i:07}abc1234\nAuthor: U <u@e.com>\nDate:   Mon\n\n    msg {i}\n\n src/file{i}.rs | 10 ++++------\n 1 file changed, 4 insertions(+), 6 deletions(-)\n\n"
            ));
        }
        let result = compress("git log --stat", &output).unwrap();
        assert!(
            result.len() < output.len() / 2,
            "stat output should be compressed ({} vs {})",
            result.len(),
            output.len()
        );
    }

    #[test]
    fn git_commit_with_feature_branch() {
        let output = "[feature/my-branch abc1234] feat: add new thing\n 3 files changed, 20 insertions(+), 5 deletions(-)\n";
        let result = compress("git commit -m 'feat'", output).unwrap();
        assert!(
            result.contains("abc1234"),
            "should extract hash from feature branch, got: {result}"
        );
        assert!(
            result.contains("feature/my-branch"),
            "should preserve branch name, got: {result}"
        );
    }

    #[test]
    fn git_commit_many_hooks_compressed() {
        let mut output = String::new();
        for i in 0..30 {
            output.push_str(&format!("check-{i}..........passed\n"));
        }
        output.push_str("[main abc1234] fix: resolve bug\n 1 file changed, 1 insertion(+)\n");
        let result = compress("git commit -m 'fix'", &output).unwrap();
        assert!(result.contains("abc1234"), "should contain commit hash");
        assert!(
            result.contains("hooks passed"),
            "should summarize passed hooks, got: {result}"
        );
        assert!(
            result.len() < output.len() / 2,
            "should compress verbose hook output ({} vs {})",
            result.len(),
            output.len()
        );
    }

    #[test]
    fn stash_push_preserves_short_message() {
        let output = "Saved working directory and index state WIP on main: abc1234 fix stuff\n";
        let result = compress("git stash", output).unwrap();
        assert!(
            result.contains("Saved working directory"),
            "short stash messages must be preserved, got: {result}"
        );
    }

    #[test]
    fn stash_drop_preserves_short_message() {
        let output = "Dropped refs/stash@{0} (abc123def456)\n";
        let result = compress("git stash drop", output).unwrap();
        assert!(
            result.contains("Dropped"),
            "short drop messages must be preserved, got: {result}"
        );
    }

    #[test]
    fn stash_list_short_preserved() {
        let output = "stash@{0}: WIP on main: abc1234 fix\nstash@{1}: On feature: def5678 add\n";
        let result = compress("git stash list", output).unwrap();
        assert!(
            result.contains("stash@{0}"),
            "short stash list must be preserved, got: {result}"
        );
    }

    #[test]
    fn stash_list_long_reformats() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("stash@{{{i}}}: WIP on main: abc{i:04} commit {i}"))
            .collect();
        let output = lines.join("\n");
        let result = compress("git stash list", &output).unwrap();
        assert!(result.contains("@0:"), "should reformat @0, got: {result}");
        assert!(result.contains("@9:"), "should reformat @9, got: {result}");
    }

    #[test]
    fn stash_show_routes_to_show_compressor() {
        let output = " src/main.rs | 10 +++++-----\n src/lib.rs  |  3 ++-\n 2 files changed, 7 insertions(+), 6 deletions(-)\n";
        let result = compress("git stash show", output).unwrap();
        assert!(
            result.contains("main.rs"),
            "stash show should preserve file names, got: {result}"
        );
    }

    #[test]
    fn stash_show_patch_not_over_compressed() {
        let lines: Vec<String> = (0..40)
            .map(|i| format!("+line {i}: some content here"))
            .collect();
        let output = format!(
            "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n{}",
            lines.join("\n")
        );
        let result = compress("git stash show -p", &output).unwrap();
        let result_lines = result.lines().count();
        assert!(
            result_lines >= 10,
            "stash show -p must not over-compress to 3 lines, got {result_lines} lines"
        );
    }

    #[test]
    fn show_stash_ref_routes_correctly() {
        let output = "commit abc1234\nAuthor: User <u@e.com>\nDate: Mon Jan 1\n\n    WIP on main\n\ndiff --git a/f.rs b/f.rs\n";
        let result = compress("git show stash@{0}", output).unwrap();
        assert!(
            result.len() > 10,
            "git show stash@{{0}} must not be over-compressed, got: {result}"
        );
    }

    #[test]
    fn extract_subcommand_basic() {
        assert_eq!(extract_git_subcommand("git status"), Some("status"));
        assert_eq!(extract_git_subcommand("git log --oneline"), Some("log"));
        assert_eq!(extract_git_subcommand("git diff HEAD~1"), Some("diff"));
        assert_eq!(
            extract_git_subcommand("git -C /tmp commit -m 'x'"),
            Some("commit")
        );
    }

    #[test]
    fn extract_subcommand_avoids_filename_ambiguity() {
        assert_eq!(
            extract_git_subcommand("git log status.txt"),
            Some("log"),
            "should NOT match 'status' in filename"
        );
        assert_eq!(
            extract_git_subcommand("git add commit.rs"),
            Some("add"),
            "should NOT match 'commit' in filename"
        );
    }

    #[test]
    fn extract_subcommand_full_path() {
        assert_eq!(
            extract_git_subcommand("/usr/bin/git status"),
            Some("status")
        );
    }

    #[test]
    fn extract_subcommand_no_git() {
        assert_eq!(extract_git_subcommand("cargo build"), None);
    }

    #[test]
    fn filename_not_treated_as_subcommand() {
        let output = "On branch main\nnothing to commit\n";
        assert!(
            compress("git log status.txt", output).is_some(),
            "should route to log, not status"
        );
    }
}
