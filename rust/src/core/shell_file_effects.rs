// SPDX-License-Identifier: Apache-2.0

//! Which files a shell command reads or rewrites, for bounce accounting.
//!
//! The bounce tracker books a correction when a lossy compressed view of a
//! file is followed by its full content, and cancels it when the file is
//! edited right after (read-before-edit). Agents read and edit through the
//! shell as well, so `ctx_shell` commands are classified here:
//!
//! * **full reads** — `cat`, `bat`/`batcat` (without `-r`), `less`, `more`
//!   whose output reaches the model unfiltered;
//! * **partial reads** — `head`, `tail`, `sed` without `-i`, `bat -r`, or a
//!   full reader whose output is piped onward: they never start or complete
//!   a correction;
//! * **edits** — `sed -i`, `perl -i`, output redirections (`>`, `>>`, `>|`,
//!   `N>`, `&>`), `tee`, `cp`/`mv`/`install` onto a path, and `apply_patch`
//!   patch bodies (`*** Update File:` …).
//!
//! The parse is deliberately conservative: duplications (`2>&1`, `>&2`),
//! `/dev/*` targets and targets built from expansions (`$VAR`, `` `cmd` ``)
//! are never edits, and anything not recognised is ignored rather than
//! guessed. A missed edit leaves today's behaviour; a wrongly reported edit
//! only cancels a provisional correction.

use crate::core::shell_allowlist::{Separator, segments_with_separators, strip_all_heredoc_bodies};

/// How much of a file a shell read delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellReadKind {
    /// The whole file, unfiltered.
    Full,
    /// A window or filtered slice of it.
    Partial,
}

/// The files one shell command reads and rewrites, paths as written
/// (relative paths joined onto a preceding `cd DIR`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ShellFileEffects {
    pub(crate) reads: Vec<(String, ShellReadKind)>,
    pub(crate) edits: Vec<String>,
}

/// Classify the file reads and edits of `command`.
#[must_use]
pub(crate) fn analyze(command: &str) -> ShellFileEffects {
    let mut effects = ShellFileEffects::default();
    // Patch bodies live inside heredocs or quoted arguments; scan them before
    // the heredoc bodies are stripped for segmenting.
    for path in apply_patch_targets(command) {
        push_unique(&mut effects.edits, path);
    }

    let stripped = strip_all_heredoc_bodies(command);
    let segments = segments_with_separators(&stripped);
    let mut cwd: Option<String> = None;
    for (segment, sep) in &segments {
        // A reader whose output feeds another command delivers a filtered
        // slice, not the file.
        let piped_out = *sep == Some(Separator::Pipe);

        let words = lex(segment);
        let mut argv: Vec<&Word> = Vec::new();
        let mut redirected_out = false;
        let mut iter = words.iter().peekable();
        while let Some(word) = iter.next() {
            match word {
                Word::Redirect { output, dup } => {
                    let target = iter.next();
                    if !*output {
                        continue;
                    }
                    redirected_out = true;
                    if *dup {
                        continue;
                    }
                    if let Some(Word::Text { text, plain_path }) = target
                        && *plain_path
                        && is_edit_target(text)
                    {
                        push_unique(&mut effects.edits, join(cwd.as_deref(), text));
                    }
                }
                Word::Text { .. } => argv.push(word),
            }
        }

        // Leading `NAME=value` assignments only set the environment.
        let argv: Vec<&str> = argv
            .iter()
            .filter_map(|w| match w {
                Word::Text { text, .. } => Some(text.as_str()),
                Word::Redirect { .. } => None,
            })
            .skip_while(|w| is_env_assignment(w))
            .collect();
        let Some((&bin, args)) = argv.split_first() else {
            continue;
        };
        let bin = bin.rsplit('/').next().unwrap_or(bin);

        match bin {
            // `lean-ctx -c 'CMD'` (the hook rewrite) and `sh -c 'CMD'` run CMD.
            "lean-ctx" | "sh" | "bash" | "zsh" | "dash" => {
                if let [flag, inner, ..] = args
                    && *flag == "-c"
                {
                    let nested = analyze(inner);
                    for (path, kind) in nested.reads {
                        push_read(&mut effects.reads, join(cwd.as_deref(), &path), kind);
                    }
                    for path in nested.edits {
                        push_unique(&mut effects.edits, join(cwd.as_deref(), &path));
                    }
                }
            }
            "cd" => {
                // Only a plain `cd DIR` moves later relative paths.
                if let [dir] = args
                    && !dir.starts_with('-')
                    && is_plain(dir)
                {
                    cwd = Some(join(cwd.as_deref(), dir));
                }
            }
            "cat" | "less" | "more" | "bat" | "batcat" => {
                if redirected_out {
                    // `cat a > b` copies; nothing reaches the model.
                    continue;
                }
                let (files, ranged) = reader_files(bin, args);
                let kind = if ranged || piped_out {
                    ShellReadKind::Partial
                } else {
                    ShellReadKind::Full
                };
                for file in files {
                    push_read(&mut effects.reads, join(cwd.as_deref(), file), kind);
                }
            }
            "head" | "tail" => {
                for file in head_tail_files(args) {
                    push_read(
                        &mut effects.reads,
                        join(cwd.as_deref(), file),
                        ShellReadKind::Partial,
                    );
                }
            }
            "sed" | "perl" => {
                let (in_place, files) = script_tool_files(bin, args);
                for file in files {
                    let path = join(cwd.as_deref(), file);
                    if in_place {
                        push_unique(&mut effects.edits, path);
                    } else if bin == "sed" && !redirected_out {
                        push_read(&mut effects.reads, path, ShellReadKind::Partial);
                    }
                }
            }
            "tee" => {
                for file in positional(args, &[]) {
                    if is_edit_target(file) {
                        push_unique(&mut effects.edits, join(cwd.as_deref(), file));
                    }
                }
            }
            "cp" | "mv" | "install" => {
                for target in copy_targets(args) {
                    push_unique(&mut effects.edits, join(cwd.as_deref(), &target));
                }
            }
            _ => {}
        }
    }
    effects
}

/// Paths an `apply_patch` body touches: `*** Update File:`, `*** Add File:`,
/// `*** Delete File:` and `*** Move to:` headers. Shared with the host-edit
/// hook, which receives Codex `apply_patch` calls as raw patch text.
#[must_use]
pub(crate) fn apply_patch_targets(text: &str) -> Vec<String> {
    if !text.contains("*** Begin Patch") {
        return Vec::new();
    }
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let path = [
            "*** Update File:",
            "*** Add File:",
            "*** Delete File:",
            "*** Move to:",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix));
        if let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) {
            push_unique(&mut paths, path.to_string());
        }
    }
    paths
}

fn push_unique(list: &mut Vec<String>, path: String) {
    if !list.contains(&path) {
        list.push(path);
    }
}

fn push_read(reads: &mut Vec<(String, ShellReadKind)>, path: String, kind: ShellReadKind) {
    if path.starts_with("/dev/") || path == "-" {
        return;
    }
    if !reads.iter().any(|(p, k)| *p == path && *k == kind) {
        reads.push((path, kind));
    }
}

fn join(cwd: Option<&str>, path: &str) -> String {
    match cwd {
        Some(dir) if !path.starts_with('/') && !path.starts_with('~') => {
            format!("{}/{path}", dir.trim_end_matches('/'))
        }
        _ => path.to_string(),
    }
}

/// A word that is a literal path: no expansion, glob or substitution.
fn is_plain(word: &str) -> bool {
    !word.is_empty() && !word.contains(['$', '`', '*', '?', '[', '{', '(', ')', '<', '>'])
}

fn is_edit_target(word: &str) -> bool {
    is_plain(word) && word != "-" && !word.starts_with("/dev/") && !word.starts_with('-')
}

fn is_env_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Positional arguments: everything that is not a flag, minus the values of
/// `takes_value` flags; `--` ends option parsing.
fn positional<'a>(args: &[&'a str], takes_value: &[&str]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut iter = args.iter();
    while let Some(&arg) = iter.next() {
        if arg == "--" {
            out.extend(iter.copied());
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            if takes_value.contains(&arg) {
                iter.next();
            }
            continue;
        }
        out.push(arg);
    }
    out
}

/// Files of a full-content reader and whether the read is a range.
fn reader_files<'a>(bin: &str, args: &[&'a str]) -> (Vec<&'a str>, bool) {
    match bin {
        "bat" | "batcat" => {
            const VALUE_FLAGS: &[&str] = &[
                "-l",
                "--language",
                "-r",
                "--line-range",
                "-H",
                "--highlight-line",
                "-m",
                "--map-syntax",
                "--style",
                "--theme",
                "--tabs",
                "--wrap",
                "--terminal-width",
                "--color",
                "--decorations",
                "--paging",
                "--italic-text",
                "--file-name",
                "--diff-context",
            ];
            let ranged = args
                .iter()
                .any(|a| *a == "-r" || *a == "--line-range" || a.starts_with("--line-range="));
            (positional(args, VALUE_FLAGS), ranged)
        }
        // `less +N` / `less -pPATTERN` are positioning options, not ranges:
        // the pager still holds the whole file.
        "less" | "more" => (
            args.iter()
                .copied()
                .filter(|a| !a.starts_with('-') && !a.starts_with('+'))
                .collect(),
            false,
        ),
        _ => (positional(args, &[]), false),
    }
}

fn head_tail_files<'a>(args: &[&'a str]) -> Vec<&'a str> {
    positional(args, &["-n", "-c", "--lines", "--bytes"])
        .into_iter()
        .filter(|a| !a.starts_with('+'))
        .collect()
}

/// `sed`/`perl`: whether the run edits in place, and its file operands.
///
/// A short-flag cluster containing `i` is in-place (GNU/BSD `sed -i[SUFFIX]`,
/// `perl -pi`); characters after the `i` are its backup suffix. `-e`/`-f`
/// (`sed`) and `-e`/`-E` (`perl`) carry the script; without one the first
/// operand is the script (`sed`) or program file (`perl`).
fn script_tool_files<'a>(bin: &str, args: &[&'a str]) -> (bool, Vec<&'a str>) {
    let script_flags: &[char] = if bin == "perl" {
        &['e', 'E']
    } else {
        &['e', 'f']
    };
    let mut in_place = false;
    let mut has_script = false;
    let mut operands = Vec::new();
    let mut iter = args.iter();
    while let Some(&arg) = iter.next() {
        if arg == "--" {
            operands.extend(iter.copied());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            if long == "in-place" || long.starts_with("in-place=") {
                in_place = true;
            } else if long == "expression" || long == "file" {
                has_script = true;
                iter.next();
            } else if long.starts_with("expression=") || long.starts_with("file=") {
                has_script = true;
            }
            continue;
        }
        if let Some(cluster) = arg.strip_prefix('-')
            && !cluster.is_empty()
        {
            // BSD `sed -i ''`: the empty suffix arrives as its own word.
            if bin == "sed" && cluster == "i" && iter.as_slice().first() == Some(&"") {
                iter.next();
            }
            for (idx, ch) in cluster.char_indices() {
                if ch == 'i' {
                    in_place = true;
                    break;
                }
                if script_flags.contains(&ch) {
                    has_script = true;
                    if idx + ch.len_utf8() == cluster.len() {
                        iter.next();
                    }
                    break;
                }
                if ch == 'l' && bin == "sed" {
                    if idx + 1 == cluster.len() {
                        iter.next();
                    }
                    break;
                }
            }
            continue;
        }
        operands.push(arg);
    }
    if !has_script && !operands.is_empty() {
        operands.remove(0);
    }
    (
        in_place,
        operands.into_iter().filter(|a| is_plain(a)).collect(),
    )
}

/// Destinations of `cp`/`mv`/`install`. With a directory destination the
/// file lands at `DIR/basename(src)`; whether `DEST` is a directory is not
/// known here, so both spellings are reported (a path that does not exist
/// cancels nothing).
fn copy_targets(args: &[&str]) -> Vec<String> {
    const VALUE_FLAGS: &[&str] = &[
        "-S", "--suffix", "-m", "--mode", "-o", "--owner", "-g", "--group",
    ];
    let mut target_dir: Option<&str> = None;
    let mut iter = args.iter();
    let mut rest = Vec::new();
    while let Some(&arg) = iter.next() {
        if arg == "-t" || arg == "--target-directory" {
            target_dir = iter.next().copied();
        } else if let Some(dir) = arg.strip_prefix("--target-directory=") {
            target_dir = Some(dir);
        } else {
            rest.push(arg);
        }
    }
    let operands: Vec<&str> = positional(&rest, VALUE_FLAGS)
        .into_iter()
        .filter(|a| is_edit_target(a))
        .collect();
    let basename = |src: &str| {
        src.trim_end_matches('/')
            .rsplit('/')
            .next()
            .map(str::to_string)
    };
    let mut out = Vec::new();
    if let Some(dir) = target_dir.filter(|d| is_edit_target(d)) {
        for src in &operands {
            if let Some(name) = basename(src) {
                out.push(format!("{}/{name}", dir.trim_end_matches('/')));
            }
        }
        return out;
    }
    let Some((dest, sources)) = operands.split_last() else {
        return out;
    };
    if sources.is_empty() {
        return out;
    }
    out.push((*dest).to_string());
    for src in sources {
        if let Some(name) = basename(src) {
            out.push(format!("{}/{name}", dest.trim_end_matches('/')));
        }
    }
    out
}

/// One lexical word of a command segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Word {
    /// A word with quotes removed; `plain_path` is false when it contained an
    /// unquoted expansion we cannot resolve.
    Text { text: String, plain_path: bool },
    /// A redirection operator; `output` for `>`-family, `dup` for `>&N`.
    Redirect { output: bool, dup: bool },
}

/// Quote-aware lexer that also splits unquoted redirection operators off
/// their neighbours (`echo hi>out`, `2>&1`, `&>log`). A quoted `">"` stays a
/// plain word.
fn lex(segment: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut plain = true;
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = segment.chars().collect();
    let mut i = 0;

    let flush =
        |words: &mut Vec<Word>, current: &mut String, started: &mut bool, plain: &mut bool| {
            if *started {
                words.push(Word::Text {
                    text: std::mem::take(current),
                    plain_path: *plain,
                });
            }
            *started = false;
            *plain = true;
        };

    while i < chars.len() {
        let c = chars[i];
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                current.push(c);
            }
            i += 1;
            continue;
        }
        if in_double {
            match c {
                '"' => in_double = false,
                '\\' if matches!(chars.get(i + 1), Some('$' | '`' | '"' | '\\')) => {
                    current.push(chars[i + 1]);
                    i += 1;
                }
                '$' | '`' => {
                    plain = false;
                    current.push(c);
                }
                _ => current.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                started = true;
            }
            '"' => {
                in_double = true;
                started = true;
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    current.push(next);
                    started = true;
                    i += 1;
                }
            }
            c if c.is_whitespace() => flush(&mut words, &mut current, &mut started, &mut plain),
            '>' | '<' if chars.get(i + 1) != Some(&'(') => {
                // An all-digit (or `&`) word directly before the operator is
                // its file descriptor, not an argument.
                let fd_prefix = started
                    && !current.is_empty()
                    && (current.chars().all(|ch| ch.is_ascii_digit()) || current == "&");
                if fd_prefix {
                    current.clear();
                    started = false;
                    plain = true;
                } else {
                    flush(&mut words, &mut current, &mut started, &mut plain);
                }
                let output = c == '>';
                let mut j = i + 1;
                // `>>`, `>|`, `<<`, `<<<`, `<<-`, `<>`.
                while j < chars.len() && matches!(chars[j], '>' | '<' | '|' | '-') {
                    if chars[j] == '-' && !(c == '<' && chars[j - 1] == '<') {
                        break;
                    }
                    j += 1;
                }
                let dup = chars.get(j) == Some(&'&');
                if dup {
                    j += 1;
                }
                words.push(Word::Redirect { output, dup });
                i = j;
                continue;
            }
            '$' | '`' | '*' | '?' => {
                plain = false;
                started = true;
                current.push(c);
            }
            _ => {
                started = true;
                current.push(c);
            }
        }
        i += 1;
    }
    flush(&mut words, &mut current, &mut started, &mut plain);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits(cmd: &str) -> Vec<String> {
        analyze(cmd).edits
    }

    fn reads(cmd: &str) -> Vec<(String, ShellReadKind)> {
        analyze(cmd).reads
    }

    use ShellReadKind::{Full, Partial};

    const NO_EDITS: [String; 0] = [];
    const NO_READS: [(String, ShellReadKind); 0] = [];

    #[test]
    fn sed_in_place_is_an_edit() {
        assert_eq!(edits("sed -i 's/a/b/' src/a.rs"), ["src/a.rs"]);
        assert_eq!(edits("sed -i.bak -e 's/a/b/' a.rs b.rs"), ["a.rs", "b.rs"]);
        assert_eq!(edits("sed -i '' 's/a/b/' 'my file.rs'"), ["my file.rs"]);
        assert_eq!(edits("sed -Ei 's/a/b/g' a.rs"), ["a.rs"]);
        assert_eq!(edits("sed --in-place -e 's/x/y/' a.rs"), ["a.rs"]);
        assert_eq!(edits("sed 's/a/b/' a.rs"), NO_EDITS);
    }

    #[test]
    fn sed_without_in_place_is_a_partial_read() {
        assert_eq!(
            reads("sed -n '10,20p' src/a.rs"),
            [("src/a.rs".into(), Partial)]
        );
        assert_eq!(analyze("sed -n '10,20p' src/a.rs").edits, NO_EDITS);
    }

    #[test]
    fn perl_in_place_is_an_edit() {
        assert_eq!(edits("perl -0pi -e 's/a/b/' a.rs"), ["a.rs"]);
        assert_eq!(edits("perl -pi -e 's/a/b/' a.rs b.rs"), ["a.rs", "b.rs"]);
        assert_eq!(edits("perl -ne 'print' a.rs"), NO_EDITS);
    }

    #[test]
    fn output_redirections_are_edits() {
        assert_eq!(edits("echo hi > out.txt"), ["out.txt"]);
        assert_eq!(edits("echo hi >> log.txt"), ["log.txt"]);
        assert_eq!(edits("echo hi>out.txt"), ["out.txt"]);
        assert_eq!(edits("printf x >| clobber.txt"), ["clobber.txt"]);
        assert_eq!(edits("cmd &> both.log"), ["both.log"]);
        assert_eq!(edits("cmd 1> one.txt"), ["one.txt"]);
        assert_eq!(edits("echo hi > \"with space.txt\""), ["with space.txt"]);
        assert_eq!(edits("cat > new.rs <<'EOF'\nfn main() {}\nEOF"), ["new.rs"]);
    }

    #[test]
    fn stream_plumbing_is_never_an_edit() {
        assert_eq!(edits("cargo build 2>/dev/null"), NO_EDITS);
        assert_eq!(edits("cargo build 2>&1"), NO_EDITS);
        assert_eq!(edits("echo oops >&2"), NO_EDITS);
        assert_eq!(edits("cmd > /dev/null 2>&1"), NO_EDITS);
        assert_eq!(edits("cmd &>/dev/null"), NO_EDITS);
        assert_eq!(edits("echo hi > \"$OUT\""), NO_EDITS);
        assert_eq!(edits("echo hi > $(mktemp)"), NO_EDITS);
        assert_eq!(edits("grep '>' src/a.rs"), NO_EDITS);
        assert_eq!(edits("diff <(sort a) >(cat)"), NO_EDITS);
        assert_eq!(edits("wc -l < input.txt"), NO_EDITS);
    }

    #[test]
    fn tee_targets_are_edits() {
        assert_eq!(edits("echo x | tee out.txt"), ["out.txt"]);
        assert_eq!(edits("echo x | tee -a a.txt b.txt"), ["a.txt", "b.txt"]);
        assert_eq!(edits("echo x | tee /dev/stderr"), NO_EDITS);
    }

    #[test]
    fn copy_and_move_destinations_are_edits() {
        assert_eq!(edits("cp new.rs src/a.rs"), ["src/a.rs", "src/a.rs/new.rs"]);
        assert_eq!(
            edits("mv -f tmp.rs src/a.rs"),
            ["src/a.rs", "src/a.rs/tmp.rs"]
        );
        assert_eq!(edits("cp -t src a.rs b.rs"), ["src/a.rs", "src/b.rs"]);
        assert_eq!(edits("cp only_one"), NO_EDITS);
    }

    #[test]
    fn chains_and_cd_are_followed() {
        assert_eq!(
            edits("cd rust && sed -i 's/a/b/' src/a.rs; echo done > /tmp/x.log"),
            ["rust/src/a.rs", "/tmp/x.log"]
        );
        assert_eq!(edits("make || echo fail > err.txt"), ["err.txt"]);
        assert_eq!(edits("lean-ctx -c \"sed -i 's/a/b/' a.rs\""), ["a.rs"]);
        assert_eq!(edits("cd sub && bash -c 'echo x > o.txt'"), ["sub/o.txt"]);
    }

    #[test]
    fn apply_patch_bodies_are_edits() {
        let cmd = "apply_patch <<'EOF'\n*** Begin Patch\n*** Update File: src/a.rs\n@@\n-a\n+b\n*** Add File: src/new.rs\n+x\n*** Delete File: old.rs\n*** Update File: src/m.rs\n*** Move to: src/n.rs\n*** End Patch\nEOF";
        assert_eq!(
            edits(cmd),
            ["src/a.rs", "src/new.rs", "old.rs", "src/m.rs", "src/n.rs"]
        );
        // Diff lines inside the body are not redirections.
        assert_eq!(
            edits("apply_patch '*** Begin Patch\n*** Update File: a.rs\n+x > y\n*** End Patch'"),
            ["a.rs"]
        );
    }

    #[test]
    fn full_and_partial_reads() {
        assert_eq!(reads("cat src/a.rs"), [("src/a.rs".into(), Full)]);
        assert_eq!(
            reads("cat -n a.rs b.rs"),
            [("a.rs".into(), Full), ("b.rs".into(), Full)]
        );
        assert_eq!(reads("bat --paging never a.rs"), [("a.rs".into(), Full)]);
        assert_eq!(reads("less +40 a.rs"), [("a.rs".into(), Full)]);
        assert_eq!(reads("bat -r 10:20 a.rs"), [("a.rs".into(), Partial)]);
        assert_eq!(reads("head -n 50 a.rs"), [("a.rs".into(), Partial)]);
        assert_eq!(reads("tail -20 a.rs"), [("a.rs".into(), Partial)]);
        assert_eq!(reads("cat a.rs | head -5"), [("a.rs".into(), Partial)]);
        assert_eq!(reads("cat a.rs | grep x"), [("a.rs".into(), Partial)]);
        assert_eq!(
            reads("cd docs && cat guide.md"),
            [("docs/guide.md".into(), Full)]
        );
        assert_eq!(reads("cat a.rs > b.rs"), NO_READS);
        assert_eq!(reads("cat"), NO_READS);
        assert_eq!(reads("ls -la"), NO_READS);
    }
}
