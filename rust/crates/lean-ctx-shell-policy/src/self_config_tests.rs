// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The 3.12.4 Windows CI: after `cd /tmp`, `$PWD` expanded to the resolved
/// `\\?\D:\tmp`, whose `?` read as a glob over the drive, so
/// `cd /tmp && rm -rf "$PWD/lcx-x"` was refused as touching lean-ctx's state.
#[test]
fn pwd_is_spelled_without_the_windows_verbatim_prefix() {
    for (dir, pwd) in [
        (r"\\?\D:\tmp", r"D:\tmp"),
        (r"\\?\UNC\host\share\x", r"\\host\share\x"),
        (r"\\?\Volume{1}\x", r"\\?\Volume{1}\x"),
        (r"C:\tmp", r"C:\tmp"),
        ("/tmp", "/tmp"),
    ] {
        assert_eq!(shell_pwd(std::path::Path::new(dir)), pwd, "{dir}");
    }
    LINE_CWD.with(|c| {
        *c.borrow_mut() = LineCwd::At(vec![std::path::PathBuf::from(r"\\?\D:\tmp")]);
    });
    let tokens: Vec<String> = ["rm", "-rf", "$PWD/lcx-x"].map(String::from).to_vec();
    let variants = expanded_variants(&tokens);
    LINE_CWD.with(|c| *c.borrow_mut() = LineCwd::Process);
    let variants = variants.expect("a few variants");
    assert!(
        variants.iter().flatten().all(|word| !word.contains('?')),
        "{variants:?}"
    );
    assert!(
        variants
            .iter()
            .any(|v| v.last().is_some_and(|w| w.starts_with(r"D:\tmp"))),
        "{variants:?}"
    );
}

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
        "rm /home/u/.local/share/lean-ctx/cloud/worker.json",
        "rm -f ~/.local/share/lean-ctx/cloud/worker.json",
        "mv $HOME/.local/share/lean-ctx/cloud/worker.json /tmp/x",
        "cp /tmp/forged.json ${HOME}/.local/share/lean-ctx/cloud/commons/claims.json",
        "dd if=/dev/null of=/home/u/.local/share/lean-ctx/cloud/worker.json",
        "truncate -s 0 /home/u/.config/lean-ctx/config.toml",
        "rm /tmp/../home/u/.local/share/lean-ctx/cloud/worker.json",
        "sed -i s/true/false/ /home/u/.local/share/lean-ctx/cloud/worker.json",
    ] {
        assert!(tampering(cmd).is_some(), "{cmd}");
    }
    for cmd in [
        "cat /home/u/.local/share/lean-ctx/cloud/worker.json",
        "ls ~/.local/share/lean-ctx",
        "rm /tmp/scratch.txt",
        "rm ./build/out.o",
        "lean-ctx cloud worker status",
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
        "lean-ctx cloud worker resume",
        "lean-ctx cloud connect --url https://cloud.example.com",
        "lean-ctx cloud grant --yes",
        "lean-ctx cloud runtime install --yes",
        "lean-ctx config set cloud.worker.max_tasks_per_day 100",
        "lean-ctx config set cloud.sealed_grants auto",
        "lean-ctx knowledge export-agents-md --include-cloud",
        "lean-ctx knowledge export-agents-md --write --include-cloud",
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
        // §37: only the person's "Send to agents" in LeanCTX Code releases
        // a directive, and only the app reads the release ids.
        "lean-ctx cloud plan release --release-id 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "lean-ctx cloud plan release --release-id x --help",
        "lean-ctx cloud decision release --release-id x",
        "lean-ctx cloud plan updates --json",
    ] {
        let err = check(cmd).expect_err(cmd).to_string();
        assert!(
            err.contains("BLOCKED") && err.contains("own terminal"),
            "{cmd}: {err}"
        );
    }
}

/// Every `[cloud]` key is the person's: the widening check matches the
/// `cloud` section name and its `cloud.` prefix (security, Cloud rename).
#[test]
fn every_cloud_config_key_is_blocked_from_an_agent_shell() {
    for key in [
        "cloud",
        "cloud.enabled",
        "cloud.url",
        "cloud.root_key",
        "cloud.metrics_upload",
        "cloud.receipts_upload",
        "cloud.worker.max_tasks_per_day",
    ] {
        let cmd = format!("lean-ctx config set {key} x");
        let err = check(&cmd).expect_err(&cmd).to_string();
        assert!(err.contains(&format!("lean-ctx config set {key}")), "{err}");
    }
    assert!(check("lean-ctx config set cloudish.enabled x").is_err());
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
        "lean-ctx cloud worker pause",
        "lean-ctx cloud worker stop",
        "lean-ctx cloud commons list",
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

fn reads_records(cmd: &str) -> Option<String> {
    let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    reads_agent_records(
        &tokens,
        &[std::path::PathBuf::from(
            "/home/u/.local/share/lean-ctx/agents",
        )],
        &[std::path::PathBuf::from("/home/u")],
    )
}

/// §37.4: a word of a single command that names the agent records
/// (directives, release ids) in any spelling is refused there too; other
/// lean-ctx state stays readable. Programs that read or list a directory
/// are judged over the whole line (`records_guard`).
#[test]
fn the_agent_records_are_not_readable_from_an_agent_shell() {
    for cmd in [
        "cat /home/u/.local/share/lean-ctx/agents/directive-release-ids.json",
        "head -c 400 ~/.local/share/lean-ctx/agents/directives.json",
        "less $HOME/.local/share/lean-ctx/agents/directive-release-ids.json.damaged",
        "cat ~/.local/share/lean-ctx/agents/*",
        "cat ~/.local/share/lean-ctx/ag*/d*",
        "cat ./directive-release-ids.json",
        "python3 -c print(open('/home/u/.local/share/lean-ctx/agents/x').read())",
        "timeout 5 cat /home/u/.local/share/lean-ctx/agents/registry.json",
        "cat /home/u/.local/share/*/agents/directives.json",
        "cat /home/**/directives.json",
    ] {
        assert!(reads_records(cmd).is_some(), "{cmd}");
    }
    for cmd in [
        "cat /home/u/.local/share/lean-ctx/cloud/worker.json",
        "ls ~/.local/share/lean-ctx",
        "grep rid_ ~/.local/share/lean-ctx/config.toml",
        "rg rid_ /home/u/src/project",
        "lean-ctx agents list",
        "find ~ -name x",
        "fd x ~",
        "ls /*",
        "ls /home/u/*",
        "[[ -d scripts ]]",
    ] {
        assert!(reads_records(cmd).is_none(), "{cmd}");
    }
}
