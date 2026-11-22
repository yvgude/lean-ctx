// SPDX-License-Identifier: Apache-2.0
//! Options of `sed`, `tar`, `rg` and `find` that run a command their value
//! names are judged like that command, in every mode: without an allowlist,
//! with one that lists only the tools, one that also lists ordinary
//! programs, and one that also lists interpreters.

use super::*;

/// Only the tools themselves.
const TOOLS: &str = "sed,tar,rg,find,xargs";
/// The tools and ordinary programs, no interpreter.
const ALLOWLIST: &str = "sed,tar,rg,find,xargs,ls,cat,echo,git,grep,gzip,zstd,rm,true,pwd";
/// The same with interpreters listed: their inline code stays refused.
const WITH_INTERPRETERS: &str =
    "sed,tar,rg,find,xargs,ls,cat,echo,git,grep,gzip,zstd,rm,true,pwd,sh,bash,python3,node,perl";
const MODES: &[&str] = &["", TOOLS, ALLOWLIST, WITH_INTERPRETERS];

fn verdict(list: &str, cmd: &str) -> Result<(), ShellError> {
    let _list = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    let _inline = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", "false");
    enforce_shell_allowlist(cmd)
}

/// Each line, also as the Claude hook wraps it, must be refused in `modes`.
fn refused_in(modes: &[&str], cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let mut passed = Vec::new();
    for cmd in cases {
        for line in [cmd.to_string(), format!("{{ {cmd}\n}} && pwd -P")] {
            for list in modes {
                if verdict(list, &line).is_ok() {
                    passed.push(format!("[allowlist {list:?}] {line:?}"));
                }
            }
        }
    }
    assert!(passed.is_empty(), "must be refused:\n{}", passed.join("\n"));
}

fn passes_in(modes: &[&str], cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let mut refused = Vec::new();
    for cmd in cases {
        for list in modes {
            if let Err(err) = verdict(list, cmd) {
                refused.push(format!("[allowlist {list:?}] {cmd:?}\n  {err}"));
            }
        }
    }
    assert!(refused.is_empty(), "should pass:\n{}", refused.join("\n"));
}

#[test]
fn sed_e_command_is_judged() {
    refused_in(
        MODES,
        &[
            "sed '1e ./x.bat' f",
            "sed -e '1e ./x.bat' f",
            "sed --expression='1e ./x.bat' f",
            "sed --expression '1e ./x.bat' f",
            "sed --expr='1e ./x.bat' f",
            "sed -n -e '1e ./x.bat' f",
            "sed -ne '1e ./x.bat' f",
            "sed '-e1e ./x.bat' f",
            "sed f -e '1e ./x.bat'",
            "sed '1e chmod 777 x' f",
            "sed '1e sh -c id' f",
            "sed -n 'e ./x.bat' f",
            "sed 'p;e ./x.bat' f",
            "sed '/a/{e ./x.bat\n}' f",
            "sed '$!e ./x.bat' f",
            "sed '0~2e ./x.bat' f",
            "sed '/a/,/b/ e ./x.bat' f",
            "sed '\\,a,e ./x.bat' f",
            "sed -e 'p' -e 'e ./x.bat' f",
            "sed 'e' f",
            "sed '1e' f",
            "sed -i 'e ./x.bat' f",
            "sed -i.bak '1e ./x.bat' f",
            "sed -E '1e ./x.bat' f",
            "gsed '1e ./x.bat' f",
            "/usr/bin/sed '1e ./x.bat' f",
            "xargs sed '1e ./x.bat'",
        ],
    );
}

#[test]
fn sed_substitute_e_flag_is_refused() {
    refused_in(
        MODES,
        &[
            "sed 's/a/b/e' f",
            "sed 's/a/b/ge' f",
            "sed 's/a/b/eg' f",
            "sed 's|a|b|e' f",
            "sed -e 's/.*/id/e' f",
            "sed '1{s/a/b/e}' f",
            "sed 's/a/b/;s/c/d/e' f",
            "sed 's/a\\/b/c/e' f",
            "sed -n 's/x/y/pe' f",
        ],
    );
}

#[test]
fn sed_script_the_line_does_not_show_is_refused() {
    refused_in(
        MODES,
        &[
            "sed -f script.sed f",
            "sed -fscript.sed f",
            "sed --file=script.sed f",
            "sed --file script.sed f",
            "sed -n -f script.sed f",
            "sed -nf script.sed f",
            // Scripts this reader cannot follow: sed would refuse them too.
            "sed 's/a/b' f",
            "sed '1K' f",
        ],
    );
}

#[test]
fn tar_command_options_are_judged() {
    refused_in(
        MODES,
        &[
            "tar -I ./x.bat -cf a.tar f",
            "tar -I./x.bat -cf a.tar f",
            "tar -cI ./x.bat -f a.tar f",
            "tar -cIx.bat -f a.tar f",
            "tar cIf ./x.bat a.tar f",
            "tar --use-compress-program ./x.bat -cf a.tar f",
            "tar --use-compress-program=./x.bat -cf a.tar f",
            "tar --use-compress=./x.bat -cf a.tar f",
            "tar -I 'chmod 777 x' -cf a.tar f",
            "tar -I 'sh -c id' -cf a.tar f",
            "tar -I sh -cf a.tar f",
            "tar -I 'env sh' -cf a.tar f",
            "tar -I '' -cf a.tar f",
            "tar --checkpoint-action=exec=./x.bat -cf a.tar f",
            "tar --checkpoint-action=exec='chmod 777 x' -cf a.tar f",
            "tar --checkpoint-action 'exec=sh -c id' -cf a.tar f",
            "tar --checkpoint-act=exec=./x.bat -cf a.tar f",
            "tar --checkpoint-action=unknown -cf a.tar f",
            "tar --info-script=./x.bat -cf a.tar f",
            "tar --new-volume-script=./x.bat -cf a.tar f",
            "tar -F ./x.bat -cf a.tar f",
            "tar -F 'chmod 777 x' -cf a.tar f",
            "tar -F 'sh -c id' -cf a.tar f",
            "tar --rsh-command=./x.bat -cf h:a.tar f",
            "tar --rsh-command 'sh -c id' -cf h:a.tar f",
            "tar --rmt-command=./x.bat -cf h:a.tar f",
            "tar --to-command=./x.bat -xf a.tar",
            "tar --to-com=./x.bat -xf a.tar",
            "gtar -I ./x.bat -cf a.tar f",
            "bsdtar --use-compress-program=./x.bat -cf a.tar f",
            "xargs tar -I ./x.bat -cf a.tar",
        ],
    );
}

#[test]
fn rg_preprocessor_is_judged() {
    refused_in(
        MODES,
        &[
            "rg --pre ./x.bat x",
            "rg --pre=./x.bat x",
            "rg --pre chmod x",
            "rg --pre=sh x",
            "rg --pre 'sh -c id' x",
            "rg --pre=bash x",
            "rg --pre-glob '*.gz' --pre ./x.bat x",
            "rg -i --pre ./x.bat x",
            "xargs rg --pre ./x.bat x",
        ],
    );
}

#[test]
fn find_command_actions_are_refused() {
    refused_in(
        MODES,
        &[
            r"find . -exec ./x.bat {} \;",
            r"find . -execdir ./x.bat {} \;",
            r"find . -ok ./x.bat {} \;",
            r"find . -okdir ./x.bat {} \;",
            "find . -exec sh -c id {} +",
            r"find . -ok sh -c id {} \;",
        ],
    );
}

#[test]
fn xargs_target_is_judged() {
    let listed = &[TOOLS, ALLOWLIST, WITH_INTERPRETERS];
    refused_in(listed, &["xargs ./x.bat", "xargs -n1 ./x.bat"]);
    refused_in(
        MODES,
        &[
            "xargs sh -c id",
            "xargs bash -c id",
            "xargs -I{} sh -c 'id {}'",
        ],
    );
}

#[test]
fn git_config_command_stays_refused() {
    refused_in(
        MODES,
        &[
            "git -c core.pager=./x.bat log",
            "git -c core.pager='rm x' log",
            "git -c core.pager='sh -c id' log",
        ],
    );
}

/// With an allowlist the value is a command line like any other: a listed
/// program passes, an unlisted one is refused. Without an allowlist the
/// option is refused, as `tar --to-command` always was.
#[test]
fn a_listed_program_as_option_value_passes_with_an_allowlist() {
    let cases = &[
        "tar -I gzip -cf a.tgz d",
        "tar -I 'zstd -19' -cf a.tzst d",
        "tar -cIzstd -f a.tzst d",
        "sed '1e ls' f",
        "tar --checkpoint=10 --checkpoint-action=exec=ls -cf a.tar d",
    ];
    passes_in(&[ALLOWLIST, WITH_INTERPRETERS], cases);
    refused_in(&["", TOOLS], cases);
}

#[test]
fn ordinary_use_of_the_tools_passes() {
    passes_in(
        MODES,
        &[
            "sed -n 1p f",
            "sed 's/a/b/g' f",
            "sed -i 's/x/y/' f",
            "sed -i.bak 's/x/y/' f",
            "sed -i '' 's/x/y/' f",
            "sed -E 's/(a|b)/\\1/g' f",
            "sed -n '/start/,/end/p' f",
            "sed '/^$/d' f",
            "sed 's/[[:space:]]*$//' f",
            "sed -e :a -e '$!N;s/\\n//;ta' f",
            "sed ':a;N;$!ba;s/\\n/ /g' f",
            "sed '1i hello; everyone' f",
            "sed '$a end' f",
            "sed 's#a#b#' f",
            "sed 's/eel/eagle/' f",
            "sed -n '10,20p' f",
            "sed '/x/{p;d}' f",
            "sed 'y/abc/xyz/' f",
            "sed -n 1p errors.log",
            "sed -e 's/a/b/' edit.txt",
            "sed --sandbox -f script.sed f",
            "sed 's/a/b/w out.txt' f",
            "sed 10q f",
            "tar -czf a.tgz d",
            "tar -xf a.tar",
            "tar xzf a.tgz",
            "tar -tvf a.tar",
            "tar -cf a.tar -C dir .",
            "tar --checkpoint=100 --checkpoint-action=dot -cf a.tar d",
            "tar -cf a.tar --exclude='*.o' d",
            "rg --pre-glob '*.gz' x",
            "rg -e pattern",
            "rg -e --pre x",
            "rg --pre= x",
            "rg -n foo src",
            "find . -name '*.rs' -print",
        ],
    );
}

#[test]
fn xdg_open_is_no_read_only_command() {
    refused_in(
        &[TOOLS, ALLOWLIST, WITH_INTERPRETERS],
        &["xdg-open ./x.bat", "xdg-open f.txt"],
    );
    passes_in(&["xdg-open,ls"], &["xdg-open f.txt"]);
    // An alias may still not take the name of either program.
    refused_in(&[""], &["alias rg=ls", "alias xdg-open=ls"]);
}

#[test]
fn rg_with_a_preprocessor_counts_as_touching_lean_ctx_state() {
    refused_in(
        &["rg,rm"],
        &[
            "rg --pre rm x ~/.config/lean-ctx",
            "rg --pre=rm x ~/.config/lean-ctx/config.toml",
        ],
    );
    passes_in(
        &["", "rg"],
        &["rg shell_security ~/.config/lean-ctx/config.toml"],
    );
}

#[test]
fn sed_script_reader_finds_every_command() {
    use super::exec_options::sed_script_commands;
    let commands = |s: &str| -> Vec<Option<String>> {
        sed_script_commands(s).into_iter().map(|(_, c)| c).collect()
    };
    assert_eq!(commands("1e ls -l"), vec![Some("ls -l".to_string())]);
    assert_eq!(commands("p;e"), vec![None]);
    assert_eq!(commands("s/a/b/e"), vec![None]);
    assert_eq!(commands("s/a/e/g"), Vec::<Option<String>>::new());
    assert_eq!(commands("/e/p"), Vec::<Option<String>>::new());
    assert_eq!(commands("a e x\ne ls"), vec![Some("ls".to_string())]);
    assert_eq!(commands("a\\\ne x\np"), Vec::<Option<String>>::new());
    assert_eq!(commands(":e;e ls"), vec![Some("ls".to_string())]);
    assert_eq!(commands("b e;e ls"), vec![Some("ls".to_string())]);
    assert_eq!(commands("# e ls\np"), Vec::<Option<String>>::new());
    assert_eq!(commands("1!{e ls\n}"), vec![Some("ls".to_string())]);
}

/// Probe modes: no allowlist, the tools only, plus `echo`/`ls`, plus
/// `xargs` and the compressors.
const PROBE_MODES: [&str; 4] = [
    "",
    "sed,tar,rg",
    "sed,tar,rg,echo,ls",
    "sed,tar,rg,xargs,echo,ls,zstd,gzip",
];

/// Expected verdict per probe mode (`P` passes, `R` is refused) and line.
const PROBE_ROWS: &[(&str, &str)] = &[
    ("RRRR", "sed '1e ./x' f"),
    ("RRRR", "sed -e '1e ./x' f"),
    ("RRRR", "sed --expression='1e ./x' f"),
    ("RRRR", "sed --expression '1e ./x' f"),
    ("RRRR", "sed --e='1e ./x' f"),
    ("RRRR", "sed --exp '1e ./x' f"),
    ("RRRR", "sed -n -e '1e ./x' f"),
    ("RRRR", "sed -s -e '1e ./x' f"),
    ("RRRR", "sed -ne '1e ./x' f"),
    ("RRRR", "sed -nse '1e ./x' f"),
    ("RRRR", "sed -E -e '1e ./x' f"),
    ("RRRR", "sed -z '1e ./x' f"),
    ("RRRR", "sed -l 5 '1e ./x' f"),
    ("RRRR", "sed -l5 -e '1e ./x' f"),
    ("RRRR", "sed --posix '1e ./x' f"),
    ("RRRR", "sed -- '1e ./x' f"),
    ("RRRR", "sed f -e '1e ./x'"),
    ("RRRR", "sed 'e ./x' f"),
    ("RRRR", "sed '$e ./x' f"),
    ("RRRR", "sed '/re/e ./x' f"),
    ("RRRR", "sed '/re/Ie ./x' f"),
    ("RRRR", "sed '1,3e ./x' f"),
    ("RRRR", "sed '1,+2e ./x' f"),
    ("RRRR", "sed '1~2e ./x' f"),
    ("RRRR", "sed '0,/x/e ./x' f"),
    ("RRRR", "sed '\\%x%e ./x' f"),
    ("RRRR", "sed '/x/,/y/!e ./x' f"),
    ("RRRR", "sed '1 e ./x' f"),
    ("RRRR", "sed '1!e ./x' f"),
    ("RRRR", "sed 'p;e ./x' f"),
    ("RRRR", "sed 'p\ne ./x' f"),
    ("RRRR", "sed '1{e ./x\n}' f"),
    ("RRRR", "sed '{e ./x\n}' f"),
    ("RRRR", "sed 'y/a/b/;e ./x' f"),
    ("RRRR", "sed 'v;e ./x' f"),
    ("RRRR", "sed '#n\ne ./x' f"),
    ("RRRR", "sed 'e' f"),
    ("RRRR", "sed 's/x/y/;e' f"),
    ("RRRR", "sed 's | a|b|e' f"),
    ("RRRR", "sed 's#a#b#ge' f"),
    ("RRRR", "sed 's/a/b/eg' f"),
    ("RRRR", "sed 's/a/b/3e' f"),
    ("RRRR", "sed 's/a/b/e;p' f"),
    ("RRRR", "sed 's/a/b/Ie' f"),
    ("RRPP", "sed '1e ls' f"),
    ("RRRR", "sed '1e sh -c id' f"),
    ("RRRR", "sed '1e rm -rf x' f"),
    ("RRPP", "sed '1e /bin/ls' f"),
    ("RRRR", "sed -i '1e ./x' f"),
    ("RRRR", "sed -i.bak '1e ./x' f"),
    ("RRRR", "sed -i '' '1e ./x' f"),
    ("RRRR", "sed -ie '1e ./x' f"),
    ("RRRR", "gsed '1e ./x' f"),
    ("RRRR", "/usr/bin/sed '1e ./x' f"),
    ("RRRR", "env sed '1e ./x' f"),
    ("RRRR", "nice sed '1e ./x' f"),
    ("RRRR", "command sed '1e ./x' f"),
    ("RRRR", "xargs sed '1e ./x'"),
    ("RRRR", "echo x | sed 'e ./x'"),
    ("PPPP", "sed --sandbox '1e ./x' f"),
    ("RRRR", "sed -f s.sed f"),
    ("RRRR", "sed --file=s.sed f"),
    ("PPPP", "sed --sand -f s.sed f"),
    ("PPPP", "sed -e 'a\\' -e 'e ./x' f"),
    ("PPPP", "sed --expression=p '1e ./x' f"),
    ("PPPP", "sed 'w /tmp/o' f"),
    ("PPPP", "sed -n 'W out' f"),
    ("PPPP", "sed 's/a/b/w /tmp/o' f"),
    ("PPPP", "sed 'r /etc/passwd' f"),
    ("RRRR", "tar -I ./x -cf a.tar d"),
    ("RRRR", "tar -I./x -cf a.tar d"),
    ("RRRR", "tar --use-compress-program=./x -cf a.tar d"),
    ("RRRR", "tar --use-compress-program ./x -cf a.tar d"),
    ("RRRR", "tar --use-comp=./x -cf a.tar d"),
    ("RRRR", "tar --use-comp ./x -cf a.tar d"),
    ("RRRR", "tar --use=./x -cf a.tar d"),
    ("RRRR", "tar --checkpoint-action=exec=./x -cf a.tar d"),
    ("RRRR", "tar --checkpoint-action exec=./x -cf a.tar d"),
    ("RRRR", "tar --checkpoint-act=exec=./x -cf a.tar d"),
    ("RRRR", "tar --checkpoint-action=unknown -cf a.tar d"),
    ("RRRR", "tar -F ./x -cf a.tar d"),
    ("RRRR", "tar -F./x -cf a.tar d"),
    ("RRRR", "tar --info-script=./x -cf a.tar d"),
    ("RRRR", "tar --info=./x -cf a.tar d"),
    ("RRRR", "tar --new-volume-script=./x -cf a.tar d"),
    ("RRRR", "tar --new-v=./x -cf a.tar d"),
    ("RRRR", "tar --rsh-command=./x -cf h:a.tar d"),
    ("RRRR", "tar --rmt-command=./x -cf h:a.tar d"),
    ("RRRR", "tar --to-command=./x -xf a.tar"),
    ("RRRR", "tar -xI ./x -f a.tar"),
    ("RRRR", "tar -cvzf a -I ./x d"),
    ("RRRR", "tar xIf ./x a.tar"),
    ("RRRR", "tar cvIf ./x a.tar d"),
    ("RRRR", "tar -I sh -cf a.tar d"),
    ("RRRR", "tar -I 'sh -c id' -cf a.tar d"),
    ("RRPP", "tar -I ls -cf a.tar d"),
    ("RRRP", "tar -I zstd -cf a.tar.zst d"),
    ("RRRP", "tar --use-comp=zstd -cf a.tar.zst d"),
    ("RRRP", "tar --use-compress-program=zstd -cf a.tar.zst d"),
    ("RRRR", "TAR_OPTIONS=-I./x tar -xf a.tar"),
    ("RRRR", "env TAR_OPTIONS=-I./x tar -xf a.tar"),
    ("RRRR", "export TAR_OPTIONS=-I./x; tar -xf a.tar"),
    ("RRRR", "tar -cH --use-comp=./x -f a.tar d"),
    ("RRRR", "tar -cL --use-comp=./x -f a.tar d"),
    ("RRRR", "tar cHf a.tar --use-comp=./x d"),
    ("RRRR", "tar -cL --checkpoint-act=exec=./x -f a.tar d"),
    ("RRRR", "tar -cH --use-compress-program=./x -f a.tar d"),
    ("RRRR", "xargs tar -I ./x -cf a.tar"),
    ("RRRR", "rg --pre ./x foo"),
    ("RRRR", "rg --pre=./x foo"),
    ("RRPP", "rg --pre ls foo"),
    ("RRRR", "rg --pre sh foo"),
    ("RRRR", "rg -i --pre ./x foo"),
    ("RRRR", "rg -uu --pre=./x foo"),
    ("RRRR", "rg -f pats --pre ./x foo"),
    ("RRRR", "rg --regexp=y --pre ./x foo"),
    ("RRRR", "rg --hostname-bin=./x foo"),
    ("RRRR", "rg --hostname-bin ./x foo"),
    ("RRRR", "RIPGREP_CONFIG_PATH=rc rg foo"),
    ("RRRR", "env RIPGREP_CONFIG_PATH=rc rg foo"),
    ("RRRR", "export RIPGREP_CONFIG_PATH=rc; rg foo"),
    ("PPPP", "RIPGREP_CONFIG_PATH=rc rg --no-config foo"),
    ("PPPP", "echo --pre=./x > rc"),
    ("RRRR", "xargs rg --pre ./x foo"),
    ("RRRR", "rg --pre=ls x ~/.config/lean-ctx/config.toml"),
    ("PPPP", "rg shell_security ~/.config/lean-ctx/config.toml"),
    (
        "PPPP",
        "/usr/bin/rg shell_security ~/.config/lean-ctx/config.toml",
    ),
    ("PRRR", "xdg-open f.txt"),
    ("RRRR", "alias rg=ls"),
    ("RRRR", "xargs ./x.bat"),
    ("RRRR", "xargs x.bat"),
    ("RRRR", "xargs X.BAT"),
    ("RRRR", "xargs -I{} ./x.bat {}"),
    ("RRRR", "echo f | xargs ./x.cmd"),
    ("RRRR", "xargs sed '1e ./x.bat'"),
    ("PRRP", "xargs ls"),
    ("RRRR", "xargs sh -c id"),
    ("RRRR", "sed '1e x.bat' f"),
    ("RRRR", "tar -I x.bat -cf a d"),
    ("RRRR", "rg --pre x.bat foo"),
    ("PPPP", "sed -n 1,5p f"),
    ("PPPP", "sed -i s/a/b/ f"),
    ("PPPP", "sed 's/e/E/' f"),
    ("PPPP", "sed -e 's/x/y/g' f"),
    ("PPPP", "sed '/end/d' f"),
    ("PPPP", "sed 'y/abc/xyz/' f"),
    ("PPPP", "sed 's/ee/e/' f"),
    ("PPPP", "sed '/e/p' f"),
    ("PPPP", "sed -n '/e/{n;p}' f"),
    ("PPPP", "sed -n '/a/,/b/{/c/p}' f"),
    ("PPPP", "sed 's/a/b/;s/c/d/' f"),
    ("PPPP", "sed '1d;$d' f"),
    ("PPPP", "sed -n '$=' f"),
    ("PPPP", "sed 's/\\(e\\)/\\1/' f"),
    ("PPPP", "sed 's/a/b/I' f"),
    ("PPPP", "sed '/^#/!s/e/E/' f"),
    ("PPPP", "sed G f"),
    ("PPPP", "sed '1~3d' f"),
    ("PPPP", "sed = f"),
    ("PPPP", "sed -n l f"),
    ("PPPP", "sed 'N;P;D' f"),
    ("PPPP", "sed '$!N;/^\\(.*\\)\\n\\1$/!P;D' f"),
    ("PPPP", "sed -e '/e/ {' -e p -e '}' f"),
    ("PPPP", "sed -i -e 's/e/f/' f"),
    ("PPPP", "sed -i '' -e 's/e/f/' f"),
    ("PPPP", "sed 's/a/b/ ' f"),
    ("PPPP", "sed '/x/ s/a/b/' f"),
    ("PPPP", "sed 's/a/b/g ; p' f"),
    ("PPPP", "sed -n '/foo/I p' f"),
    ("PPPP", "sed '1i\\\nheader' f"),
    ("PPPP", "sed 's/x/\\n/g' f"),
    ("PPPP", "sed -r 's/e+/E/' f"),
    ("PPPP", "sed --regexp-extended 's/e+/E/' f"),
    ("PPPP", "sed '/a/,/b/c\\\nX' f"),
    ("PPPP", "sed -n '1!G;h;$p' f"),
    ("PPPP", "sed 's/[^e]*//' f"),
    ("PPPP", "sed --version"),
    ("PPPP", "sed '' f"),
    ("RRRR", "sed -f script.sed f"),
    ("PPPP", "sed 's/a/b/w out.txt' f"),
    ("PPPP", "sed -n 's/e/E/gp' f"),
    ("PPPP", "sed 's/e/E/2' f"),
    ("PPPP", "sed 's/x/e/;s/y/e/' f"),
    ("PPPP", "sed -e 's/[/]/x/' f"),
    ("PPPP", "sed 's,/,e,g' f"),
    ("PPPP", "sed '/^e/!d' f"),
    ("PPPP", "sed '1,/e/d' f"),
    ("PPPP", "sed 's/e/E/m' f"),
    ("RRRR", "sed 'T;e' f"),
    ("PPPP", "tar -czf a.tgz dir"),
    ("PPPP", "tar -xf a.tar"),
    ("PPPP", "tar xvf a.tar"),
    ("PPPP", "tar -cJf a.txz d"),
    ("PPPP", "tar --zstd -cf a.tzst d"),
    ("PPPP", "tar -tzf a.tgz"),
    ("PPPP", "tar -xzf a.tgz -C out"),
    ("PPPP", "tar -cf - d"),
    ("PPPP", "tar --checkpoint=1000 -cf a.tar d"),
    ("RRRP", "tar -I 'zstd -19 -T0' -cf a.tzst d"),
    ("PPPP", "tar --exclude=.git -czf a.tgz ."),
    ("PPPP", "tar --exclude .git -czf a.tgz ."),
    ("PPPP", "tar cf a.tar -T files.txt"),
    ("PPPP", "tar -cf a.tar --exclude -Ixyz d"),
    ("PPPP", "tar --checkpoint=1 --checkpoint-action=dot -cf a d"),
    (
        "PPPP",
        "tar --checkpoint=1 --checkpoint-action=echo='%u' -cf a d",
    ),
    ("PPPP", "tar -C dir -cf a.tar ."),
    ("PPPP", "rg -e pre foo"),
    ("PPPP", "rg pre --type rust"),
    ("PPPP", "rg -- --pre"),
    ("PPPP", "rg -e --pre foo"),
    ("PPPP", "rg --regexp --pre foo"),
    ("PPPP", "rg -F -- '--pre=x' f"),
    ("PPPP", "rg --pre-glob '*.gz' foo"),
    ("PPPP", "rg -n 'e' src"),
    ("PPPP", "rg --files"),
    ("PPPP", "rg -l foo"),
    ("PPPP", "rg -g '*.rs' foo"),
    ("PPPP", "rg --type-add 'x:*.x' foo"),
    ("PPPP", "rg -uuu foo"),
    ("PPPP", "rg 'pre=x' f"),
    ("PPPP", "rg --no-config foo"),
    ("RRRR", "rg -e '' --pre ./x foo"),
    ("RRRR", "rg -e \"\" --pre ./x foo"),
    ("RRRR", "rg --regexp '' --pre=./x foo"),
    ("RRRR", "rg -f '' --pre ./x foo"),
    ("RRRR", "tar -cV '' -I ./x -f a.tar d"),
    ("RRRR", "tar -cf '' -I ./x d"),
    ("RRRR", "tar -cV '' --checkpoint-act=exec=./x -f a.tar d"),
    ("RRRR", "sed -e '' -e '1e ./x' f"),
    ("RRRR", "sed -l '' '1e ./x' f"),
    ("RRPP", "tar --to-com=ls -xf a.tar"),
    ("RRPP", "tar --to-command=ls -xf a.tar"),
    ("RRRP", "tar --use-compress-program=zstd -cf a d"),
    ("RRRP", "tar --use-comp=zstd -cf a d"),
    ("RRRP", "tar -I zstd -cf a d"),
    ("RRRP", "tar -I /usr/bin/zstd -cf a d"),
    ("RRRR", "tar -I './x' -cf a d"),
    ("RRRR", "tar -I 'zstd; ./x' -cf a d"),
    ("RRRR", "tar -I 'zstd $(./x)' -cf a d"),
    ("RRRR", "tar -I 'zstd `./x`' -cf a d"),
    ("RRRR", "rg --pre 'ls; ./x' foo"),
    ("RRRR", "sed '1e ls; ./x' f"),
    ("RRRR", "sed '1e ls && ./x' f"),
    ("RRRR", "sed '1e ls | ./x' f"),
    ("RRRR", "sed '1e ls $(./x)' f"),
    ("RRRR", "tar -I 'env ./x' -cf a d"),
    ("RRRR", "tar -I 'zstd' -cf a d && ./x"),
    ("RRRR", "echo --pre=./x > rc; RIPGREP_CONFIG_PATH=rc rg foo"),
    ("PPPP", "sed -n '' f"),
    ("PPPP", "sed 's/[/]/x/'"),
    ("PPPP", "sed '/[/]/d' f"),
    ("PPPP", "sed -n '/[]/]/p' f"),
    ("RRRR", "sed -i '/[/]/d' ~/.config/lean-ctx/config.toml"),
    ("RRRR", "sed -f s.sed f"),
    ("RRRR", "xargs sed -n 'w ~/.config/lean-ctx/x' f"),
    ("RRRR", "xargs -0 sed -n 'w ~/.config/lean-ctx/x' f"),
    ("PRRP", "xargs sed -n p f"),
    ("PPPP", "tar -cf /tmp/a.tar ~/.config/lean-ctx"),
    ("PPPP", "tar czf /tmp/a.tgz ~/.config/lean-ctx"),
    ("PPPP", "tar --create --file=/tmp/a.tar ~/.config/lean-ctx"),
    ("PPPP", "tar -C ~/.config/lean-ctx -cf /tmp/a.tar ."),
    ("RRRR", "tar -C ~/.config/lean-ctx -cf a.tar ."),
    ("RRRR", "tar -cf ~/.config/lean-ctx/a.tar src"),
    (
        "RRRR",
        "tar -cf /tmp/a.tar --remove-files ~/.config/lean-ctx",
    ),
    ("RRRR", "tar -rf ~/.config/lean-ctx/a.tar f"),
    ("RRRR", "tar cCf x /tmp/a ~/.config/lean-ctx"),
    ("RRRR", "tar -xf a.tar -C ~"),
    ("PPPP", "RIPGREP_CONFIG_PATH= rg foo"),
    ("PPPP", "RIPGREP_CONFIG_PATH='' rg foo"),
    ("PPPP", "TAR_OPTIONS= tar -cf /tmp/a.tar src"),
    ("RRRR", "RIPGREP_CONFIG_PATH+= rg foo"),
    ("RRRR", "TAR_OPTIONS=-v tar -cf /tmp/a.tar src"),
    ("RRRR", "VIMINIT='so x' sed -n p f"),
    ("RRRR", "export EXINIT=x"),
    ("RRRR", "export VIMRUNTIME=./d"),
    ("RRRR", "MYVIMRC=./x sed -n p f"),
    ("RRRR", "GVIMINIT=x sed -n p f"),
    ("RRRR", "MYGVIMRC+=x sed -n p f"),
    ("PPPP", "VIMINIT= sed -n p f"),
    ("PPPP", "export EXINIT="),
    ("RRRR", "NVIM_APPNAME=x sed -n p f"),
    ("RRRR", "EMACSLOADPATH=./d sed -n p f"),
    ("RRRR", "export NVIM_APPNAME=x; sed -n p f"),
    ("PPPP", "HOME=./x sed -n p f"),
    ("PPPP", "XDG_CONFIG_HOME=./x rg foo"),
    // PowerShell backtick escapes in a program name.
    ("RRRR", "`pwsh -c x"),
    ("RRRR", "x`.bat"),
    ("RRRR", ".\\x`.bat"),
    ("RRRR", "`p`w`s`h -c x"),
    ("RRRR", "pwsh` -c x"),
    ("RRRR", "`cmd /c x"),
    ("RRRR", "& `pwsh -c x"),
    ("RRRR", "start \"\" x`.bat"),
    ("RRRR", "xargs x`.bat"),
    ("RRRR", "echo x > ~/.local/state/lean-ctx/events.jsonl"),
    ("RRRR", "tar -xf a.tar -C $LEAN_CTX_STATE_DIR"),
    ("RRRR", "tar -xf a.tar -C ${LEAN_CTX_STATE_DIR}"),
    ("RRRR", "tar -xf a.tar --directory=$LEAN_CTX_STATE_DIR"),
    ("RRRR", "tar -xf a.tar -C $LEAN_CTX_CONFIG_DIR"),
    ("RRRR", "tar -xf a.tar -C ${LEAN_CTX_DATA_DIR}/x"),
    ("RRRR", "s=$LEAN_CTX_STATE_DIR; tar -xf a.tar -C $s"),
    ("RRRR", "echo x > $LEAN_CTX_STATE_DIR/events.jsonl"),
    ("RRRR", "sed -n 'w ${LEAN_CTX_STATE_DIR}/x' f"),
    ("PPPP", "sed -n 1p $LEAN_CTX_STATE_DIR/events.jsonl"),
    ("PPPP", "tar -xf a.tar -C $LEAN_CTX_CACHE_DIR_X"),
    (
        "RRRR",
        "sed -i d ~/.local/state/lean-ctx/context_ledger.json",
    ),
    (
        "RRRR",
        "sed -n 'w ~/.local/state/lean-ctx/agent_runtime_env.json' f",
    ),
    ("PPPP", "sed -n 1p ~/.local/state/lean-ctx/events.jsonl"),
    ("PPPP", "echo x > ~/.config/lean-ctx-other/f"),
];

/// Modes for lines whose programs the tool lists above do not name: no
/// allowlist, and one that lists the editors, sandboxes and launchers.
const LAUNCHER_MODES: [&str; 2] = [
    "",
    "sed,tar,rg,vim,nvim,view,emacs,echo,ls,xargs,firejail,dbus-run-session,start,call,cat,cp,env",
];

/// Expected verdict per [`LAUNCHER_MODES`] mode and line.
const LAUNCHER_ROWS: &[(&str, &str)] = &[
    ("RR", "start \"\" ^\"x.bat^\""),
    ("RR", "start ^\"t^\" pwsh -c x"),
    ("RR", "call ^\"x.bat^\""),
    ("RR", "^\"pwsh^\" -c x"),
    ("RR", "p^wsh -c x"),
    ("RR", "c^md /c x"),
    ("RR", "start \"\" x.b^at"),
    ("RR", "firejail ^\"pwsh^\" -c x"),
    ("RR", "p`wsh -c x"),
    ("PP", "echo ^\"x.bat^\""),
    ("PP", "echo \"p^wsh\""),
    ("PP", "rg '^foo' src"),
    ("RR", "firejail --profile p ./x.bat"),
    ("RR", "firejail --net none pwsh -c x"),
    ("RR", "dbus-run-session --config-file f pwsh -c x"),
    ("PP", "firejail --net=none ls -la"),
    ("PR", "echo ':q' | vim f"),
    ("PR", "vim f < keys.txt"),
    ("PR", "cat k | xargs vim f"),
    ("PR", "cat f | vim -"),
    ("PR", "{ vim f; } < k"),
    ("PR", "(vim f) < k"),
    ("PR", "cat k | { vim f; }"),
    ("PR", "cat k | (vim f)"),
    ("PR", "{ echo a; vim f; } < k"),
    ("PR", "cat k | ( cd d && vim f )"),
    ("PR", "vim f<k"),
    ("PR", "vim<k f"),
    ("PR", "vim f<<<':!id'"),
    ("PR", "{ vim f; } <<< ':!id'"),
    ("PP", "{ vim f; } > out"),
    ("PP", "(vim f) | cat"),
    ("PP", "{ cat k; } < k; vim f"),
    ("PP", "vim f"),
    ("RR", "vim --cmd 'set rtp^=./d' f"),
    ("RR", "vim -c 'set pp=./d' f"),
    ("RR", "vim -c 'set fde=system(1)' f"),
    ("PP", "vim -c 'set ts=4 sw=4 et' -c wq f"),
    ("PP", "vim -c 'set nonu' f"),
    ("RR", "VIMINIT='so ./x' vim f"),
    ("RR", "EXINIT=x vim f"),
    ("RR", "VIMRUNTIME=./d vim f"),
    ("RR", "MYVIMRC=./x vim f"),
    ("RR", "env VIMINIT=x vim f"),
    ("RR", "GVIMINIT=x MYGVIMRC=./x vim f"),
    ("PP", "VIMINIT= vim f"),
    ("PP", "VIMRUNTIME='' vim f"),
    ("RR", "VIMRUNTIME=./d vim f"),
    ("RR", "declare -x MYVIMRC=./x; vim f"),
    ("RR", "set -a; VIMINIT=x; vim f"),
    ("RR", "set -a; EXINIT=x vim f"),
    ("RR", "export MYVIMRC=./x; vim f"),
    ("RR", "env EXINIT=x vim f"),
    ("RR", "env -i VIMRUNTIME=./d vim f"),
    ("RR", "timeout 5 env VIMINIT=x vim f"),
    ("RR", "nohup env MYVIMRC=./x vim f"),
    ("RR", "echo f | xargs env VIMINIT=x vim"),
    ("RR", "echo f | xargs env EXINIT=x vim"),
    ("RR", "firejail env VIMRUNTIME=./d vim f"),
    ("RR", "sed '1e cat > $LEAN_CTX_STATE_DIR/x' f"),
    ("RR", "export VIMINIT=x; vim f"),
    ("RR", "export GVIMINIT=x; vim f"),
    ("RR", "XDG_CONFIG_HOME=./x vim f"),
    ("RR", "XDG_CONFIG_HOME=./x nvim f"),
    ("RR", "env XDG_DATA_HOME=./x nvim f"),
    ("RR", "NVIM_APPNAME=x nvim f"),
    ("RR", "HOME=./x vim f"),
    ("RR", "HOME=./x emacs f"),
    ("RR", "env HOME=./x nvim f"),
    ("RR", "export HOME=./x; vim f"),
    ("RR", "export XDG_CONFIG_HOME=./x && nvim f"),
    ("RR", "EMACSLOADPATH=./d emacs f"),
    ("RR", "HOME=./x xargs vim f"),
    ("PP", "HOME=./x ls"),
    ("PP", "echo $HOME; vim f"),
    ("PP", "emacs f"),
    ("PP", "cat ~/.local/state/lean-ctx/events.jsonl"),
    (
        "RR",
        "cp f $HOME/.local/state/lean-ctx/agent_runtime_env.json",
    ),
];

#[test]
fn every_launcher_row_gets_its_verdict() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let mut wrong = Vec::new();
    for (expected, cmd) in LAUNCHER_ROWS {
        for (want, list) in expected.chars().zip(LAUNCHER_MODES) {
            let got = verdict(list, cmd);
            if got.is_ok() != (want == 'P') {
                wrong.push(format!(
                    "[allowlist {list:?}] want {want}: {cmd:?} -> {got:?}"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// The state directory is protected wherever it is placed.
#[test]
fn state_dir_override_is_protected() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let dir = std::env::temp_dir().join(format!("lean-ctx-state-probe-{}", std::process::id()));
    let state = dir.display().to_string();
    let _state = lean_ctx::test_env::VarGuard::set("LEAN_CTX_STATE_DIR", &state);
    for list in MODES {
        assert!(verdict(list, &format!("echo x > {state}/events.jsonl")).is_err());
        assert!(verdict(list, &format!("sed -i d {state}/context_ledger.json")).is_err());
        assert!(verdict(list, &format!("tar -cf {state}/a.tar src")).is_err());
        assert!(verdict(list, &format!("sed -n 1p {state}/events.jsonl")).is_ok());
    }
}

#[test]
fn every_probe_row_gets_its_verdict() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let mut wrong = Vec::new();
    for (expected, cmd) in PROBE_ROWS {
        for (want, list) in expected.chars().zip(PROBE_MODES) {
            let mut lines = vec![cmd.to_string()];
            if want == 'R' {
                lines.push(format!("{{ {cmd}\n}} && pwd -P"));
            }
            for line in lines {
                let got = verdict(list, &line);
                if got.is_ok() != (want == 'P') {
                    wrong.push(format!(
                        "[allowlist {list:?}] want {want}: {line:?} -> {got:?}"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// An empty quoted word is still a word: it is the value of the option
/// before it, so the next option is read as an option.
#[test]
fn empty_words_keep_their_place() {
    refused_in(
        MODES,
        &[
            "rg -e '' --pre ./x foo",
            "rg -e \"\" --pre ./x foo",
            "rg --regexp '' --pre=./x foo",
            "rg -f '' --pre ./x foo",
            "tar -cV '' -I ./x -f a.tar d",
            "tar -cf '' -I ./x d",
            "tar -cV '' --checkpoint-act=exec=./x -f a.tar d",
            "xargs rg -e '' --pre ./x foo",
            "xargs tar -cf '' -I ./x d",
            // Refused for the `e` command, not for an unreadable script.
            "sed -e '' -e '1e ./x' f",
            "sed -l '' '1e ./x' f",
        ],
    );
    passes_in(MODES, &["sed '' f", "sed -n '' f", "rg -e '' foo"]);
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ["sed -e '' -e '1e ./x' f", "sed -l '' '1e ./x' f"] {
        let msg = verdict("", cmd).expect_err("refused").to_string();
        assert!(msg.contains("e command"), "{cmd}: {msg}");
    }
}

/// bsdtar's `-H` and `-L` take no value, so the option after them is read.
#[test]
fn tar_flags_without_value_do_not_hide_the_next_option() {
    refused_in(
        MODES,
        &[
            "tar -cH --use-comp=./x -f a.tar d",
            "tar -cL --use-comp=./x -f a.tar d",
            "tar cHf a.tar --use-comp=./x d",
            "tar -cL --checkpoint-act=exec=./x -f a.tar d",
            "tar -ch -I ./x -f a.tar d",
        ],
    );
    passes_in(
        MODES,
        &[
            "tar -cf a.tar --exclude -Ixyz d",
            "tar -cf a.tar --exclude=-Ixyz d",
            "tar -cH -f a.tar d",
            "tar -cf a.tar --transform s,a,b, d",
        ],
    );
}

/// One rule for every spelling: a listed program passes as the value with
/// an allowlist, nothing else does.
#[test]
fn tar_command_options_follow_the_allowlist() {
    let listed = "sed,tar,rg,xargs,echo,ls,zstd,gzip";
    passes_in(
        &[listed],
        &[
            "tar --use-compress-program=zstd -cf a d",
            "tar --use-compress-program zstd -cf a d",
            "tar --to-command=ls -xf a.tar",
            "tar --to-com=ls -xf a.tar",
            "tar -I zstd -cf a d",
        ],
    );
    refused_in(
        &["", "sed,tar,rg"],
        &[
            "tar --use-compress-program=zstd -cf a d",
            "tar --to-command=ls -xf a.tar",
        ],
    );
}

#[test]
fn rg_hostname_program_is_judged() {
    refused_in(
        MODES,
        &[
            "rg --hostname-bin=./x foo",
            "rg --hostname-bin ./x foo",
            "rg --hostname-bin 'sh -c id' foo",
            "xargs rg --hostname-bin=./x foo",
        ],
    );
    passes_in(MODES, &["rg -e --hostname-bin foo", "rg -- --hostname-bin"]);
}

/// Options a tool takes from the environment are not on the line.
#[test]
fn tool_option_variables_are_refused() {
    refused_in(
        MODES,
        &[
            "TAR_OPTIONS=-I./x tar -xf a.tar",
            "env TAR_OPTIONS=-I./x tar -xf a.tar",
            "export TAR_OPTIONS=-I./x; tar -xf a.tar",
            "RIPGREP_CONFIG_PATH=rc rg foo",
            "env RIPGREP_CONFIG_PATH=rc rg foo",
            "export RIPGREP_CONFIG_PATH=rc; rg foo",
            "echo --pre=./x > rc; RIPGREP_CONFIG_PATH=rc rg foo",
            "RIPGREP_CONFIG_PATH=rc ls; rg foo",
            "RIPGREP_CONFIG_PATH=rc rg -g --no-config foo",
            "RIPGREP_CONFIG_PATH=rc sh -c 'rg --no-config foo; rg foo'",
        ],
    );
    passes_in(
        MODES,
        &[
            "RIPGREP_CONFIG_PATH=rc rg --no-config foo",
            "rg --no-config foo",
        ],
    );
}

#[test]
fn sed_brackets_are_read_both_ways() {
    passes_in(
        MODES,
        &[
            "sed 's/[/]/x/'",
            "sed 's/[/]/x/' f",
            "sed -e 's/[/]/x/' f",
            "sed 's/[]/]/x/' f",
        ],
    );
    refused_in(MODES, &["sed 's/[/]/x/;e ./x' f", "sed 's/[/]/x/e' f"]);
}

/// An editor's options that run a command are judged, also when the
/// allowlist lists the editor.
#[test]
fn editor_command_options_are_refused() {
    let editors = "vim,nvim,vi,emacs,nano,ls,echo";
    refused_in(
        &["", editors],
        &[
            "vim -c '!rm x' f",
            "vim +'!rm x' f",
            "nvim --cmd '!rm x'",
            "emacs --eval '(shell-command \"rm x\")'",
            "vi -c '!rm x' f",
            "vim -c '!pwsh -c x' f",
            "vim -c '!./x.bat' f",
            "vim -c 'call system(\"rm x\")' f",
            "vim -S x.vim f",
            "vim -u x.vim f",
            "emacs -l x.el f",
            "emacs --load=x.el f",
            "emacs -f shell f",
            "ex f",
        ],
    );
    passes_in(
        &["", editors],
        &[
            "nano f",
            "vim f.txt",
            "vim ./x.bat",
            "vim ./x.cmd",
            "vim -c 'set nu' f",
            "vim +10 f",
            "vim -u NONE f",
            "emacs f",
            "emacs -nw f",
        ],
    );
}

/// sed's `w` writes the file it names: the state gate judges it like a
/// redirect, also after a `cd`. `r` reads, like `cat`.
#[test]
fn sed_file_commands_reach_the_state_gate() {
    refused_in(
        MODES,
        &[
            "sed -n 'w ~/.config/lean-ctx/x' f",
            "sed 's/a/b/w ~/.config/lean-ctx/x' f",
            "sed -n '1W ~/.config/lean-ctx/x' f",
            "sed -e p -e 'w ~/.config/lean-ctx/x' f",
            "cd ~/.config/lean-ctx && sed -n 'w x' f",
            "sed -n 'w $OUT' f",
        ],
    );
    passes_in(
        MODES,
        &[
            "sed -n 'w /dev/stdout' f",
            "sed 'r ~/.config/lean-ctx/config.toml' f",
            "sed -n 'w out.txt' f",
        ],
    );
}

/// rg reads, like `cat`; tar writes or extracts and is refused on the state.
#[test]
fn rg_and_tar_operands_reach_the_state_gate() {
    passes_in(
        &["", "rg"],
        &[
            "rg foo ~/.config/lean-ctx/config.toml",
            "rg -n foo ~/.config/lean-ctx",
        ],
    );
    refused_in(
        &["", "tar"],
        &[
            "tar -cf ~/.config/lean-ctx/x.tar d",
            "tar -xf a.tar -C ~/.config/lean-ctx",
            "cd ~/.config/lean-ctx && tar -xf /tmp/a.tar",
        ],
    );
}

#[test]
fn xargs_batch_target_is_refused() {
    refused_in(
        MODES,
        &[
            "xargs ./x.bat",
            "xargs x.bat",
            "xargs X.BAT",
            "xargs -I{} ./x.bat {}",
            "echo f | xargs ./x.cmd",
        ],
    );
}

#[test]
fn sed_script_files_follow_both_bracket_readings() {
    use super::exec_options::{sed_script_commands, sed_script_files};
    assert!(sed_script_commands("s/[/]/x/").is_empty());
    assert_eq!(sed_script_commands("s/[/]/x/;e ls").len(), 1);
    let files = |s: &str| sed_script_files(s).1;
    assert_eq!(files("w out"), vec![("out".to_string(), true)]);
    assert_eq!(files("s/a/b/w out"), vec![("out".to_string(), true)]);
    assert_eq!(files("r in"), vec![("in".to_string(), false)]);
    assert_eq!(files("1W out"), vec![("out".to_string(), true)]);
}

#[test]
fn command_of_sed_e_writing_by_state_override_is_refused() {
    refused_in(
        &["sed,sh,cat", "sed,sh,cat,tar"],
        &[
            "sed '1e cat > $LEAN_CTX_STATE_DIR/x' f",
            "sed '1e cat > ${LEAN_CTX_STATE_DIR}/x' f",
            "sed '1e cat > $LEAN_CTX_CONFIG_DIR/config.toml' f",
        ],
    );
}

/// Programs that run another command line, with `{}` for that line.
const WRAPPERS: &[&str] = &[
    "firejail {}",
    "firejail --net none {}",
    "firejail --profile p {}",
    "firejail --private {}",
    "dbus-run-session {}",
    "dbus-run-session -- {}",
    "systemd-run --user {}",
    "pkexec {}",
    "env {}",
    "env -i {}",
    "nice {}",
    "timeout 5 {}",
    "nohup {}",
    "stdbuf -oL {}",
    "sudo {}",
    "sudo -u root {}",
    "setsid {}",
    "echo f | xargs {}",
    "find . -name f -exec {} \\;",
    "sh -c \"{}\"",
    "bash -c \"{}\"",
    "firejail sh -c \"{}\"",
    "sh -c \"firejail {}\"",
    "env firejail timeout 5 {}",
];

/// Command lines that run a command through an option, an editor startup
/// variable or a write into lean-ctx's own state.
const WRAPPED_LINES: &[&str] = &[
    "vim -c '!./x' f",
    "vim -c 'set shell=./x' f",
    "vim --cmd '!./x' f",
    "nvim -l x.lua",
    "rg --pre ./x foo",
    "tar -I ./x -xf a.tar",
    "tar --use-compress-program=./x -xf a.tar",
    "tar --checkpoint-action=exec=./x -xf a.tar",
    "sed '1e ./x' f",
    "sed -n 'w ~/.local/state/lean-ctx/x' f",
    "tar -xf a.tar -C ~/.local/state/lean-ctx",
    "VIMINIT=x vim f",
    "env EXINIT=x vim f",
    "env HOME=./x vim f",
    "env XDG_CONFIG_HOME=./x nvim f",
];

fn verdict_in(list: &str, inline: bool, cmd: &str) -> Result<(), ShellError> {
    let flag = if inline { "true" } else { "false" };
    let _list = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    let _inline = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", flag);
    let _writes = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOW_WRITES", flag);
    enforce_shell_allowlist(cmd)
}

#[test]
fn a_wrapped_command_is_judged_like_the_command_itself() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let lists = [
        "",
        "sed,tar,rg,vim,nvim,echo,ls,xargs,firejail,dbus-run-session,env,nice,timeout,\
         nohup,stdbuf,sudo,setsid,find,sh,bash,systemd-run,pkexec",
    ];
    let mut wrong = Vec::new();
    for inline in [false, true] {
        for list in lists {
            for line in WRAPPED_LINES {
                if verdict_in(list, inline, line).is_ok() {
                    wrong.push(format!(
                        "[{list:?} inline={inline}] unwrapped passes: {line:?}"
                    ));
                    continue;
                }
                for wrapper in WRAPPERS {
                    let cmd = wrapper.replace("{}", line);
                    if verdict_in(list, inline, &cmd).is_ok() {
                        wrong.push(format!("[{list:?} inline={inline}] {cmd:?}"));
                    }
                }
            }
            for line in ["vim f", "rg foo", "sed -n p f", "tar -xf a.tar"] {
                for wrapper in [
                    "firejail {}",
                    "firejail --net=none {}",
                    "dbus-run-session {}",
                ] {
                    let cmd = wrapper.replace("{}", line);
                    if let Err(e) = verdict_in(list, inline, &cmd) {
                        wrong.push(format!("[{list:?} inline={inline}] refused {cmd:?}: {e}"));
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn a_wrapped_editor_reading_its_input_is_judged_like_the_editor() {
    let list = "vim,cat,firejail,dbus-run-session,env,sh,timeout";
    refused_in(
        &[list],
        &[
            "firejail vim f < k",
            "cat k | firejail vim f",
            "cat k | dbus-run-session vim f",
            "firejail --net none vim f < k",
        ],
    );
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in [
        "sh -c 'vim f < k'",
        "sh -c 'cat k | vim f'",
        "sh -c '{ vim f; } < k'",
    ] {
        assert!(
            verdict_in(list, true, cmd).is_err(),
            "must be refused: {cmd:?}"
        );
    }
}

/// Each program that runs its first plain word, in the spellings the
/// allowlist test below wraps a line in.
const RUNNING_WRAPPED: &[&str] = &[
    "firejail {}",
    "firejail --private {}",
    "firejail --net=none {}",
    "dbus-run-session {}",
    "dbus-run-session -- {}",
    "pkexec {}",
    "pkexec --user root {}",
    "systemd-run --user {}",
    "systemd-run -p Nice=5 {}",
    "sandbox-exec -f p.sb {}",
    "chpst -u nobody {}",
    "runuser -u root {}",
    "runuser -u root -- {}",
    "firejail --quiet {}",
    "firejail -- {}",
    "chpst -e d {}",
    "sandbox-exec -f p {}",
    "sandbox-exec -p '(version 1)' {}",
    "runuser -u u -- {}",
    "systemd-run --scope -q {}",
    "env firejail {}",
    "timeout 5 firejail --quiet {}",
    "firejail pkexec {}",
    "sh -c 'firejail {}'",
    "bash -c 'chpst -u nobody {}'",
];

#[test]
fn a_running_wrapper_asks_the_allowlist_about_its_target() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let wrappers = "firejail,dbus-run-session,pkexec,systemd-run,sandbox-exec,chpst,runuser";
    let tools = format!("{wrappers},ls,git,env,timeout");
    let with_interpreters = format!("{tools},sh,bash,perl,node");
    let lines = [
        "foo",
        "./foo",
        "pwsh -c x",
        "cmd /c x",
        "perl -e x",
        "node -e x",
        "ruby -e x",
        "zsh -c x",
        "sh -c x",
        "c^md /c x",
        "p^wsh -c x",
        "python3 -c x",
    ];
    let mut wrong = Vec::new();
    for list in [tools.as_str(), with_interpreters.as_str()] {
        for inline in [false, true] {
            for line in lines {
                let direct = verdict_in(list, inline, line).is_ok();
                for wrapper in RUNNING_WRAPPED {
                    let cmd = wrapper.replace("{}", line);
                    if !direct && verdict_in(list, inline, &cmd).is_ok() {
                        wrong.push(format!("[{list:?} inline={inline}] passes: {cmd:?}"));
                    }
                }
            }
            let shells_run = inline && list == with_interpreters;
            for line in ["ls -la", "git status"] {
                for wrapper in RUNNING_WRAPPED {
                    // A shell's command line needs the shell listed and inline code on.
                    if wrapper.contains(" -c '") && !shells_run {
                        continue;
                    }
                    let cmd = wrapper.replace("{}", line);
                    if let Err(e) = verdict_in(list, inline, &cmd) {
                        wrong.push(format!("[{list:?} inline={inline}] refused {cmd:?}: {e}"));
                    }
                }
            }
            for cmd in [
                "runuser root -c 'foo'",
                "runuser -l git -c 'foo'",
                "runuser -l git --command='foo'",
                "runuser -lc 'foo' git",
                "runuser --session-command 'pwsh -c x' git",
                "runuser x -c id",
                "runuser git -c id",
                "runuser -u git -c 'pwsh -c x'",
            ] {
                if verdict_in(list, inline, cmd).is_ok() {
                    wrong.push(format!("[{list:?} inline={inline}] passes: {cmd:?}"));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn lean_ctx_event_log_is_readable_by_every_reader() {
    let list = "cat,head,tail,sort,cut,wc,grep,rg,uniq,less,jq,awk,tac,nl,od,rev";
    let mut cases = Vec::new();
    for path in [
        "~/.local/state/lean-ctx/events.jsonl",
        "$HOME/.local/state/lean-ctx/events.jsonl",
        "$LEAN_CTX_STATE_DIR/events.jsonl",
    ] {
        for reader in [
            "cat {}",
            "head -n 5 {}",
            "tail -f {}",
            "sort {}",
            "sort -u {}",
            "cut -c1-80 {}",
            "cut -d, -f1 {}",
            "wc -l {}",
            "grep x {}",
            "rg x {}",
            "uniq {}",
            "less {}",
            "jq . {}",
            "tac {}",
            "nl {}",
            "od -c {}",
            "rev {}",
        ] {
            cases.push(reader.replace("{}", path));
        }
    }
    let cases: Vec<&str> = cases.iter().map(String::as_str).collect();
    passes_in(&["", list], &cases);
    refused_in(
        &["", list],
        &[
            "cat ~/.local/state/lean-ctx/Dashboard.TOKEN",
            "cat ~/.local/state/lean-ctx/LEDGER/x",
            "head $HOME/.local/state/lean-ctx/Context_Ledger.JSON",
            "cat ~/.local/state/lean-ctx/context_ledger.json.bak",
            "tail ~/.local/state/lean-ctx/Dashboard.Token~",
            "cp $HOME/.local/state/lean-ctx/cost_attribution.json.old /tmp/x",
        ],
    );
    // Writing into the state directory stays refused.
    refused_in(
        &["", list],
        &[
            "sort -o ~/.local/state/lean-ctx/events.jsonl f",
            "sort --output=$LEAN_CTX_STATE_DIR/x f",
            "sort -uo ~/.local/state/lean-ctx/x f",
            "sort -T ~/.local/state/lean-ctx f",
            "uniq f ~/.local/state/lean-ctx/events.jsonl",
        ],
    );
}

#[test]
fn a_name_starting_with_a_record_name_in_the_state_directory_is_that_record() {
    let list = "sed,rg,cat,cp,head,tail,echo,ls";
    let mut refused = Vec::new();
    for base in [
        "~/.local/state/lean-ctx",
        "$HOME/.local/state/lean-ctx",
        "$LEAN_CTX_STATE_DIR",
    ] {
        for name in [
            "context_ledgerx",
            "context_ledger.jsonx",
            "context_ledger.jsonl",
            "context_ledger.json-old",
            "context_ledger.json.lock",
            "Context_LedgerX.json",
            "dashboardx",
            "dashboard.tokenx",
            "dashboard.tokens",
            "value_assessments.jsonlx",
            "cost_attributionx",
            "agent_runtime_env.jsonl",
            "ledgerx",
            "tool-calls.log.1",
        ] {
            for reader in [
                "sed -n p {}",
                "rg x {}",
                "cat {}",
                "cp {} /tmp/y",
                "head -c 9 {}",
                "tail -f {}",
            ] {
                refused.push(reader.replace("{}", &format!("{base}/{name}")));
            }
        }
    }
    let refused: Vec<&str> = refused.iter().map(String::as_str).collect();
    refused_in(&["", list], &refused);
    passes_in(
        &["", list],
        &[
            "cat docs/dashboard.token.md",
            "sed -n p src/ledger.rs",
            "rg x src/ledger.rs",
            "cat ~/.local/state/lean-ctx/events.jsonl",
            "cat ~/.local/state/lean-ctx/mcp-live.json",
            "head ~/.local/state/lean-ctx/session_savings.jsonl",
            "tail ~/.local/state/lean-ctx/compression_session.json",
            "sed -n p ~/.local/state/lean-ctx/projects/a/journal.md",
            "cat ~/.local/state/lean-ctx/tee/proxy_1.log",
            "cat ~/.local/state/lean-ctx/pipeline_stats.json",
        ],
    );
}

#[test]
fn a_sed_address_with_a_slash_in_brackets_names_no_path() {
    passes_in(
        &["", "sed"],
        &[
            "sed '/[/]/s/a/b/' f",
            "sed -n '/[/]/s/a/b/p' f",
            "sed '/[/]/d' f",
            "sed -n '/[/]/p' f",
            "sed 's/[/]/x/' f",
            "sed 's/[/]/x/g' f",
            "sed -n '/^[^/]*\\/[/]/p' f",
        ],
    );
}

/// The lists of the review's V and W modes: editors, wrappers and shells
/// allowed, no strict mode.
const EDITOR_LISTS: &[&str] = &[
    "sed,tar,rg,vim,vi,view,gvim,vimdiff,nvim,ex,emacs,emacsclient,echo,ls,env,nice,command,time,xargs",
    "sed,tar,rg,vim,vi,nvim,ex,emacs,env,nice,timeout,firejail,stdbuf,command,time,xargs,sh,bash,\
     cat,head,grep,find,echo,ls,printf,sudo,nohup,tail,less,cp,tee,git",
];

#[test]
fn a_function_reading_input_is_refused_inside_the_agent_wrapper_too() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let forms = [
        "f() { vim f; }; f < k",
        "function f { vim f; }; f < k",
        "function f() { vim f; }; f < k",
        "f() { vim f; }; f 0<k",
        "f() { vim \"$@\"; }; f x < k",
        "f() { vim f; }; f <<< x",
        "f() { vim f; }; f <<EOF\n:q\nEOF",
        "f() ( vim f ); f < k",
        "f() { nvim f; }; cat k | f",
    ];
    let mut wrong = Vec::new();
    for form in forms {
        let mut wrapped = vec![form.to_string(), format!("{{ {form}\n}} && pwd -P")];
        if !form.contains("<<EOF") {
            wrapped.push(format!("{{ {form}; }}"));
        }
        for cmd in &wrapped {
            for inline in [false, true] {
                for list in EDITOR_LISTS {
                    if verdict_in(list, inline, cmd).is_ok() {
                        wrong.push(format!("[{list:.40} inline={inline}] passes: {cmd:?}"));
                    }
                }
                // Without an allowlist it stays a note.
                if let Err(e) = verdict_in("", inline, cmd) {
                    wrong.push(format!("[no list inline={inline}] refused {cmd:?}: {e}"));
                }
            }
        }
    }
    for cmd in [
        "{ f() { vim f; }; f\n} && pwd -P",
        "{ f() { echo x; }; f < k\n} && pwd -P",
    ] {
        for list in EDITOR_LISTS {
            if let Err(e) = verdict_in(list, true, cmd) {
                wrong.push(format!("[{list:.12}] refused {cmd:?}: {e}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

#[test]
fn editor_startup_and_input_reach_it_through_shells_wrappers_and_loops() {
    let list = "vim,emacs,cat,firejail,env,sh,bash,timeout,echo,ls";
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let refused = [
        "VIM=./d vim f",
        "EMACSDIR=./d emacs f",
        "env VIM=./d vim f",
        "firejail HOME=./x vim f",
        "firejail env HOME=./x vim f",
        "firejail XDG_CONFIG_HOME=./d emacs f",
        "timeout 5 env XDG_CONFIG_HOME=./d vim f",
        "HOME=./x sh -c 'vim f'",
        "XDG_CONFIG_HOME=./d sh -c 'emacs f'",
        "HOME=./x sh -c 'sh -c \"vim f\"'",
        "env HOME=./x bash -c 'firejail vim f'",
        "while read l; do vim f; done < k",
        "while read l; do echo $l; vim f; done < k",
        "until false; do vim f; done < k",
        "cat k | while read l; do vim f; done",
        "if true; then vim f; fi < k",
        "for i in 1; do firejail vim f; done < k",
        "sh -c 'vim f' < k",
        "cat k | sh -c 'vim f'",
        "sh -c 'sh -c \"vim f\"' < k",
        "f() { vim f; }; f < k",
        "f() { vim f; }; cat k | f",
        "function f { vim f; }; f < k",
        "f() { vim f; }; firejail f < k",
        "f() { vim f; }; { f; } < k",
        "f() { vim f; }; g() { f; }; g < k",
        "f() { firejail vim f; }; while read l; do f; done < k",
    ];
    for inline in [false, true] {
        for cmd in refused {
            assert!(
                verdict_in(list, inline, cmd).is_err(),
                "must be refused (inline={inline}): {cmd:?}"
            );
        }
    }
    for cmd in [
        "vim f",
        "emacs f",
        "sh -c 'vim f'",
        "HOME=./x sh -c 'ls'",
        "while read l; do echo $l; done < k; ls",
        "for i in 1 2; do echo $i; done",
        "sh -c 'echo x' < k",
        "HOME=./x ls",
        "f() { vim f; }; f",
        "f() { echo x; }; f < k",
    ] {
        assert!(verdict_in(list, true, cmd).is_ok(), "must pass: {cmd:?}");
    }
}
