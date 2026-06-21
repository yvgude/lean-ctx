//! Scenario tests for the bug reported by daviddatu_:
//!
//! lean-ctx was rewriting `git commit` to `git cmt` via the terse abbreviation
//! dictionary, and PowerShell quoting was wrapping full command strings in
//! single quotes when passed as a single argument.
//!
//! These tests verify:
//! 1. Git subcommand words are NEVER abbreviated in compression output
//! 2. Git write-commands (commit/push/pull/merge/rebase) are verbatim (no compression)
//! 3. PowerShell `join_command` does not wrap full command strings with & '...'

use lean_ctx::core::terse::dictionaries::{DictLevel, GIT, apply_dictionaries};
use lean_ctx::shell::compress::{has_structural_output, is_verbatim_output};
use lean_ctx::shell::join_command_for;

// ---------------------------------------------------------------------------
// Scenario 1: Terse dictionary must never abbreviate git subcommands
// ---------------------------------------------------------------------------

#[test]
fn scenario_commit_never_abbreviated_in_git_output() {
    let typical_output = "[main abc1234] feat(result-sheets): add sheet\n 2 files changed, 15 insertions(+), 3 deletions(-)";
    let result = apply_dictionaries(typical_output, DictLevel::Full);
    assert!(
        !result.contains("cmt"),
        "git commit output must not contain 'cmt' abbreviation: {result}"
    );
    assert!(
        result.contains("abc1234"),
        "commit hash must be preserved: {result}"
    );
}

#[test]
fn scenario_branch_never_abbreviated_in_status_output() {
    let status_output = "On branch feature/result-sheets\nYour branch is up to date with 'origin/feature/result-sheets'.\n\nnothing to commit, working tree clean";
    let result = apply_dictionaries(status_output, DictLevel::Full);
    assert!(
        result.contains("branch"),
        "word 'branch' must survive in output: {result}"
    );
    assert!(
        !result.contains(" br "),
        "must not abbreviate 'branch' to 'br': {result}"
    );
}

#[test]
fn scenario_merge_checkout_rebase_stash_never_abbreviated() {
    let git_words = ["commit", "branch", "checkout", "merge", "stash", "rebase"];
    for word in &git_words {
        let text = format!("the {word} operation completed successfully");
        let result = apply_dictionaries(&text, DictLevel::Full);
        assert!(
            result.contains(word),
            "git subcommand '{word}' must NOT be abbreviated in output. Got: {result}"
        );
    }
}

#[test]
fn scenario_git_dictionary_contains_no_subcommands() {
    let git_subcommands = [
        "commit",
        "branch",
        "checkout",
        "merge",
        "stash",
        "rebase",
        "push",
        "pull",
        "fetch",
        "clone",
        "tag",
        "reset",
        "bisect",
        "log",
        "diff",
        "show",
        "status",
        "add",
        "switch",
        "cherry-pick",
        "blame",
        "remote",
    ];
    for abbr in GIT {
        assert!(
            !git_subcommands.contains(&abbr.long),
            "CRITICAL: GIT dictionary abbreviates subcommand '{}' → '{}'. \
             Agents will misinterpret abbreviated output as valid commands!",
            abbr.long,
            abbr.short
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario 2: Git write-commands must be verbatim (never compressed)
// ---------------------------------------------------------------------------

#[test]
fn scenario_git_commit_is_verbatim() {
    assert!(
        is_verbatim_output("git commit -m \"feat(result-sheets): add new sheet\""),
        "git commit must be classified as verbatim"
    );
}

#[test]
fn scenario_git_push_is_verbatim() {
    assert!(
        is_verbatim_output("git push origin feature/result-sheets"),
        "git push must be classified as verbatim"
    );
}

#[test]
fn scenario_git_pull_is_verbatim() {
    assert!(
        is_verbatim_output("git pull --rebase origin main"),
        "git pull must be classified as verbatim"
    );
}

#[test]
fn scenario_git_merge_is_verbatim() {
    assert!(
        is_verbatim_output("git merge --no-ff feature/result-sheets"),
        "git merge must be classified as verbatim"
    );
}

#[test]
fn scenario_git_rebase_is_verbatim() {
    assert!(
        is_verbatim_output("git rebase -i HEAD~3"),
        "git rebase must be classified as verbatim"
    );
}

#[test]
fn scenario_git_cherry_pick_is_verbatim() {
    assert!(
        is_verbatim_output("git cherry-pick abc1234"),
        "git cherry-pick must be classified as verbatim"
    );
}

#[test]
fn scenario_git_tag_is_verbatim() {
    assert!(
        is_verbatim_output("git tag -a v3.6.10 -m \"release\""),
        "git tag must be classified as verbatim"
    );
}

#[test]
fn scenario_git_reset_is_verbatim() {
    assert!(
        is_verbatim_output("git reset --hard HEAD~1"),
        "git reset must be classified as verbatim"
    );
}

#[test]
fn scenario_git_status_still_compressible() {
    assert!(
        !is_verbatim_output("git status"),
        "git status should still be compressible (high-value compression)"
    );
}

#[test]
fn scenario_git_log_still_compressible() {
    assert!(
        !is_verbatim_output("git log --oneline -20"),
        "git log should still be compressible (high-value compression)"
    );
}

#[test]
fn scenario_git_diff_is_structural_not_verbatim_directly() {
    assert!(
        has_structural_output("git diff --cached"),
        "git diff should be structural"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: PowerShell quoting edge cases
// ---------------------------------------------------------------------------

#[test]
fn scenario_powershell_single_full_command_passthrough() {
    let args: Vec<String> = vec!["git commit -m \"feat(result-sheets): add sheet\"".into()];
    let result = join_command_for(&args, "-Command");
    assert!(
        !result.starts_with("& '"),
        "single full-command string must NOT be wrapped in & '...': {result}"
    );
    assert_eq!(
        result, "git commit -m \"feat(result-sheets): add sheet\"",
        "should pass through the command unchanged"
    );
}

#[test]
fn scenario_powershell_split_args_still_quoted() {
    let args: Vec<String> = vec![
        "git".into(),
        "commit".into(),
        "-m".into(),
        "feat(result-sheets): add sheet".into(),
    ];
    let result = join_command_for(&args, "-Command");
    assert!(
        result.starts_with("& "),
        "split args should use call operator: {result}"
    );
    assert!(result.contains("git"), "should contain git: {result}");
    assert!(
        result.contains("commit"),
        "should contain commit (not abbreviated): {result}"
    );
    assert!(
        result.contains("'feat(result-sheets): add sheet'"),
        "special chars in commit message should be quoted: {result}"
    );
}

#[test]
fn scenario_powershell_single_simple_command_uses_call_operator() {
    let args: Vec<String> = vec!["git".into()];
    let result = join_command_for(&args, "-Command");
    assert_eq!(
        result, "& git",
        "single simple command should use call operator"
    );
}

#[test]
fn scenario_powershell_parentheses_in_message_quoted() {
    let args: Vec<String> = vec![
        "git".into(),
        "commit".into(),
        "-m".into(),
        "fix(auth): resolve login issue".into(),
    ];
    let result = join_command_for(&args, "-Command");
    assert!(
        result.contains("'fix(auth): resolve login issue'"),
        "parentheses must be quoted in PowerShell: {result}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: End-to-end compression verification
// ---------------------------------------------------------------------------

#[test]
fn scenario_compress_if_beneficial_skips_git_commit_output() {
    let command = "git commit -m \"feat: add feature\"";
    let output = "[main abc1234] feat: add feature\n 1 file changed, 5 insertions(+)\n";
    let result = lean_ctx::shell::compress::compress_if_beneficial_pub(command, output);
    assert_eq!(
        result.trim(),
        output.trim(),
        "git commit output must NOT be compressed"
    );
}

#[test]
fn scenario_compress_if_beneficial_skips_git_push_output() {
    let command = "git push origin main";
    let output = "Everything up-to-date\n";
    let result = lean_ctx::shell::compress::compress_if_beneficial_pub(command, output);
    assert_eq!(
        result.trim(),
        output.trim(),
        "git push output must NOT be compressed"
    );
}

#[test]
fn scenario_compress_if_beneficial_still_compresses_git_status() {
    let command = "git status";
    let output = "On branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n\n\tmodified:   src/main.rs\n\tmodified:   src/lib.rs\n\tmodified:   src/utils.rs\n\tmodified:   src/config.rs\n\tmodified:   src/server.rs\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n";
    let result = lean_ctx::shell::compress::compress_if_beneficial_pub(command, output);
    assert!(
        result.len() < output.len(),
        "git status should still be compressed (was {} → {} bytes)",
        output.len(),
        result.len()
    );
}
