// SPDX-License-Identifier: Apache-2.0
//! Alias and function bodies are judged by the executable they run, a
//! function that takes the arguments of its call is judged like an alias
//! body, a command word the shell builds when it runs is refused, and
//! `mapfile -C` is judged like the other builtins that run a command.

use super::*;

const ALLOWLIST: &str = "echo,ls,git,true,alias";
const MODES: &[&str] = &[ALLOWLIST, ""];
/// The allowlist plus the builtins it would have to list for a test.
const WITH_BUILTINS: &str = "echo,ls,git,true,alias,unalias,mapfile,readarray";

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

fn refused_in_both_modes(cases: &[&str]) {
    for list in MODES {
        refused(list, cases);
    }
}

/// The same line, also as the Claude hook wraps it.
fn wrapped(cases: &[&str]) -> Vec<String> {
    cases
        .iter()
        .flat_map(|c| [c.to_string(), format!("{{ {c}\n}} && pwd -P")])
        .collect()
}

fn refused_wrapped_in_both_modes(cases: &[&str]) {
    let lines = wrapped(cases);
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    refused_in_both_modes(&lines);
}

#[test]
fn alias_body_with_path_to_judged_program_refused() {
    refused_wrapped_in_both_modes(&[
        "alias x=/bin/sh\nx -c true",
        "alias x=/bin/bash\nx -c true",
        "alias x=/bin/zsh\nx -c true",
        "alias x=./sh\nx -c true",
        "alias x=./bash\nx -c true",
        "alias x=bin/sh\nx -c true",
        "alias x=/usr/bin/python3\nx -c 'print(1)'",
        "alias x='/usr/bin/perl -e'\nx 1",
        "alias x=/usr/bin/git\nx -c core.pager=marker_cmd log",
        "alias x=/usr/bin/find\nx . -exec marker_cmd {} +",
        "alias x=/usr/bin/awk\nx 'BEGIN { system(\"marker_cmd\") }'",
        "alias x=/usr/bin/tar\nx -xf a.tar --to-command=marker_cmd",
        "alias x=/usr/bin/watch\nx 'eval marker_cmd'",
        "alias x=/usr/bin/script\nx -c 'eval marker_cmd'",
    ]);
    refused(
        "",
        &[
            "alias x=/bin/sh",
            "alias x=/usr/bin/python3",
            "alias x='/usr/bin/git log'",
            "alias x=/usr/bin/find",
            "alias x=/usr/bin/awk",
            "alias x=/usr/bin/tar",
            "alias x=/usr/bin/watch",
            "alias x=/usr/bin/script",
            "alias x=/bin/SH",
        ],
    );
}

#[test]
fn every_alias_definition_form_judges_the_body() {
    refused_wrapped_in_both_modes(&[
        "alias a=ls b=/bin/sh\nb -c true",
        "alias -- x=/bin/sh\nx -c true",
        "builtin alias x=/bin/sh\nx -c true",
        "command alias x=/bin/sh\nx -c true",
        "\\alias x=/bin/sh\nx -c true",
        "'alias' x=/bin/sh\nx -c true",
        "unalias -a; alias x=/bin/sh\nx -c true",
    ]);
    refused(
        "",
        &[
            "alias a=ls b=/bin/sh",
            "alias -- x=/bin/sh",
            "builtin alias x=/bin/sh",
            "command alias x=/bin/sh",
            "\\alias x=/bin/sh",
            "'alias' x=/bin/sh",
            "unalias -a; alias x=/bin/sh",
        ],
    );
}

#[test]
fn alias_named_like_reserved_builtin_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &["alias eval=ls", "alias exec=ls", "alias source=ls"],
    );
}

#[test]
fn plain_alias_forms_stay_allowed() {
    for list in [WITH_BUILTINS, ""] {
        passes(
            list,
            &[
                "alias ll='ls -l'",
                "alias la='ls -A'",
                "alias -p",
                "alias ll",
                "unalias ll",
            ],
        );
    }
    passes(
        "",
        &[
            "alias k=kubectl",
            "alias ll='ls -l'; ll",
            "unalias -a",
            "unalias ll; alias ll='ls -la'",
            "{ alias ll='ls -l'\n} && pwd -P",
        ],
    );
}

#[test]
fn function_that_passes_its_arguments_on_is_judged_like_an_alias() {
    refused_wrapped_in_both_modes(&[
        "x(){ sh \"$@\"; }\nx -c true",
        "x(){ /bin/sh \"$@\"; }\nx -c true",
        "x() { bash $*; }; x -c true",
        "function x { zsh \"$1\" \"$2\"; }; x -c true",
        "x(){ eval \"$1\"; }; x marker_cmd",
        "x(){ git \"$@\"; }; x -c core.pager=marker_cmd log",
        "x(){ /usr/bin/python3 \"$@\"; }; x -c 1",
        "x(){ a=$1; sh $a; }; x -c",
        "x(){ if true; then sh \"$@\"; fi; }; x -c true",
        "x(){ y(){ sh \"$@\"; }; y \"$@\"; }; x -c true",
        "x(){ ./tool \"$@\"; }; x",
        "x(){ \"$1\" \"$2\"; }; x sh -c",
    ]);
    // Without a function the arguments come from `set --` or the caller.
    refused(
        "",
        &[
            "set -- -c true; sh \"$@\"",
            "set -- -c true; /bin/sh \"$@\"",
            "set -- log; git \"$@\"",
        ],
    );
}

#[test]
fn function_that_reads_arguments_as_data_stays_allowed() {
    for list in MODES {
        passes(
            list,
            &[
                "f() { echo \"$1\"; }; f hi",
                "reply() { echo \"got $1\"; }",
                "f() { ls \"$@\"; }; f",
                "f() { [ -n \"$1\" ] && echo y; }; f a",
                "f() { echo a; }; f",
                "f() { git status; }; f",
                "f() { printf '%s\\n' \"$@\"; }; f a b",
                "f() { echo '$1'; }; f",
            ],
        );
    }
    passes(
        "",
        &[
            "awk '{print $1}' notes.txt",
            "f() { shift; echo \"$@\"; }; f a b",
            "case \"$1\" in -h|--help) echo help ;; *) echo other ;; esac",
        ],
    );
}

#[test]
fn builtin_reading_lines_with_a_callback_refused() {
    let lines = wrapped(&[
        "mapfile -C 'eval true' a",
        "readarray -C marker_cmd a",
        "mapfile -c 1 -C marker_cmd a",
        "mapfile -tC marker_cmd a",
        "f() { mapfile -C marker_cmd a; }; f",
    ]);
    for list in [WITH_BUILTINS, ""] {
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        refused(list, &lines);
    }
}

#[test]
fn builtin_reading_lines_stays_allowed() {
    for list in [WITH_BUILTINS, ""] {
        passes(
            list,
            &[
                "mapfile -t a < notes.txt",
                "readarray -c 1 a < notes.txt",
                "mapfile a < notes.txt",
            ],
        );
    }
}

#[test]
fn command_word_built_when_the_shell_runs_refused() {
    refused_wrapped_in_both_modes(&[
        "V=eval\n$V true",
        "V=eval; $V true",
        "${V:-eval} true",
        "\"${V:-eval}\" true",
        "ev?l true",
        "e*l true",
        "ev[a]l true",
        "e{v,}al true",
        "if true; then $V true; fi",
        "true && $V true",
        "command $V true",
        "env $V true",
    ]);
}

#[test]
fn command_words_written_out_stay_allowed() {
    for list in MODES {
        passes(
            list,
            &[
                "[ -f notes.txt ] && echo y",
                "[[ -f notes.txt ]] && echo y",
                "ls *.rs",
                "echo $V",
                "ls \"$HOME\"",
                "command -v \"$x\"",
                "case $x in *) echo y ;; esac",
                "{ ls; }",
                "for f in *.rs; do echo \"$f\"; done",
            ],
        );
    }
    passes("", &["V=1 ls", "((i++))", "a=(x y)"]);
}

#[test]
fn alias_to_builtin_reading_lines_refused() {
    refused_wrapped_in_both_modes(&[
        "alias x=mapfile\nx -C true a",
        "alias x=readarray\nx -C true a",
        "alias x='mapfile -t'\nx -C true a",
    ]);
    refused("", &["alias x=mapfile", "alias x=readarray"]);
}

#[test]
fn command_word_after_time_options_judged() {
    refused_wrapped_in_both_modes(&[
        "time -p $V",
        "time -p $V -c true",
        "time -p s? -c true",
        "time -p {sh,} -c true",
        "time -- $V",
        "! time -p $V",
        "time -p -- $V",
    ]);
    for list in MODES {
        passes(list, &["time ls"]);
    }
    passes("", &["time -p ls", "! time -p ls", "time -- ls"]);
}

#[test]
fn command_word_in_function_body_judged() {
    refused_wrapped_in_both_modes(&[
        "x(){ $V; }",
        "x(){ $V; }\nx",
        "x(){ s? -c true; }\nx",
        "x(){ {sh,} -c true; }\nx",
        "x(){ ${V:-sh} -c true; }\nx",
        "function x { $V -c true; }\nx",
        "x() ( $V -c true )\nx",
        "x(){ time -p $V; }\nx",
        "x(){ y(){ $V; }; }",
    ]);
}

#[test]
fn function_walking_its_arguments_without_naming_them_judged() {
    refused_wrapped_in_both_modes(&[
        "x(){ for a; do git log \"$a\"; done; }\nx --output=notes.txt",
        "x(){ for a do git $a log; done; }\nx -ccore.pager=marker_cmd",
        "x(){ for a\ndo git $a log\ndone; }\nx -ccore.pager=marker_cmd",
        "x(){ select a; do git $a log; break; done; }\nx -ccore.pager=marker_cmd",
        "x(){ getopts c: o; git -c \"$OPTARG\" log; }\nx -c core.pager=marker_cmd",
        "getopts c: o; git -c \"$OPTARG\" log",
        "set -- -ccore.pager=marker_cmd\nfor a; do git $a log; done",
    ]);
    let rm_list = "echo,ls,git,true,alias,rm";
    for list in [rm_list, ""] {
        let lines = wrapped(&[
            "x(){ for a; do rm -rf \"$a\"; done; }\nx notes.txt",
            "x(){ getopts c: o; rm -rf \"$OPTARG\"; }\nx -c notes.txt",
        ]);
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        refused(list, &lines);
    }
}

#[test]
fn loops_with_a_word_list_stay_allowed() {
    for list in MODES {
        passes(
            list,
            &[
                "for f in *.rs; do echo \"$f\"; done",
                "f(){ for a in x y; do git log \"$a\"; done; }; f",
                "f(){ for a; do echo \"$a\"; done; }; f a b",
                "for ((i = 0; i < 3; i++)); do echo $i; done",
            ],
        );
    }
}

#[test]
fn function_body_behind_a_redirection_judged() {
    refused_wrapped_in_both_modes(&[
        "x(){ $V; } >/dev/null",
        "x(){ $V; } 2>/dev/null",
        "x(){ $V; } >&2",
        "function x { $V; } >notes.txt",
        "x() ( $V ) >notes.txt",
        "x(){ s? -c true; } >/dev/null",
        "x(){ {sh,} -c true; } >/dev/null",
        "x(){ git log \"$@\"; } >/dev/null\nx --output=notes.txt",
        "x(){ git log \"$1\"; } 2>&1\nx --output=notes.txt",
        "x(){ getopts c: o; git -c \"$OPTARG\" log; } >/dev/null\nx -c core.pager=marker_cmd",
    ]);
    // With an allowlist a definition followed by a redirection is refused
    // as a whole, as before.
    passes(
        "",
        &["f(){ ls; } >/dev/null; f", "f(){ echo \"$1\"; } 2>&1; f a"],
    );
}

#[test]
fn nested_subshells_in_function_body_judged() {
    let nested = |levels: usize, inner: &str| {
        format!(
            "x(){{ {}{inner}{}; }}",
            "( ".repeat(levels),
            " )".repeat(levels)
        )
    };
    let lines = [nested(2, "$V"), nested(8, "$V"), nested(16, "$V")];
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    refused_wrapped_in_both_modes(&lines);
    for list in MODES {
        passes(
            list,
            &[
                "x(){ ( ls ); }; x",
                "x(){ ( ( ls ) ); }; x",
                "x(){ ((i++)); }",
            ],
        );
    }
}

#[test]
fn indirect_expansion_in_function_body_counts_as_arguments() {
    refused_wrapped_in_both_modes(&[
        "x(){ git log \"${!#}\"; }\nx --output=notes.txt",
        "x(){ git log ${!#}; }\nx --output=notes.txt",
        "x(){ n=1; git log \"${!n}\"; }\nx --output=notes.txt",
        "x(){ git log \"${!1}\"; }",
    ]);
    refused("", &["x(){ rm -rf \"${!#}\"; }\nx notes.txt"]);
    for list in MODES {
        passes(list, &["x(){ echo \"${!#}\"; }; x a"]);
    }
}

#[test]
fn time_with_assignments_or_behind_a_wrapper_judged() {
    refused_wrapped_in_both_modes(&[
        "time -p A=1 $V",
        "time -p A=1 s? -c true",
        "time -p A=1 {sh,} -c true",
        "time A=1 B=2 $V",
        "command time -p $V",
        "command time $V",
        "command time -p s? -c true",
        "/usr/bin/time -p $V",
        "/usr/bin/time $V",
        "env time -p $V",
        "nohup time -p $V",
        "command -p time $V",
        "time -p command $V",
        "time -p env $V",
    ]);
    passes(
        "",
        &[
            "time -l ls",
            "time -lp ls",
            "time -v ls",
            "time --format=%e ls",
            "/usr/bin/time ls",
        ],
    );
}

#[test]
fn alias_to_builtin_behind_builtin_word_refused() {
    refused_wrapped_in_both_modes(&[
        "alias x='builtin hash'",
        "alias x='command hash'",
        "alias x='builtin mapfile'\nx -C true a",
    ]);
    passes(ALLOWLIST, &["alias ll='ls -l'"]);
}
