// SPDX-License-Identifier: Apache-2.0
//! GHSA-rxv5-4c3x-g4q6 E: constructs that run code no segment shows —
//! assignments, builtins, zsh-only syntax — and a probe that runs tricky
//! inputs through the installed shells and checks the gate blocks each one
//! that executes its marker.

use super::*;

fn verdicts<'a>(allowlist: &str, commands: &[&'a str]) -> Vec<(&'a str, Result<(), ShellError>)> {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", allowlist);
    let results = commands
        .iter()
        .map(|c| (*c, enforce_shell_allowlist(c)))
        .collect();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    results
}

fn assert_all_block(allowlist: &str, commands: &[&str]) {
    for (command, result) in verdicts(allowlist, commands) {
        assert!(result.is_err(), "must block: {command:?}");
    }
}

fn assert_all_pass(allowlist: &str, commands: &[&str]) {
    for (command, result) in verdicts(allowlist, commands) {
        assert!(result.is_ok(), "must pass: {command:?}: {result:?}");
    }
}

#[test]
fn every_assignment_to_a_hijacking_variable_is_blocked() {
    assert_all_block(
        "ls,git,echo,true",
        &[
            "PATH=/tmp/x ls",
            "FOO=1 LD_PRELOAD=/tmp/x.so ls",
            "export LD_PRELOAD=/tmp/x.so; ls",
            "export FOO=1 BASH_ENV=/tmp/x && ls",
            "declare -x LD_PRELOAD=/tmp/x.so; ls",
            "readonly LD_PRELOAD=/tmp/x.so; ls",
            "env FOO=1 LD_PRELOAD=/tmp/x.so ls",
            "env -u HOME LD_PRELOAD=/tmp/x.so ls",
            "env -S 'LD_PRELOAD=/tmp/x.so ls'",
            "BASH_ENV=/tmp/x git status",
            "ENV=/tmp/x git status",
            "PROMPT_COMMAND=x git status",
            "ZDOTDIR=/tmp/x git status",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.pager GIT_CONFIG_VALUE_0=x git log",
            "GIT_PAGER='sh -c x' git log",
            "printf -v BASH_ENV /tmp/x; ls",
            "read LD_PRELOAD < /dev/null; ls",
            "LD_PRELOAD+=:/tmp/x.so ls",
        ],
    );
    // Owner decision: PATH stays as before — only a segment that starts with
    // `PATH=` is blocked; `export PATH=…`, `env [-i] PATH=…` and a later
    // `PATH=` assignment are allowed (agents prepend to PATH constantly).
    assert_all_pass(
        "ls,git,echo,true",
        &[
            "export PATH=/usr/local/bin:$PATH; ls",
            "FOO=1 PATH=/usr/local/bin:$PATH ls",
        ],
    );
    assert_all_pass(
        "ls,git,echo,true",
        &[
            "GIT_PAGER=cat git log",
            "PAGER=cat git log",
            "GIT_EDITOR=true git rebase --continue",
            "FOO=1 BAR=2 git status",
            "export FOO=1 && echo $FOO",
            "IFS= read -r line < /dev/null",
            "echo PATH=/tmp/x",
        ],
    );
}

#[test]
fn builtins_that_run_hidden_code_are_gated() {
    assert_all_block(
        "ls,echo,true",
        &[
            "trap 'evil_binary' EXIT",
            "trap -- 'evil_binary' EXIT INT",
            "trap 'eval x' EXIT",
            "trap '$(evil_binary)' EXIT",
            "hash -p /tmp/x ls; ls",
            "hash -rp /tmp/x ls",
            "enable -f /tmp/x.so x",
            "f() { trap 'evil_binary' EXIT; }; f",
        ],
    );
    assert_all_pass(
        "ls,echo,true,rm",
        &[
            "trap 'rm -f /tmp/x' EXIT",
            "trap - EXIT",
            "trap '' INT",
            "trap -p",
            "hash -r",
        ],
    );
}

#[test]
fn zsh_only_constructs_are_refused_or_gated() {
    assert_all_block(
        "echo,cat,ls,true,git",
        &[
            "cat =(evil_binary)",
            "x='$(evil_binary)'; echo ${(e)x}",
            "echo ${(P)x}",
            "echo *(e:'evil_binary':)",
            "echo *(+evil_binary)",
            "ls f(e:x:)",
            "{ true } always { evil_binary }",
            "{ echo a; evil_binary; } > /dev/null",
            "{ echo a; } evil_binary",
            "for i (a) evil_binary",
            "while true { evil_binary }",
            "if [[ -n x ]] { evil_binary }",
            "echo a |& evil_binary",
            "echo a &| evil_binary",
            "coproc evil_binary",
            "alias ls=evil_binary; ls",
            "repeat 2 evil_binary",
            "noglob evil_binary",
            "x='$(evil_binary)'; echo ${x@P}",
        ],
    );
    assert_all_pass(
        "echo,cat,ls,true,git",
        &[
            "{ echo a; ls; } > /dev/null",
            "{ echo a; ls; } 2>&1",
            "f() { ls; }; f",
            "function f { ls; }; f",
            "for i in a b; do echo $i; done",
            "for ((i=0; i<2; i++)); do echo $i; done",
            "x=(a b); echo ${x[@]}",
            "echo {a,b}",
            "[[ $x =~ ^a ]] && echo y",
        ],
    );
}

#[test]
fn substitutions_in_conditionals_and_after_every_separator_are_gated() {
    assert_all_block(
        "echo,true",
        &[
            "[[ $x =~ $(evil_binary) ]]",
            "true & evil_binary",
            "true; evil_binary",
            "true\nevil_binary",
            "true && evil_binary",
            "true || evil_binary",
            "true | evil_binary",
        ],
    );
}

/// One probe: an input, the shells known to run its marker, and the text the
/// block reason must contain (any one of them). Only the unix marker test
/// below runs the probes.
#[cfg(unix)]
struct Probe {
    input: &'static str,
    shells: &'static [&'static str],
    reasons: &'static [&'static str],
}

#[cfg(unix)]
const ALL: &[&str] = &["bash", "dash", "zsh"];
#[cfg(unix)]
const SUB: &[&str] = &["evil_marker"];
#[cfg(unix)]
const REFUSED: &[&str] = &["cannot be validated"];

#[cfg(unix)]
const fn probe(
    input: &'static str,
    shells: &'static [&'static str],
    reasons: &'static [&'static str],
) -> Probe {
    Probe {
        input,
        shells,
        reasons,
    }
}

#[cfg(unix)]
const PROBES: &[Probe] = &[
    probe("echo ${x:- #$(evil_marker)}", ALL, SUB),
    probe("echo ${x:-(#$(evil_marker))}", &["bash", "dash"], SUB),
    probe("echo \"${x:-'$(evil_marker)'}\"", ALL, REFUSED),
    probe("echo a\r#$(evil_marker)", ALL, REFUSED),
    probe("echo ${x:-a<<EOF}\nevil_marker", ALL, SUB),
    probe(
        "x=abc; echo ${x:0:1<<EOF}\nevil_marker",
        &["bash", "zsh"],
        SUB,
    ),
    probe("echo $[1<<EOF]\nevil_marker", &["bash", "zsh"], REFUSED),
    probe("echo \"$[\"]\" # \"; evil_marker", &["dash"], REFUSED),
    probe("(true)#'\necho $(evil_marker)", ALL, SUB),
    probe("f()#'\n{ true; }\necho $(evil_marker)", ALL, SUB),
    probe("echo \"it's $(evil_marker)\"", ALL, SUB),
    probe("(true)>$(evil_marker)", ALL, SUB),
    probe("echo \\\\\nevil_marker", ALL, SUB),
    probe("cat <<EOF\nx\\\nEOF\nit's $(evil_marker)\nEOF", ALL, SUB),
    probe("echo $((1<<2))\nevil_marker", ALL, SUB),
    probe("(( x = 1 << 2 ))\nevil_marker", &["bash", "zsh"], REFUSED),
    probe(r"echo $'\'' '$(evil_marker)' $'\'''", &["dash"], REFUSED),
    probe(
        "cat <<EOF\n$(evil_marker)\nEOF",
        ALL,
        &["evil_marker", "at command position"],
    ),
    probe("echo a\u{2028}#$(evil_marker)", ALL, REFUSED),
    probe("echo $(true)#$(evil_marker)", ALL, SUB),
    probe("cat =(evil_marker)", &["zsh"], SUB),
    probe("x='$(evil_marker)'; echo ${(e)x}", &["zsh"], REFUSED),
    probe("echo *(e:'evil_marker':)", &["zsh"], REFUSED),
    probe("echo *(+evil_marker)", &["zsh"], REFUSED),
    probe("trap 'evil_marker' EXIT", ALL, SUB),
    probe(
        "{ true } always { evil_marker }",
        &["zsh"],
        &["brace body", "evil_marker"],
    ),
    probe("{ true; evil_marker; } > /dev/null", ALL, SUB),
    probe(
        "for i (a) evil_marker",
        &["zsh"],
        &["short loop", "evil_marker"],
    ),
    probe(
        "repeat 1 { evil_marker }",
        &["zsh"],
        &["brace body", "evil_marker"],
    ),
    probe("echo a |& evil_marker", &["bash", "zsh"], SUB),
    probe(
        "coproc evil_marker",
        &["bash", "zsh"],
        &["coproc", "evil_marker"],
    ),
];

/// The probe's block reason, without the echoed command (which always names
/// the marker).
#[cfg(unix)]
fn reason_text(err: &ShellError) -> &str {
    let message = err.message();
    message.split("\nCommand:").next().unwrap_or(message)
}

/// Every probe is blocked; every probe runs its marker in at least one of
/// its shells that is installed (a probe whose shells are all missing is
/// skipped with a note); the block names the probe's reason. Offline and
/// deterministic: the marker is a local script touching a file in a temp
/// dir, each shell run is killed after a few seconds.
#[cfg(unix)]
#[test]
fn inputs_that_execute_the_marker_are_blocked() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("evil_marker");
    let flag = dir.path().join("ran");
    std::fs::write(&marker, "#!/bin/sh\n: > \"$MARK\"\n").expect("marker script");
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o700))
        .expect("chmod marker");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let inputs: Vec<&str> = PROBES.iter().map(|p| p.input).collect();
    let verdicts = verdicts("echo,cat,true", &inputs);
    let mut failures = Vec::new();
    for (probe, (input, verdict)) in PROBES.iter().zip(&verdicts) {
        let Err(err) = verdict else {
            failures.push(format!("the gate must block {input:?}"));
            continue;
        };
        let reason = reason_text(err);
        if !reason.starts_with("[BLOCKED")
            || !probe
                .reasons
                .iter()
                .any(|expected| reason.contains(expected))
        {
            failures.push(format!(
                "{input:?}: block reason names none of {:?}: {reason}",
                probe.reasons
            ));
        }

        let shells: Vec<String> = probe
            .shells
            .iter()
            .map(|s| format!("/bin/{s}"))
            .filter(|s| std::path::Path::new(s).exists())
            .collect();
        if shells.is_empty() {
            eprintln!(
                "skip marker run for {input:?}: none of {:?} installed",
                probe.shells
            );
            continue;
        }
        // A coprocess runs asynchronously: `sh -c 'coproc x'` may exit before
        // x has written the flag, which made the probe flaky under load. The
        // shell run (not the gated input) waits for it; the gate verdict above
        // is still taken on the exact probe input.
        let run_input = if input.starts_with("coproc ") {
            format!("{input}; wait")
        } else {
            (*input).to_string()
        };
        let mut ran = false;
        for shell in &shells {
            let _ = std::fs::remove_file(&flag);
            let mut child = Command::new(shell)
                .arg("-c")
                .arg(&run_input)
                .env("PATH", &path)
                .env("MARK", &flag)
                .current_dir(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn shell");
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
            ran |= flag.exists();
        }
        if !ran {
            failures.push(format!(
                "{input:?} never ran its marker in {shells:?}: the probe proves nothing"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const ROUND4_ALLOWLIST: &str =
    "echo,ls,git,cat,cargo,tail,find,xargs,grep,env,timeout,nice,nohup,lean-ctx";

/// A substituted command and a wrapper's target get the same per-command
/// checks as a top-level command: allowlist entry, environment overrides,
/// dangerous flags, self-reconfiguration and builtin side doors.
#[test]
fn substituted_and_wrapped_commands_are_gated_like_top_level_ones() {
    assert_all_block(
        ROUND4_ALLOWLIST,
        &[
            "echo $(env LD_PRELOAD=/tmp/x.so ls)",
            "echo $(trap 'evil_binary' EXIT)",
            "echo \"$(hash -p /tmp/x ls)\"",
            "echo \"$(lean-ctx allow evil_binary)\"",
            "echo `lean-ctx allow evil_binary`",
            "echo \"$(git fetch --upload-pack=evil origin)\"",
            "echo \"$(timeout 5 evil_binary)\"",
            "echo \"$(timeout 5 git fetch --upload-pack=evil origin)\"",
            "cat <(lean-ctx allow evil_binary)",
            "timeout 5 git fetch --upload-pack=evil origin",
            "nice timeout 5 git fetch --upload-pack=evil origin",
            "timeout 5 lean-ctx allow evil_binary",
            "timeout 5 env LD_PRELOAD=/tmp/x.so ls",
            "command trap 'evil_binary' EXIT",
            "builtin hash -p /tmp/x ls",
            "nohup env BASH_ENV=/tmp/x git status",
        ],
    );
    assert_all_pass(
        ROUND4_ALLOWLIST,
        &[
            "git commit -m \"$(cat <<'EOF'\nmsg\nEOF\n)\"",
            "echo \"$(git rev-parse HEAD)\"",
            "cargo test 2>&1 | tail -5",
            "timeout 60 cargo test",
            "find . -name '*.rs' | xargs grep -l foo",
            "export PATH=\"$HOME/.cargo/bin:$PATH\"; cargo build",
            "x=$((x+1))",
            "echo \"$(timeout 5 git status)\"",
            "timeout 5 env FOO=1 git status",
        ],
    );
}

/// `<&N` and `<&-` duplicate or close a descriptor; the `&` is no background
/// operator.
#[test]
fn input_descriptor_duplication_is_not_a_background_operator() {
    assert_all_pass(
        "cat,cargo",
        &["cat <&3", "cat 0<&-", "cargo test 2>&1", "cat 0<&3 2>&1"],
    );
    assert_eq!(split_on_operators("cat <&3").len(), 1);
    assert_eq!(split_on_operators("cat 0<&-").len(), 1);
}
