// SPDX-License-Identifier: Apache-2.0
//! A program word with a Windows executable suffix (`sh.exe`, `BASH.EXE`,
//! `C:\…\git.exe`) is compared like the bare program name, in both modes,
//! directly and as an alias or function body.

use super::*;

/// No interpreter listed: allowlist mode refuses it as unlisted.
const ALLOWLIST: &str = "echo,ls,git,cargo,true,alias";
/// The interpreters listed: their inline code is still refused.
const WITH_INTERPRETERS: &str = "echo,git,true,alias,sh,bash,zsh,python3";
/// The interpreters listed by their suffixed names.
const WITH_SUFFIXED_INTERPRETERS: &str = "echo,git,true,alias,sh.exe,bash.exe,zsh.exe,python3.exe";
const MODES: &[&str] = &[ALLOWLIST, WITH_INTERPRETERS, WITH_SUFFIXED_INTERPRETERS, ""];

fn verdict(list: &str, cmd: &str) -> Result<(), ShellError> {
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
    let verdict = enforce_shell_allowlist(cmd);
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    verdict
}

fn refused(list: &str, cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in cases {
        let verdict = verdict(list, cmd);
        assert!(verdict.is_err(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
    }
}

fn passes(list: &str, cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in cases {
        let verdict = verdict(list, cmd);
        assert!(verdict.is_ok(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
    }
}

/// Each line, also as the Claude hook wraps it, in every mode.
fn refused_wrapped_in_all_modes(cases: &[&str]) {
    let lines: Vec<String> = cases
        .iter()
        .flat_map(|c| [c.to_string(), format!("{{ {c}\n}} && pwd -P")])
        .collect();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    for list in MODES {
        refused(list, &lines);
    }
}

#[test]
fn program_name_drops_windows_executable_suffix_on_every_host() {
    use super::tokenizer::{exec_name_for, windows_executable_stem};
    for (fold_case, windows) in [(false, false), (true, false), (true, true)] {
        for (word, name) in [
            ("sh.exe", "sh"),
            ("SH.EXE", "sh"),
            ("bash.EXE", "bash"),
            ("zsh.exe", "zsh"),
            ("python3.exe", "python3"),
            ("sh.com", "sh"),
            ("/usr/bin/sh.exe", "sh"),
            ("./sh.exe", "sh"),
            ("C:\\Program Files\\Git\\bin\\bash.exe", "bash"),
            ("C:/Windows/System32/bash.exe", "bash"),
            ("lean-ctx.exe", "lean-ctx"),
        ] {
            assert_eq!(exec_name_for(word, fold_case, windows), name, "{word:?}");
        }
    }
    // Scripts keep their name: `cmd.exe` runs them, not the stem.
    assert_eq!(exec_name_for("sh.bat", false, false), "sh.bat");
    assert_eq!(exec_name_for("sh.cmd", false, false), "sh.cmd");
    assert_eq!(windows_executable_stem(".exe"), None);
    assert_eq!(windows_executable_stem("git"), None);
    assert_eq!(exec_name_for("/usr/bin/ENV", false, false), "ENV");
}

#[test]
fn shell_with_windows_executable_suffix_judged_like_bare_name() {
    let pairs: &[(&str, &str)] = &[
        ("sh.exe -c true", "sh -c true"),
        ("SH.EXE -c true", "sh -c true"),
        ("bash.exe -c true", "bash -c true"),
        ("bash.EXE -c true", "bash -c true"),
        ("zsh.exe -c true", "zsh -c true"),
        ("sh.com -c true", "sh -c true"),
        ("python3.exe -c 'print(1)'", "python3 -c 'print(1)'"),
        ("/usr/bin/sh.exe -c true", "/usr/bin/sh -c true"),
        ("./sh.exe -c true", "./sh -c true"),
        (
            "/mnt/c/Windows/System32/bash.exe -c true",
            "/mnt/c/Windows/System32/bash -c true",
        ),
        (
            "'C:\\Program Files\\Git\\bin\\bash.exe' -c true",
            "bash -c true",
        ),
        ("env sh.exe -c true", "env sh -c true"),
        ("timeout 5 BASH.EXE -c true", "timeout 5 bash -c true"),
    ];
    let suffixed: Vec<&str> = pairs.iter().map(|(s, _)| *s).collect();
    let bare: Vec<&str> = pairs.iter().map(|(_, b)| *b).collect();
    refused_wrapped_in_all_modes(&bare);
    refused_wrapped_in_all_modes(&suffixed);
}

#[test]
fn alias_body_with_windows_executable_suffix_judged_like_bare_name() {
    refused_wrapped_in_all_modes(&[
        "alias x=sh\nx -c true",
        "alias x=sh.exe\nx -c true",
        "alias x=SH.EXE\nx -c true",
        "alias x=bash.exe\nx -c true",
        "alias x=bash.EXE\nx -c true",
        "alias x=zsh.exe\nx -c true",
        "alias x='python3.exe -c'\nx 1",
        "alias x=/usr/bin/sh.exe\nx -c true",
        "alias x=./sh.exe\nx -c true",
        "alias x='C:\\Program Files\\Git\\bin\\bash.exe'\nx -c true",
    ]);
    refused(
        "",
        &[
            "alias x=sh.exe",
            "alias x=BASH.EXE",
            "alias x=python3.exe",
            "alias x=/usr/bin/sh.exe",
        ],
    );
}

#[test]
fn function_body_with_windows_executable_suffix_judged_like_bare_name() {
    refused_wrapped_in_all_modes(&[
        "x(){ sh \"$@\"; }\nx -c true",
        "x(){ sh.exe \"$@\"; }\nx -c true",
        "x(){ SH.EXE \"$@\"; }\nx -c true",
        "x() { bash.exe $*; }; x -c true",
        "x() { bash.EXE $*; }; x -c true",
        "function x { zsh.exe \"$1\" \"$2\"; }; x -c true",
        "x(){ python3.exe \"$@\"; }; x -c 1",
        "x(){ /usr/bin/sh.exe \"$@\"; }; x -c true",
        "x(){ ./sh.exe \"$@\"; }; x -c true",
        "x(){ sh -c true; }; x",
        "x(){ sh.exe -c true; }; x",
        "x(){ bash.EXE -c true; }; x",
    ]);
}

#[test]
fn benign_programs_with_windows_executable_suffix_stay_allowed() {
    let suffixed = [
        "git.exe status",
        "GIT.EXE status",
        "Git.Exe status",
        "cargo.exe build",
        "/usr/bin/git.exe status",
    ];
    passes("", &suffixed);
    for list in [ALLOWLIST, "git status *,cargo build *"] {
        listed_on_windows_only(list, &suffixed);
    }
    passes(
        "",
        &[
            "'C:\\Program Files\\Git\\cmd\\git.exe' status",
            "alias k=kubectl.exe",
            "echo sh.exe",
        ],
    );
    // A scoped entry still scopes the suffixed program.
    refused(
        "git status *,cargo build *",
        &["git.exe push", "cargo.exe run"],
    );
    // An entry written with the suffix admits the bare name too on Windows.
    passes("git.exe status *", &["git.exe status"]);
    listed_on_windows_only("git.exe status *", &["git status"]);
}

/// Allowed with `list` on Windows, which drops the suffix and trailing dots
/// and spaces of a program name; refused on other hosts, where the spelling
/// names another file.
fn listed_on_windows_only(list: &str, cases: &[&str]) {
    if cfg!(windows) {
        passes(list, cases);
    } else {
        refused(list, cases);
    }
}

#[test]
fn allowlist_names_compared_by_spelling_off_windows() {
    let cases = [
        "git.exe status",
        "ls.com",
        "LS.COM -l",
        "git. status",
        "git.. status",
        "'git ' status",
        "'git.exe. ' status",
        "env git.exe status",
        "timeout 5 ls.com",
        "nohup git. status",
        "env echo.exe hi",
        "f(){ git.exe status; }; f",
    ];
    listed_on_windows_only("echo,ls,git,env,timeout,nohup", &cases);
    listed_on_windows_only("echo,ls,git status *,timeout,env,nohup", &cases);
    // An entry spelled with the suffix or the dot admits that spelling.
    passes(
        "git.exe,ls.com,git.",
        &["git.exe status", "ls.com", "git. status"],
    );
    passes("env,git.exe", &["env git.exe status"]);
}

/// Each shell by its suffixed name in every letter case, from a Windows
/// path, and behind `env`/`nohup`.
fn suffixed_shell_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for shell in ["sh", "bash", "zsh", "dash"] {
        let title = format!("{}{}", shell[..1].to_ascii_uppercase(), &shell[1..]);
        for name in [
            format!("{shell}.exe"),
            format!("{}.EXE", shell.to_ascii_uppercase()),
            format!("{shell}.EXE"),
            format!("{title}.Exe"),
        ] {
            for prefix in [
                "",
                "C:/Program Files/Git/bin/",
                "C:\\Program Files\\Git\\bin\\",
                "/c/Program Files/Git/bin/",
                "/c/Windows/System32/",
                "C:\\Windows\\System32\\",
            ] {
                let word = format!("'{prefix}{name}'");
                lines.push(format!("{word} -c true"));
                lines.push(format!("env {word} -c true"));
                lines.push(format!("nohup {word} -c true"));
                lines.push(format!("alias x={word}\nx -c true"));
                lines.push(format!("x(){{ {word} \"$@\"; }}\nx -c true"));
            }
        }
    }
    lines
}

#[test]
fn every_shell_spelling_with_windows_executable_suffix_refused() {
    let lines = suffixed_shell_lines();
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    refused_wrapped_in_all_modes(&lines);
    refused_wrapped_in_all_modes(&[
        "dash -c true",
        "env dash -c true",
        "nohup dash -c true",
        "nohup sh -c true",
        "python.exe -c 'print(1)'",
        "PYTHON.EXE -c 'print(1)'",
        "'C:\\Python312\\python.exe' -c 'print(1)'",
    ]);
}

/// Windows command interpreters by their suffixed names get the verdict of
/// their bare names, in every mode.
#[test]
fn windows_command_interpreters_with_suffix_judged_like_bare_name() {
    let pairs: &[(&str, &str)] = &[
        ("cmd.exe /c echo hi", "cmd /c echo hi"),
        ("CMD.EXE /C echo hi", "cmd /C echo hi"),
        ("powershell.exe -c Get-Date", "powershell -c Get-Date"),
        (
            "PowerShell.EXE -Command Get-Date",
            "powershell -Command Get-Date",
        ),
        ("pwsh.exe -Command Get-Date", "pwsh -Command Get-Date"),
        (
            "'C:\\Program Files\\PowerShell\\7\\pwsh.exe' -c Get-Date",
            "pwsh -c Get-Date",
        ),
        ("python.exe -c 'print(1)'", "python -c 'print(1)'"),
    ];
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for list in MODES {
        for (suffixed, bare) in pairs {
            let (s, b) = (verdict(list, suffixed), verdict(list, bare));
            assert_eq!(
                s.is_err(),
                b.is_err(),
                "allowlist {list:?}: {suffixed:?}: {s:?} vs {bare:?}: {b:?}"
            );
        }
    }
}

/// Lists that name the Windows command interpreters and the other shells.
const WITH_WINDOWS_INTERPRETERS: &str =
    "echo,git,true,alias,cmd,powershell,pwsh,cmd.exe,powershell.exe,pwsh.exe,ash,busybox";

#[test]
fn windows_command_interpreters_refused_even_when_listed() {
    let lines = [
        "cmd /c echo hi",
        "cmd.exe /c echo hi",
        "CMD.EXE /C echo hi",
        "cmd /k echo hi",
        "cmd /C \"echo hi\"",
        "cmd /s /c echo hi",
        "cmd /d/c echo hi",
        "cmd /V:ON /c echo hi",
        "powershell -c 'echo hi'",
        "powershell -Command 'echo hi'",
        "powershell -command echo",
        "powershell -EncodedCommand ZQBjAGgAbwA=",
        "powershell -e ZQBjAGgAbwA=",
        "powershell -enc ZQBj",
        "powershell -ec ZQBj",
        "powershell /c echo",
        "powershell -NoProfile -c echo",
        "powershell echo hi",
        "powershell -File x.ps1",
        "powershell.exe -c echo",
        "pwsh -c echo",
        "pwsh -Command echo",
        "pwsh -CommandWithArgs echo",
        "pwsh -e ZQBj",
        "pwsh.exe -c echo",
        "'C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe' -c echo",
        "'C:\\Program Files\\PowerShell\\7\\pwsh.exe' -c echo",
        "C:/Windows/System32/cmd.exe /c echo",
        "/c/Windows/System32/cmd.exe /c echo",
        "'%SystemRoot%\\System32\\cmd.exe' /c echo",
        "%COMSPEC% /c echo",
        "'%COMSPEC%' /c echo",
        "POWERS~1.EXE -c echo",
        "CMD~1.EXE /c echo",
        "env cmd /c echo",
        "nohup pwsh -c echo",
        "alias w=cmd\nw /c echo",
        "alias p='powershell -c'\np echo",
        "w(){ pwsh.exe -c \"$@\"; }; w echo",
    ];
    refused_wrapped_in_all_modes(&lines);
    refused(WITH_WINDOWS_INTERPRETERS, &lines);
    // The names as plain arguments stay allowed.
    passes("", &["rg pwsh src", "grep -rn cmd .", "echo powershell"]);
}

#[test]
fn program_word_folded_by_the_file_system_refused() {
    let lines = [
        "\u{17f}h -c echo",
        "\u{17f}h.exe -c echo",
        "/bin/\u{17f}h -c echo",
        "\u{212a}sh -c echo",
        "\u{212a}sh.exe -c echo",
        "/usr/bin/\u{fb06}at /etc/hosts",
        "ba\u{df} -c echo",
        "env \u{17f}h -c echo",
        "alias x=\u{17f}h\nx -c echo",
        "x(){ \u{17f}h \"$@\"; }; x -c echo",
        // Not folded by APFS, refused all the same: the name is not plain ASCII.
        "\u{ff53}h -c echo",
        "s\u{301}h -c echo",
    ];
    refused_wrapped_in_all_modes(&lines);
    passes("", &["/Users/j\u{fc}rgen/bin/git status", "echo \u{17f}h"]);
}

#[test]
fn trailing_dots_and_spaces_dropped_from_program_name() {
    use super::tokenizer::windows_executable_stem;
    for (word, stem) in [
        ("sh.exe.", "sh"),
        ("sh.exe ", "sh"),
        ("sh.", "sh"),
        ("sh..", "sh"),
        ("SH.EXE. .", "sh"),
    ] {
        assert_eq!(
            windows_executable_stem(word).as_deref(),
            Some(stem),
            "{word:?}"
        );
    }
    assert_eq!(windows_executable_stem("."), None);
    assert_eq!(windows_executable_stem(".."), None);
    refused_wrapped_in_all_modes(&[
        "sh.exe. -c echo",
        "'sh.exe ' -c echo",
        "sh. -c echo",
        "sh.. -c echo",
        "alias x=sh.\nx -c echo",
    ]);
}

#[test]
fn unquoted_backslash_program_word_judged_by_both_readings() {
    refused_wrapped_in_all_modes(&[
        "C:\\Tools\\sh.exe -c echo",
        "C:\\Tools\\BASH.EXE -c echo",
        "C:\\Windows\\System32\\cmd.exe /c echo",
        "C:\\Python312\\python.exe -c 1",
    ]);
    passes("", &["C:\\Tools\\git.exe status", "git -C C:\\repo status"]);
}

#[test]
fn com_suffix_judged_like_exe() {
    listed_on_windows_only("git status *", &["git.com status", "GIT.COM status"]);
    passes("", &["git.com status", "GIT.COM status"]);
    refused_wrapped_in_all_modes(&["sh.com -c true", "alias x=bash.com\nx -c true"]);
}

#[test]
fn windows_batch_scripts_judged_like_cmd() {
    let lines = [
        "x.cmd",
        "./x.bat",
        "X.BAT arg",
        "run.Cmd",
        "./x.bat.",
        "'x.cmd ' arg",
        "C:\\Tools\\x.bat",
        "env x.cmd",
        "timeout 5 ./x.bat",
        "alias y=x.cmd\ny",
    ];
    refused_wrapped_in_all_modes(&lines);
    let listed = "echo,ls,git,true,alias,env,timeout,x.cmd,x.bat";
    refused(listed, &lines);
    // Every command of a function body, subshell or chain, in every mode.
    let mut nested = Vec::new();
    for line in [
        "./x.bat",
        "x.cmd",
        "true && ./x.bat",
        "pwsh -c 1",
        "cmd /c echo",
        "PoWeRsHeLl.EXE -c x",
        "\u{17f}h -c x",
        "%COMSPEC% /c x",
        "wsl -e x.cmd",
        "env x.cmd",
        "firejail pwsh -c echo",
        "firejail cmd /c echo",
        "firejail ./x.bat",
        "dbus-run-session pwsh -c echo",
    ] {
        nested.push(line.to_string());
        nested.push(format!("f(){{ {line}; }}; f"));
        nested.push(format!("(true; {line})"));
        nested.push(format!("true || {line}"));
    }
    let nested: Vec<&str> = nested.iter().map(String::as_str).collect();
    refused_wrapped_in_all_modes(&nested);
    refused(
        "echo,ls,git,true,env,wsl,firejail,dbus-run-session",
        &nested,
    );
    refused_wrapped_in_all_modes(&[
        "f(){ ./x.bat; }; f",
        "f(){ x.cmd; }; f",
        "f(){ ./x.bat; } >/dev/null; f",
        "f(){ cmd /c echo; }; f",
        "f(){ pwsh -c 1; }; f",
        "f(){ powershell -Command 1; }; f",
        "function f { cmd /c echo; }; f",
        "f(){ g(){ ./x.bat; }; g; }; f",
    ]);
    // A Windows command word as a plain argument is no command.
    passes(
        "",
        &[
            "rg pwsh src/",
            "grep -rn cmd .",
            "f(){ ls x.cmd; }; f",
            "f(){ rg pwsh .; }; f",
            "(rg -n pwsh src)",
            "firejail rg pwsh src",
            "firejail git add run.cmd",
        ],
    );
    passes(
        "rg,grep,ls",
        &["rg pwsh src/", "grep -rn cmd .", "ls x.cmd"],
    );
    // A batch script named as an argument, or a name that only contains
    // the suffix, is not run through cmd.
    passes(
        "",
        &[
            "ls x.cmd",
            "echo ./x.bat",
            "git add run.cmd",
            "batch.cmdline",
            "x.bat2",
        ],
    );
}

#[test]
fn start_and_call_judged_as_wrappers() {
    const LISTED: &str = "echo,ls,git status *,start,call";
    let unlisted_program = [
        "start rm x",
        "start /b rm x",
        "start /wait /min rm x",
        "start /D C:\\tmp rm x",
        "start /dC:\\tmp rm x",
        "start /high /affinity 3 rm x",
        "start \"title\" rm x",
        "start \"\" rm x",
        "start /b \"a title\" rm x",
        "call rm x",
        "call git push",
    ];
    refused(LISTED, &unlisted_program);
    refused(ALLOWLIST, &unlisted_program);
    let batch = [
        "call x.bat",
        "call ./x.cmd arg",
        "start x.bat",
        "start /b x.cmd",
        "start \"t\" x.bat",
        "start /wait \"\" C:\\run.CMD",
        "call start x.bat",
    ];
    refused_wrapped_in_all_modes(&batch);
    refused(LISTED, &batch);
    // `wsl --distribution-id ID` takes a value.
    refused_wrapped_in_all_modes(&["wsl --distribution-id X pwsh -c Get-Date"]);
    refused("echo,wsl", &["wsl --distribution-id X rm x"]);
    refused_wrapped_in_all_modes(&[
        "call cmd /c echo",
        "start pwsh -c 1",
        "start \"t\" bash -c 'echo'",
    ]);
    passes(
        LISTED,
        &[
            "start git status",
            "start /b ls",
            "start /wait /d C:\\tmp git status",
            "start \"title\" git status",
            "start \"\" ls -l",
            "call git status",
            "call echo hi",
        ],
    );
    passes("", &["start ls", "call git status", "start \"t\" ls"]);
}

#[test]
fn start_title_dropped_once() {
    refused_wrapped_in_all_modes(&[
        r#"start "t" "pwsh" -c x"#,
        r#"start "t" "cmd" /c x"#,
        r#"start "" "./x.bat""#,
        r#"start "" "x.bat""#,
        r#"start "t" "C:\p\pwsh.exe" -c x"#,
        r#"START "t" pwsh -c x"#,
        r#"start "t" /b "cmd" /c x"#,
        r#"start /wait "t" /b pwsh -c x"#,
        r#"start "" pwsh -c x"#,
        r#"start "a b" pwsh -c x"#,
        r#"start "t" ./x.bat"#,
        "start /b cmd /c x",
        "start /d . pwsh -c x",
        "start /D. pwsh -c x",
        "call x.cmd",
        "call ./x.bat",
        "call pwsh -c x",
        "f(){ start pwsh -c x; }; f",
        "f(){ call x.cmd; }; f",
    ]);
    // The title is dropped, not the program: a listed program still passes.
    passes(
        "echo,ls,git status *,start",
        &[
            r#"start "t" git status"#,
            r#"start "t" "git" status"#,
            r#"start "" /b ls"#,
        ],
    );
}

#[test]
fn batch_and_windows_shell_words_judged_only_as_a_program() {
    // The program a wrapper starts, or code a program runs.
    refused_wrapped_in_all_modes(&[
        "firejail pwsh -c echo",
        "firejail cmd /c echo",
        "firejail ./x.bat",
        "firejail --private ./x.bat",
        "firejail --private x.cmd",
        "dbus-run-session pwsh -c echo",
        "xdg-open ./x.bat",
        "git -c core.pager=./x.cmd log",
        "git -c core.pager=x.cmd log",
        "git -c core.sshCommand=./x.cmd fetch",
        "git -c core.sshCommand='pwsh -c x' fetch",
        "git -c alias.x='!pwsh -c x' x",
        "git -c alias.x='!./x.bat' x",
        "git --exec-path=. status",
        "git difftool -x ./x.cmd",
        "git difftool --extcmd 'pwsh -c x'",
        "git -c core.editor=./x.cmd commit",
        "git -c core.editor='pwsh -c x' commit",
        "git submodule foreach 'pwsh -c x'",
        "git submodule foreach ./x.bat",
        "git submodule foreach x.cmd",
        "git rebase --exec ./x.bat main",
        "git rebase -x 'pwsh -c x' main",
        "git bisect run ./x.cmd",
        "git -c core.fsmonitor=./x.cmd status",
        "tar --to-command=./x.cmd -xf a.tar",
        "tar --use-compress-program=./x.cmd -cf a.tar d",
        r"find . -exec ./x.cmd {} \;",
        r"find . -exec pwsh -c x \;",
        r"find . -execdir x.bat \;",
        "command ./x.bat",
        "command -p ./x.bat",
        "builtin command ./x.bat",
        "trap ./x.bat EXIT",
        "trap 'x.cmd' EXIT",
        "trap 'pwsh -c x' EXIT",
        "hash -p ./x.bat ls",
    ]);
    // Words that name files or text.
    let file_words = [
        "python3 ./gen.py ./x.bat",
        "cargo run -- ./x.bat",
        "vim ./x.cmd",
        "node gen.js --out=dist/x.cmd",
        "npm run fmt -- ./x.bat",
        "grep -rn cmd /usr/src",
        "rg pwsh -i src",
        "rg -w powershell -n .",
        "ls pwsh -la",
        "echo cmd /c x",
        "rm x.bat",
        "cp a.cmd b.cmd",
        "mv a.bat b.bat",
        "ln -s a.cmd b",
        "chmod +x x.cmd",
        "touch x.bat",
        "mkdir x.cmd",
        "git add ./run.cmd",
        "git add run.cmd",
        "git rm sub/x.bat",
        "git --exec-path . status",
        "git -C sub ./x.bat",
        "ls ./x.cmd",
        "echo ./x.bat",
        "diff a.bat b.bat",
        "cat ./x.bat",
        "grep -n x a.bat",
        "rg x a.cmd",
        "wc -l x.bat",
        "head x.cmd",
        "find . -name x.cmd",
        "sed -i s/a/b/ ./x.cmd",
        // `sed -f ./x.cmd f` is refused: the gate cannot read a script file
        // (tests_exec_options).
        "tar -xf a.bat",
        "tar -cf a.tar x.cmd",
        "firejail rg pwsh src",
    ];
    passes("", &file_words);
    passes(
        "cargo,grep,rg,ls,echo,git,cat,wc,head,diff,find,sed,tar,rm,cp,mv,ln,chmod,touch,mkdir",
        &[
            "cargo run -- ./x.bat",
            "grep -rn cmd /usr/src",
            "rg pwsh -i src",
            "rg -w powershell -n .",
            "ls pwsh -la",
            "echo cmd /c x",
            "git add run.cmd",
            "ls x.cmd",
            "cat ./x.bat",
            "find . -name x.cmd",
            "tar -xf a.bat",
            "rm x.bat",
        ],
    );
}

#[test]
fn alias_of_listed_program_judged_alike_with_and_without_suffix() {
    // `git` is judged by name and arguments, so it may start no alias body
    // (with or without the suffix).
    refused(
        ALLOWLIST,
        &["alias g='git status'; g", "alias g='git.exe status'; g"],
    );
}

#[test]
fn further_interpreters_refused_with_inline_code() {
    let listed = "echo,true,alias,mksh,nu,elvish,tcsh,csh,py,Rscript,rscript,osascript,ash,busybox";
    let lines = [
        "mksh -c echo",
        "nu -c echo",
        "elvish -c echo",
        "tcsh -c echo",
        "csh -c echo",
        "py -c 1",
        "py.exe -c 1",
        "py -3 -c 1",
        "Rscript -e 1",
        "Rscript.exe -e 1",
        "osascript -e 1",
        "ash -c echo",
        "busybox ash -c echo",
        "busybox.exe ash -c echo",
        "env nu -c echo",
        "alias x=tcsh\nx -c echo",
        "x(){ Rscript -e \"$@\"; }; x 1",
    ];
    refused_wrapped_in_all_modes(&lines);
    refused(listed, &lines);
    passes(
        listed,
        &["py script.py", "Rscript analysis.R", "ash script.sh"],
    );
}

#[test]
fn wsl_judged_as_a_wrapper() {
    let listed = "wsl,echo,true,ls";
    refused(
        listed,
        &[
            "wsl rm x",
            "wsl -- rm -rf x",
            "wsl --exec rm x",
            "wsl -e rm x",
            "wsl -u root rm x",
            "wsl -d Ubuntu rm x",
            "wsl.exe -u root rm x",
            "wsl sh -c 'echo hi'",
            "wsl echo 'a; rm x'",
        ],
    );
    refused_wrapped_in_all_modes(&[
        "wsl sh -c 'echo hi'",
        "wsl -e sh -c 'echo hi'",
        "wsl.exe -- bash -c 'echo hi'",
        "wsl -d Ubuntu -u root bash -c 'echo hi'",
        "wsl echo 'a; rm x'",
        "wsl ls '$(id)'",
    ]);
    passes(
        listed,
        &[
            "wsl echo hi",
            "wsl -e echo hi",
            "wsl -u root ls",
            "wsl -l -v",
        ],
    );
}
