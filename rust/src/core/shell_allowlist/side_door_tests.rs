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

/// lean-ctx's own policy lives in `LEAN_CTX_*` variables and in its config
/// directory. A command line that sets them for the lean-ctx it starts — or
/// for a nested one — turned the gate off for itself; only the run-control
/// switches (`LEAN_CTX_DISABLED`, …) may be set from a command line.
#[test]
fn a_command_cannot_set_lean_ctx_policy_through_the_environment() {
    assert_all_block(
        "ls,git,echo,lean-ctx,bash,npm,env",
        &[
            "LEAN_CTX_SHELL_SECURITY=off bash -c id",
            "LEAN_CTX_SHELL_SECURITY=off lean-ctx -c 'bash -c id'",
            "export LEAN_CTX_SHELL_SECURITY=off && lean-ctx -c 'bash -c id'",
            "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE=bash lean-ctx -c 'bash -c id'",
            "LEAN_CTX_CONFIG_DIR=./x lean-ctx -c ls",
            "env LEAN_CTX_DATA_DIR=./x lean-ctx gain",
            "HOME=/tmp/x lean-ctx -c ls",
            "env XDG_CONFIG_HOME=/tmp/x lean-ctx -c ls",
            "export HOME=/tmp/x; lean-ctx -c ls",
        ],
    );
    assert_all_pass(
        "ls,git,echo,lean-ctx,bash,npm,env",
        &[
            "LEAN_CTX_DISABLED=1 git status",
            "LEAN_CTX_RAW=1 lean-ctx -c 'git status'",
            "HOME=/tmp/x npm test",
        ],
    );
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

const STRUCTURAL_ALLOWLIST: &str =
    "ls,git,echo,cat,lean-ctx,npm,env,cargo,sed,awk,rm,ln,printf,xargs,id,head,mv";

/// A lean-ctx call from an agent shell runs only the listed subcommands, and
/// the words that pick what runs must be literal: the shell expands `$k`,
/// `{a,}` or a word fed by `xargs` after the check.
#[test]
fn leanctx_from_an_agent_shell_runs_listed_subcommands_with_literal_words() {
    assert_all_block(
        STRUCTURAL_ALLOWLIST,
        &[
            "k=shell_security; lean-ctx config set \"$k\" off --yes",
            "y=yolo; lean-ctx $y --yes",
            "m=__session-starter; lean-ctx $m prog",
            "echo 'yolo --yes' | xargs lean-ctx",
            "lean-ctx {yolo,} --yes",
            "lean-ctx",
            "lean-ctx setup",
            "lean-ctx init --global",
            "lean-ctx hook rewrite",
            "lean-ctx daemon start",
            "lean-ctx proxy enable",
            "lean-ctx doctor --fix",
            "lean-ctx profile set loose",
            "lean-ctx -c 'bash -c id'",
            "lean-ctx -c 'git status; curl -s x'",
            // A refused flag behind an expansion.
            "f=--fix; lean-ctx doctor $f",
            "lean-ctx doctor ${F:---fix}",
            "w=--web; lean-ctx gain $w",
        ],
    );
    assert_all_pass(
        STRUCTURAL_ALLOWLIST,
        &[
            "lean-ctx read src/main.rs",
            "lean-ctx search 'foo*'",
            "lean-ctx read __init__.py",
            "lean-ctx gain",
            "lean-ctx -c 'cargo test'",
            "lean-ctx -c 'cargo test && git status'",
            "lean-ctx doctor",
            "lean-ctx doctor --json",
            "lean-ctx --version",
            "lean-ctx knowledge recall auth",
            "lean-ctx session status",
            "lean-ctx config show",
        ],
    );
}

/// `config set` from an agent shell reaches presentation and compression
/// keys only; everything that can loosen security is the person's.
#[test]
fn agent_shell_config_set_is_limited_to_safe_keys() {
    assert_all_block(
        STRUCTURAL_ALLOWLIST,
        &[
            "lean-ctx config set shell_allow_writes true",
            "lean-ctx config set hook_binary /tmp/x",
            "lean-ctx config set read_redirect off",
            "lean-ctx config set allowed_destinations x",
            "lean-ctx config set policy_floor off",
        ],
    );
    assert_all_pass(
        STRUCTURAL_ALLOWLIST,
        &["lean-ctx config set compression_level max"],
    );
    let msg = verdicts(
        STRUCTURAL_ALLOWLIST,
        &["lean-ctx config set shell_allow_writes true"],
    )
    .remove(0)
    .1
    .expect_err("refused")
    .to_string();
    assert!(msg.contains("own terminal"), "{msg}");
}

/// Writes into lean-ctx's state through any redirect form, a path inside a
/// sed/awk script, an abbreviated long option, a link, a glob or a `cd`.
#[test]
fn every_spelling_of_a_state_write_is_refused() {
    let cfg = "~/.config/lean-ctx/config.toml";
    let blocked = [
        format!("echo x 1<>{cfg}"),
        format!("echo x >&{cfg}"),
        format!("sed -n '1w {cfg}' README.md"),
        format!("awk '{{print > \"{cfg}\"}}' README.md"),
        format!("sed --in-pl s/a/b/ {cfg}"),
        "ln -s ~/.config/lean-ctx /tmp/lc".to_string(),
        "rm ~/.config/lean-*/config.toml".to_string(),
        "cd ~/.config/lean-ctx && rm config.toml".to_string(),
        "rm ${HOME}/.config/lean-ctx/config.toml".to_string(),
    ];
    let blocked: Vec<&str> = blocked.iter().map(String::as_str).collect();
    assert_all_block(STRUCTURAL_ALLOWLIST, &blocked);
    let read = [
        format!("cat {cfg}"),
        format!("sed -n 1,3p {cfg}"),
        format!("head -5 {cfg}"),
        "echo x >&2".to_string(),
        "lean-ctx read README.md".to_string(),
    ];
    let read: Vec<&str> = read.iter().map(String::as_str).collect();
    assert_all_pass(STRUCTURAL_ALLOWLIST, &read);
    // The redirect reaches the state check in the real gate, not only the
    // helper.
    let _lock = crate::core::data_dir::test_env_lock();
    let _list = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
    let _mode = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    assert!(check_shell_allowlist(&format!("echo x 1<>{cfg}")).is_err());
}

/// Every way a line can set a lean-ctx policy variable for the lean-ctx it
/// starts: clustered and abbreviated `env` options, a bare `export` after a
/// `${N:=…}` default, a name built at run time, a nameref, and `HOME` in
/// another segment.
#[test]
fn policy_variables_cannot_be_set_by_any_spelling() {
    assert_all_block(
        STRUCTURAL_ALLOWLIST,
        &[
            "env -vS 'LEAN_CTX_SHELL_SECURITY=off lean-ctx -c id'",
            "env --split-s='LEAN_CTX_SHELL_SECURITY=off lean-ctx -c id'",
            ": ${LEAN_CTX_SHELL_SECURITY:=off}; export LEAN_CTX_SHELL_SECURITY; lean-ctx -c id",
            "export LEAN_CTX_SHELL_SECURITY; lean-ctx -c id",
            "v=LEAN_CTX_SHELL_SECURITY; printf -v \"$v\" off; lean-ctx -c id",
            "n=LEAN_CTX_SHELL_SECURITY; export \"$n=off\"; lean-ctx -c id",
            "env \"${N}=off\" lean-ctx -c id",
            "declare -n r=LEAN_CTX_SHELL_SECURITY; r=off; lean-ctx -c id",
            "HOME=/tmp/x; lean-ctx -c ls",
            "XDG_CONFIG_HOME=/tmp/x && lean-ctx -c ls",
        ],
    );
    assert_all_pass(
        STRUCTURAL_ALLOWLIST,
        &[
            "export PATH=/opt/bin:$PATH && cargo test",
            "HOME=/tmp/x npm test",
            "cargo test && git status",
            "read -r -p \"$prompt\" answer",
        ],
    );
}

const REACH_ALLOWLIST: &str =
    "ls,git,echo,cat,lean-ctx,cargo,gh,cp,mv,rm,ln,tar,unzip,mkdir,printf,tr";

/// lean-ctx's state is reached through the directories above it, through
/// relative paths after a `cd`, and through lean-ctx's own writing
/// subcommands — not only through paths inside it.
#[test]
fn state_is_reached_through_ancestors_relative_paths_and_leanctx_writers() {
    assert_all_block(
        REACH_ALLOWLIST,
        &[
            "mkdir -p /tmp/e/lean-ctx && printf 'x\\n' > /tmp/e/lean-ctx/config.toml && cp -r /tmp/e/lean-ctx ~/.config/",
            "cd ~/.config && cp /tmp/e.toml lean-ctx/config.toml",
            "cd ~ && rm .local/share/lean-ctx/hive/worker.json",
            "tar xf e.tar -C ~",
            "unzip -o e.zip -d ~/.config",
            "mv ~/.config /tmp/old && cp -r /tmp/e ~/.config",
            "ln -sfn ~/.config /tmp/l && cp e.toml /tmp/l/lean-ctx/config.toml",
            "lean-ctx graph export-html --out ~/.config/lean-ctx/config.toml",
            "lean-ctx roi --export ~/.local/share/lean-ctx/hive/worker.json",
            "lean-ctx knowledge export --output ~/.config/lean-ctx/x",
            "lean-ctx knowledge export-agents-md --write --file ~/.local/share/lean-ctx/hive/worker.json",
        ],
    );
    assert_all_pass(
        REACH_ALLOWLIST,
        &[
            "cp a.txt ./x",
            "cp -r src/ /tmp/out",
            "tar xf a.tar -C /tmp/x",
            "lean-ctx read ~/.config/lean-ctx/config.toml",
            "git commit -m \"fix: read ~/.config/lean-ctx/config.toml first\"",
            "gh pr create --body \"see ~/.config/lean-ctx/config.toml\"",
            "cargo run -- read ~/.config/lean-ctx/config.toml",
            "tr / _",
            "cd src && ls",
        ],
    );
}

const WRITER_ALLOWLIST: &str = "ls,git,echo,cat,lean-ctx,cargo,cp,mv,rm,ln,tar,unzip,mkdir,\
     curl,wget,gcc,sort,find,mvn,tree,fd,df,code,open,printf,bat,rsync,cpio,pax,chmod,npm,docker";

/// Conservative default: a program not known to only read is refused when
/// any word (attached option values included) is inside lean-ctx's state or
/// a directory above it. Narrow exceptions: git reads, viewers without
/// their writing flags, curl/wget output files outside the state, and
/// plain non-recursive copies judged by the path they create.
#[test]
fn writers_options_and_extractions_reach_state_only_where_they_land() {
    assert_all_block(
        WRITER_ALLOWLIST,
        &[
            "curl -o$HOME/.config/lean-ctx/config.toml https://x",
            "wget -O$HOME/.local/share/lean-ctx/hive/worker.json x",
            "gcc -o$HOME/.config/lean-ctx/x a.c",
            "sort -o$HOME/.config/lean-ctx/config.toml f",
            "tar -xf e.tar -C$HOME",
            "unzip -o e.zip -d$HOME/.config",
            "echo y | find /tmp/e -name e.toml -ok cp {} ~/.config/lean-ctx/config.toml \\;",
            "mvn -Dmaven.repo.local=$HOME/.config/lean-ctx/x install",
            "cd ~ && tar xf /tmp/e.tar",
            "cd ~/.config && unzip -o /tmp/e.zip",
            "tar -xPf e.tar",
            "unzip -: e.zip",
            "D=~/.config/lean-ctx; lean-ctx graph export-html --out \"$D/config.toml\"",
            "cd \"$D\" && lean-ctx knowledge export --output config.toml",
            "mkdir -p /tmp/e/lean-ctx && printf 'x\\n' > /tmp/e/lean-ctx/config.toml && cp -r /tmp/e/lean-ctx ~/.config/",
            "mv ~/.config /tmp/old && cp -r /tmp/e ~/.config",
            "ln -sfn ~/.config /tmp/l && cp e.toml /tmp/l/lean-ctx/config.toml",
            "tar xf e.tar -C ~",
            "tree -o ~/.config/lean-ctx/config.toml",
            "fd . ~/.config/lean-ctx -x rm",
            "fd -x cp /tmp/e {} . ~/.local/share/lean-ctx/hive",
            "code --user-data-dir ~/.config/lean-ctx",
            "open --stdout ~/.config/lean-ctx/config.toml -a TextEdit",
            "bat cache --build --target ~/.config/lean-ctx",
            "curl -sSLo ~/.config/lean-ctx/config.toml https://x",
            "curl -o /tmp/a https://x -o ~/.config/lean-ctx/config.toml https://y",
            "curl -D ~/.config/lean-ctx/config.toml https://x",
            "wget -qO ~/.local/share/lean-ctx/hive/worker.json x",
            "wget -o ~/.config/lean-ctx/config.toml x",
            "cp -r /tmp/e/ ~/.config/",
            "cp -r /tmp/e/. ~/.config/",
            "cp -r /tmp/e/* ~/.config/",
            "cp -rT /tmp/e ~/.config",
            "rsync -a /tmp/e/ ~/.config/",
            "rsync -a --delete /tmp/empty/ ~/.config/",
            "find . | cpio -pdm ~/.config",
            "pax -rw /tmp/e ~/.config",
            "chmod 000 ~/.config",
            "chmod -x ~/.local/share",
            "cd ~ && git checkout -- .",
        ],
    );
    assert_all_pass(
        WRITER_ALLOWLIST,
        &[
            "Get-Content ~/.config/lean-ctx/config.toml",
            "git -C ~ status",
            "git -C .. log",
            "df -h ~",
            "tree ~",
            "fd x ~",
            "code ~",
            "open ~/.config",
            "cp file ~",
            "mv build ~/",
            "cp x ~/.local/",
            "git add .",
            "cargo build --target-dir ~/target",
            "mkdir -p ~/.config/foo",
            "rm -rf ./target",
            "curl -o out.json https://x",
            "npm ci",
            "docker run -v ~:/h img",
            "wget -P ~ https://x/y",
            "git commit -m \"fix: read ~/.config/lean-ctx/config.toml first\"",
        ],
    );
}

/// Relative paths resolve in the directory the command runs in (ctx_shell's
/// `cwd`, a hook payload's `cwd`), not where the gate's process runs.
#[test]
fn relative_paths_resolve_in_the_run_directory() {
    let Some(home) = crate::core::config::shell_policy::real_user_home() else {
        return;
    };
    let _lock = crate::core::data_dir::test_env_lock();
    let _list = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "cp,git,tar");
    let _mode = crate::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    let project = tempfile::tempdir().expect("project");
    let up = "../".repeat(40);
    // Without the root and the drive (`C:` on Windows): the temp dir and home
    // share a drive, so 40 `../` reach its root on every platform.
    let home_rel = home
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let into_state = format!("cp /tmp/e {up}{home_rel}/.config/lean-ctx/config.toml");
    let grant: Vec<String> = Vec::new();
    let in_project = |cmd: &str| {
        super::with_run_cwd(Some(project.path().to_path_buf()), || {
            check_shell_allowlist_granted(cmd, &grant)
        })
    };
    assert!(in_project(&into_state).is_err(), "{into_state}");
    // The gate's own process sits in the home directory; the command runs
    // in the project.
    let _cwd = crate::test_env::CurrentDirGuard::enter(&home);
    assert!(in_project("git add .").is_ok());
    assert!(in_project("tar xf a.tar").is_ok());
    assert!(check_shell_allowlist("tar xf a.tar").is_err());
}
