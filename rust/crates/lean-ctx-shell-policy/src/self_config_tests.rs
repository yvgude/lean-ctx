// SPDX-License-Identifier: Apache-2.0

use super::*;

fn tampering(cmd: &str) -> Option<Tampering> {
    let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    let protected = vec![
        std::path::PathBuf::from("/home/u/.local/share/lean-ctx"),
        std::path::PathBuf::from("/home/u/.config/lean-ctx"),
    ];
    touches_protected(&tokens, &protected, &[std::path::PathBuf::from("/home/u")])
}

#[test]
fn an_agent_cannot_delete_or_edit_lean_ctx_state() {
    for cmd in [
        "rm /home/u/.local/share/lean-ctx/hive/worker.json",
        "rm -f ~/.local/share/lean-ctx/hive/worker.json",
        "mv $HOME/.local/share/lean-ctx/hive/worker.json /tmp/x",
        "cp /tmp/forged.json ${HOME}/.local/share/lean-ctx/hive/commons/claims.json",
        "dd if=/dev/null of=/home/u/.local/share/lean-ctx/hive/worker.json",
        "truncate -s 0 /home/u/.config/lean-ctx/config.toml",
        "rm /tmp/../home/u/.local/share/lean-ctx/hive/worker.json",
        "sed -i s/true/false/ /home/u/.local/share/lean-ctx/hive/worker.json",
    ] {
        assert!(tampering(cmd).is_some(), "{cmd}");
    }
    for cmd in [
        "cat /home/u/.local/share/lean-ctx/hive/worker.json",
        "ls ~/.local/share/lean-ctx",
        "rm /tmp/scratch.txt",
        "rm ./build/out.o",
        "lean-ctx hive worker status",
    ] {
        assert!(tampering(cmd).is_none(), "{cmd}");
    }
}

/// #2165: `sed -n 1,3p ~/.config/lean-ctx/config.toml` was refused as a
/// change to lean-ctx's state while `head` on the same file ran. The check
/// judges what a command does: reads pass, writes are refused as changes,
/// and a program whose effect it cannot tell is refused as one that may.
#[test]
fn gh2165_state_check_judges_write_intent_not_the_program() {
    let protected = vec![
        std::path::PathBuf::from("/home/u/.local/share/lean-ctx"),
        std::path::PathBuf::from("/home/u/.config/lean-ctx"),
    ];
    let verdict = |cmd: &str| {
        let tokens = crate::shell_tokenize(cmd);
        touches_protected(&tokens, &protected, &[std::path::PathBuf::from("/home/u")])
            .map(|t| t.writes)
    };
    let cfg = "~/.config/lean-ctx/config.toml";
    for (cmd, expected) in [
        (format!("sed -n 1,3p {cfg}"), None),
        (format!("sed -n '/archive/p' {cfg}"), None),
        (format!("sed -ne '1,3p;$=' {cfg}"), None),
        (format!("awk '{{print $1}}' {cfg}"), None),
        (format!("cp {cfg} /tmp/config.toml"), None),
        (format!("cat {cfg} > /tmp/config.toml"), None),
        (format!("sed -i s/a/b/ {cfg}"), Some(true)),
        (format!("sed -i.bak s/a/b/ {cfg}"), Some(true)),
        (format!("sed -Ei 's/a/b/' {cfg}"), Some(true)),
        (format!("sed --in-place=.bak s/a/b/ {cfg}"), Some(true)),
        (format!("sed 's/x/y/w out' {cfg}"), Some(true)),
        (format!("sed -n '1e cmd' {cfg}"), Some(true)),
        (format!("echo x > {cfg}"), Some(true)),
        (format!("printf x >>{cfg}"), Some(true)),
        (format!("perl -pi -e s/a/b/ {cfg}"), Some(true)),
        (format!("tee {cfg}"), Some(true)),
        (format!("ln -sf /tmp/x {cfg}"), Some(true)),
        (format!("python3 edit.py {cfg}"), Some(false)),
        (format!("awk '{{print > \"/tmp/o\"}}' {cfg}"), Some(false)),
    ] {
        assert_eq!(verdict(&cmd), expected, "{cmd}");
    }
}

/// #2231: a command behind `timeout`, `nice`, `env`, a reserved word or a
/// `for` header is judged by the program it runs — `~` is a directory above
/// lean-ctx's state, but `find` and `ls` only read it. The wrapper's own
/// words, `env -S`/`-C`, lookup-changing assignments and redirections
/// anywhere in the command keep the conservative verdict.
#[test]
fn gh2231_wrapped_commands_are_judged_by_their_program() {
    let protected = vec![
        std::path::PathBuf::from("/home/u/.local/share/lean-ctx"),
        std::path::PathBuf::from("/home/u/.config/lean-ctx"),
    ];
    let verdict = |cmd: &str| {
        let tokens = crate::shell_tokenize(cmd);
        touches_protected(&tokens, &protected, &[std::path::PathBuf::from("/home/u")])
    };
    for cmd in [
        "find ~ -name x",
        "timeout 120 find ~ -name x",
        "nice ls ~",
        "nice -n 5 ls ~/.config",
        "nohup ls ~",
        "stdbuf -oL ls ~",
        "timeout -s KILL 5 cat ~/.config/lean-ctx/config.toml",
        "timeout 5 nice ls ~",
        "env FOO=1 ls ~",
        "command ls ~",
        "for d in ~ /tmp",
        "do ls ~",
        "then ls ~",
        "! ls ~",
        "time -p ls ~",
        "timeout 5 ls ~ 2>/dev/null",
        "timeout 5 ls ~ > /tmp/out",
    ] {
        assert!(verdict(cmd).is_none(), "must pass: {cmd}");
    }
    for (cmd, writes) in [
        (
            "timeout 5 cat x > ~/.config/lean-ctx/config.toml",
            Some(true),
        ),
        (">~/.local/share/lean-ctx/x timeout 5 true", Some(true)),
        ("2>>~/.config/lean-ctx/log nice ls /tmp", Some(true)),
        ("timeout 5 rm -rf ~/.config/lean-ctx", None),
        ("nice rm -rf ~/.config", None),
        ("nohup timeout 5 nice rm -rf ~", None),
        ("env -C ~ ls", None),
        ("env -C /tmp ls ~", None),
        ("env --chdir=/tmp ls ~", None),
        ("env -S 'ls ~'", None),
        ("env --split-string='rm -rf ~/.config/lean-ctx'", None),
        ("env PATH=/tmp/bin ls ~", None),
        ("env LD_PRELOAD=/tmp/x.so ls ~", None),
        ("env BASH_ENV=/tmp/x ls ~", None),
        ("env -u ~/.config/lean-ctx ls /tmp", None),
        (
            "timeout 5 sed -i s/a/b/ ~/.config/lean-ctx/config.toml",
            Some(true),
        ),
        ("for d in ~/.config/lean-ctx", Some(false)),
        ("do rm -rf ~/.config/lean-ctx", None),
        ("rm -rf \">\" ~", None),
    ] {
        let found = verdict(cmd);
        assert!(found.is_some(), "must refuse: {cmd}");
        if let Some(writes) = writes {
            assert_eq!(found.map(|t| t.writes), Some(writes), "{cmd}");
        }
    }
}

fn check(cmd: &str) -> Result<(), ShellError> {
    let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    check_self_reconfiguration(cmd, &tokens)
}

#[test]
fn widening_commands_are_blocked() {
    for cmd in [
        "lean-ctx yolo --yes",
        "lean-ctx allow-path /Users/u/.ssh",
        "lean-ctx allow curl",
        "lean-ctx trust",
        "lean-ctx security open --yes",
        "lean-ctx security secrets off --yes",
        "lean-ctx hive worker resume",
        "lean-ctx hive connect --url https://hive.example.com",
        "lean-ctx hive grant --yes",
        "lean-ctx hive runtime install --yes",
        "lean-ctx config set hive.worker.max_tasks_per_day 100",
        "lean-ctx config set hive.sealed_grants auto",
        "lean-ctx knowledge export-agents-md --include-hive",
        "lean-ctx knowledge export-agents-md --write --include-hive",
        "lean-ctx knowledge dedupe --json --apply --project-root /p",
        "lean-ctx knowledge verify ops/deploy --json",
        "lean-ctx config init",
        "lean-ctx config init --full",
        "lean-ctx config create",
        "lean-ctx config set path_jail false",
        "lean-ctx config set shell_security off",
        "lean-ctx config set allow_paths /",
        "lean-ctx config set path_jail_scope home",
        "lean-ctx config set a2a_accept_untrusted_signers true",
        "lean-ctx config set mcp_import_allow_unverified true",
        "lean-ctx.exe yolo",
        "lean-ctx workspace add --file /w.json --expect-revision 1 /Users/u/.ssh",
        "lean-ctx workspace create --file /w.json --id x /Users/u",
        "lean-ctx workspace --file /w.json --expect-revision 3 add /etc",
        "lean-ctx workspace --json add /etc",
        "lean-ctx config set shell_hook_mode off",
        "lean-ctx config set hook_mode mcp",
        "lean-ctx config set shell_strict_mode false",
        "lean-ctx uninstall",
        "lean-ctx unwrap claude",
        "lean-ctx wrap claude --unwrap",
        "lean-ctx addon install x",
        "lean-ctx policy org uninstall",
        "lean-ctx policy org untrust acme",
    ] {
        let err = check(cmd).expect_err(cmd).to_string();
        assert!(
            err.contains("BLOCKED") && err.contains("own terminal"),
            "{cmd}: {err}"
        );
    }
}

/// Every `[hive]` key is the person's: the widening check matches the
/// `hive` section name and its `hive.` prefix (security, Hive rename).
#[test]
fn every_hive_config_key_is_blocked_from_an_agent_shell() {
    for key in [
        "hive",
        "hive.enabled",
        "hive.url",
        "hive.root_key",
        "hive.metrics_upload",
        "hive.receipts_upload",
        "hive.worker.max_tasks_per_day",
    ] {
        let cmd = format!("lean-ctx config set {key} x");
        let err = check(&cmd).expect_err(&cmd).to_string();
        assert!(err.contains(&format!("lean-ctx config set {key}")), "{err}");
    }
    assert!(check("lean-ctx config set hiveish.enabled x").is_err());
}

#[test]
fn reading_and_tightening_stay_available() {
    for cmd in [
        "lean-ctx doctor",
        "lean-ctx gain",
        "lean-ctx allow --list",
        "lean-ctx allow-path --list",
        "lean-ctx allow-path",
        "lean-ctx allow-path --remove /opt/src",
        "lean-ctx allow --remove curl",
        "lean-ctx security status",
        "lean-ctx security secrets on",
        "lean-ctx secure",
        "lean-ctx untrust",
        "lean-ctx config show",
        "lean-ctx config set theme dark",
        "lean-ctx knowledge export-agents-md --write",
        "lean-ctx knowledge stale --json",
        "lean-ctx knowledge dedupe --json --dry-run",
        "lean-ctx hive worker pause",
        "lean-ctx hive worker stop",
        "lean-ctx hive commons list",
        "lean-ctx workspace show",
        "lean-ctx workspace diagnose",
        "lean-ctx workspace remove --file /w.json --expect-revision 2 /opt/src",
        "lean-ctx uninstall --dry-run",
        "lean-ctx addon list",
        "lean-ctx policy org verify policy.toml",
        "git config set user.name x",
    ] {
        assert!(check(cmd).is_ok(), "{cmd}");
    }
}
