// SPDX-License-Identifier: Apache-2.0
use super::*;

const RECORDS: &str = "/home/u/.local/share/lean-ctx/agents";

fn judged(line: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    records_in_line(
        line,
        &[PathBuf::from(RECORDS)],
        &[PathBuf::from("/home/u")],
        Some(Path::new("/home/u/proj")),
        env,
        &["leases.json".to_string(), "registry.lock".to_string()],
    )
}

/// The word through which `line` reads the records, in a fixed setting:
/// records below `/home/u`, the line starting in `/home/u/proj`, an empty
/// environment.
fn refused(line: &str) -> Option<String> {
    judged(line, &|_| None)
}

fn assert_refused(lines: &[&str]) {
    for line in lines {
        assert!(refused(line).is_some(), "must refuse: {line}");
    }
}

fn assert_allowed(lines: &[&str]) {
    for line in lines {
        assert_eq!(refused(line), None, "must allow: {line}");
    }
}

#[test]
fn shell_policy_judges_diff_like_any_reader() {
    assert_refused(&[
        "diff ~/.local/share/lean-ctx/agents/directives.json /dev/null",
        "diff -u ~/.local/share/lean-ctx/agents/registry.json /tmp/e",
        "diff --from-file=/home/u/.local/share/lean-ctx/agents/tasks.json /tmp/e",
        "diff -r ~/.local/share /tmp/e",
        "diff -rq ~ /tmp/e",
        "diff -ur ~/.local /tmp/e",
        "diff -Nr ~/.local/share/lean-ctx /tmp/e",
        "diff --recursive ~/.local/share /tmp/e",
        "diff --recur ~/.local/share /tmp/e",
        "colordiff -r ~ /tmp/e",
    ]);
    assert_allowed(&[
        "diff -r a b",
        "diff -u a.txt b.txt",
        "diff ~/.local/share/lean-ctx/cloud/worker.json /tmp/e",
    ]);
}

#[test]
fn shell_policy_judges_git_no_index_like_diff() {
    assert_refused(&[
        "git diff --no-index ~/.local/share/lean-ctx /tmp/e",
        "git diff --no-index ~/.local/share /tmp/e",
        "git diff --no-index -- ~/.local/share /tmp/e",
        "git --no-pager diff --no-index ~ /tmp/e",
        "git -C ~/.local diff --no-index share /tmp/e",
        "git -C ~ grep --no-index rid_",
        "git grep --no-index rid_ ~/.local/share/lean-c*",
        "cd ~ && git grep --no-index rid_",
    ]);
    assert_allowed(&[
        "git diff --no-index a b",
        "git diff HEAD",
        "git grep rid_",
        "git -C ~/src grep --no-index rid_",
    ]);
}

#[test]
fn shell_policy_treats_produced_paths_as_unknown() {
    assert_refused(&[
        "cat $(find ~ -name directives.json)",
        "cat \"$(find ~ -name directives.json)\"",
        "cat `find ~ -name directives.json`",
        "cat $(find ~ -name directives.json | head -1)",
        "cat $(echo $(find ~ -name directives.json))",
        "for f in $(find ~ -name directives.json); do cat $f; done",
        "xargs cat <<< \"$(find ~ -name directives.json)\"",
        "f=$(find ~ -name directives.json); cat $f",
        "find ~ -name directives.json | xargs cat",
        "find / -name directives.json | xargs cat",
        "find ~ | while read f; do cat \"$f\"; done",
        "find ~ | tar -cf /tmp/x.tar -T -",
        "find ~ -name directives.json -exec cat {} +",
        "fd directives ~ | xargs cat",
        "fd directives ~ -x cat",
        "find ~/.local/share -name '*ids*' | xargs cat",
        "D=~/.local; find $D -name '*ids*' | xargs cat",
        "printf '%s/agents/directives.json' ~/.local/share/lean-ctx > /tmp/l; xargs cat < /tmp/l",
        "printf '%s/agents/directives.json' ~/.local/share/lean-ctx > /tmp/l; xargs -a /tmp/l cat",
    ]);
    assert_allowed(&[
        "find . -name '*.rs' | xargs wc -l",
        "printf '%s\\n' ~/src > /tmp/l; xargs cat < /tmp/l",
        "find . | xargs grep x",
        "cat $(git ls-files | head -1)",
    ]);
}

#[test]
fn shell_policy_reads_globs_on_the_data_directory() {
    assert_refused(&[
        "cat ~/.local/share/lean-c*",
        "diff -r ~/.local/share/lean-c* /tmp/e",
        "grep rid_ ~/.local/share/lean-c*",
        "grep -d recurse rid_ ~/.local/share/lean-c*",
        "cat ~/.local/share/lean-ct?/agents/x",
    ]);
    assert_allowed(&[
        "ls ~/.local/share/lean-c*",
        "cat ~/.local/share/lean-ctx/c*.toml",
    ]);
}

#[test]
fn shell_policy_follows_paths_built_in_substitutions() {
    assert_refused(&[
        "cat $(echo ~/.local/share/lean-ctx)/agents/*",
        "cat $(dirname ~/.local/share/lean-ctx/x)/agents/directives.json",
        "D=$(cd ~/.local/share && pwd); cat $D/lean-ctx/agents/*",
        "cat ~/.local/share/$(echo lean-ctx)/agents/directives.json",
        "cat \"$(printf '%s/agents/directives.json' ~/.local/share/lean-ctx)\"",
        "cat $X/agents/directives.json",
        "cat ${X:-~/.local/share/lean-ctx}/agents/registry.json",
    ]);
    assert_allowed(&["cat $(echo ~/src)/README.md", "cat $X/notes.txt"]);
}

#[test]
fn shell_policy_judges_arguments_fed_from_output() {
    assert_refused(&[
        "printf '%s/lean-ctx/agents/directives.json' ~/.local/share | xargs cat",
        "printf '%s/share/lean-ctx/agents/directives.json' ~/.local | xargs cat",
        "printf '%s/.local/share/lean-ctx/agents/directives.json' ~ | xargs cat",
        "printf '%s/.local/share/lean-ctx/agents/directives.json' \"$HOME\" | xargs cat",
        "printf '%s/agents/directives.json' ~/.local/share/lean-ctx | xargs cat",
        "echo ~/.local/share | xargs -I{} cat {}/lean-ctx/agents/directives.json",
        "pwd | xargs -I{} cat {}/.local/share/lean-ctx/agents/directives.json",
        "while read d; do cat $d/lean-ctx/agents/directives.json; done <<< ~/.local/share",
        "while read d; do cat \"$d\"; done < <(printf '%s/lean-ctx/agents/directives.json\\n' ~/.local/share)",
        "echo ~/.local/share/lean-ctx | parallel cat {}/agents/directives.json",
    ]);
    assert_allowed(&[
        "echo ~/.local/share | xargs ls",
        "printf '%s\\n' ~/src | xargs ls",
        "printf '%s\\n' ~/src | xargs cat",
        "git ls-files | xargs wc -l",
    ]);
}

#[test]
fn shell_policy_reads_grep_options_as_grep_does() {
    assert_refused(&[
        "grep --dir=recurse rid_ ~/.local/share",
        "grep --direc=recurse rid_ ~/.local/share",
        "grep --directories=recurse rid_ ~/.local/share/lean-c*",
        "grep -d recurse rid_ ~/.local/share/lean-ctx",
        "grep --recurs rid_ ~",
        "grep --recursi rid_ ~",
        "grep --dereference-recursiv rid_ ~",
        "grep -id recurse rid_ ~",
        "grep -sd recurse rid_ ~",
        "grep -hd recurse rid_ ~",
        "grep -r rid_ ~/.local/share/lean-ctx",
        "rg rid_ /home/u",
        "tar cf /tmp/x.tar /home/u/.local/share/lean-ctx",
        "cp -r ~/.local/share/lean-ctx /tmp/copy",
    ]);
    assert_allowed(&[
        "grep -r foo .",
        "grep -r foo ~/.config",
        "grep -d skip rid_ ~/.local/share/lean-ctx/config.toml",
        "rg rid_ /home/u/src/project",
        "grep -e x -r src",
    ]);
}

#[test]
fn shell_policy_bounds_listers_by_depth_and_name() {
    assert_refused(&[
        "du -a ~/.local/share",
        "du -ah ~/.local/share",
        "du --all ~",
        "du -a ~/.local/share/lean-ctx",
        "ls -R1 ~/.local/share",
        "ls -laR ~/.local/share",
        "ls --recursive ~",
        "ls -R ~",
        "ls -R ~/.local/share/lean-ctx",
        "tree -f ~",
        "tree -a ~",
        "tree -L 4 ~",
        "tree ~",
        "tree ~/.local/share/lean-ctx",
        "fd -H ids ~/.local/share",
        "fd -l directives ~",
        "find ~ -name directives.json 1>&2",
        "find ~ -name directives.json > /tmp/l",
        "find ~ -name directives.json -fprint /tmp/l",
        "find ~ -name '*.json' | head",
        "find ~ -name 'leases*'",
        "locate directive-release",
        "locate lean-ctx/agents",
    ]);
    assert_allowed(&[
        "find ~ -name '*.log' | head",
        "find ~ -name '*.toml' | head",
        "find ~ -name '*.log' > /tmp/logs",
        "find ~/.local -name x | wc -l",
        "find / -name foo 2>/dev/null | head",
        "find ~ -maxdepth 1 | sort",
        "find ~ -name x",
        "find ~ -name x 2>&1 >&2",
        "find ~ -name x || true",
        "tree -L 2 ~ | less",
        "fd x ~",
        "ls -R src",
        "tree rust",
        "du -a target",
        "du -sh ~",
        "ls ~/.local/share/lean-ctx",
        "locate foo.rs",
    ]);
}

#[test]
fn shell_policy_reads_bsd_find_options_before_the_path() {
    assert_refused(&[
        "find -E ~ -name directives.json | xargs cat",
        "find -x ~ -name directives.json | xargs cat",
        "find -s ~ -name directives.json | xargs cat",
        "find -d ~ -name directives.json | xargs cat",
        "find -X ~ -name directives.json | xargs cat",
        "find -L -X ~ -name directives.json",
        "find -f ~ -name directives.json | xargs cat",
        "find ~ -ls",
        "find ~/.local/share -print",
    ]);
    assert_allowed(&["find -E . -name '*.rs'", "find -x src -print"]);
}

#[test]
fn shell_policy_normalises_path_words() {
    assert_refused(&[
        "find ~//.local//share -name directives.json",
        "find ~/./.local/share/./lean-ctx -name x -o -print",
        "cat ~/.local//share/lean-ctx/./agents/directives.json",
        "cat ~/src/../.local/share/lean-ctx/agents/directives.json",
        "fd directives --search-path=$HOME",
        "fd directives --search-path $HOME/.local",
        "cat ~u/.local/share/lean-ctx/agents/directives.json",
        "cat $XDG_DATA_HOME/lean-ctx/agents/directives.json",
        "cat ${HOME}/.local/share/lean-ctx/agents/directives.json",
        "cat ~/.local/share/{lean-ctx,x}/agents/directives.json",
        "cat ~/.local/share/lean-ctx/agent{s,x}/directives.json",
    ]);
    assert_allowed(&["cat ~/src//main.rs", "cat ~/.local/share/{x,y}/agents.md"]);
}

#[test]
fn shell_policy_tracks_the_working_directory() {
    assert_refused(&[
        "cd ~/.local/share && grep -r rid_",
        "cd ~/.local/share && ls -R",
        "cd ~/.local/share && tree",
        "cd ~/.local/share && du -a",
        "cd ~/.local/share && diff -r . /tmp/e",
        "cd ~/.local/share/lean-ctx/agents && cat directives.json",
        "cd ~/.local/share/lean-ctx && cat agents/registry.json",
        "cd ~ && cd .local && cd share && grep -r x",
        "cd ~/.local && cd /tmp && cd - && grep -r x",
        "cd ~/.local; cd share; rg x",
        "pushd ~/.local/share && grep -r x",
        "cd && grep -r x",
        "cd $X && grep -r x",
        "D=~/.local/share; cd $D && grep -r x",
        "cd ~/.local/share/lean-ctx && cat $OLDPWD/x agents/tasks.json",
    ]);
    assert_allowed(&[
        "cd src && grep -r x",
        "cd /tmp && ls -R",
        "(cd ~/.local/share) && grep -r x .",
        "cd ~/.local/share/lean-ctx && cat cloud/worker.json",
    ]);
}

#[test]
fn shell_policy_decodes_ansi_c_and_locale_quoting() {
    assert_refused(&[
        "cat ~/.local/share/lean-ctx/$'agents'/directives.json",
        "cat ~/.local/share/lean-ctx/$'\\x61gents'/directives.json",
        "cat ~/.local/share/lean-ctx/$'\\141gents'/directives.json",
        "cat ~/.local/share/lean-ctx/$\"agents\"/directives.json",
        "cat ~/.local/share/lean-ctx/a\"gen\"ts/directives.json",
        "cat ~/.local/share/lean-ctx/a\\gents/directives.json",
        "cat ~/.local/share/lean-ctx/$'\\u0061'gents/directives.json",
    ]);
    assert_allowed(&["printf $'a\\tb\\n'"]);
}

#[test]
fn shell_policy_resolves_variables_through_the_line() {
    assert_refused(&[
        "D=~/.local/share; cat $D/lean-ctx/agents/directives.json",
        "d=~/.local/share/lean-ctx; cat $d/agents/directives.json",
        "d=~/.local/share/lean-ctx; grep -r rid_ $d",
        "d=~/.local/share/lean-ctx; find $d | xargs cat",
        "export D=~/.local/share; cat \"$D\"/lean-ctx/agents/*",
        "for d in ~/.local/share/lean-ctx; do cat \"$d/agents/directives.json\"; done",
        "a=lean; b=ctx; cat ~/.local/share/$a-$b/agents/directives.json",
        "D=x; D=~/.local/share; grep -r rid_ $D",
        "env D=~/.local/share sh -c 'grep -r x $D'",
        "bash -c 'cat ~/.local/share/lean-ctx/agents/directives.json'",
        "sh -c \"grep -r x $HOME\"",
        "eval cat ~/.local/share/lean-ctx/agents/tasks.json",
        "bash <<'EOF'\ngrep -r x ~/.local/share\nEOF",
    ]);
    assert_allowed(&[
        "d=~/src; grep -r x $d",
        "for f in src/*.rs; do cat \"$f\"; done",
        "bash -c 'grep -r x src'",
        "cat <<'EOF'\n~/.local/share\nEOF",
    ]);
}

#[test]
fn shell_policy_uses_the_environment_for_unassigned_variables() {
    let env = |name: &str| match name {
        "XDG_DATA_HOME" | "DATA" => Some("/home/u/.local/share".to_string()),
        "SRC" => Some("/home/u/src".to_string()),
        _ => None,
    };
    assert!(judged("grep -r x $SRC", &env).is_none());
    assert!(judged("grep -r x $DATA", &env).is_some());
    assert!(judged("cd $SRC && grep -r x", &env).is_none());
    assert!(judged("cat $XDG_DATA_HOME/lean-ctx/agents/x", &env).is_some());
}

#[test]
fn shell_policy_keeps_everyday_commands_and_other_state_readable() {
    assert_allowed(&[
        "cat ~/.local/share/lean-ctx/cloud/worker.json",
        "grep rid_ ~/.local/share/lean-ctx/config.toml",
        "lean-ctx agents list",
        "ls /*",
        "ls /home/u/*",
        "[[ -d scripts ]]",
        "cargo test --lib 2>&1 | tail -5",
        "git status && git diff",
        "find . -type f | sort | head",
        "echo agents",
        "cat README.md | grep agents",
        "rg -n agents src",
    ]);
}

#[test]
fn shell_policy_records_judgement_is_deterministic_and_independent() {
    let lines = [
        "tree ~",
        "find ~ -name x",
        "ls -R ~",
        "du -a target",
        "fd x ~",
        "find ~/.local/share -name x",
    ];
    let expected: Vec<bool> = lines.iter().map(|l| refused(l).is_some()).collect();
    assert_eq!(expected, [true, false, true, false, false, false]);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    lines
                        .iter()
                        .map(|l| refused(l).is_some())
                        .collect::<Vec<bool>>()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().ok(), Some(expected.clone()));
        }
    });
    // A refused line leaves nothing behind for the next one.
    assert!(refused("cd ~/.local/share; D=~; grep -r x $D").is_some());
    assert!(refused("grep -r x .").is_none());
}

#[test]
fn shell_policy_refuses_lines_nested_too_deep() {
    let deep = format!("{}x{}", "$(".repeat(40), ")".repeat(40));
    assert!(refused(&format!("echo {deep}")).is_some());
}

#[test]
fn shell_policy_reads_fd_extensions_as_name_tests() {
    assert_refused(&[
        "fd -e json . ~/.local/share/lean-ctx | xargs cat",
        "fd -ejson . ~/.local/share/lean-ctx | xargs cat",
        "fd --extension json . ~/.local/share/lean-ctx | xargs cat",
        "fd --extension=.json . ~/.local/share/lean-ctx | xargs cat",
        "fd -e log -e json . ~/.local/share/lean-ctx | xargs cat",
        "fd -e json -e log . ~/.local/share/lean-ctx | xargs cat",
        "cat $(fd -e json . ~/.local/share/lean-ctx)",
        "fd -H -e json . ~ | xargs cat",
        "fd -e JSON . ~/.local/share/lean-ctx | xargs cat",
    ]);
    assert_allowed(&[
        "fd -e rs . src | xargs cat",
        "fd -e log . ~/.local/share/lean-ctx | xargs cat",
        "fd -e json . ~/src | xargs cat",
        "fd -e toml -e md . ~ | xargs cat",
    ]);
}

#[test]
fn shell_policy_knows_the_app_key_before_it_exists() {
    // The test's records list no `app-launch.key` on disk: the name alone
    // is a record (LeanCTX Code writes it at launch).
    assert_refused(&[
        "find ~ -name app-launch.key | xargs cat",
        "find ~ -name app-launch.key -exec cat {} +",
        "fd app-launch.key ~ -x cat",
    ]);
}

/// The state records below `/home/u/.local/state/lean-ctx`, as
/// `record_dirs` adds them for every state directory.
fn state_judged(line: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let base = Path::new("/home/u/.local/state/lean-ctx");
    let records: Vec<PathBuf> = STATE_RECORDS.iter().map(|name| base.join(name)).collect();
    records_in_line(
        line,
        &records,
        &[PathBuf::from("/home/u")],
        Some(Path::new("/home/u/proj")),
        env,
        &[],
    )
}

#[test]
fn shell_policy_keeps_state_records_from_every_reader() {
    let env = |name: &str| match name {
        "XDG_STATE_HOME" => Some("/home/u/.local/state".to_string()),
        "LEAN_CTX_STATE_DIR" => Some("/home/u/.local/state/lean-ctx".to_string()),
        "HOME" => Some("/home/u".to_string()),
        _ => None,
    };
    let readers = [
        "cat {}",
        "rg x {}",
        "grep x {}",
        "less {}",
        "head -n 5 {}",
        "tail -f {}",
        "cp {} /tmp/x",
        "tar -cf /tmp/a.tar {}",
        "base64 {}",
        "xxd {}",
        "sed \"r {}\" notes.txt",
        "sed -e \"1r {}\" notes.txt",
        "vim {}",
        "view {}",
    ];
    let forms = [
        "~/.local/state/lean-ctx/{}",
        "$HOME/.local/state/lean-ctx/{}",
        "${HOME}/.local/state/lean-ctx/{}",
        "$XDG_STATE_HOME/lean-ctx/{}",
        "$LEAN_CTX_STATE_DIR/{}",
        "${LEAN_CTX_STATE_DIR}/{}",
        "/home/u/.local/state/lean-ctx/{}",
        "~/.local/state/lean-ctx/logs/../{}",
        "../.local/state/lean-ctx/{}",
        "$UNSET_DIR/lean-ctx/{}",
    ];
    let mut missed = Vec::new();
    for record in STATE_RECORDS {
        let record = if *record == "ledger" {
            "ledger/a.json"
        } else {
            record
        };
        for form in forms {
            let path = form.replace("{}", record);
            for reader in readers {
                // A sed script word is never tilde-expanded, so it reads no record there.
                if reader.starts_with("sed") && path.starts_with('~') {
                    continue;
                }
                let line = reader.replace("{}", &path);
                if state_judged(&line, &env).is_none() {
                    missed.push(line);
                }
            }
            for line in [
                format!("cd {} && cat {record}", form.replace("/{}", "")),
                format!("D={}; cat $D/{record}", form.replace("/{}", "")),
            ] {
                if state_judged(&line, &env).is_none() {
                    missed.push(line);
                }
            }
        }
    }
    for line in [
        "grep -r x ~/.local/state/lean-ctx",
        "rg x $LEAN_CTX_STATE_DIR",
        "tar -cf /tmp/a.tar ~/.local/state/lean-ctx",
        "cp -r $XDG_STATE_HOME/lean-ctx /tmp/x",
        "cat ~/.local/state/lean-ctx/*",
        "cat ~/.local/state/lean-ctx/ledger/*",
        "find ~/.local/state/lean-ctx -name '*ledger*' -exec cat {} +",
        // Names compare without case on every platform.
        "cat ~/.local/state/lean-ctx/Dashboard.TOKEN",
        "cat ~/.local/state/lean-ctx/LEDGER/x",
        "head ~/.LOCAL/State/Lean-Ctx/Context_Ledger.JSON",
        "cp $LEAN_CTX_STATE_DIR/AGENT_RUNTIME_ENV.json /tmp/x",
        "sh -c 'cat ~/.local/state/lean-ctx/Dashboard.Token'",
        "cat ~/.local/state/lean-ctx/tool-calls.log",
        "tail -f $LEAN_CTX_STATE_DIR/Tool-Calls.LOG",
        "grep x ~/.local/state/lean-ctx/slow-commands.log",
        "jq . ~/.local/state/lean-ctx/heatmap.json",
        "cat ~/.local/state/lean-ctx/feedback.json",
        "cat ~/.local/state/lean-ctx/work-graphs/a.json",
        // A copy beside a record holds what the record holds.
        "cat ~/.local/state/lean-ctx/context_ledger.json.bak",
        "cat ~/.local/state/lean-ctx/context_ledger.json.tmp",
        "cat ~/.local/state/lean-ctx/context_ledger.json~",
        "cat ~/.local/state/lean-ctx/Context_Ledger.JSON.old",
        "cat ~/.local/state/lean-ctx/dashboard.token.old",
        "cp ~/.local/state/lean-ctx/cost_attribution.json.bak /tmp/x",
        "sh -c 'cat ~/.local/state/lean-ctx/dashboard.token.old'",
        "cat ~/.local/state/lean-ctx/tool-calls.log.1",
    ] {
        if state_judged(line, &env).is_none() {
            missed.push(line.to_string());
        }
    }
    assert!(missed.is_empty(), "must refuse:\n{}", missed.join("\n"));
}

#[test]
fn shell_policy_keeps_the_rest_of_the_state_readable() {
    let env = |_: &str| None;
    for line in [
        "tail -n 50 ~/.local/state/lean-ctx/logs/debug.log",
        "cat ~/.local/state/lean-ctx/events.jsonl",
        "cat ~/.local/state/lean-ctx/tee/proxy_1.log",
        "grep x ~/.local/state/lean-ctx/journal.md",
        "ls ~/.local/state/lean-ctx",
        "cat src/ledger.rs",
        "cat docs/dashboard.token.md",
        "cat ~/.local/state/lean-ctx/pipeline_stats.json",
        "sh -c 'cat ~/.local/state/lean-ctx/events.jsonl'",
        "cat ~/.local/state/lean-ctx/mcp-live.json",
        "cat ~/.local/state/lean-ctx/session_savings.jsonl",
        "cat ~/.local/state/lean-ctx/compression_session.json",
        "cat ~/.local/state/lean-ctx/projects/a/journal.md",
    ] {
        assert_eq!(state_judged(line, &env), None, "must allow: {line}");
    }
}

#[test]
fn shell_policy_finds_state_records_in_every_state_directory() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let root = std::env::temp_dir().join(format!("lcx-state-records-{}", std::process::id()));
    let real = root.join("real");
    let link = root.join("link");
    let _ = std::fs::create_dir_all(&real);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&real, &link);
    // Creating a symlink on Windows needs the privilege or developer mode
    // (CI runners have it); without it `link` does not exist and resolves
    // to nothing.
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_dir(&real, &link);
    let saved = std::env::var_os("LEAN_CTX_STATE_DIR");
    lean_ctx::test_env::set_var("LEAN_CTX_STATE_DIR", link.to_string_lossy().as_ref());
    let dirs = record_dirs();
    // Where the link resolves to: the real directory when the link exists.
    let resolved = std::fs::canonicalize(&link).ok();
    if resolved.is_none() {
        eprintln!("no symlink could be created here; only the override itself is checked");
    }
    match saved {
        Some(value) => {
            lean_ctx::test_env::set_var("LEAN_CTX_STATE_DIR", value.to_string_lossy().as_ref());
        }
        None => lean_ctx::test_env::remove_var("LEAN_CTX_STATE_DIR"),
    }
    let _ = std::fs::remove_dir_all(&root);
    for name in STATE_RECORDS {
        assert!(dirs.contains(&link.join(name)), "{name} below the override");
        if let Some(resolved) = &resolved {
            assert!(
                dirs.contains(&resolved.join(name)),
                "{name} below the resolved link"
            );
        }
    }
}
