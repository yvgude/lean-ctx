// SPDX-License-Identifier: Apache-2.0
//! `tool_output` classification of shell commands (pz21 §1.8, Q6 a).
//!
//! A command whose output comes from outside the repository (an issue
//! tracker CLI, a network fetch, a package registry query, a foreign ref)
//! delivers untrusted text. The table below names those commands by head;
//! wrapped and compound forms are unwrapped first (`sh -c`, `env`,
//! `timeout`, `nice`, `sudo`, `xargs`, and the parts of `;`, `&&`, `||` and
//! `|`). Any matching part makes the whole output `tool_output`. A command
//! that cannot be parsed (unbalanced quotes, `eval`) fails closed: it is
//! `tool_output` too. The project's own build and test commands never match.
//!
//! The reference is `cmd:<blake3(command)[..8]>`: deterministic and
//! content-free. A download (`curl -o x`, `wget -O x`, a matching command
//! redirected into a file) also names the written path, so a later read of
//! exactly that file in the same session is fenced (`file:<path>`).

use crate::core::context_admission::origin::{InputKind, InputOrigin, SourceClass};

/// What the table found for one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputMatch {
    /// `cmd:<blake3[..8]>`.
    pub origin: InputOrigin,
    /// Paths the command writes downloaded text to, as written in the
    /// command (relative to its working directory).
    pub downloads: Vec<String>,
}

impl ToolOutputMatch {
    /// The class the command's output is admitted with.
    pub fn class(&self) -> SourceClass {
        SourceClass::External(self.origin.clone())
    }
}

/// The content-free reference of a command.
pub fn command_ref(command: &str) -> String {
    let hash = blake3::hash(command.as_bytes()).to_hex();
    format!("cmd:{}", &hash[..8])
}

/// Classify `command`. `None`: the output is the project's own.
pub fn classify(command: &str) -> Option<ToolOutputMatch> {
    let mut downloads = Vec::new();
    let matched = match scan(command, 0, &mut downloads) {
        Scan::Match => true,
        Scan::Clean => false,
    };
    matched.then(|| ToolOutputMatch {
        origin: InputOrigin::new(InputKind::ToolOutput, command_ref(command)),
        downloads,
    })
}

#[derive(PartialEq, Eq)]
enum Scan {
    Match,
    Clean,
}

const MAX_DEPTH: usize = 8;

fn scan(command: &str, depth: usize, downloads: &mut Vec<String>) -> Scan {
    if depth > MAX_DEPTH {
        return Scan::Match;
    }
    // Command substitutions run first; a matching inner command matches.
    for inner in substitutions(command) {
        if scan(&inner, depth + 1, downloads) == Scan::Match {
            return Scan::Match;
        }
    }
    let Some(parts) = split_parts(command) else {
        return Scan::Match;
    };
    let mut matched = false;
    for words in parts {
        if part_matches(&words, depth, downloads) {
            matched = true;
        }
    }
    if matched { Scan::Match } else { Scan::Clean }
}

/// The parts of a compound command as word lists, with redirections kept as
/// words (`>`, `>>`). `None`: not parsable (unbalanced quotes).
fn split_parts(command: &str) -> Option<Vec<Vec<String>>> {
    let mut parts = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    let flush = |word: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return None,
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(c) = chars.next() {
                                word.push(c);
                            }
                        }
                        Some(c) => word.push(c),
                        None => return None,
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    word.push(c);
                }
            }
            ';' | '&' | '|' | '\n' => {
                flush(&mut word, &mut in_word, &mut words);
                if matches!(c, '&' | '|') && chars.peek() == Some(&c) {
                    chars.next();
                }
                if !words.is_empty() {
                    parts.push(std::mem::take(&mut words));
                }
            }
            '>' | '<' => {
                flush(&mut word, &mut in_word, &mut words);
                let mut op = c.to_string();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    op.push('>');
                }
                words.push(op);
            }
            c if c.is_whitespace() => flush(&mut word, &mut in_word, &mut words),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut word, &mut in_word, &mut words);
    if !words.is_empty() {
        parts.push(words);
    }
    Some(parts)
}

/// Inner commands of `$(…)`, `<(…)`, `>(…)` and backticks.
fn substitutions(command: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // `$(…)`, and process substitution `<(…)` / `>(…)`.
        if matches!(bytes[i], b'$' | b'<' | b'>') && bytes.get(i + 1) == Some(&b'(') {
            let start = i + 2;
            let mut depth = 1usize;
            let mut j = start;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let end = if depth == 0 { j - 1 } else { bytes.len() };
            found.push(command[start..end].to_owned());
            i = j;
        } else if bytes[i] == b'`' {
            let start = i + 1;
            let end = command[start..]
                .find('`')
                .map_or(bytes.len(), |offset| start + offset);
            found.push(command[start..end].to_owned());
            i = end + 1;
        } else {
            i += 1;
        }
    }
    found
}

fn base(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn part_matches(words: &[String], depth: usize, downloads: &mut Vec<String>) -> bool {
    let (words, redirect) = split_redirect(words);
    let words = unwrap(words);
    let Some(head) = words.first() else {
        return false;
    };
    let head = base(head);
    let arg = |n: usize| words.get(n).map(String::as_str).unwrap_or_default();
    let matched = match head {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => {
            // `-c` alone or bundled with other short options (`-lc`, `-ec`).
            match words.iter().skip(1).position(|word| is_command_flag(word)) {
                Some(at) => words
                    .iter()
                    .skip(at + 2)
                    .find(|word| !word.starts_with(['-', '+']))
                    .is_some_and(|script| scan(script, depth + 1, downloads) == Scan::Match),
                // A script file: not unwrapped (named residual, §3).
                None => false,
            }
        }
        // `eval` runs text the table cannot read: fail closed. Forge and
        // HTTP clients, remote shells, raw sockets and multi-protocol
        // downloaders: their output is the remote side's.
        "eval" | "gh" | "glab" | "hub" | "jira" | "linear" | "http" | "https" | "xh" | "ssh"
        | "nc" | "ncat" | "netcat" | "socat" | "telnet" | "aria2c" | "sftp" | "ftp" => true,
        // A copy from a remote host (`host:path`, `rsync://`): the local
        // destination holds downloaded text.
        "scp" | "rsync" => {
            let operands: Vec<&String> = words
                .iter()
                .skip(1)
                .filter(|word| !word.starts_with('-'))
                .collect();
            let remote = operands.iter().any(|word| is_remote_operand(word));
            if remote
                && let Some(target) = operands.last()
                && !is_remote_operand(target)
            {
                downloads.push((*target).clone());
            }
            remote
        }
        "curl" => {
            downloads.extend(curl_outputs(words));
            true
        }
        "wget" => {
            downloads.extend(wget_outputs(words));
            true
        }
        "npm" => matches!(arg(1), "view" | "info" | "show" | "v"),
        "pnpm" => matches!(arg(1), "view" | "info"),
        "yarn" => matches!(arg(1), "info" | "npm"),
        "cargo" => matches!(arg(1), "search" | "info"),
        "pip" | "pip3" => matches!(arg(1), "download" | "index"),
        "git" => git_foreign(words),
        "python" | "python3" | "node" | "ruby" | "perl" | "deno" | "bun" => words
            .iter()
            .skip(1)
            .any(|word| word.contains("http://") || word.contains("https://")),
        _ => false,
    };
    if matched && let Some(target) = redirect {
        downloads.push(target);
    }
    matched
}

/// Words without a trailing output redirection, and its target.
fn split_redirect(words: &[String]) -> (&[String], Option<String>) {
    match words.iter().position(|word| word == ">" || word == ">>") {
        Some(at) => (&words[..at], words.get(at + 1).cloned()),
        None => (words, None),
    }
}

/// Strip leading assignments and wrapper commands.
fn unwrap(mut words: &[String]) -> &[String] {
    loop {
        let Some(head) = words.first() else {
            return words;
        };
        if is_assignment(head) {
            words = &words[1..];
            continue;
        }
        let skip = match base(head) {
            "env" => {
                1 + words[1..]
                    .iter()
                    .take_while(|word| word.starts_with('-') || is_assignment(word))
                    .count()
            }
            "timeout" => {
                // `timeout [options] DURATION command`; `-s`/`-k` take a value.
                let mut n = 1;
                while let Some(word) = words.get(n) {
                    if matches!(word.as_str(), "-s" | "--signal" | "-k" | "--kill-after") {
                        n += 2;
                    } else if word.starts_with('-') {
                        n += 1;
                    } else {
                        break;
                    }
                }
                n + 1
            }
            "nice" => {
                let mut n = 1;
                while let Some(word) = words.get(n) {
                    if word == "-n" {
                        n += 2;
                    } else if word.starts_with('-') {
                        n += 1;
                    } else {
                        break;
                    }
                }
                n
            }
            wrapper @ ("sudo" | "xargs" | "nohup" | "command" | "exec" | "time" | "stdbuf"
            | "doas") => {
                // Options, and the separate value of those that take one
                // (`sudo -u bob`, `xargs -n 1`): never the command head.
                let takes_value = option_values(wrapper);
                let mut n = 1;
                while let Some(word) = words.get(n) {
                    if word == "--" {
                        n += 1;
                        break;
                    } else if takes_value.contains(&word.as_str()) {
                        n += 2;
                    } else if word.starts_with('-') {
                        n += 1;
                    } else {
                        break;
                    }
                }
                n
            }
            _ => return words,
        };
        words = words.get(skip..).unwrap_or_default();
    }
}

/// The options of a wrapper command that take their value as the next word.
fn option_values(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "sudo" => &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-r",
            "-t",
            "-T",
            "-U",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--close-from",
            "--chdir",
            "--role",
            "--type",
            "--command-timeout",
            "--other-user",
        ],
        "xargs" => &[
            "-n",
            "-L",
            "-P",
            "-s",
            "-d",
            "-E",
            "-I",
            "-a",
            "--max-args",
            "--max-lines",
            "--max-procs",
            "--max-chars",
            "--delimiter",
            "--arg-file",
            "--process-slot-var",
        ],
        "doas" => &["-u", "-C"],
        "exec" => &["-a"],
        "time" => &["-f", "-o", "--format", "--output"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        _ => &[],
    }
}

/// `-c` alone or in a bundle of short options (`-lc`, `-ec`, `-xc`).
fn is_command_flag(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|flags| !flags.starts_with('-') && flags.contains('c'))
}

/// An scp/rsync operand on another host: `host:path`, `user@host:path`,
/// `host::module`, `rsync://…`. A local path with a slash before the first
/// colon (`./a:b`) is local, as scp reads it.
fn is_remote_operand(word: &str) -> bool {
    if word.contains("://") {
        return true;
    }
    word.find(':')
        .is_some_and(|at| at > 0 && !word[..at].contains('/'))
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn git_foreign(words: &[String]) -> bool {
    let sub = words.get(1).map(String::as_str).unwrap_or_default();
    if !matches!(sub, "log" | "show" | "diff") {
        return false;
    }
    // Every side of a range (`a..b`, `a...b`) is a ref.
    words
        .iter()
        .skip(2)
        .flat_map(|word| word.split(".."))
        .map(|word| word.trim_start_matches('.'))
        .any(|word| {
            word.contains("FETCH_HEAD")
                || (word.starts_with("remotes/") && !word.starts_with("remotes/origin/"))
                || (word.starts_with("refs/remotes/") && !word.starts_with("refs/remotes/origin/"))
        })
}

fn curl_outputs(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut remote_name = false;
    let mut iter = words.iter().skip(1).peekable();
    let mut urls = Vec::new();
    while let Some(word) = iter.next() {
        match word.as_str() {
            "-o" | "--output" => out.extend(iter.next().cloned()),
            "-O" | "--remote-name" => remote_name = true,
            w if w.starts_with("--output=") => out.push(w["--output=".len()..].to_owned()),
            w if w.starts_with("-o") && w.len() > 2 && !w.starts_with("--") => {
                out.push(w[2..].to_owned());
            }
            w if w.contains("://") => urls.push(w.to_owned()),
            _ => {}
        }
    }
    if remote_name {
        out.extend(urls.iter().filter_map(|url| url_file_name(url)));
    }
    out
}

fn wget_outputs(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut named = false;
    let mut iter = words.iter().skip(1);
    let mut urls = Vec::new();
    while let Some(word) = iter.next() {
        match word.as_str() {
            "-O" | "--output-document" => {
                named = true;
                out.extend(iter.next().cloned().filter(|target| target != "-"));
            }
            w if w.starts_with("--output-document=") => {
                named = true;
                out.push(w["--output-document=".len()..].to_owned());
            }
            w if w.contains("://") => urls.push(w.to_owned()),
            _ => {}
        }
    }
    if !named {
        out.extend(urls.iter().filter_map(|url| url_file_name(url)));
    }
    out
}

fn url_file_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let (_, rest) = path.split_once("://")?;
    let (_, path) = rest.split_once('/')?;
    let name = path.rsplit('/').next()?;
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(command: &str) -> bool {
        classify(command).is_some()
    }

    #[test]
    fn the_table_and_its_wrapped_forms_match() {
        for command in [
            "gh issue view 42",
            "sh -c \"gh issue view 42\"",
            "bash -c 'glab mr view 7'",
            "cargo build && gh pr view 3",
            "env X=1 gh issue list",
            "X=1 gh issue list",
            "timeout 5 curl https://example.com",
            "timeout -s KILL 5 curl https://example.com",
            "nice -n 5 wget https://example.com/a",
            "sudo -E xh example.com",
            "cat list | xargs -n1 curl",
            "npm view left-pad",
            "cargo search serde",
            "pip download requests",
            "git log FETCH_HEAD",
            "git diff main..remotes/upstream/main",
            "python3 -c 'import urllib; urllib.request.urlopen(\"https://x\")'",
            "echo $(gh issue view 1)",
            "echo `curl -s https://x`",
            "echo \"unbalanced",
            "eval \"$CMD\"",
        ] {
            assert!(matched(command), "{command}");
        }
    }

    #[test]
    fn the_projects_own_commands_do_not_match() {
        for command in [
            "cargo test",
            "cargo build --release",
            "git log --oneline -5",
            "git diff HEAD~1",
            "npm test",
            "python3 -c 'print(1)'",
            "ls -la && echo gh",
            "grep -rn curl src/",
        ] {
            assert!(!matched(command), "{command}");
        }
    }

    /// Review 22480 finding 4: each shape read as the project's own output.
    #[test]
    fn bundled_shell_flags_are_unwrapped() {
        assert!(matched("bash -lc 'gh issue view 1'"));
        assert!(matched("sh -ec 'curl -s https://x.test'"));
        assert!(matched("zsh -o pipefail -c 'gh pr view 2'"));
        assert!(!matched("bash -lc 'cargo test'"));
    }

    #[test]
    fn wrapper_option_values_are_not_the_command_head() {
        assert!(matched("sudo -u bob gh issue view 1"));
        assert!(matched("sudo --user bob curl https://x.test"));
        assert!(matched("cat urls | xargs -n 1 curl"));
        assert!(matched("xargs -I {} -P 4 wget {}"));
        assert!(matched("doas -u bob gh issue list"));
        assert!(!matched("sudo -u bob cargo build"));
    }

    #[test]
    fn process_substitution_is_scanned() {
        assert!(matched("diff <(curl -s https://x.test/a) local.txt"));
        assert!(matched("tee >(gh issue comment 1 -F -) < notes.md"));
        assert!(!matched("diff <(sort a.txt) b.txt"));
    }

    #[test]
    fn remote_copies_and_sockets_match() {
        for command in [
            "scp host:/etc/motd motd.txt",
            "rsync -av user@host:src/ dst/",
            "rsync rsync://mirror.test/pub/x .",
            "ssh host cat /var/log/app.log",
            "aria2c https://x.test/file.bin",
            "nc example.test 80",
        ] {
            assert!(matched(command), "{command}");
        }
        assert!(!matched("rsync -a src/ dst/"));
        assert!(!matched("scp ./a.txt ./b.txt"));
        let found = classify("scp host:/etc/motd motd.txt").unwrap();
        assert_eq!(found.downloads, vec!["motd.txt".to_owned()]);
    }

    #[test]
    fn downloads_name_the_written_path() {
        let found = classify("curl -o x.md https://example.com/issue").unwrap();
        assert_eq!(found.downloads, vec!["x.md".to_owned()]);
        let found = classify("wget https://example.com/a/page.html").unwrap();
        assert_eq!(found.downloads, vec!["page.html".to_owned()]);
        let found = classify("gh issue view 1 > issue.txt").unwrap();
        assert_eq!(found.downloads, vec!["issue.txt".to_owned()]);
        assert!(found.origin.r#ref.starts_with("cmd:"));
        assert_eq!(found.origin.r#ref.len(), 12);
        assert_eq!(classify("gh issue view 1 > issue.txt"), Some(found));
    }
}
