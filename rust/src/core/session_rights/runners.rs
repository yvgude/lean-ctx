// SPDX-License-Identifier: Apache-2.0
//! Wrappers, shells and interpreters (part of [`super`]): which words run
//! another command, and whether a shell or interpreter runs code this check
//! cannot see (`-c`, `-e`, code from stdin).

use super::{COMMAND_ENV, Walk, base_name};

/// A `KEY=value` word whose key names a command git, a pager or the shell
/// runs (`GIT_EXTERNAL_DIFF=…`, `PAGER=…`) is refused.
pub(super) fn command_env(word: &str, walk: &mut Walk<'_>) -> bool {
    let Some((key, _)) = word.split_once('=') else {
        return false;
    };
    let is_key = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if is_key && (COMMAND_ENV.contains(&key) || key.starts_with("GIT_CONFIG_")) {
        walk.refuse(
            "unanalysable",
            format!("{key} names a command or configuration that runs code this check cannot see"),
        );
    }
    is_key
}

/// The options a wrapper takes. An option not listed is refused: a value
/// mistaken for the command (`numactl -C 0 rm x`) would hide what runs.
struct WrapperSpec {
    /// Options without a value.
    flags: &'static [&'static str],
    /// Options with a value (separate, `--opt=v` or attached `-ov`).
    values: &'static [&'static str],
    /// Options refused outright (they run a shell, write a file, or move).
    refuse: &'static [&'static str],
    /// One positional argument before the command (`timeout 5`, `chrt 10`).
    argument: bool,
    /// Any `--key=value` option is fine.
    long_values: bool,
    /// `-10`-style numbers are fine.
    numbers: bool,
}

const fn spec(flags: &'static [&'static str], values: &'static [&'static str]) -> WrapperSpec {
    WrapperSpec {
        flags,
        values,
        refuse: &[],
        argument: false,
        long_values: false,
        numbers: false,
    }
}

/// The wrapper `word` (with its subcommand word `next`, for `uv run`) and
/// how many words name it, or `None` when `word` is a command of its own.
fn wrapper_spec(word: &str, next: Option<&str>) -> Option<(WrapperSpec, usize)> {
    let one = |spec| Some((spec, 1));
    match (word, next) {
        ("sudo" | "doas", _) => one(WrapperSpec {
            refuse: &["-i", "-s", "--login", "--shell", "-e", "--edit"],
            long_values: true,
            ..spec(
                &[
                    "-E",
                    "-H",
                    "-n",
                    "-P",
                    "-k",
                    "-A",
                    "-S",
                    "-b",
                    "--preserve-env",
                    "--non-interactive",
                ],
                &[
                    "-u", "-g", "-h", "-p", "-r", "-t", "-C", "-U", "--user", "--group", "--host",
                    "--prompt",
                ],
            )
        }),
        ("env", _) => one(WrapperSpec {
            refuse: &["-C", "--chdir", "-S", "--split-string"],
            ..spec(
                &[
                    "-i",
                    "--ignore-environment",
                    "-0",
                    "--null",
                    "-v",
                    "--debug",
                    "-",
                ],
                &["-u", "--unset"],
            )
        }),
        ("nice", _) => one(WrapperSpec {
            numbers: true,
            ..spec(&[], &["-n", "--adjustment"])
        }),
        ("ionice", _) => one(spec(
            &["-t", "--ignore"],
            &["-c", "--class", "-n", "--classdata"],
        )),
        ("timeout", _) => one(WrapperSpec {
            argument: true,
            ..spec(
                &["--preserve-status", "--foreground", "-v", "--verbose"],
                &["-s", "--signal", "-k", "--kill-after"],
            )
        }),
        ("stdbuf", _) => one(spec(
            &[],
            &["-i", "-o", "-e", "--input", "--output", "--error"],
        )),
        ("taskset", _) => one(WrapperSpec {
            argument: true,
            ..spec(&["-a", "--all-tasks", "-c", "--cpu-list"], &[])
        }),
        ("chrt", _) => one(WrapperSpec {
            argument: true,
            ..spec(
                &[
                    "-b",
                    "-f",
                    "-r",
                    "-o",
                    "-i",
                    "-d",
                    "-a",
                    "-R",
                    "-v",
                    "--batch",
                    "--fifo",
                    "--rr",
                    "--other",
                    "--idle",
                    "--deadline",
                    "--all-tasks",
                    "--reset-on-fork",
                    "--verbose",
                ],
                &[
                    "-T",
                    "-P",
                    "-D",
                    "--sched-runtime",
                    "--sched-period",
                    "--sched-deadline",
                ],
            )
        }),
        ("flock", _) => one(WrapperSpec {
            refuse: &["-c", "--command"],
            argument: true,
            ..spec(
                &[
                    "-s",
                    "-x",
                    "-e",
                    "-n",
                    "-u",
                    "-o",
                    "-F",
                    "--shared",
                    "--exclusive",
                    "--nonblock",
                    "--close",
                    "--unlock",
                    "--no-fork",
                ],
                &["-w", "--timeout", "-E", "--conflict-exit-code"],
            )
        }),
        ("prlimit", _) => one(WrapperSpec {
            long_values: true,
            ..spec(&["--verbose"], &[])
        }),
        ("numactl", _) => one(WrapperSpec {
            long_values: true,
            ..spec(
                &["-l", "--localalloc", "-a", "--all"],
                &[
                    "-C",
                    "--physcpubind",
                    "-N",
                    "--cpunodebind",
                    "-m",
                    "--membind",
                    "-p",
                    "--preferred",
                    "-i",
                    "--interleave",
                ],
            )
        }),
        ("valgrind", _) => one(WrapperSpec {
            long_values: true,
            ..spec(&["-q", "-v", "--quiet", "--verbose"], &[])
        }),
        ("fakeroot", _) => one(WrapperSpec {
            refuse: &["-s"],
            ..spec(
                &["-u", "--unknown-is-real"],
                &["-i", "-l", "--lib", "--faked"],
            )
        }),
        ("faketime", _) => one(WrapperSpec {
            argument: true,
            ..spec(&["-f", "-m"], &[])
        }),
        ("xvfb-run", _) => one(WrapperSpec {
            refuse: &["-e", "--error-file", "-f", "--auth-file"],
            long_values: true,
            ..spec(
                &["-a", "--auto-servernum", "-l", "--listen-tcp"],
                &[
                    "-n",
                    "--server-num",
                    "-s",
                    "--server-args",
                    "-p",
                    "--xauth-protocol",
                    "-w",
                    "--wait",
                ],
            )
        }),
        ("setsid", _) => one(spec(&["-c", "--ctty", "-f", "--fork", "-w", "--wait"], &[])),
        ("time", _) => one(WrapperSpec {
            refuse: &["-o", "--output", "-a", "--append"],
            ..spec(
                &["-p", "-v", "--portability", "--verbose"],
                &["-f", "--format"],
            )
        }),
        ("exec", _) => one(spec(&["-c", "-l"], &["-a"])),
        ("chronic", _) => one(spec(&["-e", "-v"], &[])),
        ("caffeinate", _) => one(spec(&["-d", "-i", "-m", "-s", "-u"], &["-t", "-w"])),
        ("nohup" | "builtin" | "catchsegv", _) => one(spec(&[], &[])),
        ("command", _) => one(spec(&["-p", "-v", "-V"], &[])),
        ("npx", _) => one(WrapperSpec {
            refuse: &["-c", "--call"],
            ..spec(
                &["-y", "--yes", "--no", "-q", "--quiet"],
                &["-p", "--package"],
            )
        }),
        ("bunx", _) => one(spec(&["--bun", "--silent"], &["-p", "--package"])),
        ("uvx", _) => one(WrapperSpec {
            refuse: &["--directory", "--project"],
            long_values: true,
            ..spec(
                &["-q", "--quiet", "--isolated", "--offline"],
                &["--from", "--with", "--python", "-p"],
            )
        }),
        ("uv" | "pdm", Some("run")) => Some((
            WrapperSpec {
                refuse: &["--directory", "--project", "-C"],
                long_values: true,
                ..spec(
                    &[
                        "--frozen",
                        "--locked",
                        "--no-sync",
                        "--isolated",
                        "-q",
                        "--quiet",
                        "--all-extras",
                        "--no-dev",
                        "-v",
                        "--offline",
                    ],
                    &[
                        "--with",
                        "--python",
                        "-p",
                        "--env-file",
                        "--extra",
                        "--group",
                        "--package",
                        "--index",
                    ],
                )
            },
            2,
        )),
        ("poetry" | "pipenv", Some("run")) => Some((spec(&[], &[]), 2)),
        ("conda" | "mamba" | "micromamba", Some("run")) => Some((
            WrapperSpec {
                refuse: &["--cwd"],
                ..spec(
                    &["--no-capture-output", "--live-stream"],
                    &["-n", "--name", "-p", "--prefix"],
                )
            },
            2,
        )),
        ("pnpm" | "yarn", Some("dlx" | "exec")) => Some((
            WrapperSpec {
                refuse: &["-c", "--shell-mode"],
                ..spec(&["--silent", "-s"], &["--package", "-p"])
            },
            2,
        )),
        _ => None,
    }
}

/// Whether wrapper option `a` is known to `spec`, and the words it takes.
fn wrapper_option(spec: &WrapperSpec, a: &str) -> Option<usize> {
    if spec.flags.contains(&a) {
        return Some(1);
    }
    if spec.values.contains(&a) {
        return Some(2);
    }
    let attached = spec.values.iter().any(|v| {
        if v.starts_with("--") {
            a.strip_prefix(v).is_some_and(|rest| rest.starts_with('='))
        } else {
            a.len() > v.len() && a.starts_with(v) && !a.starts_with("--")
        }
    });
    if attached
        || (spec.long_values && a.starts_with("--") && a.contains('='))
        || (spec.numbers && a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()))
    {
        return Some(1);
    }
    None
}

/// Drop leading assignments and wrappers (`sudo`, `env`, `timeout 5`,
/// `setsid`, `chrt 5`, `uv run`, `npx`, …) together with their own options
/// and arguments; an option a wrapper is not known to take is refused.
pub(super) fn unwrap_command(words: &[String], walk: &mut Walk<'_>) -> Vec<String> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if command_env(w, walk) {
            i += 1;
            continue;
        }
        let wrapper = base_name(w).to_string();
        if matches!(wrapper.as_str(), "chroot" | "nsenter" | "unshare") {
            walk.refuse(
                "unanalysable",
                format!("{wrapper} changes the root or namespace"),
            );
            return Vec::new();
        }
        let Some((spec, named)) = wrapper_spec(&wrapper, words.get(i + 1).map(String::as_str))
        else {
            break;
        };
        i += named;
        while i < words.len() && words[i].starts_with('-') {
            let a = words[i].as_str();
            if a == "--" {
                i += 1;
                break;
            }
            let refused = spec
                .refuse
                .iter()
                .any(|r| a == *r || (r.starts_with("--") && a.starts_with(&format!("{r}="))));
            if refused {
                walk.refuse(
                    "unanalysable",
                    format!("{wrapper} {a} runs, writes or moves what this check cannot see"),
                );
                return Vec::new();
            }
            let Some(taken) = wrapper_option(&spec, a) else {
                walk.refuse(
                    "unanalysable",
                    format!("{wrapper} option {a} is not one this check knows"),
                );
                return Vec::new();
            };
            i += taken;
        }
        if wrapper == "command" && words[..i].iter().any(|a| a == "-v" || a == "-V") {
            return Vec::new(); // a lookup, nothing runs
        }
        if spec.argument && i < words.len() {
            i += 1;
            // `flock FILE -c CMD`: the refused option may follow the argument.
            if let Some(a) = words.get(i).filter(|a| spec.refuse.contains(&a.as_str())) {
                walk.refuse(
                    "unanalysable",
                    format!("{wrapper} {a} runs what this check cannot see"),
                );
                return Vec::new();
            }
        }
    }
    words[i..].to_vec()
}

/// Shell options that take a value.
const SHELL_VALUE_OPTIONS: &[&str] = &["-o", "+o", "-O", "+O", "--rcfile", "--init-file"];

/// (inline code, a script file named) for a shell's arguments. Every option
/// before the script is read, values of `-o`/`-O` skipped.
pub(super) fn shell_inline(args: &[String]) -> (bool, bool) {
    let mut i = 0;
    let mut inline = false;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            return (inline, i + 1 < args.len());
        }
        if !(a.starts_with('-') || a.starts_with('+')) || a == "-" {
            return (inline, a != "-");
        }
        if SHELL_VALUE_OPTIONS.contains(&a) {
            i += 2;
            continue;
        }
        if a == "--command" || (!a.starts_with("--") && (a.contains('c') || a.contains('s'))) {
            inline = true;
        }
        i += 1;
    }
    (inline, false)
}

/// A shell runs a script file (allowed) or code it cannot show (refused).
pub(super) fn shell_rule(shell: &str, args: &[String], walk: &mut Walk<'_>) {
    let (inline, script) = shell_inline(args);
    if inline || !script {
        walk.refuse(
            "unanalysable",
            format!("{shell} -c, or a shell reading its script from stdin, runs code this check cannot see"),
        );
    }
}

pub(super) fn is_interpreter(base: &str) -> bool {
    base.starts_with("python")
        || base.starts_with("pypy")
        || base.starts_with("lua")
        || base.starts_with("ruby")
        || base.starts_with("perl")
        || base.starts_with("php")
        || matches!(
            base,
            "node"
                | "nodejs"
                | "deno"
                | "bun"
                | "Rscript"
                | "julia"
                | "tclsh"
                | "wish"
                | "osascript"
                | "pwsh"
                | "powershell"
                | "groovy"
                | "jshell"
                | "irb"
                | "ts-node"
                | "tsx"
        )
}

/// The options of an interpreter that take a separate value which is not
/// code. Per family: a flag wrongly listed here would hide the inline-code
/// flag after it, a missing one would let its value pass for the script.
fn interpreter_value_options(base: &str) -> &'static [&'static str] {
    if base.starts_with("python") || base.starts_with("pypy") {
        &["-W", "-X", "-Q"]
    } else if base.starts_with("ruby") {
        &["-I", "-r", "-C", "-E", "-F"]
    } else if base.starts_with("perl") {
        &["-I"]
    } else if base.starts_with("php") {
        &["-d", "-c", "-z"]
    } else if base.starts_with("lua") {
        &["-l"]
    } else if base == "julia" {
        &["-p", "-t", "-J", "-L", "-C", "--project"]
    } else if matches!(base, "node" | "nodejs" | "bun" | "deno" | "ts-node" | "tsx") {
        &[
            "-r",
            "--require",
            "--import",
            "--loader",
            "--experimental-loader",
            "--env-file",
            "--title",
            "-C",
            "--conditions",
        ]
    } else {
        &[]
    }
}

/// (inline code, a script or module named, only version/help asked) for an
/// interpreter's arguments; every option before the script is read.
pub(super) fn interpreter_inline(base: &str, args: &[String]) -> (bool, bool, bool) {
    let python = base.starts_with("python") || base.starts_with("pypy");
    let values = interpreter_value_options(base);
    let mut i = 0;
    let mut inline = false;
    let mut info = true;
    let mut saw_option = false;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-" {
            return (true, false, false);
        }
        if a == "--" {
            return (inline, i + 1 < args.len(), false);
        }
        if !a.starts_with('-') {
            let eval = base == "deno" && matches!(a, "eval" | "repl");
            return (inline || eval, true, false);
        }
        saw_option = true;
        if python && a == "-m" {
            return (inline, i + 1 < args.len(), false);
        }
        if !matches!(a, "-V" | "--version" | "-h" | "--help" | "-v") {
            info = false;
        }
        // node's loaders take a module, which a `data:` URL turns into code.
        let node_like = matches!(base, "node" | "nodejs" | "bun" | "deno" | "ts-node" | "tsx");
        let value = if values.contains(&a) {
            args.get(i + 1).map(String::as_str)
        } else {
            a.split_once('=').map(|(_, v)| v)
        };
        if node_like
            && value.is_some_and(|v| v.starts_with("data:") || v.starts_with("javascript:"))
        {
            inline = true;
        }
        if base.starts_with("perl") && (a.starts_with("-M") || a.starts_with("-m")) {
            // `-M'mod;code'` runs code; a plain module name does not.
            inline |= a.contains([';', '(', '{', '`', ' ', '"', '\'']);
        } else if a.starts_with("--eval")
            || a.starts_with("--print")
            || matches!(
                a,
                "--command" | "-Command" | "-EncodedCommand" | "--input-type"
            )
        {
            inline = true;
        } else if !a.starts_with("--") && !values.contains(&a) {
            let cluster = &a[1..];
            inline |= if python {
                cluster.contains('c')
            } else if base.starts_with("perl") {
                cluster.contains('e') || cluster.contains('E')
            } else if base.starts_with("ruby") {
                cluster.contains('e')
            } else if base.starts_with("php") {
                cluster.contains(['r', 'B', 'R', 'E'])
            } else if base == "julia" {
                cluster.contains(['e', 'E'])
            } else if matches!(base, "pwsh" | "powershell") {
                cluster.eq_ignore_ascii_case("c") || cluster.eq_ignore_ascii_case("command")
            } else {
                cluster.contains('e') || cluster.contains('p')
            };
        }
        i += if values.contains(&a) { 2 } else { 1 };
    }
    (inline, false, info && saw_option)
}

/// An interpreter runs a script file (allowed) or inline code (refused).
pub(super) fn interpreter_rule(base: &str, args: &[String], stdin: bool, walk: &mut Walk<'_>) {
    let (inline, script, info_only) = interpreter_inline(base, args);
    let asks_only_info = info_only && !stdin;
    if inline || !(script || asks_only_info) {
        walk.refuse(
            "unanalysable",
            format!("{base} with inline code or code from stdin can write anywhere; run a script file instead"),
        );
    }
}
