// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn listed_read_only_commands_are_safe() {
    for cmd in [
        "git status",
        "git status --short",
        "git log --oneline -5",
        "git diff HEAD~1 -- src/main.rs",
        "git show HEAD:Cargo.toml",
        "git blame src/lib.rs",
        "git branch",
        "git branch -a -vv",
        "git branch --list 'fix/*'",
        "git branch --merged main",
        "ls",
        "ls -la src",
        "ls *.rs",
        "cat src/main.rs",
        "head -n 20 src/main.rs",
        "tail -n 5 log.txt",
        "wc -l src/*.rs",
        "grep -rn 'fn main' src",
        "rg -n foo src",
        "find . -name '*.rs' -type f",
        "tree -L 2",
        "pwd",
        "which cargo",
        "npm ls --depth=0",
        "docker ps -a",
        "docker images",
        "git status && git diff",
        "git log --oneline | head -5",
        "ls src; ls tests",
        "git diff || git status",
    ] {
        assert!(is_side_effect_free(cmd), "{cmd}");
    }
}

#[test]
fn redirection_to_dev_null_or_fd_duplicate_is_safe() {
    for cmd in [
        "git status 2>/dev/null",
        "ls missing 2> /dev/null",
        "git log 2>&1 | head -3",
        "cat a >/dev/null",
    ] {
        assert!(is_side_effect_free(cmd), "{cmd}");
    }
}

/// Builds, tests and anything else that runs project code or writes are not
/// in the built-in set; on hosts that must approve to rewrite, only the
/// user's own configuration may allow them.
#[test]
fn builds_tests_and_writers_are_not_safe() {
    for cmd in [
        "cargo test",
        "cargo build",
        "npm test",
        "npm install",
        "make",
        "rm -rf build",
        "git push",
        "git commit -m x",
        "git checkout main",
        "git branch new-feature",
        "git branch -D old",
        "git branch -m a b",
        "git stash",
        "echo hi",
        "sed -n 1,5p file",
        "docker run alpine",
        "kubectl delete pod x",
        "kubectl get pods --server=https://example.invalid",
        "kubectl get pods",
        "cargo tree",
        "kubectl apply -f x.yaml",
        "true",
        "",
        "   ",
    ] {
        assert!(!is_side_effect_free(cmd), "{cmd}");
    }
}

#[test]
fn adversarial_forms_are_not_safe() {
    for cmd in [
        // Chained or piped into a writer.
        "git status; rm -rf x",
        "git status && rm -rf x",
        "ls || rm -rf x",
        "cat a | sh",
        "cat a | tee out",
        "git log | xargs rm",
        // Substitution and expansion.
        "cat $(echo /etc/passwd)",
        "ls `rm -rf x`",
        "cat \"$(rm -rf x)\"",
        "ls $HOME",
        "cat \"$FILE\"",
        "diff <(ls a) <(ls b)",
        "ls ${HOME}",
        // Redirections into files.
        "git status > out.txt",
        "ls >> out.txt",
        "cat a 2> err.log",
        "ls >out",
        "cat < /etc/passwd",
        "git log >&3 file",
        // find actions and globs.
        "find . -delete",
        "find . -name x -exec rm {} ;",
        "find . -execdir sh -c x ;",
        "find . -fprint out",
        "find . -name *.rs",
        // Option smuggling.
        "git -c core.pager=sh log",
        "git -C /tmp status",
        "git --git-dir=/x status",
        "git diff --output=x",
        "git log --output x",
        "git diff --ext-diff",
        "git log *",
        "rg --pre ./evil foo",
        "rg --pre=./evil foo",
        "tree -o out.txt",
        "tree -R",
        "tree -aR",
        "tree -ao out.txt",
        "tree --output=out.txt",
        // Env prefixes, paths, wrappers.
        "GIT_EXTERNAL_DIFF=evil git diff",
        "PAGER=sh git log",
        "/bin/ls",
        "./ls",
        "timeout 5 git status",
        "env git status",
        "sudo cat /etc/shadow",
        // Shell syntax the lexer refuses.
        "git status &",
        "ls\nrm -rf x",
        "ls \\; rm",
        "(ls)",
        "{ ls; }",
        "ls # comment",
        "cat 'unterminated",
        "cat \"unterminated",
        "git status &&",
        "&& git status",
        "ls ;; ls",
    ] {
        assert!(!is_side_effect_free(cmd), "{cmd:?}");
    }
}

#[test]
fn quoted_operators_and_globs_stay_literal() {
    let segs = parse_strict("grep 'a && b | c; d' file").expect("parses");
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].words[1].text, "a && b | c; d");
    assert!(!segs[0].words[1].glob);
    assert!(is_side_effect_free("find . -name '*.rs'"));
    assert!(is_side_effect_free("grep -n \"a > b\" file"));
}

#[test]
fn segments_keep_their_raw_text() {
    let segs = parse_strict("git status  &&  npm ls --depth=0 | head").expect("parses");
    let raw: Vec<&str> = segs.iter().map(|s| s.raw.as_str()).collect();
    assert_eq!(raw, ["git status", "npm ls --depth=0", "head"]);
}

#[test]
fn claude_builtin_flag_covers_only_claude_documented_commands() {
    let flag = |cmd: &str| safe_segments(cmd).expect(cmd)[0].1.claude_builtin;
    for cmd in [
        "ls",
        "cat a",
        "git status",
        "git diff",
        "find . -type f",
        "pwd",
        "which x",
    ] {
        assert!(flag(cmd), "{cmd}");
    }
    for cmd in [
        "rg x",
        "tree",
        "npm ls",
        "docker ps",
        "git blame f",
        "git branch",
    ] {
        assert!(!flag(cmd), "{cmd}");
    }
}
