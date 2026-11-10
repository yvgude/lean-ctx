// SPDX-License-Identifier: Apache-2.0
//! GHSA-rxv5-4c3x-g4q6 E: constructs that run code no segment shows —
//! assignments, builtins, zsh-only syntax — and a probe that runs tricky
//! inputs through the installed shells and checks the gate blocks each one
//! that executes its marker.

use super::*;

fn verdicts<'a>(allowlist: &str, commands: &[&'a str]) -> Vec<(&'a str, Result<(), ShellError>)> {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", allowlist);
    let results = commands
        .iter()
        .map(|c| (*c, enforce_shell_allowlist(c)))
        .collect();
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
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

/// G1: `env` (and `sudo`, whose `NAME=value` words set the environment too)
/// is matched by the program it runs, through any path and any wrapper.
#[test]
fn an_env_reached_by_path_or_wrapper_is_checked_like_env() {
    assert_all_block(
        "ls,git,env,sudo,nice,timeout,nohup,xargs,echo",
        &[
            "/usr/bin/env DYLD_INSERT_LIBRARIES=/tmp/x.dylib ls",
            "/usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "command /usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "command env BASH_ENV=/tmp/x ls",
            "nice -n 5 /usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "timeout 5 /usr/bin/env GIT_SSH_COMMAND=x git fetch",
            "nohup /usr/bin/env NODE_OPTIONS=--require=x ls",
            "echo . | xargs /usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "sudo LD_PRELOAD=/tmp/x.so ls",
            "sudo -E DYLD_INSERT_LIBRARIES=/tmp/x.dylib ls",
            "/usr/bin/sudo GIT_CONFIG_COUNT=1 git log",
        ],
    );
    // The owner's PATH decision is unchanged for every spelling of env.
    assert_all_pass(
        "ls,git,env,sudo,nice,xargs,echo",
        &[
            "env PATH=/usr/local/bin:/usr/bin ls",
            "/usr/bin/env PATH=/usr/local/bin:/usr/bin ls",
            "/usr/bin/env FOO=1 ls",
            "nice -n 5 /usr/bin/env FOO=1 ls",
            "sudo FOO=1 ls",
        ],
    );
}

/// G2: a git setting given on the command line whose value is a command git
/// runs is blocked like its environment form (`GIT_CONFIG_*`,
/// `GIT_SSH_COMMAND`).
#[test]
fn git_command_valued_settings_on_the_command_line_are_blocked() {
    assert_all_block(
        "git,env",
        &[
            "git -c core.pager='sh -c evil' log",
            "git -c core.sshCommand='sh -c evil' fetch",
            "git -c core.editor=evil commit",
            "git -c core.fsmonitor=./evil status",
            "git -c core.hooksPath=./hooks commit -m x",
            "git -c core.gitProxy=./evil fetch",
            "git -c core.askPass=./evil fetch",
            "git -c sequence.editor=evil rebase -i HEAD~2",
            "git -c diff.x.textconv=evil diff",
            "git -c diff.external=evil diff",
            "git -c filter.x.clean=evil add .",
            "git -c filter.x.smudge=evil checkout .",
            "git -c filter.x.process=evil checkout .",
            "git -c merge.x.driver=evil merge main",
            "git -c credential.helper='!evil' fetch",
            "git -c credential.https://h.example.helper=evil fetch",
            "git -c gpg.program=evil commit -S -m x",
            "git -c gpg.ssh.program=evil commit -S -m x",
            "git -c alias.x='!evil' x",
            "git -c alias.st=status st",
            "git -c pager.log=evil log",
            "git -c include.path=./evil.cfg log",
            "git -c includeIf.onbranch:main.path=./evil.cfg log",
            "git -c protocol.ext.allow=always fetch",
            "git -c submodule.x.update='!evil' submodule update",
            "git -c init.templateDir=./t init",
            "git -c uploadpack.packObjectsHook=evil fetch",
            "git -c CORE.PAGER=evil log",
            "git -ccore.pager=evil log",
            "git --config-env=core.pager=EVIL log",
            "git --config-env core.sshCommand=EVIL fetch",
            "git clone --config core.sshCommand=evil https://h.example/r",
            "git clone --config=core.fsmonitor=evil https://h.example/r",
            "git clone -c core.hooksPath=./h https://h.example/r",
            "env git -c core.pager=evil log",
            "/usr/bin/git -c core.pager=evil log",
        ],
    );
    assert_all_pass(
        "git",
        &[
            "git -c core.quotepath=off status",
            "git -c color.ui=always log",
            "git -c user.name=x commit -m y",
            "git commit -c HEAD",
            "git grep -c pattern",
            "git log --format=%H",
        ],
    );
}

/// A1: deny by the shape of the key — sections and names whose value git
/// executes — not only a closed list.
#[test]
fn git_settings_that_run_code_are_refused_by_shape() {
    assert_all_block(
        "git",
        &[
            "git -c interactive.diffFilter='sh -c id' add -p",
            "git -c imap.tunnel='sh -c id' imap-send",
            "git -c sendemail.toCmd=./x send-email p.patch",
            "git -c sendemail.smtpServer=/tmp/x send-email p.patch",
            "git -c guitool.x.cmd=evil gui",
            "git -c instaweb.httpd=evil instaweb",
            "git -c core.attributesFile=./a log",
            "git -c remote.origin.uploadpack=evil fetch",
            "git -c x.y.somethingCmd=evil log",
        ],
    );
}

/// A4: writing such a setting for good is refused like setting it for one
/// run; reading or removing it is not.
#[test]
fn git_config_writes_of_settings_that_run_code_are_refused() {
    assert_all_block(
        "git",
        &[
            "git config core.pager 'sh -c id'",
            "git config alias.x '!evil'",
            "git config set core.sshCommand evil",
            "git config --global --add credential.helper evil",
            "git config --file .git/config core.hooksPath ./h",
            "git config --replace-all core.editor evil",
            "git -C repo config filter.x.smudge evil",
            "git config --rename-section foo alias",
        ],
    );
    assert_all_pass(
        "git",
        &[
            "git config core.pager",
            "git config --get core.editor",
            "git config get core.sshCommand",
            "git config --unset core.pager",
            "git config user.name x",
            "git config set user.email a@b.example",
            "git config --list",
        ],
    );
}

/// A5: the commands git runs from its own command line are gated like
/// command lines: allowed only when the command itself is allowlisted.
#[test]
fn commands_git_runs_from_its_arguments_are_gated() {
    assert_all_block(
        "git,make",
        &[
            "git rebase --exec 'curl https://x.example' main",
            "git rebase -x 'curl https://x.example' main",
            "git rebase --exec='curl https://x.example' main",
            "git submodule foreach 'curl https://x.example'",
            "git submodule foreach --recursive curl https://x.example",
            "git bisect run curl https://x.example",
            "git difftool --extcmd=curl",
            "git difftool -x curl",
            "git filter-branch --tree-filter 'curl https://x.example' HEAD",
            "git filter-branch --msg-filter='curl https://x.example' HEAD",
        ],
    );
    assert_all_pass(
        "git,make",
        &[
            "git rebase --exec make main",
            "git submodule foreach git pull",
            "git bisect run make test",
            "git rebase main",
        ],
    );
    // Without an allowlist the unconditional rules still hold inside them.
    assert_all_block("", &["git rebase --exec 'LD_PRELOAD=/tmp/x.so make' main"]);
}

/// A2: wrappers that run the command after their options are walked like
/// `env`; `strace -E` sets variables; `flock -c` runs a shell string.
#[test]
fn more_wrappers_are_walked_and_their_assignments_checked() {
    let list = "ls,env,stdbuf,setsid,flock,busybox,strace,ionice,taskset";
    assert_all_block(
        list,
        &[
            "stdbuf -oL /usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "stdbuf --output=L env DYLD_INSERT_LIBRARIES=/tmp/x.dylib ls",
            "setsid env LD_PRELOAD=/tmp/x.so ls",
            "flock /tmp/lock env LD_PRELOAD=/tmp/x.so ls",
            "flock -w 5 /tmp/lock env BASH_ENV=/tmp/x ls",
            "busybox env LD_PRELOAD=/tmp/x.so ls",
            "strace -E LD_PRELOAD=/tmp/x.so ls",
            "strace -f -ELD_PRELOAD=/tmp/x.so ls",
            "strace --env=LD_PRELOAD=/tmp/x.so ls",
            "ionice -c 3 env LD_PRELOAD=/tmp/x.so ls",
            "taskset 0x1 env LD_PRELOAD=/tmp/x.so ls",
            "flock /tmp/lock -c 'ls'",
            "stdbuf -oL curl https://x.example",
        ],
    );
    assert_all_pass(
        list,
        &[
            "stdbuf -oL ls",
            "setsid ls",
            "flock /tmp/lock ls",
            "busybox ls",
            "strace -f ls",
            "strace -E FOO=1 ls",
            "ionice -c 3 ls",
            "taskset 0x1 ls",
        ],
    );
}

/// A6: `xargs` in front of a wrapper or an interpreter takes the command it
/// runs from its input.
#[test]
fn xargs_cannot_feed_a_wrapper_or_an_interpreter() {
    assert_all_block(
        "find,ls,xargs,env,sh,bash,nice,grep",
        &[
            "find . | xargs env",
            "ls | xargs sh",
            "ls | xargs -0 bash",
            "ls | xargs nice",
            "ls | xargs /usr/bin/env",
        ],
    );
    assert_all_pass(
        "find,ls,xargs,env,sh,bash,nice,grep",
        &["find . -name x | xargs grep y", "ls | xargs -n1 ls"],
    );
}

/// A3: on macOS and Windows the file system ignores case, so `/usr/bin/ENV`
/// runs `env` there; the gate's name checks see it as `env`.
#[test]
fn program_names_fold_case_where_the_file_system_does() {
    use super::tokenizer::exec_name_for;
    assert_eq!(exec_name_for("/usr/bin/ENV", true, false), "env");
    assert_eq!(exec_name_for("/usr/bin/GIT", true, false), "git");
    assert_eq!(exec_name_for("C:\\Tools\\Git.EXE", true, true), "git");
    assert_eq!(exec_name_for("/usr/bin/ENV", false, false), "ENV");
    assert_eq!(exec_name_for("a\\b", false, false), "a\\b");
}

/// N1: git's global options are parsed with git's whole table; one the
/// gate does not know is refused instead of guessing where the subcommand
/// is.
#[test]
fn git_global_options_are_parsed_with_the_full_table() {
    assert_all_block(
        "git,make",
        &[
            "git --attr-source HEAD config core.pager x",
            "git --attr-source=HEAD config alias.x '!evil'",
            "git --attr-source HEAD rebase -x 'curl https://x.example' main",
            "git --attr-source HEAD submodule foreach 'curl https://x.example'",
            "git --no-such-option config core.pager x",
            "git --git-dir .git --work-tree . config core.editor x",
            "git --exec-path=/tmp/x status",
            "git -ccore.pager=x log",
        ],
    );
    assert_all_pass(
        "git",
        &[
            "git --attr-source HEAD log",
            "git --no-pager -C repo --git-dir=.git status",
            "git -P --literal-pathspecs log -- a",
            "git --version",
            "git --exec-path",
        ],
    );
}

/// N2: the `ext::` transport runs a command; allowing it is refused in
/// every spelling.
#[test]
fn git_ext_transport_is_refused() {
    assert_all_block(
        "git,env",
        &[
            "GIT_ALLOW_PROTOCOL=ext git fetch",
            "env GIT_ALLOW_PROTOCOL=ext git fetch",
            "export GIT_ALLOW_PROTOCOL=ext; git fetch",
            "git fetch 'ext::sh -c id' main",
            "git clone 'ext::sh -c id' x",
            "git clone git+ext::sh x",
            "git fetch origin --upload-pack=x ext::sh",
            "git -c url.ext::sh.insteadOf=https:// fetch",
            "git -c url.https://h.example/.insteadOf=x fetch",
            "git config url.ext::sh.insteadOf https://",
            "git -c protocol.ext.allow=always fetch",
            "git -c protocol.allow=always fetch",
            "GIT_EXEC_PATH=/tmp/x git status",
        ],
    );
    // Searches whose text merely contains `ext::` inside an identifier.
    assert_all_pass(
        "git",
        &[
            "git grep -n 'context::'",
            "git log -S 'next::'",
            "git grep -n 'Context::new'",
        ],
    );
}

/// N3/N4: options of git subcommands that run a program, a pager or a
/// server, or take hook templates.
#[test]
fn git_subcommand_options_that_run_programs_are_refused() {
    assert_all_block(
        "git",
        &[
            "git clone -u 'sh -c id' https://h.example/r",
            "git clone -u'sh -c id' https://h.example/r",
            "git clone --upload-pack='sh -c id' https://h.example/r",
            "git ls-remote -u 'sh -c id' origin",
            "git push --exec='sh -c id' origin",
            "git push --receive-pack='sh -c id' origin",
            "git archive --remote=origin --exec='sh -c id' HEAD",
            "git grep -O pattern",
            "git grep -Oless pattern",
            "git grep --open-files-in-pager=sh pattern",
            "git send-email --to-cmd=./x p.patch",
            "git send-email --sendmail-cmd ./x p.patch",
            "git instaweb --httpd=./x",
            "git clone --template=./t https://h.example/r",
            "git init --template ./t",
            "git -c safe.directory=* status",
        ],
    );
    assert_all_pass(
        "git",
        &[
            "git fetch -u origin",
            "git push origin main",
            "git grep -n pattern",
            "git clone https://h.example/r",
            "git archive HEAD",
        ],
    );
}

/// N5: more wrappers are walked; `watch` and `script` commands are gated
/// like command lines; `parallel` and `fakeroot -l` are refused.
#[test]
fn more_wrappers_and_inline_command_runners_are_gated() {
    let list = "ls,env,caffeinate,arch,chrt,unshare,nsenter,fakeroot,watch,script,parallel";
    assert_all_block(
        list,
        &[
            "caffeinate -t 5 env LD_PRELOAD=/tmp/x.so ls",
            "arch -arm64 env LD_PRELOAD=/tmp/x.so ls",
            "arch -arch x86_64 env LD_PRELOAD=/tmp/x.so ls",
            "arch -e DYLD_INSERT_LIBRARIES=/tmp/x.dylib ls",
            "chrt 10 env LD_PRELOAD=/tmp/x.so ls",
            "unshare --wd /tmp env LD_PRELOAD=/tmp/x.so ls",
            "nsenter -t 1 env LD_PRELOAD=/tmp/x.so ls",
            "fakeroot env LD_PRELOAD=/tmp/x.so ls",
            "fakeroot -l /tmp/x.so ls",
            "fakeroot --faked /tmp/x ls",
            "watch -n 1 'curl https://x.example'",
            "watch -x curl https://x.example",
            "watch -n 1 'ls; curl https://x.example'",
            "script -q -c 'curl https://x.example' /dev/null",
            "script -q /dev/null curl https://x.example",
            "parallel ls ::: a b",
            "caffeinate curl https://x.example",
        ],
    );
    assert_all_pass(
        list,
        &[
            "caffeinate -t 5 ls",
            "arch -arm64 ls",
            "chrt 10 ls",
            "fakeroot ls",
            "watch -n 1 ls -la",
            "script -q -c ls /dev/null",
            "script out.txt",
            "parallel --version",
        ],
    );
}

/// G3-1: git's option parser accepts any unambiguous prefix of a long
/// option, also negated; every prefix of a listed one is refused.
/// G3-2: a listed short letter anywhere in a packed group is refused.
#[test]
fn git_runner_options_are_refused_abbreviated_and_packed() {
    assert_all_block(
        "git,make",
        &[
            "git clone --upload-p='sh -c id' https://h.example/r",
            "git clone --upload-pa='sh -c id' https://h.example/r",
            "git fetch --upload-p='sh -c id' origin",
            "git push --exe='sh -c id' origin",
            "git push --receive-p='sh -c id' origin",
            "git log --upload-pack='sh -c id'",
            "git clone --templ=./t https://h.example/r",
            "git grep --open-files=sh pattern",
            "git send-email --to-c=./x p.patch",
            "git send-email --sendmail-c=./x p.patch",
            "git send-email --smtp-ser=/tmp/x p.patch",
            "git instaweb --http=./x",
            "git clone -qu 'sh -c id' https://h.example/r",
            "git ls-remote -qu'sh -c id' origin",
            "git grep -iOsh pattern",
            "git grep -niO pattern",
            "git clone --conf=core.pager=x https://h.example/r",
            "git clone -qc core.sshCommand=x https://h.example/r",
            "git config --fil .git/config core.pager x",
            "git config --rename-s foo alias",
            "git rebase --exe='curl https://x.example' main",
            "git rebase -ix 'curl https://x.example' main",
            "git rebase -ixcurl main",
            "git difftool --extc=curl",
        ],
    );
    assert_all_pass(
        "git,make",
        &[
            "git clone --depth 1 https://h.example/r",
            "git fetch --prune origin",
            "git push --force-with-lease origin main",
            "git grep -in pattern",
            "git rebase --exe=make main",
            "git log --reverse",
        ],
    );
}

/// G3-3: any program whose arguments name a wrapper or an interpreter as
/// a later program word has that command line walked — no list of
/// launchers to keep growing.
#[test]
fn launchers_of_wrappers_and_interpreters_are_walked() {
    assert_all_block(
        "ls,dbus-run-session,firejail,pkexec,systemd-run,docker,env,sh,bash",
        &[
            "dbus-run-session env LD_PRELOAD=/tmp/x.so ls",
            "firejail --quiet env DYLD_INSERT_LIBRARIES=/tmp/x.dylib ls",
            "pkexec env BASH_ENV=/tmp/x ls",
            "systemd-run --user /usr/bin/env LD_PRELOAD=/tmp/x.so ls",
            "systemd-run --user sh -c 'curl https://x.example'",
            "docker exec box bash -c 'curl https://x.example'",
        ],
    );
    assert_all_pass(
        "ls,dbus-run-session,firejail,docker,env,sh,grep",
        &[
            "dbus-run-session ls",
            "firejail --quiet ls",
            "docker exec box ls",
            "grep -rn bash .",
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

/// #34: builtins that evaluate an operand expand an array subscript in it,
/// quoted or not, and some builtins run a command or function by option.
#[test]
fn builtin_operands_the_shell_evaluates_are_gated() {
    assert_all_block(
        "ls,echo,true",
        &[
            "[[ 'a[$(evil_binary)]' -eq 0 ]]",
            "[[ 0 -lt \"a[$(evil_binary)]\" ]]",
            "[[ $'a[\\x24(evil_binary)]' -eq 0 ]]",
            "[[ -v 'a[$(evil_binary)]' ]]",
            "test -v 'a[`evil_binary`]'",
            "let 'a[$(evil_binary)]'",
            "read 'a[$(evil_binary)]' <<< x",
            "printf -v 'a[$(evil_binary)]' x",
            "unset 'a[$(evil_binary)]'",
            "getopts ab 'a[$(evil_binary)]'",
            "f() { let 'a[$(evil_binary)]'; }; f",
            "command test 'a[$(evil_binary)]' -eq 0",
            "test 'a[$(evil_binary)]' -eq 0",
            "[ -v 'a[$(evil_binary)]' ]",
            "shift 'a[$(evil_binary)]'",
            "return 'a[$(evil_binary)]'",
            "exit 'a[$(evil_binary)]'",
            "ulimit -n 'a[$(evil_binary)]'",
            "printf -v X '%d' 'a[$(evil_binary)]'",
            "compgen -C 'evil_binary' x",
            "jobs -x evil_binary",
            "compgen -W '$(evil_binary)' x",
            "compgen -o default -W '`evil_binary`' x",
            "complete -C evil_binary ls",
            "compgen -F f x",
            "[[ \"a[${X:-\\$}(evil_binary)]\" -eq 0 ]]",
            "unset \"${X:-a[\\$}(evil_binary)]\"",
            "echo $(( ${X:-a[\\$}(evil_binary)] ))",
            "[[ \"a[${X:-$}(evil_binary)]\" -eq 0 ]]",
            "cd 'a[$(evil_binary)]' && echo $(( ${PWD##*/} ))",
            "cd 'a[$(evil_binary)]' && echo $((PWD))",
            "pushd 'a[$(evil_binary)]'; [[ DIRSTACK -eq 0 ]]",
            "builtin cd 'a[$(evil_binary)]'; let OLDPWD",
            "echo 'a[$(evil_binary)]' $(( ${BASH_COMMAND:6:17} ))",
            "echo 'a[$(evil_binary)]' $((BASH_COMMAND))",
            // printf reads its options after expansion.
            "o=-v; printf $o 'a[$(evil_binary)]' x",
            "set -- -v; printf \"$@\" 'a[$(evil_binary)]' x",
            "o=-v; printf \"$o\" 'a[$(evil_binary)]' x",
            "o=-v; command printf $o 'a[$(evil_binary)]' x",
            "o=-v; printf \"$o\" BASH_ENV /tmp/x",
            "o=-vBASH_ENV; printf \"$o\" /tmp/x",
            "printf -vBASH_ENV /tmp/x",
            // A `[` produced by an expansion; `wait -p` assigns a name.
            "a=(1); B='['; wait -p \"a${B}\\$(evil_binary)]\"",
            "wait -p \"a${X:-$'\\x5b'}\\$(evil_binary)]\"",
            "wait -p 'a[$(evil_binary)]'",
            "wait -np BASH_ENV",
            // `wait` reads its options after expansion too.
            "a=(1); o='-pa[$(evil_binary)]'; true & wait -n \"$o\"",
            "o=-pBASH_ENV; true & wait -n \"$o\"",
            "o=-pBASH_ENV; true & command wait -n $o",
            "true & wait $(echo -pBASH_ENV)",
            "true & wait \"$pid\"",
            "pid=$!; pid+=$o; wait \"$pid\"",
            // r4: `${#…}` with an operator is text, a glob may become an
            // option, `$((…) )` is a command substitution.
            "true & wait -n \"${#:+-pBASH_ENV}\"",
            "true & wait -n ${#/0/-pBASH_ENV}",
            "a=(1); : > '-pa[$(evil_binary)]'; true & wait -n -?a*",
            ": > -pBASH_ENV; true & wait -n -?BASH_ENV",
            "true & wait -n ?pBASH_ENV",
            "a=(1); : > '-va[$(evil_binary)]'; printf -?a* x",
            "printf ?vBASH_ENV x",
            "true & wait -n $((echo -pBASH_ENV) )",
            // r5: brace expansion builds the option word.
            "printf {-vBASH_ENV,} /tmp/x; ls",
            "true & wait -n {-pBASH_ENV,}",
            "true & wait -n -{p,}BASH_ENV",
            "printf x{,-vBASH_ENV} /tmp/x",
            // r6: tilde expansion builds the option word.
            "OLDPWD=-vBASH_ENV; printf ~- /tmp/x; ls",
            "OLDPWD=-pBASH_ENV; true & wait -n ~-",
            "PWD=-vBASH_ENV; printf ~+ /tmp/x",
            // r7: tilde expansion reads a directory variable into arithmetic.
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ ~- -eq 0 ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; x=~-; : $((x))",
            "a=(1); PWD='a[$(evil_binary)]'; [[ -v ~+ ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; x=0?2:~-; : $((x))",
            "a=(1); OLDPWD='a[$(evil_binary)]'; let x=~-",
            // r8: `[[` operands inside a group or after `&&` / `||`.
            "a=(1); X='a[$(evil_binary)]'; [[ ( X -eq 0 ) ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ ( ~- -eq 0 ) ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ (~- -eq 0) ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ ( 0 -eq ~- ) ]]",
            "a=(1); X='a[$(evil_binary)]'; [[ ( ( X -eq 0 ) ) ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ ( -v ~- ) ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ 0 -eq 1 || ~- -eq 0 ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ 0 -eq 0 && ~- -eq 0 ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ 0 -eq 0&&~- -eq 0 ]]",
            "a=(1); X='a[$(evil_binary)]'; [[ 0 -eq 1 || X -eq 0 ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ 1 -eq 1 && -v ~- ]]",
            // r9: a `]]` inside a `=~` regex group does not end the `[[`.
            "a=(1); echo='a[$(evil_binary)]'; [[ x =~ ( ]] ) || echo -eq 0 ]]",
            "a=(1); OLDPWD='a[$(evil_binary)]'; [[ x =~ ( ]] ) || echo -eq ~- ]]",
            "a=(1); echo='a[$(evil_binary)]'; [[ x =~ (]]) || echo -eq 0 ]]",
            "a=(1); echo='a[$(evil_binary)]'; [[ x =~ x|( ]] ) && echo -eq 0 ]]",
            "a=(1); echo='a[$(evil_binary)]'; [[ x =~ (( ]] )) || echo -eq 0 ]]",
            // r10: inside a `=~` regex group `#` and `<<` are regex text.
            "[[ x =~ (#) ]]; evil_binary",
            "[[ x =~ ( #) ]]; evil_binary",
            "[[ x =~ (#$(evil_binary)) ]]",
            "a=(1); echo='a[$(evil_binary)]'; [[ x =~ (#) || echo -eq 0 ]]",
            "[[ x =~ ( <<E ) ]]\nevil_binary\nE",
            "[[ x =~ ( <<-E ) ]]\nevil_binary\nE",
            // Regex text to bash, possibly a comment to zsh: the quote then
            // hides the next line from the gate but not from zsh.
            "[[ x =~ (#'\n$(evil_binary)\n') ]]",
            // r11: after a group's `)` or a `|` the regex word goes on.
            "[[ x =~ (a)# ]]; evil_binary",
            "[[ x =~ x|# ]]; evil_binary",
            "[[ x =~ (a)|# ]]; evil_binary",
            "[[ x =~ (a)(b)# ]]; evil_binary",
            "[[ x =~ (a)# ]]\nevil_binary",
            "[[ x =~ (a)#$(evil_binary) ]]",
            // An escaped blank or operator ends no word: no comment follows.
            "echo a\\ #x; evil_binary",
            "echo a\\\t#x; evil_binary",
            "echo a\\;#x; evil_binary",
            "echo a\\|#x; evil_binary",
            "echo a\\&#x; evil_binary",
            "echo \\(#x; evil_binary",
            // r12: the shell unescapes `\\`, `\``, `\$` in a backtick body
            // before it parses it.
            "echo `echo a\\\\ #x; evil_binary`",
            "echo `echo a\\\\\\\\\\\\ #x; evil_binary`",
            "echo `echo a\\\\;#x; evil_binary`",
            "echo \"`echo a\\\\ #x; evil_binary`\"",
            "echo `echo \\`echo a\\\\\\\\ #x; evil_binary\\``",
            "echo \"`echo \\\"'\\\"; evil_binary; \\\"'\\\"`\"",
            "B='[';ulimit -n \"a${B}\\$(evil_binary)]\"",
            "B='['; command ulimit -n \"a${B}\\$(evil_binary)]\"",
        ],
    );
    // Without an allowlist only the `wait` arm of `side_doors` sees an
    // option word that brace or tilde expansion builds (no dataflow check).
    assert_all_block(
        "",
        &[
            "OLDPWD=-pBASH_ENV; true & wait -n ~-",
            "true & wait -n {-pBASH_ENV,}",
            "true & wait -n -{p,}BASH_ENV",
        ],
    );
    assert_all_pass(
        "ls,echo,true",
        &[
            "[[ $x -eq 0 ]] && echo y",
            "[[ -v HOME ]]",
            "unset x",
            "read -r line <<< x",
            "printf -v out '%s' x",
            "compgen -W 'a b c' x",
            "echo 'a[$(x)]'",
            "test 'a[1]' = x",
            "jobs -l",
            "x=(a b); echo $(( ${#x[@]} + ${x[1]} + ${#x} ))",
            "[[ \"${x%/}\" == y ]] && echo y",
            "cd /tmp && echo \"${PWD##*/}\"",
            "printf 'cost: $5 [approx]\\n'",
            "printf '%s\\n' '${arr[0]}'",
            "printf '%d\\n' 'a[$(x)]'",
            "type 'a[$(x)]'",
            "cd 'a[$(x)]' && pwd",
            "printf \"done: $n files\\n\"",
            "printf '%s\\n' \"$x\" \"${y:-z}\"",
            "printf -- \"$fmt\" a",
            "printf \"$msg\\n\" a",
            "sleep 1 & wait $!",
            "sleep 1 & pid=$!; wait \"$pid\"",
            "sleep 1 & pids=\"$pids $!\"; wait $pids",
            "sleep 1 & wait $(jobs -p)",
            "sleep 1 & for p in $(jobs -p); do wait \"$p\"; done",
            "wait -n $! $((n + 1)) %1",
            "wait -p id -n",
            "sleep 1 & wait -n $(( $! )) ${#}",
            "printf '%s\\n' *.rs",
            "printf '%s\\n' {a,b}",
            "printf '%s\\n' ~/x",
            "[[ ~ == /* ]]",
            "x=~/bin; echo $x",
            "[[ ( 1 -eq 1 ) ]] && echo y",
            "x=5; [[ ( $x -gt 1 ) ]] && echo y",
            "[[ -d a && -f b ]] && echo y",
            "[[ $x -eq 0 || $y -eq 1 ]]",
            "[[ x =~ ( ]] ) ]] || echo n",
            "[[ x =~ (#) ]] || echo n",
            "echo a # ( comment",
            "( # c\nls )",
            "[[ x =~ (a)# ]] || echo n",
            "[[ x =~ x|# ]] || echo n",
            "echo a\\ b # c",
            "echo `echo a\\\\ b # c`",
            "echo `echo \\$HOME`",
            "sleep 1 & pid=$!; wait \"${pid}\"",
            "test \"${a%/}/$b\" = x",
        ],
    );
}

/// #35: a value assigned in one statement and evaluated in another — by
/// arithmetic or by a declaration builtin re-parsing an array value — is
/// gated across the statements. PATH prepends stay allowed (owner decision,
/// see `side_doors`).
#[test]
fn values_flowing_between_statements_into_evaluation_are_gated() {
    assert_all_block(
        "ls,echo,true,python3,cat",
        &[
            "P='($(evil_binary))'; declare PIPESTATUS=$P",
            ": '($(evil_binary))'; declare PIPESTATUS=$_",
            "a=(1); v='($(evil_binary))'; declare a=$v",
            "v='($(evil_binary))'; local -a FUNCNAME=\"$v\"",
            "X='a[$(evil_binary)]'; : $((X))",
            "X='a[$(evil_binary)]'; ((X))",
            "X='a[$(evil_binary)]'; let X",
            "X='a[$(evil_binary)]'; [[ X -eq 0 ]]",
            "X='a[$(evil_binary)]'; echo ${b[X]}",
            "X='a[$(evil_binary)]'; echo ${s:X}",
            "X='a[$(evil_binary)]'; Y=$X; echo $((Y + 1))",
            "X=a\\[\\$\\(evil_binary\\)\\]; echo $[X]",
            "read X; echo $((X))",
            "for X in *; do echo $((X)); done",
            "f() { echo $(($1)); }; f x",
            "echo $(( $(cat f) ))",
            ": $_; echo $((_))",
            "X=(*); echo $((X))",
            "X=(*); [[ X -eq 0 ]]",
            "declare -a X=(a?); let X",
            "X+=([0]=a[b]); echo $((X))",
            ": > x; Y='a*'; X=($Y); echo $((X))",
            "Y='a*'; X=(`echo $Y`); let X",
            "Y='a*'; for i in $Y; do echo $((i)); done",
            "o=-v; printf \"$o\" X '%s' 'a[$(evil_binary)]'; echo $((X))",
        ],
    );
    assert_all_pass(
        "ls,echo,true,cargo,make,date,nproc,wc,git,seq",
        &[
            "[[ -f x ]] && echo y",
            "test -n \"$X\"",
            "exit 1",
            "true; [[ \"${PIPESTATUS[0]}\" -eq 0 ]]",
            "x=(a b); echo ${#x[@]} ${x[1]}",
            "[[ $x =~ ^[0-9]+$ ]] && echo y",
            "start=$(date +%s); echo $(( $(date +%s) - start ))",
            "N=$(nproc); make -j$((N))",
            "n=$(git rev-list --count HEAD); echo $((n + 1))",
            "c=$(ls | wc -l); echo $((c))",
            "for i in 1 2 3; do echo $((i * 2)); done",
            "x=(1 '*' \"?\"); echo $((x + 1))",
            "for i in $(seq 1 3); do echo $((i)); done",
            "i=0; i=$((i + 1)); x=5; [[ $x -gt $i ]] && echo $i",
            "echo ${x:-1} ${x:0:2}",
            "export FOO=$HOME/bar; ls",
            "declare -x FOO=\"$BAR\"; ls",
            "unset RUSTFLAGS CARGO_INCREMENTAL; umask 077; cargo test --lib",
            "export X=\"${HOME}/x\" Y=$HOME RUSTFLAGS=\"-C target-cpu=native\"; cargo b",
            "export PATH=\"$HOME/.cargo/bin:$PATH\" && cargo test",
            "export PATH=$PWD/node_modules/.bin:$PATH; cargo test",
            "export PATH=$HOME/.nvm/versions/node/v20/bin:$PATH && cargo b",
            "export PATH=/opt/homebrew/bin:$PATH; cargo test",
            "export PATH=bin:$PATH; cargo test",
            "n=2; X=($((n + 1)) \"$n\"); echo $((X))",
            "for i in $(seq 1 3) $((4)); do echo $((i)); done",
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
/// below runs the probes. `bash_min` is the oldest bash (major, minor) that
/// has the feature the probe uses; an older bash cannot run its marker.
#[cfg(unix)]
struct Probe {
    input: &'static str,
    shells: &'static [&'static str],
    reasons: &'static [&'static str],
    bash_min: (u32, u32),
}

/// `printf -v` assigns to an array element (subscript evaluated): bash 4.1.
#[cfg(unix)]
const PRINTF_V_ARRAY: (u32, u32) = (4, 1);
/// `[[ -v NAME[SUB] ]]` tests an array element (subscript evaluated): bash 4.3.
#[cfg(unix)]
const TEST_V_ARRAY: (u32, u32) = (4, 3);
/// `wait -p VAR` (with `-n`): bash 5.1.
#[cfg(unix)]
const WAIT_P: (u32, u32) = (5, 1);

#[cfg(unix)]
const ALL: &[&str] = &["bash", "dash", "zsh"];
#[cfg(unix)]
const BASH_DASH: &[&str] = &["bash", "dash"];
#[cfg(unix)]
const SUB: &[&str] = &["evil_marker"];
#[cfg(unix)]
const REFUSED: &[&str] = &["cannot be validated"];
#[cfg(unix)]
const SUBSCRIPT: &[&str] = &["array subscript"];
#[cfg(unix)]
const COMPOUND: &[&str] = &["compound assignment"];
#[cfg(unix)]
const NAMED_AT_RUN_TIME: &[&str] = &["built at run time"];
#[cfg(unix)]
const NAMED_OR_SUBSCRIPT: &[&str] = &["built at run time", "array subscript"];

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
        bash_min: (0, 0),
    }
}

/// A bash-only probe that needs bash `bash_min` or newer to run its marker.
#[cfg(unix)]
const fn bash_probe(
    input: &'static str,
    bash_min: (u32, u32),
    reasons: &'static [&'static str],
) -> Probe {
    Probe {
        input,
        shells: &["bash"],
        reasons,
        bash_min,
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
    // #34: operands bash evaluates, and builtins that run a command.
    probe("[[ 'a[$(evil_marker)]' -eq 0 ]]", &["bash"], SUBSCRIPT),
    bash_probe("[[ -v 'a[$(evil_marker)]' ]]", TEST_V_ARRAY, SUBSCRIPT),
    probe("let 'a[$(evil_marker)]'", &["bash"], SUBSCRIPT),
    probe(
        "read 'a[$(evil_marker)]' <<< x",
        &["bash"],
        NAMED_AT_RUN_TIME,
    ),
    bash_probe(
        "printf -v 'a[$(evil_marker)]' x",
        PRINTF_V_ARRAY,
        NAMED_AT_RUN_TIME,
    ),
    probe("compgen -W '$(evil_marker)' x", &["bash"], &["compgen -W"]),
    probe("jobs -x evil_marker", &["bash"], &["jobs -x"]),
    // #35: values that flow into evaluation in a later statement.
    probe("X='a[$(evil_marker)]'; : $((X))", &["bash"], SUBSCRIPT),
    probe("X='a[$(evil_marker)]'; echo ${b[X]}", &["bash"], SUBSCRIPT),
    probe(
        ": > 'a[$(evil_marker)]'; X=(a*); echo $((X))",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        ": > 'a[$(evil_marker)]'; X=(a*); [[ X -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    // #34/#35: an expansion that builds the subscript text. (The `[[ "a[${X:-…}
    // …]" -eq 0 ]]` spelling did not run its marker in the CI bash, so it is
    // only in `builtin_operands_the_shell_evaluates_are_gated`.)
    probe(
        "echo $(( ${X:-a[\\$}(evil_marker)] ))",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "mkdir 'd[$(evil_marker)]'; cd 'd[$(evil_marker)]' && echo $(( ${PWD##*/} ))",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "echo 'a[$(evil_marker)]' $(( ${BASH_COMMAND:6:17} ))",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "P='($(evil_marker))'; declare PIPESTATUS=$P",
        &["bash"],
        COMPOUND,
    ),
    probe(
        ": '($(evil_marker))'; declare PIPESTATUS=$_",
        &["bash"],
        COMPOUND,
    ),
    // r2: printf options read after expansion, globs through an unquoted
    // expansion, a `[` an expansion produces in `wait -p`.
    bash_probe(
        "o=-v; printf $o 'a[$(evil_marker)]' x",
        PRINTF_V_ARRAY,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "set -- -v; printf \"$@\" 'a[$(evil_marker)]' x",
        PRINTF_V_ARRAY,
        NAMED_OR_SUBSCRIPT,
    ),
    probe(
        ": > 'a[$(evil_marker)]'; Y='a*'; X=($Y); echo $((X))",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        ": > 'a[$(evil_marker)]'; Y='a*'; for i in $Y; do echo $((i)); done",
        &["bash"],
        SUBSCRIPT,
    ),
    bash_probe(
        "true & wait -n -p 'a[$(evil_marker)]'",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); B='['; true & wait -n -p \"a${B}\\$(evil_marker)]\"",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); true & wait -n -p \"a${X:-$'\\x5b'}\\$(evil_marker)]\"",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    // r3: `wait` reads `-p` after expansion.
    bash_probe(
        "a=(1); o='-pa[$(evil_marker)]'; true & wait -n \"$o\"",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); o='-pa[$(evil_marker)]'; true & command wait -n $o",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    // r4: `${#:+word}`, a glob in the option word, `$((…) )`.
    bash_probe(
        "a=(1); true & wait -n ${#:+'-pa[$(evil_marker)]'}",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); : > '-pa[$(evil_marker)]'; true & wait -n -?a*",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    // `-v*`: the `-pa[…]` file of the probe above shares the directory.
    bash_probe(
        "a=(1); : > '-va[$(evil_marker)]'; printf -v* x",
        PRINTF_V_ARRAY,
        NAMED_OR_SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); true & wait -n $((echo '-pa[$(evil_marker)]') )",
        WAIT_P,
        NAMED_OR_SUBSCRIPT,
    ),
    // r5: brace expansion builds `-v NAME`.
    bash_probe(
        "a=(1); printf {-v,}'a[$(evil_marker)]' x",
        PRINTF_V_ARRAY,
        NAMED_OR_SUBSCRIPT,
    ),
    // r6: tilde expansion builds `-v NAME`.
    bash_probe(
        "a=(1); OLDPWD='-va[$(evil_marker)]'; printf ~- x",
        PRINTF_V_ARRAY,
        NAMED_OR_SUBSCRIPT,
    ),
    // r7: tilde expansion reads a directory variable into arithmetic.
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ ~- -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; x=~-; : $((x))",
        &["bash"],
        SUBSCRIPT,
    ),
    bash_probe(
        "a=(1); PWD='a[$(evil_marker)]'; [[ -v ~+ ]]",
        TEST_V_ARRAY,
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; x=0?2:~-; : $((x))",
        &["bash"],
        SUBSCRIPT,
    ),
    // `let` reads a word shaped like an assignment, tilde-expanded after `=`.
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; let x=~-",
        &["bash"],
        SUBSCRIPT,
    ),
    // r8: `[[` evaluates operands inside a group and after `&&` / `||`.
    probe(
        "a=(1); X='a[$(evil_marker)]'; [[ ( X -eq 0 ) ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ ( ~- -eq 0 ) ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ (~- -eq 0) ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ ( 0 -eq ~- ) ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ 0 -eq 1 || ~- -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ 0 -eq 0&&~- -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); X='a[$(evil_marker)]'; [[ 0 -eq 1 || X -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    // r9: after `=~` bash reads `( ]] )` as regex text, not the end of `[[`.
    probe(
        "a=(1); echo='a[$(evil_marker)]'; [[ x =~ ( ]] ) || echo -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); OLDPWD='a[$(evil_marker)]'; [[ x =~ ( ]] ) || echo -eq ~- ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); echo='a[$(evil_marker)]'; [[ x =~ (]]) || echo -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); echo='a[$(evil_marker)]'; [[ x =~ x|( ]] ) && echo -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe(
        "a=(1); echo='a[$(evil_marker)]'; [[ x =~ (( ]] )) || echo -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    // r10: inside a `=~` regex group bash reads `#` and `<<` as regex text,
    // not as a comment or a here-document.
    probe("[[ x =~ (#) ]]; evil_marker", &["bash"], SUB),
    probe("[[ x =~ ( #) ]]; evil_marker", &["bash"], SUB),
    probe(
        "[[ x =~ (#$(evil_marker)) ]]",
        &["bash"],
        &["evil_marker", "cannot be validated"],
    ),
    probe(
        "a=(1); echo='a[$(evil_marker)]'; [[ x =~ (#) || echo -eq 0 ]]",
        &["bash"],
        SUBSCRIPT,
    ),
    probe("[[ x =~ ( <<E ) ]]\nevil_marker\nE", &["bash"], REFUSED),
    probe("[[ x =~ ( <<-E ) ]]\nevil_marker\nE", &["bash"], REFUSED),
    // r11: a group's `)` or a `|` does not end the regex word.
    probe("[[ x =~ (a)# ]]; evil_marker", &["bash"], SUB),
    probe("[[ x =~ x|# ]]; evil_marker", &["bash"], SUB),
    probe("[[ x =~ (a)|# ]]; evil_marker", &["bash"], SUB),
    probe("[[ x =~ (a)(b)# ]]; evil_marker", &["bash"], SUB),
    probe("[[ x =~ (a)# ]]\nevil_marker", &["bash"], SUB),
    probe(
        "[[ x =~ (a)#$(evil_marker) ]]",
        &["bash"],
        &["evil_marker", "cannot be validated"],
    ),
    // An escaped blank or operator ends no word, so no comment starts.
    probe("echo a\\ #x; evil_marker", ALL, SUB),
    probe("echo a\\\t#x; evil_marker", ALL, SUB),
    probe("echo a\\;#x; evil_marker", ALL, SUB),
    probe("echo a\\|#x; evil_marker", ALL, SUB),
    probe("echo a\\&#x; evil_marker", ALL, SUB),
    probe("echo \\(#x; evil_marker", ALL, SUB),
    // r12: a backtick body is unescaped before the shell parses it.
    probe("echo `echo a\\\\ #x; evil_marker`", BASH_DASH, SUB),
    probe("echo `echo a\\\\\\\\\\\\ #x; evil_marker`", BASH_DASH, SUB),
    probe("echo `echo a\\\\;#x; evil_marker`", BASH_DASH, SUB),
    probe("echo \"`echo a\\\\ #x; evil_marker`\"", BASH_DASH, SUB),
    probe(
        "echo `echo \\`echo a\\\\\\\\ #x; evil_marker\\``",
        BASH_DASH,
        SUB,
    ),
    probe(
        "echo \"`echo \\\"'\\\"; evil_marker; \\\"'\\\"`\"",
        &["bash"],
        &["evil_marker", "cannot be validated"],
    ),
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

/// The distinct bash binaries on this machine with their (major, minor)
/// version: `/bin/bash` (3.2 on a stock Mac), the Homebrew and `/usr/local`
/// ones, and every `bash` on `PATH`.
#[cfg(unix)]
fn installed_bashes() -> Vec<(std::path::PathBuf, (u32, u32))> {
    let mut candidates: Vec<std::path::PathBuf> = [
        "/bin/bash",
        "/usr/bin/bash",
        "/opt/homebrew/bin/bash",
        "/usr/local/bin/bash",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .collect();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("bash")));
    }
    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for candidate in candidates {
        let Ok(real) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !seen.insert(real) {
            continue;
        }
        let Ok(out) = std::process::Command::new(&candidate)
            .arg("-c")
            .arg("echo \"${BASH_VERSINFO[0]} ${BASH_VERSINFO[1]}\"")
            .stdin(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.split_whitespace().map(str::parse::<u32>);
        if let (Some(Ok(major)), Some(Ok(minor))) = (parts.next(), parts.next()) {
            found.push((candidate, (major, minor)));
        }
    }
    found
}

/// Every probe is blocked; every probe runs its marker in at least one of
/// its shells that is installed (a probe whose shells are all missing, or
/// whose only bash predates the feature it uses, is skipped with a note
/// naming the versions); the block names the probe's reason. Offline and
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

    let bashes = installed_bashes();
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

        let mut shells: Vec<String> = Vec::new();
        let mut too_old: Vec<String> = Vec::new();
        for name in probe.shells {
            if *name == "bash" {
                for (bash, version) in &bashes {
                    if *version >= probe.bash_min {
                        shells.push(bash.display().to_string());
                    } else {
                        too_old.push(format!("{} is {}.{}", bash.display(), version.0, version.1));
                    }
                }
            } else {
                let shell = format!("/bin/{name}");
                if std::path::Path::new(&shell).exists() {
                    shells.push(shell);
                }
            }
        }
        if shells.is_empty() {
            if too_old.is_empty() {
                eprintln!(
                    "skip marker run for {input:?}: none of {:?} installed",
                    probe.shells
                );
            } else {
                eprintln!(
                    "skip marker run for {input:?}: needs bash >= {}.{}, but {}",
                    probe.bash_min.0,
                    probe.bash_min.1,
                    too_old.join(", ")
                );
            }
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
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _list = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
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
            "curl --output-dir ~/.config/lean-ctx -o config.toml https://x",
            "curl --output-dir=$HOME/.config/lean-ctx --output config.toml https://x",
            "cd /tmp && curl --output-dir ~/.config -o lean-ctx/config.toml https://x",
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

/// #2201: after a `cd` the check cannot follow, only the files curl and wget
/// write count as relative paths — the URL and the values of other options
/// (`-m 1`, `-X POST`) are not files in the directory. The refusal names the
/// unresolvable `cd` instead of a file-reading remedy.
#[test]
fn downloads_after_an_unfollowable_cd_judge_only_their_output_files() {
    assert_all_pass(
        WRITER_ALLOWLIST,
        &[
            "S=/tmp; cd $S && curl -s http://127.0.0.1:9/",
            "S=/tmp/x; cd $S && curl -s -m 1 -X POST http://127.0.0.1:9/ -d x",
            "cd \"$D\" && curl -sSL -o /tmp/out.json https://x/config.toml",
            "cd \"$D\" && curl -o - https://x/config.toml",
            "cd \"$D\" && curl --output-dir /tmp -o out.json https://x/config.toml",
            "cd \"$D\" && curl -H 'Accept: o' -d out https://x",
            "cd \"$D\" && wget -qO- https://x/config.toml",
            "cd \"$D\" && wget -O /tmp/y https://x/config.toml",
            "cd \"$D\" && wget --spider https://x/config.toml",
        ],
    );
    assert_all_block(
        WRITER_ALLOWLIST,
        &[
            "cd \"$D\" && curl -o config.toml https://x",
            "cd \"$D\" && curl --output config.toml https://x",
            "cd \"$D\" && curl --output=config.toml https://x",
            "cd \"$D\" && curl -sSLoconfig.toml https://x",
            "cd \"$D\" && curl -O https://x/config.toml",
            "cd \"$D\" && curl -D headers.txt https://x",
            "cd \"$D\" && wget https://x/config.toml",
            "cd \"$D\" && wget -P lean-ctx https://x/config.toml",
            "cd \"$D\" && wget --output-document=config.toml https://x",
            "cd \"$D\" && wget -qOconfig.toml https://x",
        ],
    );
    let (_, verdict) = verdicts(
        WRITER_ALLOWLIST,
        &["S=/tmp; cd $S && curl -s -m 1 -o out.json http://127.0.0.1:9/"],
    )
    .remove(0);
    let message = verdict
        .expect_err("relative output after cd $S")
        .message()
        .to_string();
    assert!(message.contains("`cd $S`"), "names the cd: {message}");
    assert!(
        message.contains("curl out.json"),
        "names the output: {message}"
    );
    assert!(
        !message.contains("sed -n"),
        "no file-reading remedy: {message}"
    );
}

/// #2212: the options that write a named file besides the output — curl's
/// HSTS and Alt-Svc caches, wget's cookies, HSTS, WARC and rejected-URL log
/// — are write targets after an unfollowable `cd`, in both spellings. Into
/// an absolute scratch path they pass.
#[test]
fn downloads_after_an_unfollowable_cd_judge_every_saved_file() {
    let mut blocked = Vec::new();
    let mut passed = Vec::new();
    for option in [
        "curl --hsts",
        "curl --alt-svc",
        "wget -O- --save-cookies",
        "wget -O- --hsts-file",
        "wget -O- --warc-file",
        "wget -O- --rejected-log",
    ] {
        blocked.push(format!(
            "cd \"$D\" && {option} config.toml https://example.com"
        ));
        blocked.push(format!(
            "cd \"$D\" && {option}=config.toml https://example.com"
        ));
        passed.push(format!(
            "cd \"$D\" && {option} /tmp/lcx-2212 https://example.com"
        ));
    }
    let blocked: Vec<&str> = blocked.iter().map(String::as_str).collect();
    let passed: Vec<&str> = passed.iter().map(String::as_str).collect();
    assert_all_block(WRITER_ALLOWLIST, &blocked);
    assert_all_pass(WRITER_ALLOWLIST, &passed);
}

/// #2212 review: files named inside another option's value — curl's
/// `-w '%output{FILE}'`, wget's `-e logfile=FILE` — and the config files a
/// call reads (`curl -K`, `wget --config`, whose contents the gate cannot
/// see) are write targets after an unfollowable `cd`, in every spelling and
/// cluster. A format without `%output` and scratch targets pass.
#[test]
fn downloads_after_an_unfollowable_cd_judge_files_named_in_values_and_configs() {
    let blocked = [
        "cd \"$X\" && curl -s -w '%output{gate_probe.txt}x' file:///dev/null",
        "cd \"$D\" && curl -w '%output{config.toml}' https://example.com",
        "cd \"$D\" && curl --write-out '%output{>>config.toml}' https://example.com",
        "cd \"$D\" && curl --write-out=%output{config.toml} https://example.com",
        "cd \"$D\" && curl -sw%output{config.toml} https://example.com",
        "cd \"$D\" && curl -w @format.txt https://example.com",
        "cd \"$D\" && curl -K curl.cfg https://example.com",
        "cd \"$D\" && curl -sK curl.cfg https://example.com",
        "cd \"$D\" && curl -K - https://example.com",
        "cd \"$D\" && curl -K /tmp/lcx-2212.cfg https://example.com",
        "cd \"$D\" && curl --config curl.cfg https://example.com",
        "cd \"$D\" && curl --config=curl.cfg https://example.com",
        "cd \"$D\" && wget -O- -e logfile=config.toml https://example.com",
        "cd \"$D\" && wget -O- -e 'log_file = config.toml' https://example.com",
        "cd \"$D\" && wget -O- -qe save_cookies=config.toml https://example.com",
        "cd \"$D\" && wget -O- -eHSTS_FILE=config.toml https://example.com",
        "cd \"$D\" && wget -O- --execute warc-file=config.toml https://example.com",
        "cd \"$D\" && wget -O- --execute=rejected_log=config.toml https://example.com",
        "cd \"$D\" && wget -e output_document=config.toml https://example.com",
        "cd \"$D\" && wget -e dir_prefix=sub https://example.com",
        "cd \"$D\" && wget -O- --config wget.rc https://example.com",
        "cd \"$D\" && wget -O- --config=wget.rc https://example.com",
    ];
    let passed = [
        "cd \"$D\" && curl -s -w '%{http_code}' -o /dev/null https://example.com",
        "cd \"$D\" && curl -w '%output{/tmp/lcx-2212}x' https://example.com",
        "cd \"$D\" && wget -O- -e logfile=/tmp/lcx-2212 https://example.com",
        "cd \"$D\" && wget -O- -e robots=off https://example.com",
        "cd \"$D\" && wget -e output_document=- https://example.com",
    ];
    assert_all_block(WRITER_ALLOWLIST, &blocked);
    assert_all_pass(WRITER_ALLOWLIST, &passed);
}

/// #2212 re-review: a config file curl or wget reads may name lean-ctx's
/// state (`output = ~/.config/lean-ctx/worker.json`) in a way no token shows,
/// so it is refused without any `cd` too — the #2165 write-anywhere path.
#[test]
fn downloads_reading_a_config_file_are_refused_without_a_cd() {
    assert_all_block(
        WRITER_ALLOWLIST,
        &[
            "curl -K /tmp/c.cfg https://example.com",
            "curl -sK - https://example.com",
            "curl --config=/tmp/c.cfg https://example.com",
            "curl -w @/tmp/fmt https://example.com",
            "wget -O- --config=/tmp/w.rc https://example.com",
            "wget -O- --config /tmp/w.rc https://example.com",
        ],
    );
    assert_all_pass(
        WRITER_ALLOWLIST,
        &[
            "curl -s -w '%{http_code}' -o /tmp/lcx-2212 https://example.com",
            "wget -O- -e robots=off https://example.com",
        ],
    );
}

/// Relative paths resolve in the directory the command runs in (ctx_shell's
/// `cwd`, a hook payload's `cwd`), not where the gate's process runs.
#[test]
fn relative_paths_resolve_in_the_run_directory() {
    let Some(home) = lean_ctx::core::config::shell_policy::real_user_home() else {
        return;
    };
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _list =
        lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "cp,git,tar");
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
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
    let _cwd = lean_ctx::test_env::CurrentDirGuard::enter(&home);
    assert!(in_project("git add .").is_ok());
    assert!(in_project("tar xf a.tar").is_ok());
    assert!(check_shell_allowlist("tar xf a.tar").is_err());
}
