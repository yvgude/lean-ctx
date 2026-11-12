// SPDX-License-Identifier: Apache-2.0
//! The shell-gate reports fixed in 3.12.4, row by row, through the whole gate
//! (`enforce`, with no allowlist and with one), in a run directory three
//! levels below the home directory:
//! - GHSA-4gm5-2mgw-9r9r: a redirection first or glued to a word;
//! - GHSA-9jrf-q94p-frhr: commands inside a `( … )` subshell;
//! - GHSA-cfmv-qg29-3rvp: the working directory and values a line builds;
//! - GHSA-fv44-pm58-9c8p: git "read" subcommands that write or run programs.

const ALLOWLIST: &str = "ls,cat,echo,printf,rm,touch,sed,tar,git,cd,go,grep,lean-ctx,awk,sort,\
     gzip,patch,split,npx,uv,deno,read,true,mkdir,cp,head,tail,wc,for,while,f,g";

fn gate(cmd: &str, allowlist: &str) -> Result<(), String> {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _list = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", allowlist);
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    let _inline = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", "false");
    let home = lean_ctx::core::config::shell_policy::real_user_home().expect("home");
    let run = home.join("proj").join("a").join("b");
    super::with_run_cwd(Some(run), || super::enforce_shell_allowlist(cmd))
        .map_err(|err| err.to_string())
}

fn blocked(cmds: &[&str]) {
    let mut passed = Vec::new();
    for cmd in cmds {
        for list in ["", ALLOWLIST] {
            if gate(cmd, list).is_ok() {
                passed.push(format!("[allowlist {:?}] {cmd:?}", !list.is_empty()));
            }
        }
    }
    assert!(passed.is_empty(), "must be blocked:\n{}", passed.join("\n"));
}

fn passes(cmds: &[&str]) {
    let mut refused = Vec::new();
    for cmd in cmds {
        for list in ["", ALLOWLIST] {
            if let Err(err) = gate(cmd, list) {
                refused.push(format!(
                    "[allowlist {:?}] {cmd:?}\n  {err}",
                    !list.is_empty()
                ));
            }
        }
    }
    assert!(refused.is_empty(), "should pass:\n{}", refused.join("\n"));
}

/// GHSA-4gm5-2mgw-9r9r: bash reads a redirection anywhere in a simple
/// command, glued to a word (`cmd>file`) or before the program
/// (`>file cmd`); the gate read the first word as the program.
#[test]
fn ghsa_4gm5_redirection_first_or_glued() {
    blocked(&[
        "echo x > ~/.config/lean-ctx/config.toml",
        ">~/.config/lean-ctx/config.toml",
        "2>~/.config/lean-ctx/config.toml ls",
        "cat /etc/hostname>~/.config/lean-ctx/config.toml",
        "ls {fd}>~/.config/lean-ctx/config.toml",
        "ls {fd}> ~/.config/lean-ctx/config.toml",
        "echo x>>~/.config/lean-ctx/config.toml",
        "echo x 2>&1>~/.config/lean-ctx/config.toml",
        ">/tmp/cat rm -r -f ~/.config/lean-ctx",
        ">/tmp/cat eval echo hi",
        "2>/dev/null eval echo hi",
        "2>/dev/null lean-ctx config set shell_security off",
        "lean-ctx>/tmp/cat config set shell_security off",
        "lean-ctx config set shell_security off",
        ">/tmp/x bash -c 'echo hi'",
        "<>/tmp/x rm -rf ~/.config/lean-ctx",
    ]);
    passes(&[
        "ls 2>/dev/null",
        "echo hi >/tmp/lcx-out.txt",
        "echo a>/tmp/lcx-x",
        "ls 2>&1 | head -5",
        "cat <<'EOF'\na > b < c\nEOF",
        "cat < /etc/hosts",
        "echo x>&2",
        "grep -c '<div>' /tmp/lcx-page.html",
    ]);
}

/// GHSA-9jrf-q94p-frhr: a `( … )` subshell was one segment whose first word
/// (`(eval`, `(rm`) matched no rule.
#[test]
fn ghsa_9jrf_subshell_commands_are_judged() {
    blocked(&[
        "{ eval echo hi; }",
        "(eval echo hi)",
        "( eval echo hi )",
        "(bash -c 'echo hi')",
        "(lean-ctx config set shell_security off)",
        "(rm -rf ~/.config/lean-ctx)",
        "(time -p eval echo hi)",
        "(time -- eval echo hi)",
        "(echo hi) > ~/.config/lean-ctx/config.toml",
        "{ echo hi; } > ~/.config/lean-ctx/config.toml",
        "for i in 1; do echo $i; done > ~/.config/lean-ctx/config.toml",
        "(rm -rf ~/.config/lean-ctx) <<< x",
        "(rm -rf ~/.config/lean-ctx) <<EOF\nx\nEOF",
        "(rm -rf ~/.config/lean-ctx) < <(echo x)",
        "cd ~/.config; (rm -rf lean-ctx) <<< x",
        "echo ok && (rm -rf ~/.config/lean-ctx)",
        "true | (rm -rf ~/.config/lean-ctx)",
    ]);
    passes(&[
        "(cd /tmp && ls)",
        "for d in a b; do (cd $d && go test ./...); done",
        "(echo hi) 2>&1",
        "{ echo a; echo b; } > /tmp/lcx-out",
        "(ls; echo done) | head -3",
    ]);
}

/// GHSA-cfmv-qg29-3rvp: the working directory a line may reach.
#[test]
fn ghsa_cfmv_working_directory() {
    blocked(&[
        "cd ..; cd ..; cd ..; rm -rf .config/lean-ctx",
        "(cd ..; cd ..; cd ..; rm -rf .config/lean-ctx)",
        "{ cd ..; cd ..; cd ..; rm -rf .config/lean-ctx; }",
        "cd ~; cd /does-not-exist; rm -rf .config/lean-ctx",
        "cd ~/proj/a/b; for i in 1 2 3; do cd ..; done; rm -rf .config/lean-ctx",
        "cd ~/.config && rm -rf *",
        "cd ~/.config/lean-ctx && rm -f *",
        "CDPATH=~ cd .config && rm -rf lean-ctx",
        "cd \"$D\" && sed -i s/a/b/ config.toml",
        "cd \"$D\" && tar -xf /tmp/x.tar",
        "cd \"$D\" && sort -o config.toml /tmp/x",
        "cd \"$D\" && gzip config.toml",
        "cd \"$D\" && patch config.toml /tmp/x.diff",
        "cd \"$D\" && split -l 1 /tmp/x config",
        "f() { cd ..; }; f; f; f; rm -rf .config/lean-ctx",
        "HOME=/tmp/h; cd; cd ~/proj/a/b; cd ../../..; rm -rf .config/lean-ctx",
        "pushd ~/.config && rm -rf lean-ctx",
    ]);
    passes(&[
        "cd /tmp && rm -rf lcx-build",
        "cd /tmp; rm -f lcx-x.o",
        "rm -rf ./target/tmp",
        "sed -i s/a/b/ config.toml",
        "cd ~/proj && tar -xf /tmp/x.tar",
        "for d in a b; do (cd $d && go test ./...); done",
        "cd .. && ls",
        "cd ~/.config && cat lean-ctx/config.toml",
        "cd ~/.config && ls *",
    ]);
}

/// GHSA-cfmv-qg29-3rvp addenda: targets a line builds itself.
#[test]
fn ghsa_cfmv_values_the_line_builds() {
    blocked(&[
        "d=~/.config/lean-ctx; touch $d/x",
        "d=$HOME/.config; e=$d/lean-ctx; rm -rf $e",
        "{ d=~/.config/lean-ctx; touch $d/x; }",
        "a=(x ~/.config/lean-ctx); touch ${a[1]}/x",
        "printf -v d %s ~/.config/lean-ctx; touch $d/x",
        "read -a a <<< \"x ~/.config/lean-ctx\"; rm -rf ${a[1]}",
        "d=~/.config/lean-ctx; r=d; touch ${!r}/x",
        "d=~/.config/x; touch ${d/x/lean-ctx}/y",
        "touch ~/.config/lean-{ctx,x}/y",
        "f() { rm -rf \"$1\"; }; f ~/.config/lean-ctx",
        "f() { rm -rf \"$@\"; }; f ~/.config/lean-ctx",
        "f() { for x; do rm -rf $x; done; }; f ~/.config/lean-ctx",
        "f() { rm -rf ${@:1}; }; f ~/.config/lean-ctx",
        "f() { rm -rf \"$1\"; }; f ~/.config/lean-c*",
        "GIT_DIR=~/.config/lean-ctx/.git git add -A",
        "GIT_WORK_TREE=~/.config/lean-ctx git checkout .",
        "npx node -e 'require(\"fs\")'",
        "uv run python -c 'print(1)'",
        "python3.12 -c 'print(1)'",
        "node@18 -e 'x'",
        "deno eval 'x'",
        "awk 'BEGIN{print 1 | \"sh\"}'",
        "awk 'BEGIN{system (\"id\")}'",
        "awk 'BEGIN{print 1 > ENVIRON[\"HOME\"] \"/.config/lean-ctx/x\"}'",
        "awk '{print $1 | \"sh\"}' /tmp/x",
        "awk 'BEGIN{\"id\" | getline x}'",
        "alias f='rm -rf'; cd ~/.config; f lean-ctx",
        "read d <<EOF\n~/.config/lean-ctx\nEOF\nrm -rf \"$d\"",
        "while read f; do rm -rf \"$f\"; done <<EOF\n~/.config/lean-ctx\nEOF",
        "read d < <(echo ~/.config/lean-ctx); rm -rf \"$d\"",
        "alias f='rm -rf ~/.config/lean-ctx'; alias g='echo; f'; g",
        "alias f='rm -rf ~/.config/lean-ctx'; g() { f; }; g",
        "mapfile -t a <<< ~/.config/lean-ctx; rm -rf \"${a[0]}\"",
    ]);
    passes(&[
        "d=/tmp/lcx-x; rm -rf $d",
        "for f in a b; do rm -f $f.o; done",
        "x=1; echo $x",
        "awk '{print $1}' /tmp/x",
        "awk -F: '{print $1}' /etc/passwd | head -3",
        "f() { echo \"$1\"; }; f hi",
        "read -r line < /tmp/x; echo \"$line\"",
        "git status",
    ]);
}

/// GHSA-fv44-pm58-9c8p: options that make a git "read" write or run.
#[test]
fn ghsa_fv44_git_read_options() {
    blocked(&[
        "rm -rf ~/.config/lean-ctx",
        "git -C /tmp/r log --output=$HOME/.config/lean-ctx/config.toml",
        "git -C /tmp/r log --output $HOME/.config/lean-ctx/config.toml",
        "git -C /tmp/r diff --output=~/.config/lean-ctx/config.toml",
        "git -C ~/.config/lean-ctx --git-dir=/tmp/r/.git log --output=config.toml",
        "git -C ~/.config/lean-ctx log --output=config.toml",
        "git -C ~/.config/lean-ctx diff --output=config.toml",
        "git -C ~/.config/lean-ctx -c diff.external=/tmp/x.sh diff --ext-diff",
        "git -C ~/.config/lean-ctx -c core.pager=/tmp/x.sh -p log",
        "git -C ~/.config/lean-ctx --paginate log",
        "git -C ~/.config/lean-ctx -c core.fsmonitor=/tmp/x.sh status",
        "git -C ~/.config/lean-ctx -c alias.log='!sh' log",
        "git -C ~/.config/lean-ctx --config-env=core.pager=X log",
        "git -C ~/.config/lean-ctx show --textconv HEAD:config.toml",
        "git -C ~/.config/lean-ctx grep -O/tmp/x.sh x",
        "git -C ~/.config/lean-ctx grep --open-files-in-pager=/tmp/x.sh x",
        "git -C ~/.config/lean-ctx --exec-path=/tmp log",
        "git -C ~/.config/lean-ctx diff --ou=config.toml",
        "cd ~/.config/lean-ctx && git log --output=config.toml",
        "sort --output=$HOME/.config/lean-ctx/config.toml /tmp/x",
    ]);
    passes(&[
        "git -C ~/.config/lean-ctx log --oneline -5",
        "git -C ~/.config/lean-ctx status",
        "git -C ~/.config/lean-ctx -c color.ui=always log -1",
        "git -C ~/.config/lean-ctx diff --stat",
        "git -C ~/.config/lean-ctx log -p -1",
        "git log --output=/tmp/lcx-log.txt",
        "git diff --output=/tmp/lcx.patch",
    ]);
}

/// Independent review of the 3.12.4 fixes: further spellings of the same
/// classes (values bound outside an assignment, directory-stack tildes,
/// commands behind `case`/`then`/`do`, awk programs given by option).
#[test]
fn ghsa_3124_review_variants() {
    blocked(&[
        "set -- ~/.config/lean-ctx; rm -rf \"$1\"",
        "ls ~/.config/lean-ctx; rm -rf \"$_\"",
        ": ${d:=~/.config/lean-ctx}; rm -rf \"$d\"",
        ": ${d=~/.config/lean-ctx}; rm -rf \"$d\"",
        "d=~/.config/lean-ctx; declare -n r=d; rm -rf \"$r\"",
        "cd ~/.config && rm -rf \"$PWD/lean-ctx\"",
        "cd ~/.config; cd /tmp; rm -rf \"$OLDPWD/lean-ctx\"",
        "cd ~/.config && rm -rf ~+/lean-ctx",
        "cd ~/.config; cd /tmp; rm -rf ~-/lean-ctx",
        "pushd ~/.config; pushd /tmp; rm -rf ~1/lean-ctx",
        "case x in x) eval echo hi;; esac",
        "(case x in x) bash -c id;; esac)",
        "if true; then eval echo hi; fi",
        "while :; do eval x; done",
        "coproc eval echo hi",
        "gawk --source='BEGIN{system(\"id\")}' /dev/null",
        "awk -e 'BEGIN{system(\"id\")}' /dev/null",
        "awk --source 'BEGIN{system(\"id\")}' /dev/null",
        "awk 'BEGIN{i++ /2; system(\"id\"); x=1/1}'",
        "read d <<< ~/.config/lean-ctx; sqlite3 \"$d/x.db\" 'drop table t'",
        "read d <<< lean-ctx; rm -rf ~/.config/\"$d\"",
        ": lean-ctx; rm -rf ~/.config/\"$_\"",
        ": ${d:=lean-ctx}; rm -rf ~/.config/\"$d\"",
        "set -- lean-ctx; rm -rf ~/.config/\"$1\"",
        "read d <<< -ctx; rm -rf ~/.config/lean\"$d\"",
        "awk -f /dev/stdin /tmp/x <<< 'BEGIN{system(\"id\")}'",
        "awk -f - /tmp/x <<< 'BEGIN{system(\"id\")}'",
        "rm -rf \"${d:-$HOME/.config/lean-ctx}\"",
        "rm -rf ${d-~/.config/lean-ctx}",
        "rm -rf \"${HOME:-/x}/.config/lean-ctx\"",
        "rm -rf \"${HOME%/}/.config/lean-ctx\"",
        "rm -rf \"${HOME:+$HOME}/.config/lean-ctx\"",
        "rm -rf \"${d:-${HOME}/.config/lean-ctx}\"",
        "rm -rf \"$(echo ~/.config/lean-ctx)\"",
        "rm -rf ~/.config/\"$(printf lean-ctx)\"",
        "rm -rf `echo ~/.config/lean-ctx`",
        "rm -rf \"$((echo ~/.config/lean-ctx) )\"",
        "rm -rf ~/.config/\"$((printf lean-ctx) )\"",
        // A `cd` inside a subshell does not move the rest of the line.
        "(cd /tmp); rm -rf ../../../.config/lean-ctx",
        "(cd /tmp) && rm -rf ../../../.config/lean-ctx",
        "{ (cd /tmp); }; rm -rf ../../../.config/lean-ctx",
        "(cd ~/.config && rm -rf lean-ctx)",
        "true; (cd ~/.config; rm -rf lean-ctx)",
    ]);
    passes(&[
        "case x in x) echo hi;; esac",
        "if [ -f /tmp/x ]; then echo ok; else echo no; fi",
        "while read -r f; do echo \"$f\"; done < /tmp/list",
        "mkdir -p /tmp/lcx-b && cd \"$_\" && ls",
        "echo \"$PWD\" \"$OLDPWD\" ~+ ~-",
        "cd /tmp && rm -rf \"$PWD/lcx-x\"",
        "rm -rf ~+/target/tmp",
        "(cd /tmp && ls) > /tmp/lcx-out",
        "while read -r f; do rm -f \"/tmp/lcx-out/$f\"; done < /tmp/list",
        "awk -f /tmp/prog.awk /tmp/x",
        "cd \"$(git rev-parse --show-toplevel)\" && ls",
        "git commit -m \"$(cat /tmp/msg)\"",
        "echo \"$(cat /tmp/x)\" > /tmp/lcx-stamp",
        "cat \"${XDG_CONFIG_HOME:-$HOME/.config}/git/config\"",
        "rm -f /tmp/lcx-${n:-1}.log",
        "echo $((1 + 2)) > /tmp/lcx-n",
        "touch /tmp/lcx-out/$((1 + (2 * 3)))",
        "awk -F, '{print $1}' /tmp/x.csv",
        "awk 'BEGIN{i++; print i/2}'",
    ]);
}
