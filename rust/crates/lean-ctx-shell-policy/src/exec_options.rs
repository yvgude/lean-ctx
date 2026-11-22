// SPDX-License-Identifier: Apache-2.0
//! Options of file and search tools that run a command their value names:
//! `sed`'s `e` command and the `e` flag of `s`, `tar`'s compression program,
//! checkpoint action, volume script and remote shell, `rg --pre` and
//! `--hostname-bin`, and the editor commands of vim (`-c`, `+cmd`, `--cmd`)
//! and emacs (`--eval`, `--funcall`, `--load`).
//!
//! Such a value is a command line of its own. The caller judges it like one
//! (see `enforcement::check_exec_options`): with an allowlist it must pass the
//! allowlist and every other per-command rule; without one it is refused,
//! like `tar --to-command`. A command the line does not show (a sed script
//! file, the pattern space `s///e` or a bare `e` runs) is refused in every
//! mode.

/// One command-running option found on a command line.
pub(super) struct ExecOption {
    /// The program word as the command names it (`tar`, `/usr/bin/sed`).
    pub(super) program: String,
    /// The option as the refusal names it (`-I`, `--pre`, `e command`).
    pub(super) option: String,
    /// The command line the option runs; `None` when the command line does
    /// not show it.
    pub(super) command: Option<String>,
}

/// The command-running options of `tokens` (starting at the program word).
pub(super) fn exec_options(tokens: &[String]) -> Vec<ExecOption> {
    let Some(program) = tokens.first() else {
        return Vec::new();
    };
    let found = match super::tokenizer::exec_name(program).as_str() {
        "sed" | "gsed" => sed_options(&tokens[1..]),
        "tar" | "gtar" | "bsdtar" => tar_options(&tokens[1..]),
        "rg" => rg_options(&tokens[1..]),
        "vi" | "vim" | "nvim" | "view" | "vimdiff" | "gvim" | "gview" | "gvimdiff" | "rvim"
        | "rview" | "mvim" | "ex" => vim_options(program, &tokens[1..]),
        "emacs" | "emacsclient" => emacs_options(&tokens[1..]),
        _ => return Vec::new(),
    };
    found
        .into_iter()
        .map(|(option, command)| ExecOption {
            program: program.clone(),
            option,
            command,
        })
        .collect()
}

type Found = Vec<(String, Option<String>)>;

// -------------------------------------------------------------- words ----

/// The words of `segment` as the shell splits them, keeping an empty quoted
/// word (`''`, `""`), which the engine's tokenizer drops. Otherwise the same
/// rules as `shell_tokenize`.
fn words_keeping_empty(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = segment.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut parameter_depth: u32 = 0;
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                quoted = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                quoted = true;
            }
            '\\' if !in_single => {
                let escapes_next =
                    !in_double || matches!(chars.peek(), Some('$' | '`' | '"' | '\\' | '\n'));
                if escapes_next {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push('\\');
                }
            }
            '$' if !in_single && chars.peek() == Some(&'{') => {
                parameter_depth += 1;
                current.push(c);
            }
            '}' if !in_single && parameter_depth > 0 => {
                parameter_depth -= 1;
                current.push(c);
            }
            c if c.is_whitespace() && !in_single && !in_double && parameter_depth == 0 => {
                if !current.is_empty() || quoted {
                    words.push(std::mem::take(&mut current));
                }
                quoted = false;
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() || quoted {
        words.push(current);
    }
    words
}

/// `tokens` (the caller's words of `segment`, starting at the program word)
/// with the empty quoted words of `segment` put back where they stand: an
/// empty word is an option value (`rg -e '' --pre CMD`), and without it every
/// later value shifts by one. The words are matched from the end, so a
/// segment with a wrapper or assignments in front still lines up. When they
/// do not line up, `tokens` as given.
pub(super) fn exec_words(segment: &str, tokens: &[String]) -> Vec<String> {
    let Some((program, args)) = tokens.split_first() else {
        return Vec::new();
    };
    let words = words_keeping_empty(segment);
    if !words.iter().any(String::is_empty) {
        return tokens.to_vec();
    }
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut left = args.len();
    for word in words.iter().rev() {
        if word.is_empty() {
            out.push(String::new());
        } else if left == 0 {
            // The program word (or what stands before it).
            break;
        } else if *word == args[left - 1] {
            out.push(word.clone());
            left -= 1;
        } else {
            return tokens.to_vec();
        }
    }
    if left != 0 {
        return tokens.to_vec();
    }
    out.push(program.clone());
    out.reverse();
    out
}

/// `words` as one command line again, an empty word as `''`.
pub(super) fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| if w.is_empty() { "''" } else { w.as_str() })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- sed ----

/// GNU sed's long options; an unambiguous prefix selects one.
const SED_LONG_OPTIONS: &[&str] = &[
    "binary",
    "debug",
    "expression",
    "file",
    "follow-symlinks",
    "help",
    "in-place",
    "line-length",
    "null-data",
    "posix",
    "quiet",
    "regexp-extended",
    "sandbox",
    "separate",
    "silent",
    "unbuffered",
    "version",
    "zero-terminated",
];

/// The long option `name` (without `--`) selects, as getopt resolves it.
fn long_option<'a>(name: &str, options: &[&'a str]) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    if let Some(exact) = options.iter().find(|o| **o == name) {
        return Some(exact);
    }
    let mut matching = options.iter().filter(|o| o.starts_with(name));
    match (matching.next(), matching.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// `sed` arguments: the script from every `-e`/`--expression`, else the
/// first operand; a script file (`-f`/`--file`) is refused unread. GNU sed
/// permutes its arguments, so options count wherever they stand. With
/// `--sandbox` sed itself refuses `e`, `r` and `w`.
fn sed_options(args: &[String]) -> Found {
    let (mut found, script, sandbox) = sed_arguments(args);
    if sandbox {
        return Found::new();
    }
    found.extend(sed_script_commands(&script));
    found
}

/// The files the `w`/`W` commands and `s///w` of a `sed` command line
/// (`tokens` from the program word) write; none with `--sandbox`, where sed
/// refuses them.
pub(super) fn sed_written_files(tokens: &[String]) -> Vec<String> {
    let (_, script, sandbox) = sed_arguments(tokens.get(1..).unwrap_or_default());
    if sandbox {
        return Vec::new();
    }
    sed_script_files(&script)
        .1
        .into_iter()
        .filter_map(|(file, writes)| writes.then_some(file))
        .collect()
}

/// `sed` arguments: the script-file options found, the script (every `-e`
/// joined by line breaks, else the first operand) and `--sandbox`.
fn sed_arguments(args: &[String]) -> (Found, String, bool) {
    let mut scripts: Vec<String> = Vec::new();
    let mut found = Found::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut sandbox = false;
    // A bare `-i` (BSD: its suffix is the next word, `-i ''`, `-i .bak`).
    let mut bare_in_place = false;
    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        if word == "--" {
            operands.extend(args[i + 1..].iter().map(String::as_str));
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let takes_value = matches!(
                long_option(name, SED_LONG_OPTIONS),
                Some("expression" | "file" | "line-length")
            );
            let value = if takes_value && attached.is_none() {
                i += 1;
                args.get(i).cloned()
            } else {
                attached
            };
            match long_option(name, SED_LONG_OPTIONS) {
                Some("expression") => scripts.extend(value),
                Some("file") => found.push((format!("--{name}"), None)),
                Some("sandbox") => sandbox = true,
                _ => {}
            }
        } else if word.len() > 1 && word.starts_with('-') {
            for (at, letter) in word[1..].char_indices() {
                match letter {
                    'e' | 'f' | 'l' => {
                        let rest = &word[1 + at + letter.len_utf8()..];
                        let value = if rest.is_empty() {
                            i += 1;
                            args.get(i).cloned()
                        } else {
                            Some(rest.to_string())
                        };
                        match letter {
                            'e' => scripts.extend(value),
                            'f' => found.push(("-f".to_string(), None)),
                            _ => {}
                        }
                        break;
                    }
                    // GNU `-i[SUFFIX]`: the rest of the word is the suffix;
                    // BSD `-i SUFFIX` / `-I SUFFIX`: the next word is.
                    'i' | 'I' => {
                        bare_in_place |= at + 2 == word.len();
                        break;
                    }
                    _ => {}
                }
            }
        } else {
            operands.push(word);
        }
        i += 1;
    }
    if scripts.is_empty() && found.is_empty() {
        // After a bare `-i`, an empty or `.suffix` first operand is BSD's
        // suffix and the script follows it; GNU would take it as a script
        // that runs nothing (`''`) or that it refuses (`.bak`).
        let script = match operands.as_slice() {
            [suffix, script, ..]
                if bare_in_place && (suffix.is_empty() || suffix.starts_with('.')) =>
            {
                Some(*script)
            }
            _ => operands.first().copied(),
        };
        scripts.extend(script.map(str::to_string));
    }
    (found, scripts.join("\n"), sandbox)
}

/// The commands a sed script runs: the text of every `e` command (`None` for
/// a bare `e`, which runs the pattern space) and the `e` flag of `s`.
///
/// GNU sed ends a regex at the first unescaped delimiter; BSD sed skips a
/// bracket expression first (`s/[/]/x/`). The script is read both ways and
/// what either reading runs counts. A sed refuses a script it cannot parse
/// before it runs any of it, so only a script that neither reading can follow
/// to the end is refused as a whole.
pub(super) fn sed_script_commands(script: &str) -> Found {
    sed_script_files(script).0
}

/// [`sed_script_commands`] and the files the script names: `(path, writes)`
/// for `w`/`W` and the `w` flag of `s` (writes) and `r`/`R` (reads).
pub(super) fn sed_script_files(script: &str) -> (Found, Vec<(String, bool)>) {
    let mut found = Found::new();
    let mut files = Vec::new();
    let mut readable = false;
    for brackets in [false, true] {
        let mut reader = SedReader {
            c: script.chars().collect(),
            i: 0,
            brackets,
            found: Found::new(),
            files: Vec::new(),
        };
        readable |= reader.script().is_some();
        for item in reader.found {
            if !found.contains(&item) {
                found.push(item);
            }
        }
        for file in reader.files {
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    if !readable {
        found.push(("a script the gate cannot read".to_string(), None));
    }
    (found, files)
}

struct SedReader {
    c: Vec<char>,
    i: usize,
    /// BSD sed: a bracket expression in a regex may hold the delimiter.
    brackets: bool,
    found: Found,
    files: Vec<(String, bool)>,
}

impl SedReader {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn skip_blanks(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }

    /// The rest of the line, without its line break.
    fn rest_of_line(&mut self) -> String {
        let start = self.i;
        while self.peek().is_some_and(|c| c != '\n') {
            self.i += 1;
        }
        self.c[start..self.i].iter().collect()
    }

    /// `a`/`i`/`c` text: to the end of the line, continued past a line break
    /// escaped by a backslash.
    fn skip_text(&mut self) {
        let mut escaped = false;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '\n' if escaped => escaped = false,
                '\n' => return,
                '\\' => escaped = !escaped,
                _ => escaped = false,
            }
        }
    }

    /// A label (`b`, `t`, `T`, `:`) or version (`v`): up to `;` or the line end.
    fn skip_label(&mut self) {
        while self.peek().is_some_and(|c| c != ';' && c != '\n') {
            self.i += 1;
        }
    }

    fn skip_digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
    }

    /// After the opening delimiter: up to the unescaped closing `delim`.
    /// A line break before it is an error, as it is to sed. In a `regex`
    /// read the BSD way, a bracket expression is skipped whole.
    fn delimited(&mut self, delim: char, regex: bool) -> Option<()> {
        loop {
            match self.peek()? {
                '\\' => {
                    self.peek_at(1)?;
                    self.i += 2;
                }
                '\n' => return None,
                '[' if regex && self.brackets => self.bracket()?,
                c if c == delim => {
                    self.i += 1;
                    return Some(());
                }
                _ => self.i += 1,
            }
        }
    }

    /// A bracket expression from its `[`: an optional `^`, a leading `]`
    /// that is literal, `[:class:]`, `[.coll.]` and `[=equiv=]` inside, up to
    /// the closing `]`. A backslash in it is literal.
    fn bracket(&mut self) -> Option<()> {
        self.i += 1;
        if self.peek() == Some('^') {
            self.i += 1;
        }
        if self.peek() == Some(']') {
            self.i += 1;
        }
        loop {
            match self.peek()? {
                '\n' => return None,
                ']' => {
                    self.i += 1;
                    return Some(());
                }
                '[' if matches!(self.peek_at(1), Some(':' | '.' | '=')) => {
                    let kind = self.peek_at(1)?;
                    self.i += 2;
                    while !(self.peek()? == kind && self.peek_at(1)? == ']') {
                        if self.peek()? == '\n' {
                            return None;
                        }
                        self.i += 1;
                    }
                    self.i += 2;
                }
                _ => self.i += 1,
            }
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.c.get(self.i + offset).copied()
    }

    /// A regex address (`/re/`, `\cREc`) with its `I`/`M` flags.
    fn regex_address(&mut self) -> Option<()> {
        let delim = if self.peek()? == '/' {
            '/'
        } else {
            self.i += 1;
            let delim = self.peek()?;
            if matches!(delim, '\n' | '\\') {
                return None;
            }
            delim
        };
        self.i += 1;
        self.delimited(delim, true)?;
        while matches!(self.peek(), Some('I' | 'M')) {
            self.i += 1;
        }
        Some(())
    }

    /// An optional address: a line number (`first~step`), `$` or a regex.
    fn address(&mut self) -> Option<()> {
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                self.skip_digits();
                if self.peek() == Some('~') {
                    self.i += 1;
                    self.skip_digits();
                }
            }
            Some('$') => self.i += 1,
            Some('/' | '\\') => self.regex_address()?,
            _ => {}
        }
        Some(())
    }

    /// The end of a command: blanks, then `;`, a line break, `}`, `#` or the end.
    fn command_end(&mut self) -> Option<()> {
        self.skip_blanks();
        match self.peek() {
            None | Some(';' | '\n' | '}' | '#') => Some(()),
            _ => None,
        }
    }

    fn script(&mut self) -> Option<()> {
        loop {
            while matches!(self.peek(), Some(' ' | '\t' | ';' | '\n' | '{' | '}')) {
                self.i += 1;
            }
            let Some(first) = self.peek() else {
                return Some(());
            };
            if first == '#' {
                self.rest_of_line();
                continue;
            }
            self.address()?;
            self.skip_blanks();
            if self.peek() == Some(',') {
                self.i += 1;
                self.skip_blanks();
                if matches!(self.peek(), Some('+' | '~')) {
                    self.i += 1;
                    self.skip_digits();
                } else {
                    self.address()?;
                }
            }
            self.skip_blanks();
            while self.peek() == Some('!') {
                self.i += 1;
                self.skip_blanks();
            }
            let command = self.peek()?;
            self.i += 1;
            match command {
                '{' | '}' => {}
                'e' => {
                    let text = self.rest_of_line();
                    let text = text.trim();
                    let command = (!text.is_empty()).then(|| text.to_string());
                    self.found.push(("e command".to_string(), command));
                }
                's' => self.substitute()?,
                'y' => {
                    let delim = self.peek()?;
                    if matches!(delim, '\n' | '\\') {
                        return None;
                    }
                    self.i += 1;
                    self.delimited(delim, false)?;
                    self.delimited(delim, false)?;
                    self.command_end()?;
                }
                'a' | 'i' | 'c' => self.skip_text(),
                'r' | 'R' | 'w' | 'W' => {
                    let file = self.rest_of_line();
                    let file = file.trim_start();
                    self.files
                        .push((file.to_string(), matches!(command, 'w' | 'W')));
                }
                'b' | 't' | 'T' | ':' | 'v' => self.skip_label(),
                'q' | 'Q' | 'l' | 'L' => {
                    self.skip_blanks();
                    self.skip_digits();
                    self.command_end()?;
                }
                '=' | 'd' | 'D' | 'g' | 'G' | 'h' | 'H' | 'n' | 'N' | 'p' | 'P' | 'x' | 'z'
                | 'F' => self.command_end()?,
                _ => return None,
            }
        }
    }

    /// `s/RE/REPLACEMENT/FLAGS`: an `e` among the flags runs the pattern
    /// space as a command.
    fn substitute(&mut self) -> Option<()> {
        let delim = self.peek()?;
        if matches!(delim, '\n' | '\\') {
            return None;
        }
        self.i += 1;
        self.delimited(delim, true)?;
        self.delimited(delim, false)?;
        let mut runs = false;
        while let Some(flag) = self.peek() {
            match flag {
                'g' | 'p' | 'i' | 'I' | 'm' | 'M' => self.i += 1,
                c if c.is_ascii_digit() => self.i += 1,
                'e' => {
                    runs = true;
                    self.i += 1;
                }
                'w' => {
                    self.i += 1;
                    let file = self.rest_of_line();
                    self.files.push((file.trim_start().to_string(), true));
                    break;
                }
                _ => break,
            }
        }
        if runs {
            self.found.push(("the e flag of s".to_string(), None));
        }
        self.command_end()
    }
}

// ---------------------------------------------------------------- tar ----

/// GNU and BSD tar long options that run a command their value names.
const TAR_EXEC_LONG: &[&str] = &[
    "use-compress-program",
    "checkpoint-action",
    "info-script",
    "new-volume-script",
    "rsh-command",
    "rmt-command",
    "to-command",
];

/// Other tar long options that are a prefix of one in [`TAR_EXEC_LONG`].
const TAR_OTHER_PREFIXES: &[&str] = &["checkpoint"];

/// Short options taking a value (attached or the next word) in GNU tar. Not
/// `-H` and `-L`: bsdtar's take none (`tar -cH --use-compress-program=CMD`),
/// and a letter left out only makes the scan see more options, never fewer.
const TAR_SHORT_VALUE: &str = "bCfFgIKNTVX";

/// Short options that run their value: `-I` (compression program) and `-F`
/// (volume script). bsdtar's `-I` is `-T` (a file of names) and runs
/// nothing; it is judged like GNU's all the same, since the gate cannot tell
/// which tar runs.
const TAR_SHORT_EXEC: &str = "IF";

/// Long options whose value is always the next word unless attached with `=`
/// (`--exclude -Ixyz` excludes the pattern `-Ixyz`). Exact names only: an
/// abbreviation is scanned without skipping its value.
const TAR_LONG_VALUE: &[&str] = &[
    "directory",
    "exclude",
    "exclude-from",
    "file",
    "files-from",
    "format",
    "group",
    "include",
    "mode",
    "newer",
    "newer-mtime",
    "options",
    "owner",
    "strip-components",
    "transform",
];

/// `checkpoint-action` values that run nothing.
const TAR_QUIET_ACTIONS: &[&str] = &[
    "bell", "dot", ".", "echo", "sleep", "ttyout", "totals", "wait",
];

/// The exec long option `name` (without `--`) spells, also abbreviated. An
/// ambiguous abbreviation counts: tar refuses it, so refusing it too costs
/// nothing.
fn tar_exec_long(name: &str) -> Option<&'static str> {
    if name.is_empty() || TAR_OTHER_PREFIXES.contains(&name) {
        return None;
    }
    TAR_EXEC_LONG.iter().copied().find(|o| o.starts_with(name))
}

fn tar_value(option: &str, value: Option<String>, found: &mut Found) {
    let value = value.unwrap_or_default();
    if option == "checkpoint-action" {
        if let Some(command) = value.strip_prefix("exec=") {
            let command = (!command.is_empty()).then(|| command.to_string());
            found.push(("--checkpoint-action=exec".to_string(), command));
        } else if !TAR_QUIET_ACTIONS.contains(&value.split('=').next().unwrap_or_default()) {
            found.push(("--checkpoint-action".to_string(), None));
        }
        return;
    }
    let shown = if option.len() == 1 {
        format!("-{option}")
    } else {
        format!("--{option}")
    };
    // An empty value is no command line to judge.
    found.push((shown, (!value.is_empty()).then_some(value)));
}

/// `tar` arguments: long options (also abbreviated), short clusters
/// (`-cIzstd`, `-I zstd`) and the old-style first word (`cIf zstd a.tar`).
fn tar_options(args: &[String]) -> Found {
    let mut found = Found::new();
    let mut i = 0;
    // Old style: the first word holds option letters; their values follow
    // as the next words, in order.
    if let Some(first) = args.first()
        && !first.starts_with('-')
        && !first.is_empty()
        && first.chars().all(|c| c.is_ascii_alphabetic())
    {
        i = 1;
        for letter in first.chars() {
            if TAR_SHORT_VALUE.contains(letter) {
                let value = args.get(i).cloned();
                i += 1;
                if TAR_SHORT_EXEC.contains(letter) {
                    tar_value(&letter.to_string(), value, &mut found);
                }
            }
        }
    }
    while i < args.len() {
        let word = args[i].as_str();
        if word == "--" {
            break;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            if let Some(option) = tar_exec_long(name) {
                let value = if attached.is_some() {
                    attached
                } else {
                    i += 1;
                    args.get(i).cloned()
                };
                tar_value(option, value, &mut found);
            } else if attached.is_none() && TAR_LONG_VALUE.contains(&name) {
                i += 1;
            }
        } else if word.len() > 1 && word.starts_with('-') {
            for (at, letter) in word[1..].char_indices() {
                if !TAR_SHORT_VALUE.contains(letter) {
                    continue;
                }
                let rest = &word[1 + at + letter.len_utf8()..];
                let value = if rest.is_empty() {
                    i += 1;
                    args.get(i).cloned()
                } else {
                    Some(rest.to_string())
                };
                if TAR_SHORT_EXEC.contains(letter) {
                    tar_value(&letter.to_string(), value, &mut found);
                }
                break;
            }
        }
        i += 1;
    }
    found
}

// ----------------------------------------------------------------- rg ----

/// `rg --pre COMMAND` runs COMMAND on every file it searches, `rg
/// --hostname-bin COMMAND` once for the hyperlink host name. The values of
/// `-e`/`--regexp` and `-f`/`--file` are patterns and pattern files.
fn rg_options(args: &[String]) -> Found {
    let mut found = Found::new();
    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        match word {
            "--" => break,
            "-e" | "--regexp" | "-f" | "--file" => i += 1,
            "--pre" | "--hostname-bin" => {
                i += 1;
                if let Some(value) = args.get(i).filter(|v| !v.is_empty()) {
                    found.push((word.to_string(), Some(value.clone())));
                }
            }
            _ => {
                for option in ["--pre", "--hostname-bin"] {
                    if let Some(value) = word
                        .strip_prefix(option)
                        .and_then(|v| v.strip_prefix('='))
                        .filter(|v| !v.is_empty())
                    {
                        found.push((option.to_string(), Some(value.to_string())));
                    }
                }
            }
        }
        i += 1;
    }
    found
}

// ------------------------------------------------------------- editors ----

/// Ex commands a `-c`/`+cmd`/`--cmd` value may hold without running anything
/// else: options, syntax and colours, searching for nothing, and leaving.
/// A write takes no file name here (`w ~/x` writes wherever it says).
const VIM_PLAIN_COMMANDS: &[&str] = &[
    "set",
    "se",
    "setlocal",
    "setl",
    "setglobal",
    "setg",
    "syntax",
    "syn",
    "filetype",
    "filet",
    "colorscheme",
    "colo",
    "nohlsearch",
    "noh",
    "w",
    "write",
    "wq",
    "x",
    "xit",
    "q",
    "quit",
    "qa",
    "qall",
    "wa",
    "wall",
    "wqa",
    "wqall",
    "xa",
    "xall",
    "update",
    "up",
];

/// Commands that write or reach a file and so may not take an argument.
const VIM_WRITES: &[&str] = &[
    "w", "write", "wq", "x", "xit", "wa", "wall", "wqa", "wqall", "xa", "xall", "update", "up",
];

/// What one vim `-c`/`+`/`--cmd` value runs.
enum VimRuns {
    /// Plain commands only.
    Nothing,
    /// The shell command of a lone `:!CMD`, judged like one.
    Shell(String),
    /// A command the gate cannot read.
    Unread,
}

/// What one vim `-c`/`+`/`--cmd` value runs: nothing for plain commands,
/// the shell command of a lone `:!CMD` to judge, and an unreadable command
/// for anything else —
/// `!` elsewhere, `system()`, `:execute`, `:call`, `:source`, `:terminal`,
/// the script interfaces, a backtick file name, `:make`, `:grep`, `:normal`.
fn vim_command(value: &str) -> VimRuns {
    let trimmed = value.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    if let Some(shell) = trimmed.strip_prefix('!') {
        let shell = shell.trim();
        // vim puts the previous command in for `!`, and `|` is part of it.
        if shell.is_empty() || shell.contains(['!', '\n']) {
            return VimRuns::Unread;
        }
        return VimRuns::Shell(shell.to_string());
    }
    if value.contains(['!', '`', '\n']) || value.contains("system") {
        return VimRuns::Unread;
    }
    for part in value.split('|') {
        let part = part.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
        let part = part.trim_end();
        if part.is_empty() || part == "$" || part.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        // A search (`+/pattern`), with nothing after its closing delimiter.
        if let Some(delim) = part.chars().next().filter(|c| matches!(c, '/' | '?')) {
            let body = &part[1..];
            let mut escaped = false;
            let end = body.char_indices().find(|&(_, c)| {
                let close = c == delim && !escaped;
                escaped = c == '\\' && !escaped;
                close
            });
            if end.is_none_or(|(at, _)| at + 1 == body.len()) {
                continue;
            }
            return VimRuns::Unread;
        }
        let name: String = part.chars().take_while(char::is_ascii_alphabetic).collect();
        let args = part[name.len()..].trim();
        let writes_a_file = VIM_WRITES.contains(&name.as_str()) && !args.is_empty();
        if writes_a_file || !VIM_PLAIN_COMMANDS.contains(&name.as_str()) {
            return VimRuns::Unread;
        }
        if VIM_SET_COMMANDS.contains(&name.as_str()) && !vim_set_is_plain(args) {
            return VimRuns::Unread;
        }
    }
    VimRuns::Nothing
}

/// The `:set` family.
const VIM_SET_COMMANDS: &[&str] = &["set", "se", "setlocal", "setl", "setglobal", "setg"];

/// Options a `:set` may change in a `-c`/`--cmd` value: display, search,
/// indentation and file format. Not here: every option holding an
/// expression or a command (`foldexpr`, `statusline`, `tabline`,
/// `indentexpr`, `formatexpr`, `shell`, `makeprg`, …) or a path vim loads
/// code or writes files from (`runtimepath`, `packpath`, `backupdir`,
/// `viminfofile`, `verbosefile`, …), and `modeline`, which lets the file
/// set options.
const VIM_PLAIN_OPTIONS: &[&str] = &[
    "number",
    "nu",
    "relativenumber",
    "rnu",
    "list",
    "wrap",
    "paste",
    "ignorecase",
    "ic",
    "smartcase",
    "scs",
    "hlsearch",
    "hls",
    "incsearch",
    "is",
    "expandtab",
    "et",
    "tabstop",
    "ts",
    "shiftwidth",
    "sw",
    "softtabstop",
    "sts",
    "autoindent",
    "ai",
    "smartindent",
    "si",
    "textwidth",
    "tw",
    "wrapmargin",
    "wm",
    "fileformat",
    "ff",
    "fileencoding",
    "fenc",
    "encoding",
    "enc",
    "filetype",
    "ft",
    "syntax",
    "syn",
    "background",
    "bg",
    "readonly",
    "ro",
    "modifiable",
    "ma",
    "binary",
    "bin",
    "endofline",
    "eol",
    "fixendofline",
    "fixeol",
    "bomb",
    "spell",
    "cursorline",
    "cul",
    "cursorcolumn",
    "cuc",
    "ruler",
    "ru",
    "laststatus",
    "ls",
    "showmode",
    "smd",
    "showcmd",
    "sc",
    "wrapscan",
    "ws",
    "magic",
    "compatible",
    "cp",
    "hidden",
    "hid",
    "undolevels",
    "ul",
    "more",
    "lazyredraw",
    "lz",
    "linebreak",
    "lbr",
    "scrolloff",
    "so",
    "termguicolors",
    "tgc",
    "title",
    "visualbell",
    "vb",
    "errorbells",
    "eb",
    "write",
    "swapfile",
    "swf",
    "backup",
    "bk",
    "writebackup",
    "wb",
    "undofile",
    "udf",
    "cindent",
    "cin",
    "foldenable",
    "fen",
    "foldlevel",
    "fdl",
    "foldmethod",
    "fdm",
    "conceallevel",
    "cole",
    "mouse",
];

/// Whether the words of a `:set` change only [`VIM_PLAIN_OPTIONS`]
/// (`ts=4 et`, `nonu`, `invlist`, `sw+=2`, `ft?`). A value naming
/// `expr` (`foldmethod=expr` evaluates `foldexpr`) or holding a backslash
/// (an escaped blank joins the next word) is not plain.
fn vim_set_is_plain(args: &str) -> bool {
    if args.is_empty() || args.contains('\\') {
        return false;
    }
    args.split_whitespace().all(|item| {
        let (name, value) = match item.find(['=', ':', '+', '-', '^']) {
            Some(at) => (&item[..at], Some(&item[at..])),
            None => (item.trim_end_matches(['!', '&', '?']), None),
        };
        let name = ["no", "inv"]
            .iter()
            .find_map(|p| {
                name.strip_prefix(p)
                    .filter(|n| VIM_PLAIN_OPTIONS.contains(n))
            })
            .unwrap_or(name);
        VIM_PLAIN_OPTIONS.contains(&name) && !value.is_some_and(|v| v.contains("expr"))
    })
}

/// vim and its relatives: `-c CMD`/`-cCMD`, `+CMD` and `--cmd CMD` run an
/// Ex command; `:!CMD` runs a shell command, judged like one, and every
/// other Ex command that can run one is refused (see [`vim_command`]).
/// Options that run commands from a file or standard input are refused
/// unread: `-S` (source), `-s` (keystrokes from a file), `-u`/`-U` with a
/// vimrc other than `NONE`/`NORC`/`DEFAULTS`, `-e`/`-E` and `ex` (Ex
/// commands from standard input), nvim's `-l` (a Lua script), and
/// `--remote-send`/`--remote-expr` (keys or an expression for a server).
fn vim_options(program: &str, args: &[String]) -> Found {
    let mut found = Found::new();
    let unread = |found: &mut Found, option: &str| found.push((option.to_string(), None));
    if super::tokenizer::exec_name(program) == "ex" {
        unread(&mut found, "Ex mode");
    }
    let nvim = super::tokenizer::exec_name(program) == "nvim";
    let judge = |found: &mut Found, option: &str, value: Option<&String>| match value {
        None => unread(found, option),
        Some(value) => match vim_command(value) {
            VimRuns::Nothing => {}
            VimRuns::Shell(command) => found.push((option.to_string(), Some(command))),
            VimRuns::Unread => unread(found, option),
        },
    };
    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        if word == "--" {
            break;
        }
        if let Some(command) = word.strip_prefix('+') {
            judge(&mut found, "+", Some(&command.to_string()));
        } else if let Some(long) = word.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            match name {
                "cmd" => {
                    let value = attached.or_else(|| {
                        i += 1;
                        args.get(i).cloned()
                    });
                    judge(&mut found, "--cmd", value.as_ref());
                }
                "remote-send" | "remote-expr" => unread(&mut found, word),
                _ => {}
            }
        } else if word.len() > 1 && word.starts_with('-') {
            for (at, letter) in word[1..].char_indices() {
                let rest = &word[1 + at + letter.len_utf8()..];
                let value = |i: &mut usize| {
                    if rest.is_empty() {
                        *i += 1;
                        args.get(*i).cloned()
                    } else {
                        Some(rest.to_string())
                    }
                };
                match letter {
                    'c' => {
                        let value = value(&mut i);
                        judge(&mut found, "-c", value.as_ref());
                        break;
                    }
                    'u' | 'U' => {
                        let value = value(&mut i);
                        if !value
                            .as_deref()
                            .is_some_and(|v| matches!(v, "NONE" | "NORC" | "DEFAULTS"))
                        {
                            unread(&mut found, &format!("-{letter}"));
                        }
                        break;
                    }
                    'S' | 's' | 'e' | 'E' => {
                        unread(&mut found, &format!("-{letter}"));
                        break;
                    }
                    'l' if nvim => {
                        unread(&mut found, "-l");
                        break;
                    }
                    _ => {}
                }
            }
        }
        i += 1;
    }
    found
}

/// emacs and emacsclient options that evaluate Lisp, call a function, load a
/// file, read another user's init file or start another program; also as the
/// unambiguous abbreviations emacs accepts. Refused unread: Lisp can run any
/// command.
const EMACS_EXEC_LONG: &[&str] = &[
    "eval",
    "execute",
    "funcall",
    "load",
    "script",
    "user",
    "init-directory",
    "alternate-editor",
];

fn emacs_options(args: &[String]) -> Found {
    let mut found = Found::new();
    for word in args {
        let word = word.as_str();
        if word == "--" {
            break;
        }
        // `-f`, `-l`, `-u`, `-x`; emacsclient's `-e` and `-a`.
        let refused = matches!(word, "-f" | "-l" | "-e" | "-u" | "-a" | "-x") || {
            let long = word
                .strip_prefix("--")
                .or_else(|| word.strip_prefix('-'))
                .unwrap_or_default();
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            name.len() >= 2 && EMACS_EXEC_LONG.iter().any(|o| o.starts_with(name))
        };
        if refused {
            found.push((word.to_string(), None));
        }
    }
    found
}
