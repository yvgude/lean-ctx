// SPDX-License-Identifier: Apache-2.0
//! What a shell command line writes, for the session rights (process-v1 §9).
//!
//! [`analyse`] reads a command line the way the shell would split it and
//! returns every write target it names plus a refusal for everything it
//! cannot judge. The principle: under an active profile, what cannot be
//! analysed is refused with a named rule, never passed.
//!
//! Seen and judged: redirections (`>`, `>>`, `>|`, `&>`, `n>`), command and
//! process substitutions (analysed as commands of their own), `cd` (every
//! directory a later relative path may mean is checked), and the writers
//! `tee cp mv install ln rsync rm rmdir unlink touch mkdir shred truncate
//! chmod chown chgrp sed -i dd sort -o uniq curl -o wget -O tar -cf`, the
//! compressors, and the git commands that name their paths.
//!
//! Refused (`unanalysable`): `sh/bash/zsh -c` (every option before the script
//! is read, `-o`/`-O` values skipped) or a shell reading its script from
//! stdin, `eval`, `source`/`.`, `alias`, interpreters with inline code (`-c`,
//! `-e`, `-`, a heredoc or pipe), a shell or interpreter with inline code
//! anywhere in a command's words (`setsid bash -c …`), runners this check
//! cannot follow (`watch`, `su`, `script`, `strace`, `flock -c`, …), an
//! assignment of a command-valued variable (`GIT_EXTERNAL_DIFF=`, `PAGER=`,
//! `GIT_SSH_COMMAND=`, …), `awk -i`/an awk program that writes, a sed script
//! that writes or runs, `xargs`, `parallel`, `find -delete/-exec`, archive
//! extraction, `patch`, editors, `pushd`/`popd`, brace expansion in a target.
//! `git_rewrite`: checkout/switch/restore/reset --hard/stash/apply/am/
//! cherry-pick/revert/clean/rebase and every git command not known to keep
//! the worktree, unless it names every path it writes; `merge` of anything but
//! the bound `base_commit`, `pull`. `git_config`: `git -c`, `git config`
//! writes, `--exec-path`, `--upload-pack`/`ext::` transports, `grep -O`. The
//! output files of kept git commands (`diff --output=f`, `archive -o f`,
//! `format-patch -o d`) are judged as targets. `daemon_socket`: talking to
//! the LeanCTX daemon directly. `rights_env`: naming `LEAN_CTX_RIGHTS_FILE`.
//! `bash_unresolved_target`: a target behind a variable or substitution.
//!
//! Running a script file or a build tool (`python3 tools/x.py`, `cargo`,
//! `npm`) stays allowed: what such a program opens is not seen here.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ENV_FILE;
use crate::core::shell_allowlist::{
    segments_with_separators, shell_tokenize, strip_all_heredoc_bodies, strip_for_analysis,
};

/// One result of the analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// The command writes (creates, changes, removes) this path.
    Write(PathBuf),
    /// The command cannot run under the profile.
    Refuse { rule: &'static str, detail: String },
}

#[path = "git.rs"]
mod git;
#[path = "runners.rs"]
mod runners;
use git::git_rule;
use runners::{
    command_env, interpreter_inline, interpreter_rule, is_interpreter, shell_inline, shell_rule,
    unwrap_command,
};

const MAX_DEPTH: usize = 8;
const SINKS: &[&str] = &[
    "/dev/null",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
    "/dev/zero",
];
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh",
];
const EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "view", "ex", "ed", "nano", "pico", "emacs", "micro", "hx", "kak", "joe",
];
const ARCHIVERS: &[&str] = &[
    "unzip", "cpio", "7z", "7za", "7zr", "unrar", "ar", "pax", "rpm2cpio", "jar",
];
const COMPRESSORS: &[&str] = &[
    "gzip",
    "gunzip",
    "bzip2",
    "bunzip2",
    "xz",
    "unxz",
    "zstd",
    "unzstd",
    "lz4",
    "lzma",
    "unlzma",
    "compress",
    "uncompress",
];
/// Environment variables whose value git, a pager or the shell runs.
const COMMAND_ENV: &[&str] = &[
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
    "PAGER",
    "MANPAGER",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "EDITOR",
    "VISUAL",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_TEMPLATE_DIR",
    "GIT_REPLACE_REF_BASE",
    "GIT_GRAFT_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "LESSOPEN",
    "LESSCLOSE",
    "BASH_ENV",
    "ENV",
    "PROMPT_COMMAND",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "DYLD_INSERT_LIBRARIES",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "PERL5OPT",
    "RUBYOPT",
];
/// Programs that run another command in a way this check cannot follow.
const OPAQUE_RUNNERS: &[&str] = &[
    "watch",
    "su",
    "runuser",
    "sg",
    "newgrp",
    "script",
    "strace",
    "ltrace",
    "gdb",
    "lldb",
    "expect",
    "tmux",
    "screen",
    "at",
    "batch",
    "crontab",
    "systemd-run",
    "dbus-run-session",
];

struct Walk<'a> {
    /// The base line's name (`base_ref` of the rights file).
    base_ref: Option<&'a str>,
    /// The base commit a Developer may merge (`base_commit`).
    base_commit: Option<&'a str>,
    /// An earlier command may have moved refs (`fetch`, `tag`, …).
    refs_may_move: bool,
    /// Every directory a relative path may mean at this point.
    cwds: Vec<PathBuf>,
    /// A `cd` to a place this analysis cannot tell.
    cwd_unknown: bool,
    /// Names the caller will add to the child's environment (ctx_shell `env`).
    env_keys: &'a [&'a str],
    out: Vec<Finding>,
}

impl Walk<'_> {
    fn refuse(&mut self, rule: &'static str, detail: impl Into<String>) {
        self.out.push(Finding::Refuse {
            rule,
            detail: detail.into(),
        });
    }
}

/// The base line of the rights file: its name and its bound commit.
#[derive(Debug, Clone, Copy, Default)]
pub struct Base<'a> {
    pub name: Option<&'a str>,
    pub commit: Option<&'a str>,
}

/// Every write and refusal a command line holds, relative to `cwd`.
pub fn analyse(command: &str, cwd: &Path, base: Base<'_>, env_keys: &[&str]) -> Vec<Finding> {
    let mut walk = Walk {
        base_ref: base.name,
        base_commit: base.commit,
        refs_may_move: false,
        cwds: vec![cwd.to_path_buf()],
        cwd_unknown: false,
        env_keys,
        out: Vec::new(),
    };
    // The caller's `env` argument is another way to set a command-valued or
    // namespace variable; judge each key like a leading `KEY=value`.
    for key in env_keys {
        if COMMAND_ENV.contains(key) || key.starts_with("GIT_CONFIG_") {
            walk.refuse(
                "unanalysable",
                format!("the env key {key} names a command or namespace that runs or redirects what this check cannot see"),
            );
        }
    }
    analyse_into(command, &mut walk, 0);
    walk.out
}

fn analyse_into(command: &str, walk: &mut Walk<'_>, depth: usize) {
    if depth > MAX_DEPTH {
        walk.refuse("unanalysable", "substitutions are nested too deep");
        return;
    }
    if command.contains(ENV_FILE) {
        walk.refuse(
            "rights_env",
            format!("a command may not name or change {ENV_FILE}"),
        );
    }
    if let Some(why) = daemon_access(command) {
        walk.refuse("daemon_socket", why);
    }
    // An unquoted heredoc expands `$(…)`, so only quoted bodies are text.
    let text = strip_for_analysis(command, true, false);
    let (outer, bodies) = match extract_substitutions(&text) {
        Ok(parts) => parts,
        Err(reason) => {
            walk.refuse("unanalysable", reason);
            return;
        }
    };
    for body in bodies {
        analyse_into(&body, walk, depth + 1);
    }
    let outer = strip_all_heredoc_bodies(&outer);
    let mut piped = false;
    for (segment, separator) in segments_with_separators(&outer) {
        let fed = piped;
        piped = matches!(
            separator,
            Some(crate::core::shell_allowlist::Separator::Pipe)
        );
        // A subshell `( … )` or group `{ …; }` keeps its separators inside
        // one segment: its commands are analysed on their own.
        match extract_groups(&segment) {
            Ok((rest, groups)) => {
                for group in groups {
                    analyse_into(&group, walk, depth + 1);
                }
                analyse_segment(&rest, walk, fed);
            }
            Err(reason) => walk.refuse("unanalysable", reason),
        }
    }
}

/// Split out `( … )` and `{ …; }` groups that start a word. In the returned
/// text each group becomes `$GROUP`, an unresolved word. A `{` directly
/// followed by text (`{a,b}`) is brace expansion, not a group, and stays.
fn extract_groups(segment: &str) -> Result<(String, Vec<String>), String> {
    let chars: Vec<char> = segment.chars().collect();
    let mut rest = String::with_capacity(segment.len());
    let mut groups = Vec::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_single {
            rest.push(c);
            if let Some(&next) = chars.get(i + 1) {
                rest.push(next);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
        } else if c == '"' && !in_single {
            in_double = !in_double;
        }
        let word_start = i == 0 || chars[i - 1].is_whitespace() || "&|;".contains(chars[i - 1]);
        if !in_single && !in_double && word_start {
            if c == '(' {
                let end = matching_paren(&chars, i + 1)
                    .ok_or_else(|| "a subshell is not closed".to_string())?;
                groups.push(chars[i + 1..end].iter().collect());
                rest.push_str("$GROUP");
                i = end + 1;
                continue;
            }
            if c == '{' && chars.get(i + 1).is_some_and(|n| n.is_whitespace()) {
                let end = matching_brace(&chars, i + 1)
                    .ok_or_else(|| "a command group is not closed".to_string())?;
                groups.push(chars[i + 1..end].iter().collect());
                rest.push_str("$GROUP");
                i = end + 1;
                continue;
            }
        }
        rest.push(c);
        i += 1;
    }
    Ok((rest, groups))
}

/// Index of the `}` closing a `{ …; }` group whose body starts at `start`.
fn matching_brace(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' if !in_single => i += 1,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '{' if !in_single && !in_double => depth += 1,
            '}' if !in_single && !in_double => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Talking to the LeanCTX daemon directly would run a call without the
/// session's rights.
fn daemon_access(command: &str) -> Option<String> {
    let mut needles: Vec<String> = [
        "daemon.sock",
        "--unix-socket",
        "/v1/tools/call",
        "UNIX-CONNECT",
        "UNIX-CLIENT",
        "UNIX-SENDTO",
        "ABSTRACT-CONNECT",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = crate::core::data_dir::resolve_data_dir() {
        dirs.push(dir);
    }
    dirs.push(crate::ipc::legacy_endpoint_dir());
    let home = dirs::home_dir();
    for dir in dirs {
        let shown = dir.to_string_lossy().to_string();
        if shown.len() > 1 {
            needles.push(shown);
        }
        if let Some(rel) = home.as_ref().and_then(|h| dir.strip_prefix(h).ok()) {
            let rel = rel.to_string_lossy();
            if !rel.is_empty() {
                needles.push(format!("~/{rel}"));
                needles.push(format!("$HOME/{rel}"));
                needles.push(format!("${{HOME}}/{rel}"));
            }
        }
    }
    needles
        .iter()
        .find(|needle| command.contains(needle.as_str()))
        .map(|needle| {
            format!("a profiled session may not reach the LeanCTX daemon or its files (`{needle}`)")
        })
}

/// Split out `$(…)`, `` `…` ``, `<(…)` and `>(…)`: their bodies are commands
/// of their own. In the returned outer text a command substitution becomes
/// `$SUBST` (an unresolved word) and a process substitution `/dev/fd/63`.
fn extract_substitutions(text: &str) -> Result<(String, Vec<String>), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut outer = String::with_capacity(text.len());
    let mut bodies = Vec::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && !in_single {
            outer.push(c);
            if let Some(&next) = chars.get(i + 1) {
                outer.push(next);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
        } else if c == '"' && !in_single {
            in_double = !in_double;
        }
        if !in_single {
            let opener = chars.get(i + 1) == Some(&'(');
            if opener && (c == '$' || ((c == '<' || c == '>') && !in_double)) {
                let end = matching_paren(&chars, i + 2)
                    .ok_or_else(|| "a substitution is not closed".to_string())?;
                bodies.push(chars[i + 2..end].iter().collect());
                outer.push_str(if c == '$' { "$SUBST" } else { "/dev/fd/63" });
                i = end + 1;
                continue;
            }
            if c == '`' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '`' {
                    if chars[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return Err("a backquote substitution is not closed".into());
                }
                bodies.push(chars[i + 1..j].iter().collect());
                outer.push_str("$SUBST");
                i = j + 1;
                continue;
            }
        }
        outer.push(c);
        i += 1;
    }
    Ok((outer, bodies))
}

/// Index of the `)` closing a group whose body starts at `start`.
fn matching_paren(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if !in_single => i += 1,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '(' if !in_single && !in_double => depth += 1,
            ')' if !in_single && !in_double => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// `piped`: the segment reads the previous one's output.
fn analyse_segment(segment: &str, walk: &mut Walk<'_>, piped: bool) {
    let mut tokens = shell_tokenize(segment);
    normalise_groups(&mut tokens);
    let (words, redirected) = redirections(&tokens, walk);
    let stdin = redirected || piped;
    let words = unwrap_command(&words, walk);
    let Some(first) = words.first() else {
        return;
    };
    let base = base_name(first).to_string();
    let args = &words[1..];
    // A shell or interpreter later in the words (`setsid bash -c …`,
    // `chrt 5 python3 -c …`) still runs inline code.
    for (at, word) in args.iter().enumerate() {
        let name = base_name(word);
        let rest = &args[at + 1..];
        // Inline code, or no script while stdin feeds it (a pipe, a heredoc).
        let inline = if SHELLS.contains(&name) {
            let (inline, script) = shell_inline(rest);
            inline || (!script && stdin)
        } else if is_interpreter(name) {
            let (inline, script, _) = interpreter_inline(name, rest);
            inline || (!script && stdin)
        } else {
            false
        };
        if inline {
            walk.refuse(
                "unanalysable",
                format!("{name} with inline code runs what this check cannot see"),
            );
            return;
        }
    }
    match base.as_str() {
        "cd" => change_dir(args.first().map(String::as_str), walk),
        b if OPAQUE_RUNNERS.contains(&b) => walk.refuse(
            "unanalysable",
            format!("{b} runs a command this check cannot follow"),
        ),
        "alias" => walk.refuse("unanalysable", "an alias changes what a later word runs"),
        "export" | "declare" | "typeset" | "readonly" | "local" => {
            for arg in args {
                command_env(arg, walk);
            }
        }
        "pushd" | "popd" => walk.refuse("unanalysable", "pushd/popd change the directory stack"),
        "eval" | "source" | "." => walk.refuse("unanalysable", format!("{base} runs text as code")),
        "xargs" | "parallel" => walk.refuse(
            "unanalysable",
            format!("{base} builds commands from its input"),
        ),
        "patch" => walk.refuse("unanalysable", "patch writes the files its input names"),
        b if SHELLS.contains(&b) => shell_rule(b, args, walk),
        b if EDITORS.contains(&b) => {
            walk.refuse("unanalysable", format!("the editor {b} can write any file"));
        }
        b if ARCHIVERS.contains(&b) => walk.refuse(
            "unanalysable",
            format!("{b} writes the files its archive holds"),
        ),
        b if is_interpreter(b) => interpreter_rule(b, args, stdin, walk),
        "awk" | "gawk" | "mawk" | "nawk" | "busybox" => awk_rule(&base, args, walk),
        "find" => find_rule(args, walk),
        "tar" | "bsdtar" | "gtar" => tar_rule(args, walk),
        "nc" | "ncat" | "netcat" if args.iter().any(|a| a == "-U" || a == "--unixsock") => walk
            .refuse(
                "daemon_socket",
                "a profiled session may not open a local socket",
            ),
        "socat" if args.iter().any(|a| a.to_ascii_uppercase().contains("UNIX")) => walk.refuse(
            "daemon_socket",
            "a profiled session may not open a local socket",
        ),
        "git" => git_rule(args, walk),
        "sed" => sed_rule(args, walk),
        _ => {
            for raw in command_targets(&base, args) {
                push_target(&raw, walk);
            }
        }
    }
}

/// Drop subshell and group markers and the keywords of compound commands.
fn normalise_groups(tokens: &mut Vec<String>) {
    const KEYWORDS: &[&str] = &[
        "if", "then", "else", "elif", "do", "while", "until", "!", "{", "(",
    ];
    loop {
        let Some(first) = tokens.first_mut() else {
            return;
        };
        if first.len() > 1 && first.starts_with('(') && !first.starts_with("((") {
            first.remove(0);
            continue;
        }
        if KEYWORDS.contains(&first.as_str()) {
            tokens.remove(0);
            continue;
        }
        // A `case` pattern (`pat)`) in front of its command.
        if first.ends_with(')') && !first.contains('(') && tokens.len() > 1 {
            tokens.remove(0);
            continue;
        }
        break;
    }
    while let Some(last) = tokens.last_mut() {
        if matches!(last.as_str(), ")" | "}" | "fi" | "done" | "esac" | ";;") {
            tokens.pop();
        } else if last.len() > 1 && last.ends_with(')') && !last.contains('(') {
            last.pop();
        } else {
            break;
        }
    }
}

/// Pull redirections out of `tokens`: output targets are recorded, input
/// redirections dropped. Returns the remaining words and whether stdin was
/// redirected (a heredoc, a here-string or a file).
fn redirections(tokens: &[String], walk: &mut Walk<'_>) -> (Vec<String>, bool) {
    let mut words = Vec::new();
    let mut stdin = false;
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token.starts_with('<')
            && !token.starts_with("<>")
            && !token.contains(char::is_whitespace)
        {
            stdin = true;
            i += if token.trim_start_matches('<').is_empty() {
                2
            } else {
                1
            };
            continue;
        }
        // A `>` inside a word with whitespace was quoted text (`awk '{print > x}'`);
        // an operator at the start still redirects (`>"my file"`).
        let leads = token.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        let operator = if token.contains(char::is_whitespace) && !leads.starts_with('>') {
            None
        } else {
            token.find('>')
        };
        let Some(at) = operator else {
            words.push(token.clone());
            i += 1;
            continue;
        };
        let before = &token[..at];
        if before.ends_with(['-', '=']) {
            words.push(token.clone());
            i += 1;
            continue;
        }
        let before = before.trim_end_matches('<');
        if !before.is_empty() && !before.chars().all(|c| c.is_ascii_digit() || c == '&') {
            words.push(before.to_string());
        }
        let rest = token[at..].trim_start_matches('>').trim_start_matches('|');
        if rest.starts_with('&') {
            // `2>&1`, `>&2`: a descriptor, not a file.
            i += 1;
            continue;
        }
        if rest.is_empty() {
            if let Some(next) = tokens.get(i + 1) {
                push_target(next, walk);
            }
            i += 2;
        } else {
            push_target(rest, walk);
            i += 1;
        }
    }
    (words, stdin)
}

fn change_dir(target: Option<&str>, walk: &mut Walk<'_>) {
    let Some(raw) = target.filter(|t| *t != "-") else {
        if let Some(home) = dirs::home_dir() {
            walk.cwds.push(home);
        }
        return;
    };
    let mut next = Vec::new();
    for cwd in &walk.cwds {
        match resolve_word(raw, cwd) {
            Some(dir) => next.push(dir),
            None => walk.cwd_unknown = true,
        }
    }
    walk.cwds.extend(next);
}

fn awk_rule(base: &str, args: &[String], walk: &mut Walk<'_>) {
    if base == "busybox" {
        walk.refuse("unanalysable", "busybox runs any applet");
        return;
    }
    if args
        .iter()
        .any(|a| a == "-i" || a.starts_with("--include") || a == "-f" || a.starts_with("--file"))
    {
        walk.refuse(
            "unanalysable",
            format!("{base} -i/-f runs a program this check cannot see"),
        );
        return;
    }
    let mut program = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-v" || a == "-F" {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            program = Some(a);
            break;
        }
        i += 1;
    }
    if program.is_some_and(|p| p.contains('>') || p.contains('|') || p.contains("system")) {
        walk.refuse(
            "unanalysable",
            format!("the {base} program writes or runs commands"),
        );
    }
}

fn find_rule(args: &[String], walk: &mut Walk<'_>) {
    const ACTIONS: &[&str] = &[
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
    ];
    if let Some(action) = args.iter().find(|a| ACTIONS.contains(&a.as_str())) {
        walk.refuse(
            "unanalysable",
            format!("find {action} acts on every file it finds"),
        );
    }
}

fn tar_rule(args: &[String], walk: &mut Walk<'_>) {
    let first = args.first().map(String::as_str).unwrap_or("");
    let old_style = !first.starts_with('-') && !first.is_empty();
    let clusters: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| a.starts_with('-') && !a.starts_with("--"))
        .map(|a| &a[1..])
        .chain(old_style.then_some(first))
        .collect();
    let extract = clusters.iter().any(|c| c.contains('x'))
        || args.iter().any(|a| a == "--extract" || a == "--get");
    if extract {
        walk.refuse(
            "unanalysable",
            "tar extraction writes the files its archive holds",
        );
        return;
    }
    // The archive a create or append writes.
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(file) = a.strip_prefix("--file=") {
            push_target(file, walk);
        } else if a == "--file"
            || (a.starts_with('-') && !a.starts_with("--") && a.ends_with('f'))
            || (i == 0 && old_style && a.ends_with('f'))
        {
            if let Some(file) = args.get(i + 1) {
                if file != "-" {
                    push_target(file, walk);
                }
            }
            i += 1;
        }
        i += 1;
    }
}

/// `sed -i` writes its files; a script that writes (`w`, `W`, `s///w`) or
/// runs commands (`e`, `s///e`) is refused.
fn sed_rule(args: &[String], walk: &mut Walk<'_>) {
    let mut scripts = Vec::new();
    let mut files = Vec::new();
    let mut in_place = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-e" || a == "--expression" {
            if let Some(s) = args.get(i + 1) {
                scripts.push(s.clone());
            }
            i += 2;
            continue;
        }
        if let Some(s) = a.strip_prefix("--expression=") {
            scripts.push(s.to_string());
        } else if a == "-f" || a == "--file" || a.starts_with("--file=") {
            walk.refuse("unanalysable", "sed -f runs a script this check cannot see");
            return;
        } else if a == "--in-place" || a.starts_with("--in-place=") {
            in_place = true;
        } else if a.starts_with('-') && !a.starts_with("--") && a.len() > 1 {
            if a[1..].contains('i') {
                in_place = true;
            }
            if a[1..].contains('e') && !a.starts_with("-i") {
                if let Some(s) = args.get(i + 1) {
                    scripts.push(s.clone());
                }
                i += 2;
                continue;
            }
        } else if !a.starts_with('-') {
            files.push(a.clone());
        }
        i += 1;
    }
    if scripts.is_empty() && !files.is_empty() {
        scripts.push(files.remove(0));
    }
    if scripts.iter().any(|s| sed_script_writes(s)) {
        walk.refuse(
            "unanalysable",
            "the sed script writes files or runs commands",
        );
        return;
    }
    if in_place {
        for file in files {
            push_target(&file, walk);
        }
    }
}

/// Whether a sed script holds a `w`/`W`/`e` command or an `s` flag `w`/`e`.
/// A script this scanner cannot follow counts as writing.
fn sed_script_writes(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    let mut at_command = true;
    while i < chars.len() {
        let c = chars[i];
        if at_command {
            match c {
                ' ' | '\t' | ';' | '\n' | '{' | '}' | '!' | ',' | '$' | '0'..='9' | '~' | '+' => {}
                '/' | '\\' => {
                    // An address regex: skip to its closing delimiter.
                    let delim = if c == '\\' {
                        i += 1;
                        match chars.get(i) {
                            Some(&d) => d,
                            None => return true,
                        }
                    } else {
                        '/'
                    };
                    match skip_delimited(&chars, i + 1, delim) {
                        Some(end) => i = end,
                        None => return true,
                    }
                }
                's' | 'y' => {
                    let Some(&delim) = chars.get(i + 1) else {
                        return true;
                    };
                    let Some(mid) = skip_delimited(&chars, i + 2, delim) else {
                        return true;
                    };
                    let Some(end) = skip_delimited(&chars, mid + 1, delim) else {
                        return true;
                    };
                    i = end + 1;
                    let flags_start = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric()) {
                        i += 1;
                    }
                    let flags: String = chars[flags_start..i].iter().collect();
                    if c == 's' && (flags.contains('w') || flags.contains('e')) {
                        return true;
                    }
                    at_command = false;
                    continue;
                }
                'w' | 'W' | 'e' => return true,
                'a' | 'i' | 'c' | 'r' | 'R' | 'b' | 't' | 'T' | ':' => {
                    // Text or a label up to the end of the line.
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                _ => at_command = false,
            }
        } else if matches!(c, ';' | '\n' | '}') {
            at_command = true;
        }
        i += 1;
    }
    false
}

/// Index of the unescaped `delim` at or after `start`.
fn skip_delimited(chars: &[char], start: usize, delim: char) -> Option<usize> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == delim {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn positional(args: &[String], takes_value: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut after_dashdash = false;
    while i < args.len() {
        let a = &args[i];
        if after_dashdash {
            out.push(a.clone());
        } else if a == "--" {
            after_dashdash = true;
        } else if a.starts_with('-') && a.len() > 1 {
            if takes_value.contains(&a.as_str()) {
                i += 1;
            }
        } else {
            out.push(a.clone());
        }
        i += 1;
    }
    out
}

fn command_targets(base: &str, args: &[String]) -> Vec<String> {
    match base {
        "tee" | "rm" | "rmdir" | "unlink" | "touch" | "mkdir" | "shred" | "mv" => {
            let mut out = positional(args, &["-t", "-S", "--target-directory"]);
            if let Some(i) = args
                .iter()
                .position(|a| a == "-t" || a == "--target-directory")
            {
                out.extend(args.get(i + 1).cloned());
            }
            out
        }
        // Hard and symbolic links: the source is judged too, so no new name
        // can lead to a locked file.
        "ln" | "link" => {
            let mut out = positional(args, &["-t", "-S", "--target-directory", "--suffix"]);
            // A relative symlink source is read from the link's directory.
            let symbolic = args.iter().any(|a| {
                a == "--symbolic" || (a.starts_with('-') && !a.starts_with("--") && a.contains('s'))
            });
            if symbolic && out.len() >= 2 {
                let link = out.last().cloned().unwrap_or_default();
                let dir = Path::new(&link)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let sources: Vec<String> = out[..out.len() - 1]
                    .iter()
                    .filter(|s| !s.starts_with('/'))
                    .map(|s| dir.join(s).to_string_lossy().into_owned())
                    .collect();
                out.extend(sources);
            }
            out
        }
        "truncate" => positional(args, &["-s", "-r", "--size", "--reference"]),
        "chmod" | "chown" | "chgrp" => positional(args, &["--reference"])
            .into_iter()
            .skip(1)
            .collect(),
        "cp" | "install" | "rsync" | "scp" => {
            if let Some(i) = args
                .iter()
                .position(|a| a == "-t" || a == "--target-directory")
            {
                return args.get(i + 1).cloned().into_iter().collect();
            }
            let pos = positional(
                args,
                &["-m", "-o", "-g", "-S", "-e", "--mode", "--owner", "--group"],
            );
            // `cp -l`/`-s` make links: the sources are judged like `ln`'s.
            let links = base == "cp"
                && args.iter().any(|a| {
                    matches!(a.as_str(), "--link" | "--symbolic-link")
                        || (a.starts_with('-')
                            && !a.starts_with("--")
                            && (a.contains('l') || a.contains('s')))
                });
            if links {
                return pos;
            }
            if pos.len() >= 2 {
                pos.last().cloned().into_iter().collect()
            } else {
                Vec::new()
            }
        }
        "dd" => args
            .iter()
            .filter_map(|a| a.strip_prefix("of=").map(str::to_string))
            .collect(),
        "sort" => {
            let mut out = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if a == "-o" || a == "--output" {
                    out.extend(args.get(i + 1).cloned());
                } else if let Some(file) = a.strip_prefix("--output=") {
                    out.push(file.to_string());
                } else if let Some(file) = a.strip_prefix("-o").filter(|f| !f.is_empty()) {
                    out.push(file.to_string());
                }
            }
            out
        }
        "uniq" => positional(args, &["-f", "-s", "-w"])
            .into_iter()
            .skip(1)
            .take(1)
            .collect(),
        "curl" => {
            let mut out = Vec::new();
            for (i, a) in args.iter().enumerate() {
                match a.as_str() {
                    "-o" | "--output" | "-D" | "--dump-header" | "-c" | "--cookie-jar" => {
                        out.extend(args.get(i + 1).cloned());
                    }
                    "-O"
                    | "--remote-name"
                    | "--remote-name-all"
                    | "-J"
                    | "--remote-header-name" => {
                        out.push("$REMOTE_NAME".to_string());
                    }
                    _ => {}
                }
            }
            out
        }
        "wget" => {
            let mut named = false;
            let mut out = Vec::new();
            for (i, a) in args.iter().enumerate() {
                if a == "-O" || a == "--output-document" || a == "-o" || a == "--output-file" {
                    named |= a == "-O" || a == "--output-document";
                    out.extend(args.get(i + 1).cloned().filter(|f| f != "-"));
                } else if let Some(f) = a.strip_prefix("--output-document=") {
                    named = true;
                    if f != "-" {
                        out.push(f.to_string());
                    }
                } else if a == "-P" || a == "--directory-prefix" {
                    out.extend(args.get(i + 1).cloned());
                }
            }
            if !named {
                out.push("$REMOTE_NAME".to_string());
            }
            out
        }
        b if COMPRESSORS.contains(&b) => {
            if args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-c" | "--stdout" | "-l" | "--list" | "-t" | "--test"
                )
            }) {
                return Vec::new();
            }
            let mut out = Vec::new();
            for file in positional(args, &["-S", "--suffix", "-o"]) {
                if let Some((stem, _)) = file.rsplit_once('.') {
                    out.push(stem.to_string());
                }
                out.push(format!("{file}.gz"));
                out.push(file);
            }
            out
        }
        "split" | "csplit" => positional(args, &["-l", "-b", "-n", "-a", "-C", "-d", "-f"])
            .into_iter()
            .skip(1)
            .collect(),
        _ => Vec::new(),
    }
}

/// A word as a path, or `None` when only the shell could tell (a variable, a
/// substitution, another user's home).
fn resolve_word(raw: &str, cwd: &Path) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains(['$', '`']) {
        return None;
    }
    let expanded = if raw == "~" || raw.starts_with("~/") {
        dirs::home_dir()?.join(raw.trim_start_matches('~').trim_start_matches('/'))
    } else if raw.starts_with('~') {
        return None;
    } else {
        PathBuf::from(raw)
    };
    Some(if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    })
}

fn push_target(raw: &str, walk: &mut Walk<'_>) {
    if SINKS.contains(&raw) || raw.starts_with("/dev/fd/") || raw.starts_with("/proc/self/fd/") {
        return;
    }
    if raw.contains('{') && raw.contains('}') {
        walk.refuse(
            "unanalysable",
            format!("brace expansion in `{raw}` names paths this check cannot list"),
        );
        return;
    }
    let relative = !raw.starts_with('/') && !raw.starts_with('~');
    if raw.is_empty() || raw.contains(['$', '`']) || (relative && walk.cwd_unknown) {
        walk.refuse(
            "bash_unresolved_target",
            format!("cannot tell where `{raw}` points; write it with a literal path"),
        );
        return;
    }
    let cwds = if relative {
        walk.cwds.clone()
    } else {
        vec![PathBuf::from("/")]
    };
    for cwd in cwds {
        let Some(path) = resolve_word(raw, &cwd) else {
            walk.refuse(
                "bash_unresolved_target",
                format!("cannot tell where `{raw}` points; write it with a literal path"),
            );
            return;
        };
        if raw.contains(['*', '?', '[']) {
            // The pattern stands for its directory and every file it matches now.
            if let Some(parent) = path.parent() {
                walk.out.push(Finding::Write(parent.to_path_buf()));
            }
            if let Ok(paths) = glob::glob(&path.to_string_lossy()) {
                walk.out.extend(paths.flatten().map(Finding::Write));
            }
        } else {
            walk.out.push(Finding::Write(path));
        }
    }
}

/// The files an `apply_patch` call names (`*** Add File:`, `*** Update File:`,
/// `*** Delete File:`, `*** Move to:`), from any string in its input.
pub fn patch_targets(input: &Value) -> Vec<String> {
    const MARKERS: &[&str] = &[
        "*** Add File:",
        "*** Update File:",
        "*** Delete File:",
        "*** Move to:",
    ];
    let mut texts = Vec::new();
    collect_strings(input, &mut texts);
    let mut out = Vec::new();
    for text in texts {
        for line in text.lines() {
            let line = line.trim_start();
            for marker in MARKERS {
                if let Some(path) = line.strip_prefix(marker) {
                    let path = path.trim();
                    if !path.is_empty() {
                        out.push(path.to_string());
                    }
                }
            }
        }
    }
    out
}

fn collect_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// A command given as an argument vector, as one command line. `[sh, -c,
/// script]` (and `-lc`) is the script itself.
pub fn argv_command(argv: &[Value]) -> Option<String> {
    let words: Vec<&str> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
    if let [shell, flag, script] = words.as_slice()
        && SHELLS.contains(&base_name(shell))
        && matches!(*flag, "-c" | "-lc")
    {
        return Some((*script).to_string());
    }
    Some(
        words
            .iter()
            .map(|w| quote_word(w))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// `word` as one shell word.
fn quote_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}
